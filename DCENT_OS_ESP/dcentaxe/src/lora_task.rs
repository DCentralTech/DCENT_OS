// DCENT_axe — on-board SX1262 LoRa radio task + $DCM mesh integration
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Wires the committed `dcentaxe-lora` crate (SX1262 driver + $DCM mesh) into the
//! `dcentaxe` binary as its OWN FreeRTOS task, so a DCENT_axe board joins the mesh
//! natively. Compiled ONLY under the default-OFF `lora` feature — a non-LoRa SKU
//! never sees this module (byte-identical image; "no lying UI").
//!
//! ## Invariants (mirrored from `mqtt.rs` — do NOT regress)
//! - **Own task.** The radio runs on its own thread and NEVER blocks the mining or
//!   safety loops. Every SX1262 SPI op (blocking) happens here, not on a hot path.
//! - **Fail-soft.** A radio init failure logs ONCE, leaves `present=false`, and
//!   returns — mining continues. No panic, no retry storm.
//! - **No HTTP handler.** The MCP tools fold into the single `/mcp` endpoint
//!   (`crate::mcp`); the dashboard panel is appended to the existing `/` response.
//!   `MAX_URI_HANDLERS` is unchanged.
//! - **Owner-gated control.** `lora_send_beacon` is routed through
//!   `authorize_mcp_control()` in `crate::mcp` before it reaches [`request_beacon`].
//! - **Region duty-bounded.** Every transmit (task beacon OR MCP-requested) passes
//!   its estimated airtime through the shared [`DutyCycle`] governor first.
//!
//! ## ⚠️ Integration seam — NEEDS-VERIFY (esp-idf only, not host-tested)
//! This module is esp-idf-gated (built only for the xtensa firmware target under
//! `--features …,lora`) and is NOT exercised by host tests. Treat it — like
//! `dcentaxe-lora::esp_hal` and `dcentaxe-hal::lora_pins::open_lora_bus` — as the
//! documented bring-up entry point, to be verified on hardware at wire-up. The
//! host-proven pieces are the pin-map table test and the MCP access-class contract.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{error, info, warn};
use serde_json::{json, Value};

use dcentaxe_hal::lora_pins::LoraBus;
use dcentaxe_lora::config::MeshConfig;
use dcentaxe_lora::duty::{DutyCycle, ModulationParams};
use dcentaxe_lora::esp_hal::{EspInputPin, EspOutputPin, EspSpiBus};
use dcentaxe_lora::flood::{RebroadcastPlanner, RxAction};
use dcentaxe_lora::gate::{CommandGate, ControlLimits, GateOutcome, MeshControl};
use dcentaxe_lora::mcp::{LoraStatusResponse, MeshPeerInfo, MeshPeersResponse, SendBeaconResponse};
use dcentaxe_lora::mesh::{
    BlockFound, Identify, MeshFrame, MeshKind, NodeId, PeerTable, Telemetry,
};
use dcentaxe_lora::relay::TxQueue;
use dcentaxe_lora::sx1262::{Region, Sx1262};

use crate::shared::SharedState;

/// Default region. NA 915 MHz is the fork-plan default (an EU 868 build variant
/// flips this); the driver + duty governor clamp to the legal band either way.
const LORA_REGION: Region = Region::Na915;

/// Radio task stack. The RX path decodes a ≤255-byte `$DCM` frame and touches
/// serde_json only for the shared snapshot — comfortable headroom, matching MQTT.
const LORA_TASK_STACK: usize = 8 * 1024;

/// Telemetry beacon cadence. INFREQUENT by design — the duty governor is the hard
/// bound, this is just the nominal spacing so we do not spam the mesh.
const TLM_BEACON_INTERVAL_S: u64 = 300;

/// RX poll cadence when idle. Short enough to be responsive to DIO1, long enough
/// not to busy-spin the CPU away from mining.
const RX_POLL_MS: u64 = 200;

