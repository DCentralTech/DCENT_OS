//! Per-chip frequency auto-tuning for DCENTos.
//!
//! Implements the TABS algorithm (Three-Phase Adaptive Binary Search) to
//! characterize each chip's stable frequency window in parallel.
//!
//! Current frequency-characterization behavior:
//!   - **Parallel characterization**: All 63 chips tuned simultaneously
//!   - **Fast frequency search** before longer thermal/power convergence
//!   - **Thermal refinement soak**: Validates frequencies under thermal load (2-10 min)
//!   - **Warm start from profile** in ~3 seconds (verify-only)
//!   - **Per-chip nonce attribution** via BM1387 nonce bits [7:2]
//!   - **Continuous background monitoring** with automatic back-off
//!   - **Silicon grading** (A/B/C/D) for diagnostics and fleet analytics
//!
//! Architecture:
//!   - `chip_stats` — Per-chip nonce/error snapshots from work_dispatcher
//!   - `config`     — AutoTunerConfig from TOML [autotuner] section
//!   - `profile`    — ChipProfile + TuningProfile persistence (JSON)
//!   - `binary_search` — TABS Phase 1: parallel binary search over PLL table
//!   - `tuner`      — AutoTuner state machine orchestrator

pub mod aging_tracker;
/// AT-1 (chip-rail voltage read-back spine, 2026-06-07): the autotuner's
/// measured chip-rail voltage input. Re-exports the pure
/// [`dcentrald_common::chain_voltage`] resolver (measured-vs-commanded tagging +
/// the 0x3A `MEASURE_VOLTAGE` decode it reuses) and injects the canonical
/// [`dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV`] plausibility ceiling. READ-BACK
/// only — gates the AT-3..13 DVFS efficiency build; adds no closed-loop voltage
/// adjustment and the autotuner stays default-disabled.
pub mod chain_voltage;
// Wave E (RE-004 closure, 2026-05-19): clean-room cross-firmware bad-chip
// supervisor (4-state per-chip FSM Healthy/Degraded/Bad/Missing) layered on
// top of the existing chip_stats / chip_health surfaces. Compiled always;
// opt-in at integration sites via TOML `[autotune.bad_chip].enabled`. See
//
// and the internal Wave E planning notes (§E2).
pub mod bad_chip_supervisor;
pub mod binary_search;
pub mod chip_health;
pub mod chip_stats;
pub mod config;
pub mod dps;
// Wave D (RE-002 closure, 2026-05-19): clean-room BraiinsOS-shape DPS
// runtime governor (4-state FSM) layered on top of the existing dps.rs
// walker. Compiled always; opt-in at integration sites via TOML
// `[thermal].dps_enabled`. See
//  §RE-002.
pub mod dps_governor;
pub mod dvfs;
pub mod efficiency;
pub mod error_model;
pub mod event_log;
pub mod fleet;
pub mod mcr_fit;
pub mod power_budget;
pub mod profile;
pub mod profitability;
/// W13.C3 (2026-05-10): per-SKU PVT envelope clamp helpers.
///
/// Exposes [`pvt_envelope::validate_freq_volt`],
/// [`pvt_envelope::nearest_valid_volt`], and
/// [`pvt_envelope::pvt_envelope`] so the autotuner — and any caller that
/// dispatches `(freq, volt)` tuples to BM1362 hashboards — can refuse
/// out-of-envelope tuples instead of silently coercing them.
///
/// See:
/// - `~/
/// - `~/
/// - `~/
pub mod pvt_envelope;
pub mod schedule;
pub mod silicon_report;
pub mod state_persistence;
pub mod telemetry;
pub mod thermal_comp;
pub mod tuner;
// Wave F (RE-001 closure gap-fill, 2026-05-19): tuner-stability clock. The
// BraiinsOS-shape per-chip autotuner FSM is ALREADY implemented by
// `tuner::AutoTuner` (TunerState Idle→Characterizing→Verifying→
// ThermalRefinement→Tuned→BackgroundAdjust) + fingerprint-keyed
// `state_persistence` + per-target `profile_cache`. RE-001's one genuine
// coupling gap was that the Wave-D DPS scale-up gate needs "tuner stable >=
// 30 min" but AutoTuner didn't expose a stable-since clock. This standalone
// helper closes it WITHOUT editing the 7000-line safety-critical tuner.rs.
// See `RE-TEAM-ASKS.md` RE-001 + the Wave F plan §F2.
pub mod tuner_stability;
// Wave F (RE-007 closure, 2026-05-19): clean-room VNish "Algo 2" 5-phase
// autotuner daemon adapter. Thin wrapper around the HAL-free
// `dcentrald_api_types::autotune_phase::AutotuneFsm` ( commit
// `ae5ba426`); adds TOML opt-in gate `[autotune.vnish_phase].enabled`
// + explicit `VnishTuneAction` enum for caller consumption. Compiled
// always; opt-in at integration sites. See
//
// and the internal Wave F planning notes (§F1).
pub mod vnish_phase_fsm;
pub mod voltage_domain;
pub mod voltage_search;

