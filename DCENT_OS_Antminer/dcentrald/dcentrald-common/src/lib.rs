//! Shared no-HAL contract utilities for dcentrald.
//!
//! This crate is intentionally HAL-free, OS-free, and async-runtime-free, in
//! the same spirit as `dcentrald-api-types`. It is the host-safe boundary
//! for utilities that must be reachable from both the API surface
//! (`dcentrald-api`) and the protocol clients (`dcentrald-stratum`) without
//! pulling in a hardware dependency. New no-HAL helpers (config-validation,
//! ID redaction, units conversion) belong here.
//!
//! Modules:
//! - [`wallet_mask`] — Bitcoin/Litecoin wallet-address masking for log and
//!   UI emission. Provides `mask_wallet()`, `mask_in_string()`, and
//!   `is_likely_wallet()`. See module docs for the threat model and which
//!   addresses are recognized (bech32, bech32m, base58 P2PKH/P2SH, hex).
//! - [`chain_voltage`] — AT-1 measured-vs-commanded per-chain rail-voltage
//!   resolver. Reuses [`dspic_decode`]'s 0x3A `MEASURE_VOLTAGE` decode to feed
//!   the autotuner/telemetry a provenance-tagged rail voltage (read-back only).
//! - [`at3_rail`] — AT-3 process-global publish/consume slot. The am2 hybrid
//!   loop's gated, default-OFF quiet-window 0x3A read publishes a fresh measured
//!   rail here; the API per-chain telemetry projection reads it back so a
//!   plausible reading is tagged `measured`. Read-only/measure-only.
//! - [`atomic_file`] — bounded same-filesystem state replacement and durable
//!   deletion with directory fsync plus explicit publication/failure evidence.

pub mod am2_topology;
pub mod at3_rail;
pub mod atomic_file;
/// Canonical build target and published filename for each artifact claim.
pub mod artifact_producer;
/// Declarative control-board composition identity (ADR-0011). Scaffold registry.
pub mod board_desc;
pub mod chain_voltage;
pub mod dspic_decode;
pub mod dspic_heartbeat;
/// Install/packaging matrix from BoardDesc (toolbox/CI/docs generators).
pub mod install_matrix;
/// PowerCut + FanCommand policy (cut-hash-before-noise; home PWM cap).
pub mod safety_command;
/// Pure serial work-engine bookkeeping (history ring, dedup, job-id cursor).
pub mod serial_work_engine;
/// Shared serial work-history / job-id / dedup policy (mining strangler).
pub mod serial_work_policy;
pub mod time;
pub mod units;
/// VoltageRail facet trait + errors (ADR-0010). Adapters live in asic/hal.
pub mod voltage_rail;
pub mod wallet_mask;

pub use board_desc::{
    AsicProtocolAdmission, AsicProtocolIdentity, BoardDesc, BoardFamily, ChainTransportKind,
    SlotPolicy, VoltageControllerClass, WorkEngineKind,
};
pub use dcent_schema::hardware::{
    ArtifactKind, ArtifactMaturity, HardwareEnablementPolicy, ImplementationMaturity,
    InstallAuthorization, RecoveryMaturity, StorageTopology, UpdateMechanism,
    HARDWARE_ENABLEMENT_SCHEMA_VERSION,
};
pub use install_matrix::{
    ab_sysupgrade_board_targets, install_matrix, install_matrix_json, install_matrix_tsv,
    public_beta_board_targets, InstallMatrixRow,
};
pub use safety_command::{
    power_precedes_fan_raise, violates_home_fan_cap, FanCommand, PowerCut, PowerCutReason,
    SafetyAction, SafetyStep, FAN_PWM_ABSOLUTE_MAX, HOME_FAN_PWM_SAFETY_MAX,
};
pub use serial_work_engine::{
    AsicJobIdCursor, SeenShareSet, SerialWorkBookkeeping, WorkHistoryEntry, WorkHistoryRing,
};
pub use serial_work_policy::{
    next_asic_job_id, serial_share_dedup_key, should_clear_seen_shares,
    work_history_depth_for_chip_id, AM3_BB_WORK_HISTORY_PER_ID, BM1362_SERIAL_NONCE_LEN,
    BM1398_WORK_HISTORY_PER_ID, DEFAULT_SEEN_SHARES_CAP, DEFAULT_SERIAL_JOB_ID_STEP,
    DEFAULT_WORK_HISTORY_PER_ID,
};
pub use voltage_rail::{
    is_hard_refuse, refuse_degraded_firmware, refuse_wrong_mode, unsupported_voltage_path,
    VoltageRail, VoltageRailError, VoltageRefuseReason,
};

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide gate for the log-tail passthrough sanitizer.
///
/// W1.4: when `true` (the default), the `/api/debug/log` endpoint masks
/// any wallet-shaped substrings before serializing the response. Setting
/// this to `false` is an opt-out for operators with structured-log
/// collectors that need raw addresses.
///
/// Per-call masking on `worker=` / `username=` / `wallet=` log fields is
/// independent of this flag and cannot be disabled via the config. To see
/// raw addresses on the wire, use `RUST_LOG=trace` (TRACE-level only,
/// gated by EnvFilter, off by default in production).
static MASK_LOGS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set the process-wide log-tail mask flag. Called once at daemon startup
/// from the [logging] section of dcentrald.toml.
pub fn set_mask_logs(enabled: bool) {
    MASK_LOGS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Read the process-wide log-tail mask flag.
pub fn mask_logs_enabled() -> bool {
    MASK_LOGS_ENABLED.load(Ordering::Relaxed)
}
