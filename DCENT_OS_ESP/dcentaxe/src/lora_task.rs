// DCENT_axe — on-board SX1262 LoRa radio task + $DCM mesh integration
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Wires the committed `dcentaxe-lora` crate (SX1262 driver + $DCM mesh) into the
//! `dcentaxe` binary as its OWN FreeRTOS task, so a DCENT_axe board joins the mesh
//! natively. Compiled ONLY under the default-OFF `lora` feature — a non-LoRa SKU
//! never sees this module (byte-identical image; "no lying UI").
//!
//! ## Two radio modes (one at a time)
//! A single SX1262 sits on ONE sync word + modulation at a time, so the radio runs
//! in exactly one of two modes, selected by config:
//!   * **`$DCM` native** (default) — the D-Central mesh grammar + managed flood.
//!   * **Meshtastic Router** (`--features meshtastic` + `MeshConfig.meshtastic_mode`) —
//!     the radio is programmed for the Meshtastic PHY (sync word 0x24B4) and the
//!     node is a genuine Router on an existing Meshtastic mesh: it relays with the
//!     managed [`MeshtasticRouter`], announces itself as NodeInfo, and beacons
//!     miner status as text every node can read. The Meshtastic path is
//!     `#[cfg(feature = "meshtastic")]`; runtime is further gated by
//!     `MeshConfig.meshtastic_mode` (default-OFF, dormant until provisioned — the
//!     same posture as the `$DCM` owner-key gate).
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
use dcentaxe_lora::bitcoin::{self, BitcoinTracker};
use dcentaxe_lora::config::MeshConfig;
use dcentaxe_lora::duty::{DutyCycle, ModulationParams};
use dcentaxe_lora::esp_hal::{EspInputPin, EspOutputPin, EspSpiBus};
use dcentaxe_lora::flood::{RebroadcastPlanner, RxAction};
use dcentaxe_lora::gate::{CommandGate, ControlLimits, GateOutcome, MeshControl};
use dcentaxe_lora::mcp::{LoraStatusResponse, MeshPeerInfo, MeshPeersResponse, SendBeaconResponse};
use dcentaxe_lora::mesh::{
    BlockFound, Identify, MeshFrame, MeshKind, NetInfo, NodeId, PeerTable, Telemetry, Tip,
};
use dcentaxe_lora::relay::TxQueue;
use dcentaxe_lora::sx1262::{Region, Sx1262};

use crate::mesh_solo_runtime;

#[cfg(feature = "meshtastic")]
use dcentaxe_lora::meshtastic::{
    self, Channel, MeshtasticNode, MeshtasticPhyConfig, MeshtasticRouter, PacketHeader,
};
#[cfg(feature = "meshtastic")]
use std::collections::VecDeque;

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

/// Hop budget for Meshtastic packets this node originates.
#[cfg(feature = "meshtastic")]
const MT_HOP_LIMIT: u8 = 3;
/// Bound on the Meshtastic outbound-packet queue (each entry is a full packet, so
/// keep it small on the 300 KB entry board).
#[cfg(feature = "meshtastic")]
const MT_TX_CAP: usize = 16;
/// Bound on the discovered-Meshtastic-node list.
#[cfg(feature = "meshtastic")]
const MT_NODES_CAP: usize = 32;