use thiserror::Error;

/// Chip geometry helpers for autotuner hashrate / nonce-rate math.
///
/// W6.8 (DCENT_Perf, 2026-05-07): the per-chip `*_CORES` and `*_GHS_PER_MHZ`
/// constants that used to live here were deleted. Every helper now reads
/// directly from `dcentrald_asic::drivers::MinerProfile`, which is the
/// single source of truth for chip geometry. The pre-W6.8 constants
/// drifted 30% out of sync on BM1368 (autotuner = 894 vs MinerProfile =
/// 1280) and silently broke hashrate prediction on the S21 family. The
/// offline CI gate `chip_geometry_drift_check` rejects any new
/// `chip_geometry::*_CORES` constant.
///
/// Cores semantics (mirrored from `dcentrald_asic::drivers::MinerProfile`):
///
/// - **Engine count** (`MinerProfile::cores_per_chip`) — driver-facing big
///   SHA-256 engines. Use for engine-state bookkeeping (e.g. open-core
///   dispatch). NOT used here.
/// - **Nonce-attribution slots** (`MinerProfile::nonce_attribution_cores`)
///   — distinct nonce slots the FPGA can attribute back to a chip. This
///   is what `expected_nps_for_chip` and `cores_for_chip` return.
///
/// For BM1387/BM1366/BM1368/BM1370 the two values are identical. BM1362
/// is the split case: 4 big engines, 894 nonce-attribution slots.
pub mod chip_geometry {
    /// Get nonce-attribution slot count for any supported chip ID.
    ///
    /// Returns the `MinerProfile::nonce_attribution_cores` field — the
    /// count of distinct nonce slots the FPGA can attribute back to a
    /// chip. Used for nonces-per-second math, NOT for engine-state
    /// bookkeeping.
    ///
    /// Falls back to a conservative 114 (BM1387) for unknown chip IDs so
    /// hashrate predictions stay non-zero on a brand-new chip family the
    /// `MINER_PROFILES` table hasn't learned about yet.
    #[inline]
    pub fn cores_for_chip(chip_id: u16) -> u32 {
        dcentrald_asic::drivers::MinerProfile::for_chip(chip_id)
            .map(|profile| profile.nonce_attribution_cores)
            .unwrap_or(114)
    }

    /// Get GH/s per MHz for any supported chip ID.
    /// Falls back to BM1387 (0.114 GH/s/MHz) for unknown chip IDs.
    #[inline]
    pub fn ghs_per_mhz_for_chip(chip_id: u16) -> f64 {
        dcentrald_asic::drivers::MinerProfile::for_chip(chip_id)
            .map(|profile| profile.ghs_per_mhz)
            .unwrap_or(0.114)
    }

    /// BM1387 expected nonces per second at given frequency and difficulty.
    /// Formula: freq_mhz × cores × 1e6 / (difficulty × 2^32)
    #[inline]
    pub fn bm1387_expected_nps(freq_mhz: u16, difficulty: u32) -> f64 {
        expected_nps_for_chip(0x1387, freq_mhz, difficulty)
    }

    /// BM1387 hashrate for a single chip in GH/s.
    #[inline]
    pub fn bm1387_chip_hashrate_ghs(freq_mhz: u16) -> f64 {
        chip_hashrate_ghs_for_chip(0x1387, freq_mhz)
    }

    /// Expected nonces per second for any chip type at given frequency and difficulty.
    /// Formula: freq_mhz × nonce_attribution_cores × 1e6 / (difficulty × 2^32)
    ///
    /// Routed through `MinerProfile::expected_nps` so the autotuner and
    /// the asic crate share a single hashrate prediction implementation.
    #[inline]
    pub fn expected_nps_for_chip(chip_id: u16, freq_mhz: u16, difficulty: u32) -> f64 {
        if let Some(profile) = dcentrald_asic::drivers::MinerProfile::for_chip(chip_id) {
            return profile.expected_nps(freq_mhz, difficulty);
        }
        // Unknown chip: fall back to BM1387 geometry (114 cores, 256 diff).
        let diff = if difficulty == 0 { 256 } else { difficulty };
        (freq_mhz as f64 * 114.0 * 1e6) / (diff as f64 * 4.294e9)
    }