/// Bounded wait for a TxDone IRQ after keying the radio (DIO1 poll iterations).
const TX_DONE_POLL_BUDGET: u32 = 250; // ~250 × RX_POLL_MS/… bounded, never infinite

/// Listen-before-talk carrier-sense threshold (dBm). If the instantaneous
/// channel RSSI is at or above this, the channel is considered busy and TX is
/// deferred a cycle — collision avoidance for a dense always-on swarm. Chosen a
/// few dB above a typical sub-GHz noise floor; tune per deployment.
const LBT_RSSI_THRESHOLD_DBM: i16 = -90;

// ───────────────────────────────────────────────────────────────────────────
// Shared snapshot (read by the MCP tools + /api/system/info; written by the task)
// ───────────────────────────────────────────────────────────────────────────

/// The live radio/mesh snapshot shared between the radio task (writer) and the
/// HTTP/MCP threads (readers). The task owns the `Sx1262` itself EXCLUSIVELY; the
/// readers only ever touch this snapshot, never the radio — so an MCP read can
/// never block on or corrupt a live SPI transaction (the correct concurrency
/// model for "own task, never blocking").
struct LoraShared {
    /// Radio-init proven on hardware (cold-boot `begin()` succeeded). Gates the
    /// dashboard panel + `/api/system/info` `lora.present`.
    present: bool,
    region: Region,
    /// Honest proof-ladder lifecycle: "uninitialized" | "standby" | "rx" | "tx" |
    /// "fault". Never an optimistic default.
    radio_state: &'static str,
    node_id: NodeId,
    /// Per-source originator sequence (wraps at 256).
    seq: u8,
    last_beacon_unix_ms: Option<u64>,
    last_rx_rssi_dbm: Option<i16>,
    last_rx_snr_db: Option<f32>,
    peers: PeerTable,
    duty: DutyCycle,
    /// Outbound frames queued by the task or an MCP `lora_send_beacon`, drained by
    /// the radio task. Priority-aware so a BLK never yields its slot to a TLM.
    tx_queue: TxQueue,
    /// Managed-flood rebroadcast planner: SNR-weighted contention window +
    /// per-node jitter + cancel-on-duplicate + role gating (supersedes the old
    /// naive dedup-only `RelayCache` flood — mesh maturity audit #1/#2).
    planner: RebroadcastPlanner,
    /// Owner-command gate: HMAC auth + anti-replay + safe-clamp for an inbound
    /// air `Command` (was previously blindly reflooded, never authenticated).
    gate: CommandGate,
    /// Rising-edge tracker for the block-found proxy (nonces_found delta).
    last_nonces_found: u64,
}

static LORA: Mutex<Option<LoraShared>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn region_str(r: Region) -> &'static str {
    match r {
        Region::Eu868 => "eu868",
        Region::Na915 => "na915",
    }
}

/// Format a difficulty magnitude the way the mesh telemetry / Raven `best_diff`
/// field expects (e.g. "1.05k", "21.3M") — compact and human-readable.
fn format_diff(d: f64) -> String {
    if !d.is_finite() || d <= 0.0 {
        return "0".to_string();
    }
    const UNITS: [(&str, f64); 5] = [("P", 1e15), ("T", 1e12), ("G", 1e9), ("M", 1e6), ("k", 1e3)];
    for (suffix, scale) in UNITS {
        if d >= scale {
            return format!("{:.2}{}", d / scale, suffix);
        }
    }
    format!("{d:.0}")
}

// ───────────────────────────────────────────────────────────────────────────
// Public read/telemetry surface (consumed by crate::mcp + crate::api)
// ───────────────────────────────────────────────────────────────────────────

/// `true` once the radio cold-boot (`begin()`) succeeded on hardware. Gates the
/// dashboard panel + `/api/system/info` `lora.present` — until proven, the panel
/// first-paints "Radio pending" and never claims a live radio (honesty rule).
pub fn lora_present() -> bool {
    LORA.lock()
        .map(|g| g.as_ref().map(|s| s.present).unwrap_or(false))
        .unwrap_or(false)
}