/// Runtime state for a node operating in **Meshtastic Router** mode. Held in
/// [`LoraShared::mt`] ONLY when the operator selected `meshtastic_mode`; its
/// presence IS the mode flag. A single SX1262 cannot be on both the `$DCM` and
/// Meshtastic PHYs at once, so when this is `Some` the `$DCM` planner/gate/peer
/// path is dormant and the radio speaks Meshtastic exclusively.
#[cfg(feature = "meshtastic")]
struct MeshtasticStack {
    /// Managed rebroadcast router: `(from,id)` dedup + hop-aware SNR-delay flood.
    router: MeshtasticRouter,
    /// The one channel we hold the key for (encrypt/decrypt + channel hash).
    channel: Channel,
    /// Programmed PHY (SF/BW/CR + sync word 0x24B4 + centre frequency).
    phy: MeshtasticPhyConfig,
    /// Our own originated-packet id counter (also the AES-CTR nonce input).
    packet_id: u32,
    /// Raw ready-to-send packets (16-byte header + encrypted payload) — self
    /// beacons + due rebroadcasts. Bounded; drained by the radio task, LBT-gated.
    tx: VecDeque<Vec<u8>>,
    /// Meshtastic peers learned off the air (from NodeInfo), bounded LRU.
    nodes: Vec<MeshtasticNode>,
    /// Our NodeInfo long/short display names.
    long_name: String,
    short_name: String,
    /// Hop budget for packets we originate.
    hop_limit: u8,
}

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
    /// Freshest Bitcoin network snapshot heard over the mesh (Phase-3 "Bitcoin on
    /// mesh" ticker); updated on a `NetInfo` RX, surfaced to the dashboard/MCP.
    bitcoin: BitcoinTracker,
    /// This node has internet and originates the `NetInfo` tip beacon from its
    /// own stratum feed (no HTTP). From `MeshConfig.is_gateway`.
    is_gateway: bool,
    /// Meshtastic Router state — `Some` ⇒ the radio runs in Meshtastic mode and
    /// the `$DCM` planner/gate above are dormant. `None` ⇒ native `$DCM`.
    #[cfg(feature = "meshtastic")]
    mt: Option<MeshtasticStack>,
}