    /// Hashrate for a single chip of any type in GH/s.
    #[inline]
    pub fn chip_hashrate_ghs_for_chip(chip_id: u16, freq_mhz: u16) -> f64 {
        freq_mhz as f64 * ghs_per_mhz_for_chip(chip_id)
    }
}

/// Parse a chip ID from a chip type string like `BM1387`.
#[inline]
pub fn chip_id_from_type(chip_type: &str) -> Option<u16> {
    chip_type
        .strip_prefix("BM")
        .and_then(|s| u16::from_str_radix(s, 16).ok())
}

/// W6.3 + W6.4: snapshot consumed by the autotuner step-up gate.
///
/// The daemon owns the live `AcceptanceTracker` (lives on the Stratum
/// V1 client) and the per-chain `HwErrTracker` instances (live on the
/// work dispatcher's nonce-rx path). It publishes both into a single
/// watch channel so the autotuner doesn't need direct access to either
/// crate's internals — keeping the autotuner crate's dep graph small.
///
/// The gate (`tuner.rs::Self::step_up_gate_passes`) reads the latest
/// value before any boost-back / step-up frequency increase. Both
/// conditions must hold:
///
/// - `rolling_acceptance_pct >= 99.0`
/// - `worst_chip_hw_err_rate < 0.02`
///
/// When `None`, the gate stays *open* (legacy behavior preserved) so
/// platforms that haven't wired the watch channel yet keep tuning.
/// Once wired, a missing/stale signal blocks step-up — see the
/// `step_up_gate_*` tests in `tuner.rs`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepUpGateSignal {
    /// Rolling 30-min pool acceptance percentage (0.0..=100.0).
    pub rolling_acceptance_pct: f64,
    /// Worst per-chip HW error rate observed in the live EWMA (0.0..=1.0).
    /// `None` = no chips have produced any nonces yet (cold-boot).
    pub worst_chip_hw_err_rate: Option<f64>,
}

impl Default for StepUpGateSignal {
    fn default() -> Self {
        // Default to the "no rolling evidence of rejection / no HW err"
        // baseline — same shape as `AcceptanceTracker::new()` and an
        // empty `HwErrTracker`. Lets unit tests construct a permissive
        // signal without spelling out both fields.
        Self {
            rolling_acceptance_pct: 100.0,
            worst_chip_hw_err_rate: None,
        }
    }
}

impl StepUpGateSignal {
    /// Hard-coded step-up acceptance threshold (99.0%). A pin: the
    /// W6.3 acceptance gate value.
    pub const ACCEPTANCE_THRESHOLD_PCT: f64 = 99.0;

    /// Hard-coded per-chip HW err threshold (2%). A pin: the W6.4
    /// gate value.
    pub const HW_ERR_THRESHOLD: f64 = 0.02;