/// `lora_status` MCP tool body — radio lifecycle, region, last beacon, last-RX
/// link quality, and mesh peer count, read from the live snapshot.
pub fn status_json() -> Value {
    let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let resp = match g.as_ref() {
        Some(s) => LoraStatusResponse {
            region: region_str(s.region).to_string(),
            radio_state: s.radio_state.to_string(),
            last_beacon_unix_ms: s.last_beacon_unix_ms,
            last_rx_rssi_dbm: s.last_rx_rssi_dbm,
            last_rx_snr_db: s.last_rx_snr_db,
            mesh_peer_count: s.peers.len() as u32,
        },
        None => LoraStatusResponse {
            region: region_str(LORA_REGION).to_string(),
            radio_state: "uninitialized".to_string(),
            last_beacon_unix_ms: None,
            last_rx_rssi_dbm: None,
            last_rx_snr_db: None,
            mesh_peer_count: 0,
        },
    };
    serde_json::to_value(resp).unwrap_or_else(|_| json!({}))
}

/// `get_mesh_peers` MCP tool body — discovered peers, freshest first, from the
/// live [`PeerTable`].
pub fn mesh_peers_json() -> Value {
    let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let peers: Vec<MeshPeerInfo> = match g.as_ref() {
        Some(s) => s
            .peers
            .peers_by_recency()
            .iter()
            .map(|p| MeshPeerInfo {
                node_id: p.node_id.to_hex(),
                device_model: p.device_model.clone().unwrap_or_default(),
                asic_model: p.asic_model.clone().unwrap_or_default(),
                last_seen_unix_ms: p.last_seen,
                rssi_dbm: p.rssi_dbm,
                snr_db: p.snr_db as f32,
            })
            .collect(),
        None => Vec::new(),
    };
    serde_json::to_value(MeshPeersResponse { peers }).unwrap_or_else(|_| json!({ "peers": [] }))
}

/// `lora_send_beacon` MCP tool body (OWNER-CONTROL — the caller in `crate::mcp`
/// MUST have already passed `authorize_mcp_control()`). Builds the requested
/// beacon from live miner state, checks the region duty governor, and enqueues it
/// for the radio task. Returns HONEST proof-ladder semantics: `queued:false` with
/// a `reason` when the radio is unavailable or the duty budget is spent — never an
/// optimistic "sent" (LoRa broadcast is unacknowledged anyway).
pub fn request_beacon(state: &SharedState, kind: &str, message: Option<String>) -> Value {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else {
        return beacon_response(false, 0, Some("radio_unavailable"));
    };
    if !s.present {
        return beacon_response(false, 0, Some("radio_unavailable"));
    }

    // Build the frame body from live miner state (config/telemetry/stats).
    let Some(kind) = build_beacon_kind(state, kind, message) else {
        return beacon_response(false, 0, Some("unsupported_beacon_kind"));
    };
    let frame = MeshFrame::originate(s.node_id, s.seq, kind);
    let bytes = match frame.encode() {
        Ok(b) => b,
        Err(_) => return beacon_response(false, 0, Some("encode_failed")),
    };

    // Region airtime governor: refuse honestly when the budget/dwell is spent.
    let airtime = ModulationParams::default().airtime_ms(bytes.len());
    if !s.duty.try_acquire(airtime, now_ms()) {
        return beacon_response(false, bytes.len() as u16, Some("duty_budget"));
    }

    s.seq = s.seq.wrapping_add(1);
    s.tx_queue.push(frame);
    beacon_response(true, bytes.len() as u16, None)
}

fn beacon_response(queued: bool, frame_bytes: u16, reason: Option<&str>) -> Value {
    let resp = SendBeaconResponse {
        queued,
        frame_bytes,
        reason: reason.map(|r| r.to_string()),
    };
    serde_json::to_value(resp).unwrap_or_else(|_| json!({ "queued": queued }))
}