static LORA: Mutex<Option<LoraShared>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wall-clock unix SECONDS (Bitcoin-data freshness is coarse). `0` if the clock
/// is unset (pre-NTP); the tracker measures freshness from local receive time so
/// a wrong-but-advancing clock still ages data sensibly.
fn now_unix_s() -> u64 {
    now_ms() / 1000
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

/// The peer count to report on the read surfaces — the Meshtastic node list when
/// the radio is in Meshtastic mode, otherwise the native `$DCM` [`PeerTable`]. So
/// the dashboard/MCP count never shows a stale-empty `$DCM` table while the node
/// is actually a Meshtastic Router with peers.
fn mesh_peer_count(s: &LoraShared) -> u32 {
    #[cfg(feature = "meshtastic")]
    if let Some(mt) = s.mt.as_ref() {
        return mt.nodes.len() as u32;
    }
    s.peers.len() as u32
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
            mesh_peer_count: mesh_peer_count(s),
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
        peer_count: mesh_peer_count(s),
        last_beacon_unix_ms: s.last_beacon_unix_ms,
        last_rx_rssi_dbm: s.last_rx_rssi_dbm,
        bitcoin: Some(s.bitcoin.view(now_unix_s())),
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
    // Mesh runtime config: prefer NVS-backed config.mesh when present; else
    // fail-closed defaults (solo mesh OFF).
    let cfg = {
        let g = state.config.lock().unwrap_or_else(|e| e.into_inner());
        g.mesh.clone()
    };
    // Solo mesh empty-block controller (pure crates); no-op when not fully opted in.
    mesh_solo_runtime::configure(&cfg, node_id);
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
            bitcoin: BitcoinTracker::new(),
            is_gateway: cfg.is_gateway,
            // Meshtastic Router mode is opt-in (config + `meshtastic` feature).
            // With the default MeshConfig this resolves to `None` ⇒ native `$DCM`.
            #[cfg(feature = "meshtastic")]
            mt: build_meshtastic_stack(&cfg, node_id, &state),
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
    // R-24: attach the host-driven E22 RF-switch enables (TXEN=GPIO2 /
    // RXEN=GPIO9 on DCENT_axe BM1397). With the pins attached the driver routes
    // the switch on every TX/RX/standby transition and skips DIO2-switch mode;
    // a pinless map keeps the DIO2 posture.
    let mut radio = Sx1262::new(
        EspSpiBus::new(bus.spi),
        EspInputPin::new(bus.busy),
        EspInputPin::new(bus.dio1),
        EspOutputPin::new(bus.nreset),
        LORA_REGION,
    )
    .with_rf_switch(
        bus.txen.map(EspOutputPin::new),
        bus.rxen.map(EspOutputPin::new),
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

    // If configured as a Meshtastic Router, re-program the radio for the
    // Meshtastic PHY (sync word 0x24B4 + preset modulation), overriding the `$DCM`
    // LoRa config `begin()` applied. A failure clears the mode and we run `$DCM`.
    #[cfg(feature = "meshtastic")]
    mt_apply_phy(&mut radio);

    // Announce ourselves once so peers learn us immediately — a Meshtastic
    // NodeInfo in Router mode, an `$DCM` Identify beacon in native mode.
    beacon_self_announce(&state);

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
    let mut last_netinfo = 0u64; // 0 ⇒ a gateway beacons the tip on the first loop
    loop {
        // 1) Drain outbound queue (MCP beacons, self beacons, relays) — TX first.
        drain_tx(&mut radio);

        // 2) Periodic telemetry beacon (duty-bounded at enqueue time).
        let now = now_ms();
        if now.saturating_sub(last_tlm) >= TLM_BEACON_INTERVAL_S * 1000 {
            last_tlm = now;
            beacon_periodic_telemetry(&state);
        }

        // 2b) Gateway: beacon the Bitcoin tip — the block height comes from THIS
        //     node's own stratum feed (no internet), so off-grid nodes learn the
        //     chain tip over LoRa (Bitcoin on mesh). No-op unless is_gateway.
        if now.saturating_sub(last_netinfo) >= bitcoin::GATEWAY_BEACON_INTERVAL_S * 1000 {
            last_netinfo = now;
            beacon_netinfo_if_gateway(&state);
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
        beacon_block_found(state);
    }
}

/// Announce this node once at startup — a Meshtastic NodeInfo in Router mode, an
/// `$DCM` Identify beacon in native mode.
fn beacon_self_announce(state: &SharedState) {
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_enqueue_nodeinfo(state);
        return;
    }
    enqueue_self_beacon(MeshKind::Identify(build_identify(state)));
}

/// Periodic status beacon — a human-readable Meshtastic text line in Router mode,
/// an `$DCM` Telemetry frame in native mode. Duty-bounded either way.
fn beacon_periodic_telemetry(state: &SharedState) {
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_enqueue_status(state);
        return;
    }
    enqueue_beacon_if_duty(MeshKind::Telemetry(build_telemetry(state)));
}

/// Block-found announcement — a Meshtastic text broadcast in Router mode, an
/// `$DCM` BlockFound frame in native mode.
fn beacon_block_found(state: &SharedState) {
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_enqueue_block_found(state);
        return;
    }
    enqueue_beacon_if_duty(MeshKind::BlockFound(build_block_found(state)));
}

