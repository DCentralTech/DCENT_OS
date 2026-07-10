//! Read-only hardware probing for the API + dashboard.
//!
//! W2.1 extraction from `daemon.rs` (2026-05-07). Every helper here is
//! best-effort: a failure on any probe falls back to `None` / `Unknown`
//! rather than aborting daemon bring-up.
//!
//! Touches I²C, sysfs, MAC address — all read-only paths. Per
//! , the EEPROM range
//! `0x50..=0x57` is HAL write-denied on am2; the helpers here only
//! READ from those addresses so they're safe across all platforms.

use sha2::{Digest, Sha256};

use crate::config::DcentraldConfig;
use crate::model;
use dcentrald_asic::drivers::{MinerProfile, PicType};

pub fn read_hashboard_eeprom_fingerprints(chain_slots: usize) -> Vec<Option<String>> {
    (0..chain_slots)
        .map(read_hashboard_eeprom_fingerprint_for_slot)
        .collect()
}

pub fn read_hashboard_eeprom_fingerprint_for_slot(slot: usize) -> Option<String> {
    let addr = 0x50u8.checked_add(u8::try_from(slot).ok()?)?;
    for bus in [1u8, 0] {
        let path = format!("/sys/bus/i2c/devices/{}-{:04x}/eeprom", bus, addr);
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if let Some(fingerprint) = hash_hashboard_eeprom_bytes(bus, addr, &data) {
            tracing::info!(
                slot,
                bus,
                addr = format_args!("0x{:02x}", addr),
                fingerprint = %fingerprint,
                "Hashboard EEPROM fingerprint collected from sysfs"
            );
            return Some(fingerprint);
        }
    }
    None
}

/// Wave K Lane B: read the hashboard EEPROM **preamble** (first 2 bytes) for a
/// chain slot, reusing the same read-only sysfs path as the fingerprint reader.
/// Returns `None` if the EEPROM node is absent or all-0x00/0xFF (no board).
/// Observe-only — the caller classifies the SKU for telemetry; this never
/// writes I2C and never drives mining setpoints.
pub fn read_hashboard_eeprom_preamble_for_slot(slot: usize) -> Option<[u8; 2]> {
    let addr = 0x50u8.checked_add(u8::try_from(slot).ok()?)?;
    for bus in [1u8, 0] {
        let path = format!("/sys/bus/i2c/devices/{}-{:04x}/eeprom", bus, addr);
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if data.len() < 2 {
            continue;
        }
        if data.iter().all(|&b| b == 0x00) || data.iter().all(|&b| b == 0xff) {
            continue;
        }
        return Some([data[0], data[1]]);
    }
    None
}