/// Build a typed mesh body for an MCP-requested beacon from live miner state.
/// Returns `None` for an unknown beacon kind.
fn build_beacon_kind(state: &SharedState, kind: &str, message: Option<String>) -> Option<MeshKind> {
    match kind {
        "telemetry" => Some(MeshKind::Telemetry(build_telemetry(state))),
        "block_found" => Some(MeshKind::BlockFound(build_block_found(state))),
        "identify" => Some(MeshKind::Identify(build_identify(state))),
        // Free-text custom broadcast rides an Ack frame (its param is an arbitrary
        // string) — the lightest reuse of the existing grammar.
        "custom" => Some(MeshKind::Ack(message.unwrap_or_default())),
        _ => None,
    }
}

fn build_telemetry(state: &SharedState) -> Telemetry {
    let snap = state
        .stats
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot();
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    Telemetry {
        hashrate_ghs: snap.hashrate_5m_ghs,
        chip_temp_c: telem.chip_temp_c as f64,
        power_w: telem.power_w as f64,
        shares_accepted: snap.accepted_shares,
        shares_rejected: snap.rejected_shares,
        best_diff: format_diff(telem.best_diff_ever),
        // block_height is not wired as a live signal on axe (solo-share firmware);
        // 0 = unknown rather than a fabricated height.
        block_height: 0,
    }
}

fn build_block_found(state: &SharedState) -> BlockFound {
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    BlockFound {
        block_height: 0,
        best_diff: format_diff(telem.best_diff_ever),
    }
}