/// Gateway origination for "Bitcoin on mesh": beacon the Bitcoin tip (block height
/// from THIS node's own stratum feed — no internet) so off-grid nodes get a
/// no-Wi-Fi chain-tip ticker over LoRa. No-op unless this node is a gateway AND
/// has a real tip (mining is up).
///
/// Network difficulty from stratum job nBits via pure `netinfo_from_stratum_nbits`
/// (no HTTP). Price/fee stay 0 until an HTTPS client supplies them (operator).
fn beacon_netinfo_if_gateway(state: &SharedState) {
    let is_gw = {
        let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map(|s| s.is_gateway).unwrap_or(false)
    };
    if !is_gw {
        return;
    }
    let height = state
        .stats
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot()
        .block_height;
    if height == 0 {
        return; // no chain tip yet (mining not up) — don't beacon a zero height
    }
    // Difficulty from compact nBits (no HTTP). Use diff-1 placeholder until a
    // live job nBits field is plumbed through StratumStatus (operator follow-up).
    let nbits: u32 = 0x1d00_ffff;
    let info = bitcoin::netinfo_from_stratum_nbits(height as i64, nbits, 0.0, 0, now_unix_s());
    // A Meshtastic gateway pushes the ticker as a human-readable text broadcast
    // (any client renders it); native `$DCM` originates the structured NetInfo.
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_enqueue_ticker_text(&info);
        return;
    }
    enqueue_beacon_if_duty(MeshKind::NetInfo(info));

    // Origin a mining Tip only when solo-mesh runtime is configured (not every
    // gateway NetInfo tick with a zero prev_hash). Real prev_hash plumbing is
    // operator follow-up before any mainnet tip origin.
    if state
        .config
        .lock()
        .map(|c| c.mesh.solo_mesh_empty_active() && c.mesh.is_gateway)
        .unwrap_or(false)
    {
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            mesh_solo_runtime::maybe_origin_tip(
                [0u8; 32],
                nbits,
                now_unix_s() as u32,
                height as i64,
                &mut s.tx_queue,
            );
        }
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
    // Meshtastic mode drains its own raw-packet queue (LBT already checked above).
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_drain_tx(radio);
        return;
    }
    // Pull solo-mesh BFG frames produced by the share hook into the TX queue.
    {
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            mesh_solo_runtime::drain_outbound_bfg(&mut s.tx_queue);
        }
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
    // In Meshtastic mode the RX path decodes/routes Meshtastic packets instead.
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_service_rx(radio, state);
        return;
    }
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
        // Bitcoin on mesh: a received tip snapshot updates the local ticker (a
        // relayed OLD copy is rejected by the tracker's newer-timestamp gate).
        if let MeshKind::NetInfo(n) = &frame.kind {
            s.bitcoin.observe(n, now_unix_s());
        }
        // Solo mesh tip / BFG gateway path (pure controllers in mesh_solo_runtime).
        mesh_solo_runtime::on_rx_frame(&frame, now_ms(), &mut s.tx_queue);
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
    #[cfg(feature = "meshtastic")]
    if mt_active() {
        mt_drain_due();
        return;
    }
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
// Meshtastic Router mode (feature = "meshtastic"; esp-idf integration seam)
// ───────────────────────────────────────────────────────────────────────────

/// Build the Meshtastic runtime state from config, or `None` to run native
/// `$DCM`. Returns `None` unless `enabled && meshtastic_mode` AND a valid channel
/// resolves — so a bad PSK cleanly falls back to `$DCM` rather than half-meshing.
#[cfg(feature = "meshtastic")]
fn build_meshtastic_stack(
    cfg: &MeshConfig,
    node_id: NodeId,
    state: &SharedState,
) -> Option<MeshtasticStack> {
    if !(cfg.enabled && cfg.meshtastic_mode) {
        return None;
    }
    let channel = match cfg.meshtastic_channel() {
        Some(c) => c,
        None => {
            warn!("LoRa: meshtastic_mode set but channel PSK invalid — running $DCM");
            return None;
        }
    };
    let phy = cfg.meshtastic_phy(meshtastic_default_freq(LORA_REGION));
    let (long_name, short_name) = {
        let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let board = config.board_config();
        let long = board.device_model.clone();
        let short = if cfg.meshtastic_short_name.is_empty() {
            derive_short_name(&board.device_model)
        } else {
            cfg.meshtastic_short_name.chars().take(4).collect()
        };
        (long, short)
    };
    info!(
        "LoRa: Meshtastic Router mode — channel '{}' (hash 0x{:02x}), preset {}, {} Hz",
        channel.name,
        channel.hash(),
        cfg.meshtastic_preset,
        phy.freq_hz
    );
    Some(MeshtasticStack {
        router: MeshtasticRouter::new(node_id.0, cfg.role()),
        channel,
        phy,
        packet_id: 0,
        tx: VecDeque::new(),
        nodes: Vec::new(),
        long_name,
        short_name,
        hop_limit: MT_HOP_LIMIT,
    })
}