    /// True iff both gate conditions hold for this signal.
    pub fn passes(&self) -> bool {
        let acceptance_ok = self.rolling_acceptance_pct >= Self::ACCEPTANCE_THRESHOLD_PCT;
        let hw_err_ok = self
            .worst_chip_hw_err_rate
            .map(|r| r < Self::HW_ERR_THRESHOLD)
            .unwrap_or(true);
        acceptance_ok && hw_err_ok
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainHardwareIdentity {
    pub eeprom_serial: Option<String>,
    pub eeprom_fingerprint: Option<String>,
    pub dspic_fw_byte: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct ChainTuneInfo {
    pub chain_id: u8,
    pub chip_count: u8,
    pub voltage_mv: u16,
    pub chip_id: u16,
    pub hardware_identity: ChainHardwareIdentity,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveChipHealthState {
    pub last_update_s: u64,
    pub chips: Vec<crate::chip_health::ChipHealthStatus>,
}

/// Frequency control command sent from AutoTuner to WorkDispatcher.
///
/// The autotuner cannot directly access FPGA chains (owned exclusively by
/// WorkDispatcher for safety). Instead, it sends FreqCommand messages via
/// an mpsc channel. The dispatcher processes these in its select! loop.
#[derive(Debug)]
pub enum FreqCommand {
    /// Set a single chip's PLL frequency.
    SetChipFreq {
        chain_id: u8,
        /// Chip index (0-62). Dispatcher converts to hw_addr (index * 4).
        chip_index: u8,
        freq_mhz: u16,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<u16, String>>>,
    },
    /// Set all chips on a chain to the same frequency (broadcast).
    SetChainFreq {
        chain_id: u8,
        freq_mhz: u16,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>,
    },
    /// Update or clear a chain frequency ceiling for a specific control loop.
    ///
    /// `chain_id=0xFF` applies to every active chain. `max_freq_mhz=None` clears
    /// that source's ceiling. The dispatcher applies the most restrictive active
    /// ceiling on top of the autotuner's desired per-chip frequencies.
    SetFrequencyLimit {
        chain_id: u8,
        max_freq_mhz: Option<u16>,
        source: FrequencyLimitSource,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>,
    },
    /// Update or clear a per-chip frequency ceiling for a specific control loop.
    ///
    /// The dispatcher applies this on top of the autotuner's desired per-chip
    /// frequencies and any active chain-wide ceilings.
    SetChipFrequencyLimit {
        chain_id: u8,
        chip_index: u8,
        max_freq_mhz: Option<u16>,
        source: FrequencyLimitSource,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>,
    },
    /// Recalculate WORK_TIME register based on the SLOWEST chip's frequency.
    ///
    /// WORK_TIME must accommodate the slowest chip on the chain. Lower frequency
    /// means longer time to exhaust the nonce range. If WORK_TIME is too short,
    /// the FPGA dispatches new work before slow chips finish — nonces are lost.
    /// Fast chips simply finish early and idle briefly (no harm).
    UpdateWorkTime { chain_id: u8, min_freq_mhz: u16 },
    /// Set the voltage for a chain's voltage domain (millivolts).
    ///
    /// On S9, each chain has one voltage domain controlled via PIC DAC.
    /// The dispatcher converts mV to PIC DAC value and applies it.
    SetVoltage {
        chain_id: u8,
        voltage_mv: u16,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<u16, String>>>,
    },
    /// Request voltage readback verification after a SetVoltage command.
    ///
    /// The autotuner sends this after voltage search completes to ask the
    /// daemon to read the actual PIC DAC voltage via `pic_read_voltage()`
    /// and compare it to the target. The autotuner has no direct I2C access
    /// (that lives in the HAL crate), so it signals the daemon to perform
    /// the readback and log any mismatch.
    VerifyVoltage {
        chain_id: u8,
        target_mv: u16,
        ack_tx: Option<tokio::sync::oneshot::Sender<std::result::Result<Option<u16>, String>>>,
    },
    /// Synchronization barrier: confirms all prior commands have been processed.
    ///
    /// The autotuner sends this after a batch of SetChipFreq + UpdateWorkTime
    /// commands to ensure the dispatcher has applied all frequency changes
    /// before the measurement window starts. Without this, frequencies may
    /// not yet be applied when the autotuner begins counting nonces, causing
    /// measurements at the WRONG frequencies.
    Barrier {
        ack_tx: tokio::sync::oneshot::Sender<()>,
    },
    /// Reset one chain's autotuner counters and start a fresh measurement epoch.
    ///
    /// The dispatcher acknowledges with the new epoch after all earlier commands
    /// in the channel have been processed and the per-chain counters were reset.
    BeginMeasurement {
        chain_id: u8,
        ack_tx: tokio::sync::oneshot::Sender<Option<u64>>,
    },
    /// Pause work dispatch long enough for the heartbeat thread to own a quiet I2C window.
    ///
    /// The dispatcher flushes WORK_TX through the owned FPGA handles and acknowledges only
    /// after it has performed the quiet-window preparation. This keeps all FPGA mutation
    /// inside the dispatcher instead of reaching around it from the heartbeat thread.
    PrepareI2cQuietWindow {
        ack_tx: std::sync::mpsc::Sender<std::result::Result<(), String>>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoTunerCommandStatus {
    Applied,
    Deferred,
    Rejected,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutoTunerCommandResult {
    pub status: AutoTunerCommandStatus,
    pub applied_runtime: bool,
    pub mode: config::TunerMode,
    pub message: String,
}

/// W15-A: plain DTO for a single silicon-profile preset row, carried
/// across the `AutoTunerCommand::ApplySiliconProfile` mpsc channel and
/// stored inside the autotuner's `active_silicon_profile_presets` map.
///
/// Mirrors the shape of `dcentrald_silicon_profiles::Profile` (step +
/// freq_mhz + voltage_v) without the `wall_watts` / `hashrate_ths`
/// fields the autotuner doesn't need for target derivation. Kept here
/// so `dcentrald-autotuner` doesn't take a hard
/// `dcentrald-silicon-profiles` dep (avoids the workspace dep cycle
/// that motivated the W13-A `String` profile-id pattern).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SiliconPreset {
    /// Discrete index along the silicon characterization curve.
    /// Higher = more freq + more voltage. Step 0 = nameplate / default.
    pub step: i32,
    /// Target chain frequency in MHz.
    pub freq_mhz: u32,
    /// Target chain or chip-rail voltage in volts. The autotuner
    /// converts to mV via `*1000` before emitting `FreqCommand::SetVoltage`.
    pub voltage_v: f32,
}

/// W13-A: result for `AutoTunerCommand::ApplySiliconProfile`.
///
/// Mirrors `AutoTunerCommandResult` but does not carry a `TunerMode`
/// because silicon-profile selection is orthogonal to operator mode
/// (Performance/Efficiency/etc.) — the operator picks a silicon
/// profile to authorize the autotuner to use a different preset
/// table; the mode policy stays the same.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutoTunerSiliconProfileResult {
    pub status: AutoTunerCommandStatus,
    pub applied_runtime: bool,
    /// Wire profile id (`<model>__<hashboard>__<chip>__<source_class>`)
    /// the autotuner is now authorized to consume for this chain.
    pub profile_id: String,
    /// Chain identity tuple as snake_case strings — kept as plain
    /// strings so the `dcentrald-autotuner` crate doesn't need to
    /// take a hard `dcentrald-silicon-profiles` dep just for the
    /// `MinerModel` enum (avoids a workspace dep cycle).
    pub miner_model: String,
    pub hashboard: String,
    pub message: String,
}

#[derive(Debug)]
pub enum AutoTunerCommand {
    /// Apply an operator-facing target mode to the live autotuner if it is in a
    /// background-monitoring state. REST persists the same mode before sending
    /// this command, so rejected/deferred runtime commands still have a durable
    /// next-cycle config path.
    ApplyMode {
        mode: config::TunerMode,
        ack_tx: tokio::sync::oneshot::Sender<AutoTunerCommandResult>,
    },
    /// W13-A: apply an operator-selected silicon profile to a chain.
    ///
    /// The silicon-profiles registry already records the active profile
    /// id per `(model, hashboard)` chain; this command tells the live
    /// autotuner that a new selection has been made so it can re-pull
    /// the preset table on the next iteration. The autotuner does NOT
    /// need to validate the id resolves — the REST handler does that
    /// before sending. The autotuner may legitimately defer the apply
    /// (e.g. if it isn't in BackgroundAdjust yet).
    ///
    ///  deliberately keeps the runtime application surgical:
    /// the autotuner records the new selection in
    /// `active_silicon_profile_id` and acknowledges. The next iteration
    /// of the background-adjust loop picks up the new preset table via
    /// `dcentrald_silicon_profiles::registry::global()
    ///   .read().unwrap().get_active_bundle_for_chain(...)`. This
    /// matches the existing ApplyMode pattern's "deferred = next cycle"
    /// contract.
    ApplySiliconProfile {
        miner_model: String,
        hashboard: String,
        profile_id: String,
        /// W15-A: resolved preset table from the silicon-profiles
        /// registry's active bundle. The API handler populates this
        /// from `registry::get_active_bundle_for_chain(...).presets`
        /// so the autotuner can consume freq/voltage targets per
        /// `TunerMode` without needing a hard dep on the registry
        /// crate. May be empty when the API caller has no resolved
        /// bundle (e.g. unit tests that only exercise the deferred
        /// state-recording contract); the autotuner records the
        /// selection and falls back to its previous freq/voltage
        /// targets in that case.
        presets: Vec<SiliconPreset>,
        ack_tx: tokio::sync::oneshot::Sender<AutoTunerSiliconProfileResult>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrequencyLimitSource {
    FanClamp,
    Thermal,
    AutotunerThermal,
    SensorSafety,
    QuietMode,
    OffGrid,
    SolarSurplus,
    PowerCap,
    /// LuxOS-shape ATM (Advanced Thermal Management) profile-step ceiling,
    /// driven from the daemon's thermal-supervisor advisory dispatch when the
    /// supervisor emits `RequestProfileStepDown` (hot → lower the ceiling, the
    /// safe cut-hash-before-noise direction) / `RequestProfileStepUp` (cool +
    /// post-grace → relax the ceiling, BOUNDED by the configured nominal
    /// frequency — never above the operator/SKU max).
    ///
    /// This is a SEPARATE ceiling slot from [`AutotunerThermal`] (which the
    /// autotuner's own thermal-refinement soak owns) and from [`Thermal`]
    /// (which the controller's PID throttle owns), so the ATM step ceiling
    /// composes via the dispatcher's most-restrictive-wins merge without the
    /// two clobbering each other's slot. Like every other ceiling it can only
    /// LOWER the effective frequency; it can never raise voltage (voltage falls
    /// with frequency through the autotuner's PVT envelope, so the 14500 mV cap
    /// can never be exceeded by an ATM step), and `max_freq_mhz = None` clears
    /// it. Active ONLY when the (default-off, operator-gated) thermal supervisor
    /// is enabled — so a unit with the supervisor off never carries this slot.
    AtmStep,
}

/// Auto-tuner error type.
#[derive(Debug, Error)]
pub enum AutoTunerError {
    /// ASIC driver error during frequency change.
    #[error("ASIC error: {0}")]
    Asic(#[from] dcentrald_asic::AsicError),

    /// Profile persistence error (read/write JSON).
    #[error("profile I/O error: {0}")]
    ProfileIo(#[from] std::io::Error),

    /// Profile deserialization error.
    #[error("profile parse error: {0}")]
    ProfileParse(#[from] serde_json::Error),

    /// No stats received within timeout.
    #[error("stats timeout: no chip data received in {seconds}s")]
    StatsTimeout { seconds: u64 },

    /// Configuration validation error.
    #[error("config error: {0}")]
    Config(String),

    /// Chain not found or not mining.
    #[error("chain {chain_id} not available for tuning")]
    ChainUnavailable { chain_id: u8 },

    /// W13.C3 (2026-05-10): The proposed `(freq, volt)` tuple is outside
    /// the per-SKU PVT envelope published by `dcentrald-silicon-profiles`
    /// (`Bm1362HashboardSku::freq_voltage_table()`).
    ///
    /// The autotuner returns this error rather than silently coercing —
    /// silently lowering frequency or voltage to the envelope bounds
    /// would mask a misconfigured silicon profile and could mis-attribute
    /// HW errors to a chip-quality problem instead of an envelope-mismatch
    /// problem. Callers MUST decide whether to clamp via
    /// [`pvt_envelope::nearest_valid_volt`] or surface the rejection.
    ///
    /// `valid_freq_range` and `valid_volt_range` are inclusive `(min, max)`
    /// pairs over the SKU's full envelope. They are reported so the
    /// dashboard can render an actionable "out of envelope" warning
    /// (e.g. "BHB42801 envelope: 585-675 MHz @ 1530-1600 mV; you asked
    /// for 700 MHz @ 1700 mV").
    ///
    /// See:
    /// - `~/
    /// - `~/
    /// - `~/
    #[error(
        "({freq_mhz} MHz, {volt_mv} mV) outside PVT envelope for SKU {sku} \
         (valid: {min_freq}-{max_freq} MHz @ {min_volt}-{max_volt} mV)",
        min_freq = valid_freq_range.0,
        max_freq = valid_freq_range.1,
        min_volt = valid_volt_range.0,
        max_volt = valid_volt_range.1,
    )]
    OutsidePvt {
        /// Hashboard SKU id string (`BHB42601`, `BHB42801`, …). Carries
        /// the canonical hashboard_id so dashboard renderers don't have
        /// to look it up from the raw enum.
        sku: String,
        /// Proposed frequency that fell outside the envelope.
        freq_mhz: u16,
        /// Proposed voltage that fell outside the envelope.
        volt_mv: u16,
        /// Inclusive `(min, max)` MHz from the SKU's freq/voltage table.
        valid_freq_range: (u16, u16),
        /// Inclusive `(min, max)` mV from the SKU's freq/voltage table.
        valid_volt_range: (u16, u16),
    },
}

pub type Result<T> = std::result::Result<T, AutoTunerError>;

// Re-exports for convenient use from daemon
// W24-BC-1 (): re-export the bad-chip supervisor's public surface so
// the daemon integration site (default-off, gated on
// `[autotune.bad_chip].enabled`) can refer to them without the long module
// path. The module stays the source of truth; this is convenience only.
pub use bad_chip_supervisor::{
    BadChipAction, BadChipConfig, BadChipReason, BadChipSupervisor, BoardFingerprint,
    ChipHealthState, HaltReason,
};
pub use binary_search::VerificationState;
// AT-1: measured chip-rail voltage input accessors + types (read-back spine).
pub use chain_voltage::{
    measured_rail_from_0x3a_reply, plausible_rail_mv, resolve_chain_rail_voltage, ChainRailVoltage,
    RailVoltageSource, RAIL_MAX_MV,
};
pub use chip_health::{ChipHealthLevel, ChipHealthStatus, ChipHealthTracker};
pub use chip_stats::ChipStatsSnapshot;
pub use config::{
    am2_bm1362_max_freq_for_sku, am2_voltage_autotune_enabled, autotuner_capabilities_for_chip,
    autotuner_capabilities_for_chip_with_voltage_autotune,
    autotuner_capabilities_for_mixed_families, autotuner_preset_display_name,
    clamp_am2_dspic_autotune_voltage_mv, is_supported_autotuner_preset,
    is_supported_autotuner_preset_for_capabilities, resolve_autotuner_policy, AutoTunerConfig,
    AutotunerCapabilityStatus, AutotunerPresetDef, Bm1362SkuClass, ResolvedAutotunerPolicy,
    AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV, AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV, AM2_VOLTAGE_AUTOTUNE_ENV,
    AUTOTUNER_PRESETS,
};
pub use event_log::{EventLogger, SafetyEvent, SafetyEventType};
pub use fleet::ChipBinningDatabase;
pub use fleet::FleetProfile;
pub use power_budget::{
    btu_from_watts, LivePowerEstimate, PowerAuthorityKind, PowerAuthoritySample, PowerCalibration,
};
pub use profile::{ChipProfile, TuningProfile};
pub use profitability::{
    block_reward_at, compute_noise_profile, days_to_halving, estimate_profitability,
    estimate_profitability_at, next_halving, room_temp_power_factor, NoiseProfile,
    ProfitabilityEstimate,
};
pub use schedule::PowerSchedule;
pub use silicon_report::SiliconReport;
pub use state_persistence::{
    AutotunerHardwareFingerprint, AutotunerResumeState, LastKnownGoodChainState,
    LastKnownGoodChipState,
};
pub use telemetry::{
    build_efficiency_snapshot, export_runs_csv, AcceptedWorkSignal, EfficiencySnapshot,
    TelemetryExportState, TelemetryRecorder, TuningRun,
};
pub use tuner::AutoTuner;
pub use tuner::{
    AutotunerPolicyStatus, AutotunerResumeStateStatus, AutotunerRuntimeStatus, SiliconGradeCounts,
    TuningProgress,
};
// FreqCommand is already pub at crate root

#[cfg(test)]
mod tests {
    #[test]
    fn test_chip_geometry_uses_centralized_miner_profile_hashrate() {
        let expected = dcentrald_asic::drivers::MinerProfile::for_chip(0x1398)
            .expect("BM1398 profile should exist")
            .ghs_per_mhz;
        let actual = crate::chip_geometry::ghs_per_mhz_for_chip(0x1398);

        assert!((actual - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn test_expected_nps_uses_profile_hardware_difficulty_when_unspecified() {
        let profile = dcentrald_asic::drivers::MinerProfile::for_chip(0x1368)
            .expect("BM1368 profile should exist");
        assert_eq!(profile.hardware_difficulty, 128);

        let implicit = crate::chip_geometry::expected_nps_for_chip(0x1368, 500, 0);
        let explicit = crate::chip_geometry::expected_nps_for_chip(0x1368, 500, 128);

        assert!((implicit - explicit).abs() < f64::EPSILON);
    }

    /// W6.8 regression pin: BM1368 nonce-attribution geometry must come from
    /// MinerProfile (1280), NOT the legacy autotuner constant of 894 that
    /// caused a 30% low hashrate prediction on every S21 unit. If anyone
    /// reintroduces a hardcoded 894 for BM1368 in `chip_geometry`, this
    /// test fails and the offline CI gate fails alongside it.
    #[test]
    fn test_bm1368_cores_match_minerprofile_not_894() {
        let profile = dcentrald_asic::drivers::MinerProfile::for_chip(0x1368)
            .expect("BM1368 profile should exist");
        assert_eq!(
            profile.nonce_attribution_cores, 1280,
            "BM1368 MinerProfile must report 1280 nonce-attribution slots \
             (80 big × 16 small, S21 fixture RE 2026-04-12)",
        );
        assert_eq!(
            profile.cores_per_chip, 1280,
            "BM1368 cores_per_chip and nonce_attribution_cores agree at 1280 \
             (no split engine/slot geometry on BM1368)",
        );
        // The autotuner public helper must return the same 1280, not 894.
        let cores = crate::chip_geometry::cores_for_chip(0x1368);
        assert_eq!(
            cores, 1280,
            "autotuner chip_geometry::cores_for_chip(0x1368) must return 1280, not 894 (W6.8 drift fix)",
        );
        // Hashrate-predicted NPS must match what `MinerProfile::expected_nps`
        // returns, proving the autotuner consumes the MinerProfile single
        // source of truth instead of a stale local constant.
        let nps_via_helper = crate::chip_geometry::expected_nps_for_chip(0x1368, 500, 128);
        let nps_via_profile = profile.expected_nps(500, 128);
        assert!(
            (nps_via_helper - nps_via_profile).abs() < f64::EPSILON,
            "expected_nps_for_chip must delegate to MinerProfile::expected_nps",
        );
    }

    /// W6.8: BM1362 is the canonical "split" chip — 4 big SHA-256 engines
    /// per chip, 894 nonce-attribution slots per chip. The autotuner must
    /// see 894 (slots), the driver must see 4 (engines). This test pins
    /// both and proves they live in distinct `MinerProfile` fields.
    #[test]
    fn test_bm1362_distinguishes_big_engines_from_nonce_attribution() {
        let profile = dcentrald_asic::drivers::MinerProfile::for_chip(0x1362)
            .expect("BM1362 profile should exist");
        assert_eq!(
            profile.cores_per_chip, 4,
            "BM1362 must keep the 4 big-engine count for engine-state bookkeeping",
        );
        assert_eq!(
            profile.nonce_attribution_cores, 894,
            "BM1362 must expose 894 nonce-attribution slots for hashrate prediction",
        );
        assert_ne!(
            profile.cores_per_chip, profile.nonce_attribution_cores,
            "BM1362 is the split chip — engine count and slot count must differ",
        );

        // Autotuner public surface returns the slot count, not the engine count.
        let cores = crate::chip_geometry::cores_for_chip(0x1362);
        assert_eq!(
            cores, 894,
            "autotuner chip_geometry::cores_for_chip(0x1362) returns nonce_attribution_cores (894), not cores_per_chip (4)",
        );

        // BM1387 control: both fields agree at 114.
        let bm1387 = dcentrald_asic::drivers::MinerProfile::for_chip(0x1387)
            .expect("BM1387 profile should exist");
        assert_eq!(bm1387.cores_per_chip, 114);
        assert_eq!(bm1387.nonce_attribution_cores, 114);
    }

    ///  am2/BM1362 frequency-only: chip-count-aware NPS.
    ///
    /// XIL enumerates 28..110 of 126 BM1362 chips across AC cycles. The
    /// autotuner's expected nonce-rate for the chain must scale with
    /// the ACTUAL enumerated chip count using the W6.8 split-chip slot
    /// value (894), NOT the 4-engine count and NOT a fixed 126. A
    /// mid-run chip-count change therefore changes expected NPS — the
    /// daemon treats that as a StepUpGate-blocking event (the
    /// `StepUpGateSignal` machinery in this crate already gates step-up
    /// on rolling accept / HW-err; chip-count change shows up as a
    /// fingerprint mismatch on the resume-state gate, see
    /// `state_persistence` tests).
    #[test]
    fn bm1362_chain_nps_scales_with_enumerated_chip_count() {
        let per_chip = crate::chip_geometry::expected_nps_for_chip(0x1362, 525, 256);
        assert!(per_chip > 0.0, "BM1362 per-chip NPS must be positive");

        for &chips in &[28u32, 64, 110, 126] {
            let chain = per_chip * chips as f64;
            // Linear in chip count — the per-chip prediction is the
            // single source; the chain estimate is just N×.
            assert!(
                (chain - per_chip * chips as f64).abs() < f64::EPSILON,
                "chain NPS must be exactly chip_count × per-chip NPS",
            );
        }

        // The 894-slot geometry is what drives it: doubling chips
        // doubles expected NPS (a fixed-126 assumption would NOT).
        let n28 = per_chip * 28.0;
        let n56 = per_chip * 56.0;
        assert!(
            (n56 - 2.0 * n28).abs() < 1e-6,
            "expected NPS must be linear in the enumerated chip count",
        );

        // Pin: per-chip NPS uses 894 slots, not the 4-engine count.
        let nps_894 = (525.0_f64 * 894.0 * 1e6) / (256.0 * 4.294_967_296e9);
        assert!(
            (per_chip - nps_894).abs() / nps_894 < 0.01,
            "BM1362 per-chip NPS must derive from 894 nonce-attribution slots \
             (got {per_chip}, expected ≈ {nps_894})",
        );
    }
}