fn build_identify(state: &SharedState) -> Identify {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board = config.board_config();
    Identify {
        device_model: board.device_model.clone(),
        asic_model: board.asic_model.clone(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// /api/system/info view (feature-gated; omitted entirely when lora is OFF)
// ───────────────────────────────────────────────────────────────────────────

/// Build the additive `lora` object for `/api/system/info`. Present ⇒ the
/// dashboard renders the mesh panel; otherwise the panel first-paints
/// "Radio pending". Called ONLY from the `#[cfg(feature = "lora")]` construction
/// site so a non-LoRa image never emits the key (byte-identical wire format).
pub fn system_info_view() -> Option<crate::api_system_info::LoraInfoView> {
    let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.as_ref()?;
    Some(crate::api_system_info::LoraInfoView {
        present: s.present,
        region: region_str(s.region),
        radio_state: s.radio_state,
        peer_count: s.peers.len() as u32,
        last_beacon_unix_ms: s.last_beacon_unix_ms,
        last_rx_rssi_dbm: s.last_rx_rssi_dbm,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Radio task
// ───────────────────────────────────────────────────────────────────────────

/// Derive the mesh node id from the low 4 bytes of the eFuse MAC (Meshtastic-style
/// 32-bit id; matches the `dcentaxe-lora` README node-identity contract).
fn node_id_from_mac() -> NodeId {
    let mut mac = [0u8; 6];
    // SAFETY: esp_efuse_mac_get_default fills exactly 6 bytes into our buffer.
    let err = unsafe { esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    let id = if err == esp_idf_svc::sys::ESP_OK {
        u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]])
    } else {
        0
    };
    NodeId(id)
}

/// Spawn the SX1262 radio task IFF a LoRa bus was acquired. Safe to call at boot;
/// the caller (main) only builds the bus + calls this under `#[cfg(feature =
/// "lora")]`. Fail-soft: a spawn/init failure logs and returns — mining continues.
pub fn spawn(state: SharedState, bus: LoraBus<'static>) {
    let node_id = node_id_from_mac();
    // Mesh runtime config. TODO(wiring): source this from the persisted binary
    // Config / NVS (region, relay role, owner key) instead of the default. The
    // default is FAIL-CLOSED (no owner key ⇒ the gate refuses all air control)
    // and role=Router (a mains-powered axe is a backbone relay), which is the
    // correct safe starting posture until the operator provisions a key.
    let cfg = MeshConfig::default();
    // Publish the pre-init snapshot immediately so the MCP tools / dashboard can
    // truthfully report "uninitialized" before the cold boot completes.
    {
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(LoraShared {
            present: false,
            region: LORA_REGION,
            radio_state: "uninitialized",
            node_id,
            seq: 0,
            last_beacon_unix_ms: None,
            last_rx_rssi_dbm: None,
            last_rx_snr_db: None,
            peers: PeerTable::new(),
            duty: DutyCycle::for_region(LORA_REGION),
            tx_queue: TxQueue::new(),
            planner: RebroadcastPlanner::new(node_id, cfg.role()),
            gate: CommandGate::new(cfg.owner_key(), ControlLimits::DEFAULT),
            last_nonces_found: 0,
        });
    }
    info!(
        "LoRa: starting radio task (region={}, node_id={})",
        region_str(LORA_REGION),
        node_id.to_hex()
    );
    let _ = std::thread::Builder::new()
        .name("lora".into())
        .stack_size(LORA_TASK_STACK)
        .spawn(move || run(state, bus))
        .map_err(|e| warn!("LoRa: failed to spawn radio thread: {e}"));
}

/// Radio task body. Owns the `Sx1262` exclusively. Cold-boots the radio; on
/// failure logs ONCE + sets `radio_state=fault` and returns (mining continues).
fn run(state: SharedState, bus: LoraBus<'static>) {
    let mut radio = Sx1262::new(
        EspSpiBus::new(bus.spi),
        EspInputPin::new(bus.busy),
        EspInputPin::new(bus.dio1),
        EspOutputPin::new(bus.nreset),
        LORA_REGION,
    );

    // Cold boot (reset → standby → DC-DC → TCXO → calibrate → LoRa config). A real
    // wiring/TCXO/power fault surfaces here as BusyTimeout — fail-soft.
    if let Err(e) = radio.begin() {
        error!("LoRa: radio cold-boot failed ({e}) — mesh disabled, mining continues");
        set_state("fault", false);
        return;
    }
    set_state("standby", true);
    info!("LoRa: radio up (present=true)");

    // Announce ourselves once so peers learn our model immediately.
    enqueue_self_beacon(MeshKind::Identify(build_identify(&state)));

    // Backbone-relay radio setup: run boosted (max-sensitivity) RX so this
    // always-on node hears the whole mesh, pre-load CAD params for LBT, and arm
    // continuous RX up-front so the first carrier-sense in `drain_tx` is valid.
    if let Err(e) = radio.set_rx_gain_boosted() {
        warn!("LoRa: set_rx_gain_boosted failed: {e}");
    }
    if let Err(e) = radio.configure_cad_lbt() {
        warn!("LoRa: configure_cad_lbt failed: {e}");
    }
    if let Err(e) = radio.set_rx(0x00FF_FFFF) {
        warn!("LoRa: initial set_rx failed: {e}");
    } else {
        set_state_str("rx");
    }

    let mut last_tlm = now_ms();
    loop {
        // 1) Drain outbound queue (MCP beacons, self beacons, relays) — TX first.
        drain_tx(&mut radio);

        // 2) Periodic telemetry beacon (duty-bounded at enqueue time).
        let now = now_ms();
        if now.saturating_sub(last_tlm) >= TLM_BEACON_INTERVAL_S * 1000 {
            last_tlm = now;
            enqueue_beacon_if_duty(MeshKind::Telemetry(build_telemetry(&state)));
        }

        // 3) Block-found proxy: a rising nonces_found edge → high-priority BLK.
        maybe_beacon_block_found(&state);

        // 4) RX: arm continuous receive, then poll DIO1 for an inbound frame.
        //    service_rx SCHEDULES managed-flood rebroadcasts (SNR + jitter) and
        //    authenticates + applies inbound owner commands.
        if let Err(e) = radio.set_rx(0x00FF_FFFF) {
            warn!("LoRa: set_rx failed: {e}");
        } else {
            set_state_str("rx");
        }
        service_rx(&mut radio, &state);

        // 5) Move any managed-flood rebroadcasts whose contention slot has arrived
        //    into the TX queue (duty-bounded); the next drain_tx sends them,
        //    listen-before-talk-gated.
        drain_due_rebroadcasts();

        std::thread::sleep(Duration::from_millis(RX_POLL_MS));
    }
}

/// Update the shared radio lifecycle + presence flag.
fn set_state(radio_state: &'static str, present: bool) {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        s.radio_state = radio_state;
        s.present = present;
    }
}

fn set_state_str(radio_state: &'static str) {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        s.radio_state = radio_state;
    }
}

/// Enqueue a self-originated beacon WITHOUT a duty check (used for the one-shot
/// startup Identify; the task-generated periodic beacons use the duty-checked
/// path). Assigns the next originator sequence.
fn enqueue_self_beacon(kind: MeshKind) {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        let frame = MeshFrame::originate(s.node_id, s.seq, kind);
        s.seq = s.seq.wrapping_add(1);
        s.tx_queue.push(frame);
    }
}

/// Enqueue a self-originated beacon only if the region duty budget admits its
/// estimated airtime — the honest airtime bound for task-generated traffic.
fn enqueue_beacon_if_duty(kind: MeshKind) {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        let frame = MeshFrame::originate(s.node_id, s.seq, kind);
        if let Ok(bytes) = frame.encode() {
            let airtime = ModulationParams::default().airtime_ms(bytes.len());
            if s.duty.try_acquire(airtime, now_ms()) {
                s.seq = s.seq.wrapping_add(1);
                s.tx_queue.push(frame);
            }
        }
    }
}