/// The region LongFast default centre frequency (used when the operator leaves
/// `meshtastic_freq_hz` at 0). Confirm against your Meshtastic app for a
/// non-LongFast channel (see `dcentaxe_lora::meshtastic::phy`).
#[cfg(feature = "meshtastic")]
fn meshtastic_default_freq(region: Region) -> u32 {
    match region {
        Region::Eu868 => meshtastic::phy::EU868_LONGFAST_HZ,
        Region::Na915 => meshtastic::phy::US_LONGFAST_HZ,
    }
}

/// Derive a ≤4-char Meshtastic short name from the board model (uppercased
/// alphanumerics), falling back to `"DCAX"`.
#[cfg(feature = "meshtastic")]
fn derive_short_name(model: &str) -> String {
    let s: String = model
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(4)
        .collect();
    if s.is_empty() {
        "DCAX".to_string()
    } else {
        s.to_ascii_uppercase()
    }
}

/// `true` when the radio is operating in Meshtastic Router mode.
#[cfg(feature = "meshtastic")]
fn mt_active() -> bool {
    let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().map(|s| s.mt.is_some()).unwrap_or(false)
}

/// Program the SX1262 for the configured Meshtastic PHY. On failure, clear the
/// mode so the task reverts to `$DCM` (fail-soft — a PHY fault is a hardware
/// problem `begin()` would usually have caught first).
#[cfg(feature = "meshtastic")]
fn mt_apply_phy<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>)
where
    SPI: dcentaxe_lora::SpiBus,
    B: dcentaxe_lora::GpioPin,
    D: dcentaxe_lora::GpioPin,
    N: dcentaxe_lora::GpioPin,
{
    let phy = {
        let g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().and_then(|s| s.mt.as_ref().map(|mt| mt.phy))
    };
    let Some(phy) = phy else {
        return;
    };
    match radio.apply_meshtastic_phy(&phy) {
        Ok(()) => info!(
            "LoRa: Meshtastic PHY applied (sync 0x{:04x}, {} Hz, SF{})",
            phy.sync_word, phy.freq_hz, phy.sf
        ),
        Err(e) => {
            warn!("LoRa: apply_meshtastic_phy failed ({e}) — reverting to $DCM mode");
            let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = g.as_mut() {
                s.mt = None;
            }
        }
    }
}

/// A [`ModulationParams`] matching a Meshtastic preset, for an HONEST duty-cycle
/// airtime estimate (LongFast SF11 is far slower than the crate default — the
/// governor must not under-count it).
#[cfg(feature = "meshtastic")]
fn mt_modulation(phy: MeshtasticPhyConfig) -> ModulationParams {
    ModulationParams {
        spreading_factor: phy.preset.sf(),
        bandwidth_hz: phy.preset.bandwidth_khz() * 1000,
        coding_rate_denom: phy.preset.coding_rate_denom(),
        explicit_header: true,
        crc_on: phy.crc_on,
        preamble_syms: phy.preamble_len,
    }
}

/// Build a broadcast packet carrying `data` from our node, duty-gate it, and
/// enqueue the raw bytes. Allocates the next originator packet id.
#[cfg(feature = "meshtastic")]
fn mt_push_broadcast(s: &mut LoraShared, data: &meshtastic::Data) {
    let node = s.node_id.0;
    let (bytes, airtime) = {
        let Some(mt) = s.mt.as_mut() else {
            return;
        };
        mt.packet_id = mt.packet_id.wrapping_add(1);
        if mt.packet_id == 0 {
            mt.packet_id = 1;
        }
        let pid = mt.packet_id;
        let Ok(bytes) = meshtastic::build_broadcast(&mt.channel, node, pid, mt.hop_limit, data)
        else {
            return;
        };
        let airtime = mt_modulation(mt.phy).airtime_ms(bytes.len());
        (bytes, airtime)
    };
    if s.duty.try_acquire(airtime, now_ms()) {
        if let Some(mt) = s.mt.as_mut() {
            if mt.tx.len() >= MT_TX_CAP {
                mt.tx.pop_front();
            }
            mt.tx.push_back(bytes);
        }
    }
}