///  B2 drive-half: probe one chain's EEPROM for the energize
/// gate, bounded by `deadline`. Returns the raw bytes if the sysfs
/// read completed before the deadline AND non-empty data is available;
/// returns `Err` (timeout) otherwise.
///
/// The AT24 EEPROM is a kernel-bound sysfs file on AM2/am3 — once the
/// `at24` driver enumerates it, the read itself is microseconds. The
/// deadline is here for the **bus-readiness** case: if the i2c-0
/// service isn't fully bound yet (e.g. xiic-i2c rebind in progress),
/// `std::fs::read` will return `Err` until it is. We retry with a
/// small sleep until the deadline elapses, then surface a `Timeout`.
///
/// The deadline is operator-set at the call site; the
/// `DEFAULT_EEPROM_READINESS_DEADLINE` constant below is the
/// conservative 2-second budget that matches
/// timing on `a lab unit`/`a lab unit` cold boots.
pub fn read_hashboard_eeprom_for_energize_gate(
    slot: usize,
    deadline: std::time::Instant,
) -> Result<Vec<u8>, EepromReadinessError> {
    let Some(addr) = 0x50u8
        .checked_add(u8::try_from(slot).map_err(|_| EepromReadinessError::InvalidSlot { slot })?)
    else {
        return Err(EepromReadinessError::InvalidSlot { slot });
    };
    loop {
        for bus in [1u8, 0] {
            let path = format!("/sys/bus/i2c/devices/{}-{:04x}/eeprom", bus, addr);
            if let Ok(data) = std::fs::read(&path) {
                if !data.is_empty() {
                    return Ok(data);
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(EepromReadinessError::Timeout { slot });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Errors surfaced by `read_hashboard_eeprom_for_energize_gate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EepromReadinessError {
    /// Slot index doesn't map to a valid I²C address (0x50..=0x57).
    InvalidSlot { slot: usize },
    /// No EEPROM bytes available before the deadline.
    Timeout { slot: usize },
}

impl std::fmt::Display for EepromReadinessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EepromReadinessError::InvalidSlot { slot } => {
                write!(f, "invalid chain slot {} (must be 0..=7)", slot)
            }
            EepromReadinessError::Timeout { slot } => {
                write!(f, "EEPROM readiness timeout on chain slot {}", slot)
            }
        }
    }
}

impl std::error::Error for EepromReadinessError {}

/// Conservative default deadline for the AM2 EEPROM readiness probe.
/// Two seconds matches the worst-case xiic-i2c rebind window observed
/// on `a lab unit`/`a lab unit` cold boots.
pub const DEFAULT_EEPROM_READINESS_BUDGET_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
//  Phase 2B — EEPROM-authoritative PIC-vs-NoPic detection
// ---------------------------------------------------------------------------
//
// SAFETY-CRITICAL. Driving a NoPic hashboard (S19k Pro BHB56902 / BM1366,
// and some BHB42xxx/BM1362 S19j-family SKUs) through the dsPIC path sends
// SET_VOLTAGE to a controller that does not exist. The declarative result
// (model string → MinerProfile.pic_type → S9 default) can map a NoPic board
// with no model string set onto `DsPic33EP`, because the default profiles
// assume a voltage controller. Chain-EEPROM preamble is authoritative only
// for unique families such as BHB56902 (`0x05 0x11`); BHB42xxx (`0x04 0x11`)
// is a family preamble shared by both PIC and NoPic SKUs, so it must not be
// used alone to force dsPIC.
//
// Rollout discipline mirrors `DCENT_AM2_STRICT_SKU_REFUSE` /
// `strict_pvt_clamp_enabled`: default-OFF, telemetry-first. With the gate
// OFF the daemon's behavior is BYTE-IDENTICAL to today (the declarative
// result is returned unchanged). The gate only ever overrides toward
// `NoPic` on a CLEAR NoPic preamble/SKU; it NEVER overrides toward dsPIC on
// a weak/ambiguous/absent signal — ambiguity always falls back to today's
// behavior.
//
// EEPROM reads at 0x50-0x57 are READ-allowed (the HAL denylist is for
// WRITES only); no write is ever issued by this path. The actual byte
// read reuses `read_hashboard_eeprom_for_energize_gate` + the energize
// gate's `classify_chain` — no EEPROM I/O is duplicated here.
//
// Optional second signal (DEFERRED): the w24-nopic-hashboard report (F3)
// suggests a 0x20 I²C ACK probe (EEPROM-says-NoPic AND no-ACK-at-0x20 ⇒
// NoPic) as a corroborating signal. It is intentionally NOT added here:
// (1) the EEPROM authority alone closes the SET_VOLTAGE-to-NoPic bug;
// (2) an ACK probe pulls live HAL I²C into this resolution and would
// contend with the single-I²C-owner service, which is neither "cheap"
// nor side-effect-free at this call site. The probe stays a future
// belt-and-suspenders, gated separately if/when it is wired through the
// I²C service rather than a raw bus open.

use dcentrald_silicon_profiles::energize_gate::{classify_chain, ChainProbe};
use dcentrald_silicon_profiles::hashboards::Hashboard;

/// Is the EEPROM-authoritative PIC-vs-NoPic detection gate ON?
///
/// `DCENT_AM2_EEPROM_PIC_DETECT=1` (or `=true`) enables it. Default OFF
/// for first-deploy rollout, matching `strict_sku_refuse_enabled()` /
/// `accept_degraded_hardware_enabled()`. The operator flips this on after
/// confirming a known unit (`a lab unit`/`a lab unit` BHB42601, S19k Pro BHB56902)
/// classifies as expected with no false NoPic/PIC override logged.
pub fn eeprom_pic_detect_enabled() -> bool {
    std::env::var("DCENT_AM2_EEPROM_PIC_DETECT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// The PIC family a classified hashboard SKU implies, if it is *clear*.
///
/// Returns:
/// - `Some(PicType::NoPic)` for `Bhb56902` (S19k Pro, BM1366 NoPic,
///   EEPROM preamble `0x05 0x11`) — the unique production NoPic hashboard
///   reachable through `classify_by_eeprom_preamble`.
/// - `Some(PicType::NoPic)` for refined BHB42xxx NoPic SKUs from RE-016.
/// - `Some(PicType::DsPic33EP)` only for refined BHB42xxx SKUs whose PIC
///   presence is pinned by stock/Bosminer evidence. The canonical
///   `Bhb42601` value returned by `classify_by_eeprom_preamble([0x04,0x11])`
///   is deliberately treated as ambiguous because that preamble is shared
///   by PIC and NoPic BHB42xxx variants.
/// - `Some(PicType::Pic16F1704)` for the S9 hashboard placeholder.
/// - `None` for SKUs whose voltage-controller family is not pinned by the
///   preamble alone (`BhbS11`, `BhbS17` placeholders). `None` means
///   "no clear signal" → caller falls back to the declarative result.
fn pic_type_for_classified_sku(sku: Hashboard) -> Option<PicType> {
    match sku {
        // NoPic SKUs. BHB56902 is uniquely identified by preamble; the
        // BHB42xxx rows require a refined SKU/subtype signal, not preamble
        // alone.
        Hashboard::Bhb56902
        | Hashboard::Bhb42611
        | Hashboard::Bhb42603
        | Hashboard::Bhb42631
        | Hashboard::Bhb42632
        | Hashboard::Bhb42651
        | Hashboard::Bhb42811
        | Hashboard::Bhb42821
        | Hashboard::Bhb42831
        | Hashboard::Bhb42841 => Some(PicType::NoPic),
        // Refined BHB42xxx SKUs with PIC/dsPIC evidence. `Bhb42601` is not
        // listed here because the current preamble-only classifier returns it
        // as the canonical stand-in for every `0x04 0x11` BHB42xxx board.
        Hashboard::Bhb42621
        | Hashboard::Bhb42641
        | Hashboard::Bhb42801
        | Hashboard::Bhb42803
        | Hashboard::Bhb42701 => Some(PicType::DsPic33EP),
        // S9 hashboards — PIC16F1704.
        Hashboard::BhbS9 { .. } => Some(PicType::Pic16F1704),
        // Voltage-controller family not pinned by preamble alone.
        Hashboard::Bhb42601 | Hashboard::BhbS11 | Hashboard::BhbS17 | Hashboard::BhbT15 => None,
    }
}

/// PURE decision function: resolve the effective `PicType` from the
/// declarative result and an optional per-chain EEPROM probe, under the
/// gate.
///
/// This is the single load-bearing decision and is unit-tested directly
/// (no I/O). The contract:
///
/// 1. **Gate OFF** → always return `declarative` (byte-identical to
///    today's behavior; the probe is ignored entirely).
/// 2. **Gate ON + `Classified` with a CLEAR NoPic preamble/SKU** → force
///    `NoPic`, regardless of carrier / chip-id / declarative.
/// 3. **Gate ON + `Classified` with a CLEAR PIC/dsPIC refined SKU** → use
///    that PIC family (it is the physical truth for this board).
/// 4. **Gate ON + any weak/absent signal** (`MalformedPreamble`,
///    `Timeout`, `Unpopulated`, `ReadError`, a `Classified` SKU whose
///    family the preamble doesn't pin, or `None` probe) → fall back to
///    `declarative`.
///
/// Rule (load-bearing): NEVER override toward dsPIC on a weak signal —
/// only a clean unique/refined classification may move the answer. The
/// shared BHB42xxx preamble is not enough. Every uncertain case fails toward
/// the existing declarative behavior.
pub fn resolve_pic_type(
    declarative: PicType,
    probe: Option<&ChainProbe>,
    gate_on: bool,
) -> PicType {
    if !gate_on {
        return declarative;
    }
    match probe {
        Some(ChainProbe::Classified { sku, .. }) => {
            // Only a SKU whose family the preamble clearly pins moves the
            // answer; anything else falls back to declarative.
            pic_type_for_classified_sku(*sku).unwrap_or(declarative)
        }
        // Weak / absent signal — fail toward today's behavior.
        _ => declarative,
    }
}

/// Probe the chain EEPROMs and resolve an EEPROM-authoritative `PicType`,
/// gated by `DCENT_AM2_EEPROM_PIC_DETECT`. Falls back to `declarative`
/// when the gate is OFF or no chain produces a clear signal.
///
/// Reuses the energize-gate read + classify path; never writes I²C.
/// Best-effort and bounded — a read timeout on a slot is just a weak
/// signal (→ declarative), never an abort. The first chain that yields a
/// clear NoPic/PIC classification wins; a clear NoPic anywhere is
/// authoritative because mixed PIC/NoPic boards are not a shipping
/// configuration. BHB42xxx preamble-only classification is intentionally
/// ambiguous and does not override the declarative result.
pub fn resolve_pic_type_from_eeprom(declarative: PicType, chain_slots: usize) -> PicType {
    let gate_on = eeprom_pic_detect_enabled();
    if !gate_on {
        return declarative;
    }

    let mut nopic_seen: Option<u8> = None;
    let mut pic_family: Option<(PicType, u8)> = None;

    for slot in 0..chain_slots {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(DEFAULT_EEPROM_READINESS_BUDGET_MS);
        let bytes = read_hashboard_eeprom_for_energize_gate(slot, deadline).ok();
        let chain_id = u8::try_from(slot).unwrap_or(u8::MAX);
        let probe = classify_chain(chain_id, bytes.as_deref());
        let resolved = resolve_pic_type(declarative, Some(&probe), true);
        if resolved == PicType::NoPic && declarative != PicType::NoPic {
            nopic_seen = Some(chain_id);
            break; // clear NoPic is authoritative; stop probing.
        }
        if matches!(resolved, PicType::Pic16F1704 | PicType::DsPic33EP) && pic_family.is_none() {
            pic_family = Some((resolved, chain_id));
        }
    }

    if let Some(chain_id) = nopic_seen {
        tracing::warn!(
            chain_id,
            declarative = ?declarative,
            "DCENT_AM2_EEPROM_PIC_DETECT: chain EEPROM classifies as a NoPic \
             hashboard (BHB56902 / 0x05 0x11) — forcing PicType::NoPic over the \
             declarative result. No dsPIC SET_VOLTAGE will be issued."
        );
        return PicType::NoPic;
    }
    if let Some((fam, chain_id)) = pic_family {
        if fam != declarative {
            tracing::info!(
                chain_id,
                resolved = ?fam,
                declarative = ?declarative,
                "DCENT_AM2_EEPROM_PIC_DETECT: chain EEPROM classifies as a PIC/dsPIC \
                 hashboard — using the EEPROM-derived PIC family."
            );
        }
        return fam;
    }
    // No chain produced a clear signal — fall back to today's behavior.
    declarative
}

/// Boolean overlay of the EEPROM PIC-vs-NoPic authority for callers that
/// only need the NoPic-vs-not decision (e.g. `serial_mining::is_nopic`).
///
/// Built on the SAME pure [`resolve_pic_type`] decision + the SAME gate so
/// `is_nopic()` and `pic_type()` can never disagree. The declarative
/// boolean is mapped to a `PicType` sentinel (`NoPic` ⇔ true, `DsPic33EP`
/// ⇔ false), the gated EEPROM authority is applied, and the result is
/// folded back to a boolean (`== NoPic`). With the gate OFF this returns
/// `declarative_nopic` unchanged — byte-identical to today.
pub fn resolve_is_nopic_from_eeprom(declarative_nopic: bool, chain_slots: usize) -> bool {
    // DsPic33EP is just a non-NoPic sentinel here; the boolean result only
    // distinguishes NoPic vs not-NoPic, so the specific PIC family of the
    // `false` branch is irrelevant.
    let declarative = if declarative_nopic {
        PicType::NoPic
    } else {
        PicType::DsPic33EP
    };
    resolve_pic_type_from_eeprom(declarative, chain_slots) == PicType::NoPic
}

pub fn hash_hashboard_eeprom_bytes(bus: u8, addr: u8, data: &[u8]) -> Option<String> {
    if data.len() < 16 {
        return None;
    }
    if data.iter().all(|&byte| byte == 0x00) || data.iter().all(|&byte| byte == 0xff) {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    Some(format!(
        "i2c{}-0x{:02x}:sha256:{}",
        bus,
        addr,
        hex_lower(&digest[..16])
    ))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Read the baked `/etc/dcentos/board_target` marker, if present.
///
/// Best-effort and read-only. Resolution of the marker into a chip label is
/// handled by `resolve_chip_identity` so the selected label and confidence
/// evidence stay coupled.
fn read_board_target_marker() -> Option<String> {
    std::fs::read_to_string("/etc/dcentos/board_target")
        .ok()
        .map(|marker| marker.trim().to_string())
        .filter(|marker| !marker.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChipIdentityResolution {
    chip_label: &'static str,
    identification: dcentrald_api::HardwareIdentification,
}

fn chip_source(source: &str, key: &str, chip: &str) -> String {
    format!("{}:{}->{}", source, key, chip)
}

/// Resolve the public ASIC label and structured evidence for that identity.
///
/// Precedence intentionally matches the historical chip-type behavior:
/// configured model first, then baked board_target, then "Auto-detect".
fn resolve_chip_identity(
    config_model: Option<&str>,
    board_target_marker: Option<&str>,
) -> ChipIdentityResolution {
    let config_chip = config_model.and_then(model::model_chip_label);
    let board_chip = board_target_marker.and_then(model::board_target_chip_label);

    match (config_model, config_chip, board_target_marker, board_chip) {
        (Some(model_key), Some(config_chip), Some(board_target), Some(board_chip))
            if config_chip == board_chip =>
        {
            ChipIdentityResolution {
                chip_label: config_chip,
                identification: dcentrald_api::HardwareIdentification {
                    confidence: "high".to_string(),
                    sources: vec![
                        chip_source("config_model", model_key, config_chip),
                        chip_source("board_target", board_target, board_chip),
                    ],
                    note: Some(
                        "configured model and baked board target agree on ASIC family"
                            .to_string(),
                    ),
                },
            }
        }
        (Some(model_key), Some(config_chip), Some(board_target), Some(board_chip)) => {
            ChipIdentityResolution {
                chip_label: config_chip,
                identification: dcentrald_api::HardwareIdentification {
                    confidence: "low".to_string(),
                    sources: vec![
                        chip_source("config_model", model_key, config_chip),
                        chip_source("board_target", board_target, board_chip),
                    ],
                    note: Some(
                        "configured model and baked board target disagree; using configured model chip label"
                            .to_string(),
                    ),
                },
            }
        }
        (Some(model_key), Some(config_chip), _, _) => ChipIdentityResolution {
            chip_label: config_chip,
            identification: dcentrald_api::HardwareIdentification {
                confidence: "medium".to_string(),
                sources: vec![chip_source("config_model", model_key, config_chip)],
                note: Some("ASIC family is pinned by configured model only".to_string()),
            },
        },
        (_, _, Some(board_target), Some(board_chip)) => ChipIdentityResolution {
            chip_label: board_chip,
            identification: dcentrald_api::HardwareIdentification {
                confidence: "high".to_string(),
                sources: vec![chip_source("board_target", board_target, board_chip)],
                note: Some("ASIC family is pinned by baked board target".to_string()),
            },
        },
        _ => ChipIdentityResolution {
            chip_label: "Auto-detect",
            identification: dcentrald_api::HardwareIdentification {
                confidence: "unknown".to_string(),
                sources: Vec::new(),
                note: Some(
                    "no configured model or recognized board target pins the ASIC family"
                        .to_string(),
                ),
            },
        },
    }
}

/// Collect hardware information at startup for the API.
///
/// Reads miner serial (EEPROM/MAC), control board type, chip type, PSU info.
/// Non-fatal — any failure results in None/unknown for that field.
pub fn collect_hardware_info(config: &DcentraldConfig) -> dcentrald_api::HardwareInfo {
    let mut info = dcentrald_api::HardwareInfo::default();
    let profile_chip_id = config
        .mining
        .model
        .as_deref()
        .and_then(model::model_chip_id);
    let board_target_marker = read_board_target_marker();
    let chip_identity = resolve_chip_identity(
        config.mining.model.as_deref(),
        board_target_marker.as_deref(),
    );

    // Control board type from platform detection
    info.control_board = detect_control_board();
    info.capabilities.fan_rpm_feedback = Some(!info.control_board.starts_with("AML"));

    // Chip type from config, else reconcile from the identified board.
    //
    // P1-1 (D-12): when the configured model yields no chip label the chip
    // is still pinned by the identified control board / board family. Fall
    // back to `/etc/dcentos/board_target` so `chip_type` never surfaces as
    // "Auto-detect" (and CGMiner `Type` never as "Antminer (Auto-detect)")
    // on REST / CGMiner / Prometheus once the board is known — letting fleet
    // tools (pyasic / asic-rs / Foreman) classify the unit. Only falls
    // through to "Auto-detect" for an absent/ambiguous marker (truth-contract:
    // never fabricate a chip on an unidentified board).
    info.chip_type = chip_identity.chip_label.to_string();
    info.identification = chip_identity.identification;

    if let Some(chip_id) = profile_chip_id {
        if let Some(profile) = MinerProfile::for_chip(chip_id) {
            info.capabilities.voltage_control = match profile.pic_type {
                PicType::Pic16F1704 => "pic16".to_string(),
                PicType::DsPic33EP => "dspic".to_string(),
                PicType::NoPic => "nopic".to_string(),
            };
            info.capabilities.sleep_wake_supported = matches!(
                profile.pic_type,
                PicType::Pic16F1704 | PicType::DsPic33EP | PicType::NoPic
            );
            let caps = dcentrald_autotuner::autotuner_capabilities_for_chip(
                chip_id,
                &info.capabilities.voltage_control,
            );
            let policy = dcentrald_autotuner::resolve_autotuner_policy(&config.autotuner, &caps);
            info.autotuner = Some(dcentrald_autotuner::AutotunerPolicyStatus {
                requested_preset: policy.requested_preset.clone(),
                effective_preset: policy.effective_preset.clone(),
                requested_preset_supported: policy.requested_preset_supported,
                requested_preset_display_name: policy
                    .requested_preset
                    .as_deref()
                    .and_then(dcentrald_autotuner::autotuner_preset_display_name)
                    .map(str::to_string),
                effective_preset_display_name: policy
                    .effective_preset
                    .as_deref()
                    .and_then(dcentrald_autotuner::autotuner_preset_display_name)
                    .map(str::to_string),
                requested_preset_reason: policy.requested_preset_reason.clone(),
                degraded_from_requested: policy.degraded_from_requested,
                capabilities: Some(policy.capabilities.clone()),
                active_objective: None,
                active_limiting_factor: None,
                safety_override: None,
            });
        }
    }

    // Miner serial number: try EEPROM at 0x51, then fall back to MAC-derived
    info.miner_serial = read_miner_serial();

    // Hash board type: try EEPROM, fall back to config model
    info.hb_type = read_hb_type();

    // PSU information: override takes priority, then I2C probe
    let psu_override_active = config
        .power
        .psu_override
        .as_ref()
        .map(|o| o.enabled)
        .unwrap_or(false);

    if psu_override_active {
        // PSU Override mode — skip I2C probe entirely. User declared a fixed-voltage PSU.
        let ovr = config.power.psu_override.as_ref().unwrap();
        info.psu_override_active = true;
        info.psu_model = Some(ovr.model.clone());
        info.psu_fw_version = None; // Non-smart PSU has no firmware
        info.psu_serial = None;
        info.psu_voltage_range = Some(format!("{:.2} V (fixed)", ovr.voltage_v));
        tracing::info!(
            model = %ovr.model,
            voltage = format_args!("{:.2}V", ovr.voltage_v),
            "PSU OVERRIDE active — skipping I2C probe. No Loki device needed."
        );
    } else if !matches!(info.capabilities.voltage_control.as_str(), "nopic") {
        if let Some((model, fw_version, serial, voltage_range)) = probe_psu_info() {
            info.psu_model = model;
            info.psu_fw_version = fw_version;
            info.psu_serial = serial;
            info.psu_voltage_range = voltage_range;
        } else {
            tracing::debug!(
                "Smart PSU probe unavailable on kernel I2C — use PSU override for fixed-voltage PSUs"
            );
        }
    }

    tracing::info!(
        control_board = %info.control_board,
        chip_type = %info.chip_type,
        identification_confidence = %info.identification.confidence,
        identification_sources = ?info.identification.sources,
        serial = ?info.miner_serial,
        psu = ?info.psu_model,
        "Hardware info collected"
    );

    info
}

/// Detect control board type from platform signatures.
pub fn detect_control_board() -> String {
    // Check for Amlogic
    if std::path::Path::new("/dev/ttyS1").exists() && !std::path::Path::new("/dev/uio0").exists() {
        return "AML Amlogic".to_string();
    }

    // Check for Zynq (UIO devices exist)
    if std::path::Path::new("/dev/uio0").exists() {
        // Distinguish am1-s9 vs am2-s17 via UIO count or naming
        let uio_count = std::fs::read_dir("/sys/class/uio")
            .map(|d| d.count())
            .unwrap_or(0);
        return if uio_count > 14 {
            "Zynq am2-s17".to_string()
        } else {
            "Zynq am1-s9".to_string()
        };
    }

    // Check for BeagleBone
    if std::path::Path::new("/dev/ttyO1").exists() {
        return "BeagleBone S9".to_string();
    }

    "Unknown".to_string()
}

/// Read miner serial number from EEPROM at I2C 0x51, fallback to MAC-derived.
pub fn read_miner_serial() -> Option<String> {
    // Try EEPROM (24C02 at addr 0x51) on available I2C buses.
    // Zynq: EEPROM on bus 1. Amlogic S21: EEPROM on bus 1 (verified at .135).
    // Try bus 1 first, then bus 0 as fallback.
    let eeprom_buses: &[u8] = if std::path::Path::new("/dev/uio0").exists() {
        &[1] // Zynq: bus 1 only (bus 0 is AXI IIC for PICs)
    } else {
        &[1, 0] // Amlogic: try bus 1 then bus 0
    };
    for &bus in eeprom_buses {
        // Identity-only EEPROM read — uses HAL-public read-only helper
        // (W11 fix: I2cBus::open is pub(crate); read_eeprom_bytes is the
        // sanctioned daemon-side path for one-shot identity reads before
        // the main I²C service starts).
        if let Ok(buf) = dcentrald_hal::i2c::read_eeprom_bytes(bus, 0x51, 0x00, 32) {
            let serial = String::from_utf8_lossy(&buf)
                .trim_end_matches(|c: char| {
                    c == '\0' || c == '\u{00ff}' || !c.is_ascii_alphanumeric()
                })
                .to_string();
            if serial.len() >= 8 && serial.chars().all(|c| c.is_ascii_alphanumeric()) {
                tracing::info!(serial = %serial, "Miner serial from EEPROM");
                return Some(serial);
            }
        }
    }

    // Fallback: derive from MAC address
    std::fs::read_to_string("/sys/class/net/eth0/address")
        .ok()
        .map(|mac| {
            let mac = mac.trim().to_uppercase().replace(':', "");
            format!("DCENT{}", mac)
        })
}

/// Read hash board type from EEPROM at I2C 0x51 (offset varies by model).
pub fn read_hb_type() -> Option<String> {
    // Native text/sysfs hints first.
    if let Ok(data) = std::fs::read_to_string("/tmp/miner_hwver") {
        let trimmed = data.trim();
        if !trimmed.is_empty() && trimmed.len() < 32 {
            return Some(trimmed.to_string());
        }
    }

    // EEPROM sysfs is BINARY — read as bytes and extract ASCII portion
    // Try both bus 1 and bus 0 (Amlogic may use different bus numbering)
    let eeprom_data = std::fs::read("/sys/bus/i2c/devices/1-0051/eeprom")
        .or_else(|_| std::fs::read("/sys/bus/i2c/devices/0-0051/eeprom"));
    if let Ok(data) = eeprom_data {
        if let Some(token) = extract_hb_type_token(&data) {
            return Some(token);
        }
    }

    None
}

/// Extract the LONGEST contiguous ASCII-alphanumeric run from the first 64
/// EEPROM bytes, returning it only if its length is in `[6, 32)`.
///
/// prod-readiness hunt-2 #H1: the prior implementation GLOBALLY filtered the
/// first 64 bytes to alphanumeric and concatenated the survivors — dropping
/// separators/NUL/binary instead of treating them as field delimiters, which
/// SPLICED unrelated EEPROM fields (board model + serial digits + revision) into
/// one contiguous token that looked like a board-type string but was not any real
/// field. A real EEPROM string field is contiguous, so we stop at the first
/// non-alphanumeric byte and keep the longest single run. The AUTHORITATIVE
/// hashboard SKU classification is the structured 2-byte preamble path
/// (`read_hashboard_eeprom_preamble_for_slot` + `classify_by_eeprom_preamble`,
/// wired telemetry-only in `daemon.rs`); this loose scan is a display-only
/// fallback and must never fabricate a spliced string. Read-only inventory path.
fn extract_hb_type_token(data: &[u8]) -> Option<String> {
    let mut run = String::new();
    let mut best = String::new();
    for &b in data.iter().take(64) {
        if b.is_ascii_alphanumeric() {
            run.push(b as char);
        } else if run.len() > best.len() {
            best = std::mem::take(&mut run);
        } else {
            run.clear();
        }
    }
    if run.len() > best.len() {
        best = run;
    }
    if best.len() >= 6 && best.len() < 32 {
        Some(best)
    } else {
        None
    }
}

/// Probe PSU via I2C for model, firmware version, serial, and voltage range.
/// Returns (model, fw_version, serial, voltage_range).
pub fn probe_psu_info() -> Option<(
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    let mut psu = match dcentrald_hal::psu::PsuController::open_kernel_only() {
        Ok(p) => p,
        Err(_) => return None,
    };

    if !psu.probe() {
        return None;
    }

    let fw_version = psu.get_version().ok();
    let voltage_range = fw_version
        .as_deref()
        .and_then(dcentrald_hal::psu::PsuController::format_voltage_range);
    let model = fw_version
        .as_deref()
        .map(dcentrald_hal::psu::PsuController::model_name_from_version);

    Some((model, fw_version, None, voltage_range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static EEPROM_PIC_DETECT_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn chip_identity_high_confidence_when_config_and_board_target_agree() {
        let resolved = resolve_chip_identity(Some("s19jpro"), Some("am2-s19jpro-zynq"));

        assert_eq!(resolved.chip_label, "BM1362");
        assert_eq!(resolved.identification.confidence, "high");
        assert_eq!(
            resolved.identification.sources,
            vec![
                "config_model:s19jpro->BM1362".to_string(),
                "board_target:am2-s19jpro-zynq->BM1362".to_string(),
            ]
        );
    }

    #[test]
    fn chip_identity_surfaces_conflict_without_changing_precedence() {
        let resolved = resolve_chip_identity(Some("s19pro"), Some("am2-s19jpro-zynq"));

        assert_eq!(
            resolved.chip_label, "BM1398",
            "configured model remains the selected chip label for compatibility"
        );
        assert_eq!(resolved.identification.confidence, "low");
        assert!(resolved
            .identification
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("disagree"));
    }

    #[test]
    fn chip_identity_board_target_only_stays_high_confidence() {
        let resolved = resolve_chip_identity(None, Some("am1-s9"));

        assert_eq!(resolved.chip_label, "BM1387");
        assert_eq!(resolved.identification.confidence, "high");
        assert_eq!(
            resolved.identification.sources,
            vec!["board_target:am1-s9->BM1387".to_string()]
        );
    }

    #[test]
    fn chip_identity_unknown_is_explicit() {
        let resolved = resolve_chip_identity(None, Some("am2"));

        assert_eq!(resolved.chip_label, "Auto-detect");
        assert_eq!(resolved.identification.confidence, "unknown");
        assert!(resolved.identification.sources.is_empty());
    }

    /// prod-readiness hunt-2 #H1: the hb-type extractor must take a single
    /// contiguous alphanumeric field, NOT splice across separator/NUL/binary
    /// bytes (the old global filter fused model+serial+revision into a fake SKU).
    #[test]
    fn extract_hb_type_token_does_not_splice_across_separators() {
        // model "BHB42601" then NUL then serial digits then NUL then revision.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"BHB42601");
        buf.push(0x00);
        buf.extend_from_slice(b"SN12345678");
        buf.push(0x00);
        buf.extend_from_slice(b"R1");
        // Longest contiguous run is the 10-char serial — NOT a splice of all three.
        assert_eq!(
            extract_hb_type_token(&buf).as_deref(),
            Some("SN12345678"),
            "must return the longest contiguous field, never a cross-separator splice"
        );
        // A clean single field returns as-is.
        assert_eq!(
            extract_hb_type_token(b"BHB42831").as_deref(),
            Some("BHB42831")
        );
        // All-binary / no usable run -> None (not a fabricated token).
        assert_eq!(extract_hb_type_token(&[0x00, 0xff, 0x10, 0x80]), None);
        // Too-short run -> None.
        assert_eq!(extract_hb_type_token(b"AB\x00CD"), None);
        // Bound at 64 bytes + the [6,32) length window (32+ char run rejected).
        assert_eq!(extract_hb_type_token(&[b'A'; 40]), None);
    }

    #[test]
    fn hash_hashboard_eeprom_bytes_rejects_blank_data() {
        assert!(hash_hashboard_eeprom_bytes(1, 0x50, &[0x00; 256]).is_none());
        assert!(hash_hashboard_eeprom_bytes(1, 0x50, &[0xff; 256]).is_none());
        assert!(hash_hashboard_eeprom_bytes(1, 0x50, &[0x05; 8]).is_none());
    }

    #[test]
    fn hash_hashboard_eeprom_bytes_is_stable_and_labels_bus_addr() {
        let mut data = [0u8; 256];
        data[0] = 0x05;
        data[1] = 0x11;
        data[2] = 0xa5;

        let first = hash_hashboard_eeprom_bytes(1, 0x50, &data).expect("fingerprint");
        let second = hash_hashboard_eeprom_bytes(1, 0x50, &data).expect("fingerprint");

        assert_eq!(first, second);
        assert!(first.starts_with("i2c1-0x50:sha256:"));
        assert_eq!(first.len(), "i2c1-0x50:sha256:".len() + 32);
    }

    // ----  Phase 2B: resolve_pic_type (pure decision function) ----

    fn nopic_probe() -> ChainProbe {
        // BHB56902 / 0x05 0x11 — S19k Pro BM1366 NoPic.
        ChainProbe::Classified {
            chain_id: 0,
            preamble: [0x05, 0x11],
            sku: Hashboard::Bhb56902,
        }
    }

    fn bhb42_family_probe() -> ChainProbe {
        // BHB42xxx / 0x04 0x11 — family preamble only. RE-016 found both
        // PIC and NoPic BHB42xxx SKUs, so this is not authoritative.
        ChainProbe::Classified {
            chain_id: 0,
            preamble: [0x04, 0x11],
            sku: Hashboard::Bhb42601,
        }
    }

    #[test]
    fn nopic_preamble_gate_on_forces_nopic_over_dspic_declarative() {
        // The exact bug: declarative says dsPIC (BM1366 default profile),
        // but the EEPROM says NoPic. Gate ON ⇒ NoPic wins.
        assert_eq!(
            resolve_pic_type(PicType::DsPic33EP, Some(&nopic_probe()), true),
            PicType::NoPic
        );
        // Also overrides the S9 default.
        assert_eq!(
            resolve_pic_type(PicType::Pic16F1704, Some(&nopic_probe()), true),
            PicType::NoPic
        );
    }

    #[test]
    fn bhb42_family_preamble_gate_on_is_ambiguous() {
        // `0x04 0x11` only proves BHB42xxx family membership; RE-016 found
        // both PIC and NoPic variants in that family. Gate ON must not force
        // dsPIC from the preamble-only canonical Bhb42601 stand-in.
        assert_eq!(
            resolve_pic_type(PicType::DsPic33EP, Some(&bhb42_family_probe()), true),
            PicType::DsPic33EP
        );
        assert_eq!(
            resolve_pic_type(PicType::NoPic, Some(&bhb42_family_probe()), true),
            PicType::NoPic
        );
    }

    #[test]
    fn weak_signals_gate_on_fall_back_to_declarative() {
        for probe in [
            ChainProbe::MalformedPreamble {
                chain_id: 0,
                preamble: [0xde, 0xad],
            },
            ChainProbe::Timeout { chain_id: 0 },
            ChainProbe::Unpopulated { chain_id: 0 },
            ChainProbe::ReadError { chain_id: 0 },
            // Classified but to a SKU whose family the preamble can't pin.
            ChainProbe::Classified {
                chain_id: 0,
                preamble: [0x00, 0x00],
                sku: Hashboard::BhbS17,
            },
        ] {
            // NEVER overrides toward dsPIC on a weak signal: a NoPic
            // declarative stays NoPic, a dsPIC declarative stays dsPIC.
            assert_eq!(
                resolve_pic_type(PicType::DsPic33EP, Some(&probe), true),
                PicType::DsPic33EP,
                "weak {:?} must not change a dsPIC declarative",
                probe
            );
            assert_eq!(
                resolve_pic_type(PicType::NoPic, Some(&probe), true),
                PicType::NoPic,
                "weak {:?} must not change a NoPic declarative",
                probe
            );
        }
        // Absent probe entirely → declarative.
        assert_eq!(
            resolve_pic_type(PicType::DsPic33EP, None, true),
            PicType::DsPic33EP
        );
    }

    #[test]
    fn gate_off_is_byte_identical_to_declarative() {
        // With the gate OFF, the probe is ignored entirely — every input
        // returns the declarative result unchanged (the no-regression
        // guarantee: default behavior is byte-identical to today).
        for declarative in [PicType::Pic16F1704, PicType::DsPic33EP, PicType::NoPic] {
            for probe in [
                Some(nopic_probe()),
                Some(bhb42_family_probe()),
                Some(ChainProbe::Timeout { chain_id: 0 }),
                None,
            ] {
                assert_eq!(
                    resolve_pic_type(declarative, probe.as_ref(), false),
                    declarative,
                    "gate OFF must return declarative {:?} for probe {:?}",
                    declarative,
                    probe
                );
            }
        }
    }

    #[test]
    fn sku_family_mapper_covers_nopic_and_pic_families() {
        assert_eq!(
            pic_type_for_classified_sku(Hashboard::Bhb56902),
            Some(PicType::NoPic)
        );
        for sku in [
            Hashboard::Bhb42603,
            Hashboard::Bhb42631,
            Hashboard::Bhb42632,
            Hashboard::Bhb42651,
            Hashboard::Bhb42611,
            Hashboard::Bhb42811,
            Hashboard::Bhb42821,
            Hashboard::Bhb42831,
            Hashboard::Bhb42841,
        ] {
            assert_eq!(
                pic_type_for_classified_sku(sku),
                Some(PicType::NoPic),
                "RE-016 refined SKU {:?} is NoPic",
                sku
            );
        }
        for sku in [
            Hashboard::Bhb42621,
            Hashboard::Bhb42641,
            Hashboard::Bhb42801,
            Hashboard::Bhb42803,
            Hashboard::Bhb42701,
        ] {
            assert_eq!(
                pic_type_for_classified_sku(sku),
                Some(PicType::DsPic33EP),
                "refined SKU {:?} has PIC/dsPIC evidence",
                sku
            );
        }
        // The current preamble classifier returns Bhb42601 as a canonical
        // stand-in for the whole BHB42xxx family, so it is ambiguous here.
        assert_eq!(pic_type_for_classified_sku(Hashboard::Bhb42601), None);
        assert_eq!(
            pic_type_for_classified_sku(Hashboard::BhbS9 { chain_index: 0 }),
            Some(PicType::Pic16F1704)
        );
        // Placeholders whose family the preamble can't pin → None.
        assert_eq!(pic_type_for_classified_sku(Hashboard::BhbS17), None);
        assert_eq!(pic_type_for_classified_sku(Hashboard::BhbS11), None);
    }

    #[test]
    fn gate_default_off_resolves_to_declarative_without_io() {
        let _guard = EEPROM_PIC_DETECT_ENV_LOCK.lock().unwrap();
        // Default-OFF: with the env unset, the EEPROM-authority entry
        // points return the declarative result and do NOT touch the
        // filesystem (host-safe, byte-identical to today). We assert the
        // helper is default-off and the resolvers short-circuit.
        std::env::remove_var("DCENT_AM2_EEPROM_PIC_DETECT");
        assert!(!eeprom_pic_detect_enabled());
        assert_eq!(
            resolve_pic_type_from_eeprom(PicType::DsPic33EP, 3),
            PicType::DsPic33EP
        );
        assert_eq!(
            resolve_pic_type_from_eeprom(PicType::NoPic, 3),
            PicType::NoPic
        );
        assert!(!resolve_is_nopic_from_eeprom(false, 3));
        assert!(resolve_is_nopic_from_eeprom(true, 3));
    }

    #[test]
    fn env_helper_recognizes_truthy_values() {
        let _guard = EEPROM_PIC_DETECT_ENV_LOCK.lock().unwrap();
        std::env::set_var("DCENT_AM2_EEPROM_PIC_DETECT", "1");
        assert!(eeprom_pic_detect_enabled());
        std::env::set_var("DCENT_AM2_EEPROM_PIC_DETECT", "true");
        assert!(eeprom_pic_detect_enabled());
        std::env::set_var("DCENT_AM2_EEPROM_PIC_DETECT", "0");
        assert!(!eeprom_pic_detect_enabled());
        std::env::remove_var("DCENT_AM2_EEPROM_PIC_DETECT");
        assert!(!eeprom_pic_detect_enabled());
    }
}