/// Emit a high-priority BlockFound beacon on a rising `nonces_found` edge — the
/// closest live "found" signal axe exposes (solo-share firmware has no true
/// block-height edge). Duty-bounded like every transmit.
fn maybe_beacon_block_found(state: &SharedState) {
    let nonces = state
        .stats
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot()
        .nonces_found;
    let mut fire = false;
    {
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            if nonces > s.last_nonces_found {
                s.last_nonces_found = nonces;
                fire = true;
            }
        }
    }
    if fire {
        enqueue_beacon_if_duty(MeshKind::BlockFound(build_block_found(state)));
    }
}

/// Pop and transmit every queued frame (highest priority first). Half-duplex: we
/// switch to TX, key the radio, poll DIO1 for TxDone, then return to the RX arm in
/// the main loop.
fn drain_tx<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>)
where
    SPI: dcentaxe_lora::SpiBus,
    B: dcentaxe_lora::GpioPin,
    D: dcentaxe_lora::GpioPin,
    N: dcentaxe_lora::GpioPin,
{
    // Listen-before-talk: carrier-sense the channel before keying the PA. If it
    // is busy, defer ALL TX this cycle and retry next loop — the collision
    // avoidance the mesh maturity audit flagged as the #1 dense-swarm risk. The
    // radio is in RX here (armed at setup + at the end of every iteration); a
    // read failure falls through to transmit rather than stalling the mesh.
    if let Ok(false) = radio.channel_clear_rssi(LBT_RSSI_THRESHOLD_DBM) {
        return;
    }
    loop {
        let frame = {
            let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut() {
                Some(s) => s.tx_queue.pop(),
                None => None,
            }
        };
        let Some(frame) = frame else { break };
        let Ok(bytes) = frame.encode() else { continue };
        set_state_str("tx");
        if let Err(e) = radio.transmit(&bytes) {
            warn!("LoRa: transmit failed: {e}");
            continue;
        }
        // Wait (bounded) for TxDone so we do not clobber the next TX mid-air.
        wait_tx_done(radio);
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            s.last_beacon_unix_ms = Some(now_ms());
        }
    }
}