/// Announce this node as Meshtastic NodeInfo (so it appears by name in every
/// stock client's node list).
#[cfg(feature = "meshtastic")]
fn mt_enqueue_nodeinfo(_state: &SharedState) {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else {
        return;
    };
    let Some((node, long, short)) =
        s.mt.as_ref()
            .map(|mt| (s.node_id.0, mt.long_name.clone(), mt.short_name.clone()))
    else {
        return;
    };
    let user = meshtastic::build_user(node, &long, &short);
    let data = meshtastic::nodeinfo_data(&user);
    mt_push_broadcast(s, &data);
}

/// Beacon a human-readable miner-status text line (any Meshtastic client renders
/// it).
#[cfg(feature = "meshtastic")]
fn mt_enqueue_status(state: &SharedState) {
    let t = build_telemetry(state);
    let msg = format!(
        "DCENT_axe {:.0}GH {:.0}C {:.1}W acc:{}",
        t.hashrate_ghs, t.chip_temp_c, t.power_w, t.shares_accepted
    );
    let data = meshtastic::text_data(&msg);
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        mt_push_broadcast(s, &data);
    }
}

/// Beacon a block-found text broadcast.
#[cfg(feature = "meshtastic")]
fn mt_enqueue_block_found(state: &SharedState) {
    let b = build_block_found(state);
    let msg = format!("DCENT_axe found a block! best diff {}", b.best_diff);
    let data = meshtastic::text_data(&msg);
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        mt_push_broadcast(s, &data);
    }
}

/// Beacon the Bitcoin ticker as a Meshtastic text broadcast — Bitcoin on a stock
/// Meshtastic mesh (Phase-2 bridge × Phase-3 ticker).
#[cfg(feature = "meshtastic")]
fn mt_enqueue_ticker_text(info: &NetInfo) {
    let data = meshtastic::text_data(&bitcoin::ticker_line(info));
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        mt_push_broadcast(s, &data);
    }
}

/// Transmit every queued Meshtastic packet (self beacons + due rebroadcasts).
/// LBT was already checked by the caller (`drain_tx`).
#[cfg(feature = "meshtastic")]
fn mt_drain_tx<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>)
where
    SPI: dcentaxe_lora::SpiBus,
    B: dcentaxe_lora::GpioPin,
    D: dcentaxe_lora::GpioPin,
    N: dcentaxe_lora::GpioPin,
{
    loop {
        let bytes = {
            let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
            g.as_mut()
                .and_then(|s| s.mt.as_mut())
                .and_then(|mt| mt.tx.pop_front())
        };
        let Some(bytes) = bytes else {
            break;
        };
        set_state_str("tx");
        if let Err(e) = radio.transmit(&bytes) {
            warn!("LoRa: meshtastic transmit failed: {e}");
            continue;
        }
        wait_tx_done(radio);
        let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            s.last_beacon_unix_ms = Some(now_ms());
        }
    }
}

/// Service one Meshtastic RX opportunity: decode the header, schedule a managed
/// rebroadcast via the router, and learn a peer's identity from a decodable
/// NodeInfo on our channel. Owner control over the mesh is intentionally NOT on
/// this path — it stays on the HMAC-authenticated `$DCM` command grammar.
#[cfg(feature = "meshtastic")]
fn mt_service_rx<SPI, B, D, N>(radio: &mut Sx1262<SPI, B, D, N>, _state: &SharedState)
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
    let Some(header) = PacketHeader::decode(&payload) else {
        return;
    };
    // Safe: PacketHeader::decode returned Some ⇒ payload.len() >= HEADER_LEN.
    let body = payload[meshtastic::HEADER_LEN..].to_vec();

    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else {
        return;
    };
    s.last_rx_rssi_dbm = Some(rssi);
    s.last_rx_snr_db = Some(snr as f32);
    let Some(mt) = s.mt.as_mut() else {
        return;
    };
    let decision = mt.router.on_receive(&header, &body, snr as f32, now_ms());
    // Learn peer identity from a decodable NodeInfo on a channel we hold.
    if let Some(dec) = meshtastic::decode_packet(std::slice::from_ref(&mt.channel), &payload) {
        if let Some(data) = dec.data {
            if data.portnum == meshtastic::portnum::NODEINFO_APP {
                if let Ok(user) = meshtastic::User::decode(&data.payload) {
                    let node = MeshtasticNode::from_nodeinfo(&header, &user, rssi, snr);
                    mt_upsert_node(&mut mt.nodes, node);
                }
            }
        }
    }
    match decision {
        meshtastic::RelayDecision::Scheduled { .. } => {
            info!("LoRa[mt]: scheduled relay of {:08x}", header.from)
        }
        meshtastic::RelayDecision::Canceled => {
            info!(
                "LoRa[mt]: relay canceled — neighbor covered {:08x}",
                header.from
            )
        }
        meshtastic::RelayDecision::Suppressed(_) => {}
    }
}

/// Insert-or-refresh a discovered Meshtastic node (bounded LRU by node id).
#[cfg(feature = "meshtastic")]
fn mt_upsert_node(nodes: &mut Vec<MeshtasticNode>, node: MeshtasticNode) {
    if let Some(existing) = nodes.iter_mut().find(|n| n.node == node.node) {
        *existing = node;
    } else {
        if nodes.len() >= MT_NODES_CAP {
            nodes.remove(0);
        }
        nodes.push(node);
    }
}

/// Move every Meshtastic rebroadcast whose contention slot has arrived into the
/// outbound queue, duty-bounded. The next `drain_tx` sends them (LBT-gated).
#[cfg(feature = "meshtastic")]
fn mt_drain_due() {
    let mut g = LORA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else {
        return;
    };
    let now = now_ms();
    let due = match s.mt.as_mut() {
        Some(mt) => mt.router.due(now),
        None => return,
    };
    for pkt in due {
        let bytes = pkt.to_bytes();
        let airtime = match s.mt.as_ref() {
            Some(mt) => mt_modulation(mt.phy).airtime_ms(bytes.len()),
            None => return,
        };
        if s.duty.try_acquire(airtime, now) {
            if let Some(mt) = s.mt.as_mut() {
                if mt.tx.len() >= MT_TX_CAP {
                    mt.tx.pop_front();
                }
                mt.tx.push_back(bytes);
            }
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
      +'<div>Last RX RSSI: <b>'+(l.lastRxRssiDbm!=null?l.lastRxRssiDbm+' dBm':'-')+'</b></div>'
      +(l.bitcoin&&l.bitcoin.present
        ?'<div style="margin-top:4px;border-top:1px solid rgba(250,165,0,0.22);padding-top:4px">'
          +'₿ <b>#'+l.bitcoin.blockHeight+'</b>'
          +(l.bitcoin.priceUsd>0?' | $'+Math.round(l.bitcoin.priceUsd).toLocaleString():'')
          +(l.bitcoin.fresh?'':' <span style="color:#6b7a8d">(stale)</span>')+'</div>'
        :'');
  }
  function poll(){
    fetch('/api/system/info').then(function(r){return r.json();}).then(function(d){
      render(d&&d.lora);}).catch(function(){});
  }
  poll();setInterval(poll,5000);
})();
</script>"##;