fn wait_tx_done<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>)
where
    SPI: dcentaxe_lora::SpiBus,
    B: dcentaxe_lora::GpioPin,
    D: dcentaxe_lora::GpioPin,
    N: dcentaxe_lora::GpioPin,
{
    for _ in 0..TX_DONE_POLL_BUDGET {
        match radio.irq_pending() {
            Ok(true) => {
                if let Ok(irq) = radio.process_irq() {
                    if irq.tx_done || irq.timeout {
                        return;
                    }
                }
            }
            Ok(false) => {}
            Err(_) => return,
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Service one RX opportunity: if DIO1 is asserted, read + decode the frame,
/// refresh the peer table + link quality, SCHEDULE a managed-flood rebroadcast
/// (SNR-weighted + jittered, cancel-on-duplicate, role-gated), and — for an
/// inbound owner `Command` — authenticate + safe-clamp it and apply the control.
fn service_rx<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>, state: &SharedState)
where
    SPI: dcentaxe_lora::SpiBus,
    B: dcentaxe_lora::GpioPin,
    D: dcentaxe_lora::GpioPin,
    N: dcentaxe_lora::GpioPin,
{
    if !matches!(radio.irq_pending(), Ok(true)) {
        return;
    }
    let irq = match radio.process_irq() {
        Ok(i) => i,
        Err(e) => {
            warn!("LoRa: irq read failed: {e}");
            return;
        }
    };
    if !irq.rx_done || irq.crc_err {
        return;
    }
    let (payload, rssi, snr) = match radio.receive() {
        Ok(v) => v,
        Err(e) => {
            warn!("LoRa: rx read failed: {e}");
            return;
        }
    };
    let Ok(frame) = MeshFrame::decode(&payload) else {
        return;
    };
    // Observe + schedule + authenticate under the lock; apply any authorized
    // control OUTSIDE the lock (never hold the radio/mesh lock across a hardware
    // write on a different subsystem).
    let (action, control) = {
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = g.as_mut() else { return };
        s.peers.observe(&frame, rssi, snr, now_ms());
        s.last_rx_rssi_dbm = Some(rssi);
        s.last_rx_snr_db = Some(snr as f32);
        // Managed flood: the planner schedules the rebroadcast after an
        // SNR-weighted + jittered contention delay (emitted later by
        // drain_due_rebroadcasts), or cancels/suppresses it — it is deliberately
        // NOT transmitted immediately, which is what stops correlated collisions.
        let action = s.planner.on_receive(&frame, snr as f32, now_ms());
        // Owner control: an inbound Command must clear HMAC auth + anti-replay +
        // the safe setpoint clamp before it can touch hardware. Every other kind
        // is observe/relay only.
        let control = match &frame.kind {
            MeshKind::Command(_) => match s.gate.admit(&frame) {
                GateOutcome::Apply(ctrl) => Some(ctrl),
                GateOutcome::Reject(reason) => {
                    warn!(
                        "LoRa: mesh command from {} refused ({reason:?})",
                        frame.src.to_hex()
                    );
                    None
                }
            },
            _ => None,
        };
        (action, control)
    };
    match action {
        RxAction::Scheduled { .. } => {
            info!("LoRa: scheduled relay of frame from {}", frame.src.to_hex());
        }
        RxAction::Canceled => {
            info!(
                "LoRa: relay canceled — a neighbor already covered {}",
                frame.src.to_hex()
            );
        }
        RxAction::Suppressed(_) => {}
    }
    if let Some(ctrl) = control {
        apply_mesh_control(state, &ctrl);
    }
}

/// Move every managed-flood rebroadcast whose contention slot has arrived (per
/// the planner's SNR + jitter schedule) into the priority TX queue, duty-bounded.
/// The next `drain_tx` transmits them, listen-before-talk-gated.
fn drain_due_rebroadcasts() {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        let now = now_ms();
        for hop in s.planner.due(now) {
            if let Ok(bytes) = hop.encode() {
                let airtime = ModulationParams::default().airtime_ms(bytes.len());
                if s.duty.try_acquire(airtime, now) {
                    s.tx_queue.push(hop);
                }
            }
        }
    }
}

/// Apply an owner-authenticated, safe-clamped [`MeshControl`] to the miner. The
/// command has already cleared HMAC auth + anti-replay + the setpoint clamp in
/// [`CommandGate::admit`]; this is the actuation step.
///
/// TODO(actuation-wiring): route the autotuner setpoints through the SAME
/// owner-control surface the MCP `set_*`/`run_autotune` tools use (so mesh and
/// MCP control share one code path). Until that hookup lands, an authenticated
/// command is logged, not silently dropped — and because the gate is fail-closed
/// (no owner key ⇒ every command refused), this path is dormant until the
/// operator provisions a key, so no half-wired control can ship active.
fn apply_mesh_control(_state: &SharedState, ctrl: &MeshControl) {
    info!("LoRa: owner-authenticated mesh control accepted: {ctrl:?}");
    match ctrl {
        MeshControl::Identify => { /* pulse the identify LED (actuation wiring pending) */ }
        MeshControl::RestartMining => { /* request a mining restart (actuation wiring pending) */ }
        MeshControl::SetTargetWatts(_)
        | MeshControl::SetTargetTempC(_)
        | MeshControl::SetAutotunerMode(_) => { /* apply the clamped autotuner setpoint (wiring pending) */
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Dashboard mesh panel (appended to the `/` response ONLY under feature=lora)
// ───────────────────────────────────────────────────────────────────────────

/// Self-injecting inline mesh panel. Appended AFTER `DASHBOARD_HTML` by the `/`
/// and `/index.html` handlers under `#[cfg(feature = "lora")]`, so a non-LoRa SKU
/// serves byte-identical HTML (the string is never written). It creates its own
/// floating card, polls `/api/system/info`, and:
///   * shows NOTHING but "Radio pending" until `lora.present === true`
///     (honesty-gated — no live claim before on-hardware proof);
///   * then renders peer count / region / last beacon / last-RX RSSI.
/// There is deliberately NO "LoRa enabled" toggle — that waits for a live TX/RX
/// proof follow-up (project brief W2.7).
pub const LORA_DASHBOARD_PANEL_HTML: &str = r##"<script>
(function(){
  var card=document.createElement('div');
  card.id='lora-mesh-panel';
  card.style.cssText='position:fixed;right:16px;bottom:16px;z-index:9998;min-width:200px;max-width:280px;'
    +'background:rgba(16,24,32,0.92);border:1px solid rgba(250,165,0,0.35);border-radius:10px;'
    +'padding:10px 12px;font:12px/1.5 -apple-system,Segoe UI,Roboto,sans-serif;color:#e8ecf2;'
    +'box-shadow:0 8px 32px rgba(0,0,0,0.5);backdrop-filter:blur(8px)';
  card.innerHTML='<div style="font-weight:700;letter-spacing:.5px;color:#FAA500;'
    +'text-transform:uppercase;font-size:10px;margin-bottom:4px">LoRa Mesh</div>'
    +'<div id="lora-body" style="color:#6b7a8d">Radio pending</div>';
  document.body.appendChild(card);
  function fmtAgo(ms){if(!ms)return'never';var s=Math.max(0,Math.round((Date.now()-ms)/1000));
    return s<60?s+'s ago':(s<3600?Math.round(s/60)+'m ago':Math.round(s/3600)+'h ago');}
  function render(l){
    var b=document.getElementById('lora-body');if(!b)return;
    if(!l||l.present!==true){b.style.color='#6b7a8d';b.textContent='Radio pending';return;}
    b.style.color='#e8ecf2';
    b.innerHTML='<div>Peers: <b>'+(l.peerCount||0)+'</b></div>'
      +'<div>Region: <b>'+(l.region||'-')+'</b></div>'
      +'<div>State: <b>'+(l.radioState||'-')+'</b></div>'
      +'<div>Last beacon: <b>'+fmtAgo(l.lastBeaconUnixMs)+'</b></div>'
      +'<div>Last RX RSSI: <b>'+(l.lastRxRssiDbm!=null?l.lastRxRssiDbm+' dBm':'-')+'</b></div>';
  }
  function poll(){
    fetch('/api/system/info').then(function(r){return r.json();}).then(function(d){
      render(d&&d.lora);}).catch(function(){});
  }
  poll();setInterval(poll,5000);
})();
</script>"##;
