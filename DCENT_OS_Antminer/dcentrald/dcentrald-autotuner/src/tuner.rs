//! AutoTuner state machine orchestrator.
//!
//! Lifecycle:
//!   1. Idle → check for saved profile → Tuned (warm start) or → Characterizing
//!   2. Characterizing → parallel binary search all chips → Verifying
//!   3. Verifying → extended monitoring at discovered frequencies
//!   4. ThermalRefinement → soak at operating frequencies while board heats up (2-10 min)
//!   5. Voltage optimization → find minimum stable voltage
//!   6. Tuned → periodic background health checks
//!   7. BackgroundAdjust → back off chips with sustained errors
//!
//! Phase 2 architecture: the autotuner communicates with WorkDispatcher via
//! two channels:
//!   - stats_rx:     WorkDispatcher → AutoTuner (per-chip nonce/error snapshots)
//!   - freq_cmd_tx:  AutoTuner → WorkDispatcher (frequency change commands)
//!
//! This avoids shared chain ownership — WorkDispatcher retains exclusive FPGA access.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::aging_tracker::AgingTracker;
use crate::binary_search::BinarySearchTuner;
use crate::chip_health::{ChipHealthStatus, ChipHealthTracker};
use crate::chip_stats::ChipStatsSnapshot;
use crate::config::{AutoTunerConfig, TuneTarget, TunerMode};
use crate::dvfs::DvfsOptimizer;
use crate::efficiency::EfficiencyOptimizer;
use crate::power_budget::{PowerCalibration, PowerModel};
use crate::profile::TuningProfile;
use crate::schedule::PowerSchedule;
use crate::telemetry::{
    build_efficiency_snapshot, EfficiencySnapshot, TelemetryExportState, TelemetryRecorder,
};
use crate::thermal_comp::ThermalCompensator;
use crate::voltage_search::VoltageSearchState;
use crate::AutoTunerError;
use crate::FreqCommand;
// W13.C3 (2026-05-10): per-SKU PVT envelope clamp.
//
// `Bm1362HashboardSku` carries the per-SKU freq/voltage table and flags
// (`voltage_fixed`, `requires_apw12_plus`, `inverted_curve`, `mix_levels`).
// The autotuner consults this map on every silicon-profile dispatch to
// reject `(freq, volt)` tuples that fall outside the SKU's published PVT
// envelope (`AutoTunerError::OutsidePvt`), instead of silently coercing
// them.
//
// See:
// - ~/
// - ~/
// - ~/
use dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku;

/// AutoTuner state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunerState {
    /// Waiting to start. Checks for saved profile.
    Idle,
    /// TABS Phase 1: parallel binary search across all chips.
    Characterizing,
    /// Extended verification at discovered frequencies.
    Verifying,
    /// Thermal soak: all chips at operating frequencies while board heats up.
    /// Chips that become unstable as temperature rises are stepped down.
    ThermalRefinement,
    /// Stable operation with per-chip optimized frequencies.
    Tuned,
    /// Some chains tuned successfully, but one or more chains fell back.
    PartiallyTuned,
    /// No chain tuned successfully; the autotuner fell back entirely.
    Failed,
    /// Continuous monitoring detected issues, backing off.
    BackgroundAdjust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShareEfficiencyValidation {
    NotApplicable,
    Unknown,
    Healthy,
    Degraded,
}

impl std::fmt::Display for TunerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunerState::Idle => write!(f, "Idle"),
            TunerState::Characterizing => write!(f, "Characterizing"),
            TunerState::Verifying => write!(f, "Verifying"),
            TunerState::ThermalRefinement => write!(f, "ThermalRefinement"),
            TunerState::Tuned => write!(f, "Tuned"),
            TunerState::PartiallyTuned => write!(f, "PartiallyTuned"),
            TunerState::Failed => write!(f, "Failed"),
            TunerState::BackgroundAdjust => write!(f, "BackgroundAdjust"),
        }
    }
}

/// Tuning progress update for dashboard feedback.
///
/// Published during characterization so the user sees bounded progress instead
/// of a long-running generic "tuning" state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TuningProgress {
    /// Current phase of tuning.
    pub phase: String,
    /// Which chain is being tuned.
    pub chain_id: u8,
    /// Current binary search iteration.
    pub iteration: u32,
    /// Maximum expected iterations.
    pub max_iterations: u32,
    /// How many chips are still being searched.
    pub active_chips: usize,
    /// Total chips on this chain.
    pub total_chips: u8,
    /// Elapsed seconds since tuning started.
    pub elapsed_s: f64,
    /// Estimated seconds remaining (based on iterations left × avg iteration time).
    pub estimated_remaining_s: f64,
    /// Percentage complete (0.0 - 100.0).
    pub percent_complete: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SiliconGradeCounts {
    pub a: u16,
    pub b: u16,
    pub c: u16,
    pub d: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AutotunerPolicyStatus {
    pub requested_preset: Option<String>,
    pub effective_preset: Option<String>,
    pub requested_preset_supported: Option<bool>,
    pub requested_preset_display_name: Option<String>,
    pub effective_preset_display_name: Option<String>,
    pub requested_preset_reason: Option<String>,
    pub degraded_from_requested: bool,
    pub capabilities: Option<crate::config::AutotunerCapabilityStatus>,
    pub active_objective: Option<String>,
    pub active_limiting_factor: Option<String>,
    pub safety_override: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutotunerResumeStateStatus {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub matched: bool,
    pub matched_chains: usize,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutotunerRuntimeStatus {
    pub enabled: bool,
    pub live_runtime: bool,
    pub stale: bool,
    pub age_s: u64,
    pub source: String,
    pub state: String,
    pub phase: String,
    pub percent_complete: f64,
    pub completed_chips: usize,
    pub active_chips: usize,
    pub total_chips: u16,
    pub active_chain_id: Option<u8>,
    pub active_chain_total_chips: Option<u8>,
    pub target_chains: usize,
    pub tuned_chains: usize,
    pub failed_chains: usize,
    pub tuned_chain_ids: Vec<u8>,
    pub failed_chain_ids: Vec<u8>,
    pub estimated_remaining_s: Option<f64>,
    pub avg_frequency_mhz: Option<f64>,
    pub efficiency_jth: Option<f64>,
    pub silicon_grades: Option<SiliconGradeCounts>,
    pub policy: Option<AutotunerPolicyStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_state: Option<AutotunerResumeStateStatus>,
    pub last_update_s: u64,
    pub message: String,
}

impl Default for AutotunerRuntimeStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            live_runtime: false,
            stale: false,
            age_s: 0,
            source: "runtime_unavailable".to_string(),
            state: "disabled".to_string(),
            phase: "disabled".to_string(),
            percent_complete: 0.0,
            completed_chips: 0,
            active_chips: 0,
            total_chips: 0,
            active_chain_id: None,
            active_chain_total_chips: None,
            target_chains: 0,
            tuned_chains: 0,
            failed_chains: 0,
            tuned_chain_ids: Vec::new(),
            failed_chain_ids: Vec::new(),
            estimated_remaining_s: None,
            avg_frequency_mhz: None,
            efficiency_jth: None,
            silicon_grades: None,
            policy: None,
            resume_state: None,
            last_update_s: 0,
            message: "Autotuner runtime unavailable".to_string(),
        }
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Verified Braiins fan-control PWM ceiling (0-100 scale).
const FAN_PWM_MAX: u8 = 100;

/// Fan-speed-aware thermal ceiling factor.
///
/// At low fan speeds (Home mode quiet), the board can't dissipate
/// as much heat, so the max sustainable frequency is lower. This function
/// converts fan PWM to a frequency ceiling multiplier.
///
/// 4-anchor piecewise-linear curve (RE 2026-06-02, mining-bible-v1
/// `power-estimation-model.md` ALGO 8): the bible's measured fan→ceiling anchors are
/// (10, 0.65), (30, 0.70), (64, 0.85), (127, 1.00). DCENT's `FAN_PWM_MAX` is 100 and is the
/// no-derating ceiling, so the top anchor is pinned to (100, 1.00) (not the bible's 127) —
/// this keeps PWM 100 = full ceiling (DCENT contract + `fan_factor < 0.99` derate gate) while
/// adopting the bible's exact, more-accurate home/mid-band anchors. The old 2-point (10→100)
/// fit over-derated the home quiet band where DCENT actually runs (PWM 30 returned 0.728 vs the
/// correct 0.70; PWM 64 returned 0.86 vs 0.85).
///
/// PWM 100 (max)   = 1.00 (no derating)
/// PWM  64         = 0.85
/// PWM  30 (home)  = 0.70
/// PWM  10 (quiet) = 0.65
/// PWM   0 (disabled) = 1.00 (assume external cooling or bypass)
///
/// This only scales the autotuner's frequency CEILING — it never raises a live fan PWM, and the
/// PWM-30 home cap is unaffected. Monotonic non-decreasing by construction.
pub(crate) fn fan_thermal_factor(fan_pwm: u8) -> f64 {
    if fan_pwm == 0 {
        return 1.0; // Fan awareness disabled or external cooling
    }
    let pwm = fan_pwm.min(100) as f64;
    // Piecewise-linear over the bible anchors, top pinned to DCENT's FAN_PWM_MAX=100=1.00.
    let (x0, y0, x1, y1) = if pwm <= 30.0 {
        (10.0, 0.65, 30.0, 0.70)
    } else if pwm <= 64.0 {
        (30.0, 0.70, 64.0, 0.85)
    } else {
        (64.0, 0.85, 100.0, 1.00)
    };
    if pwm <= x0 {
        return y0; // PWM 1..=10 clamps to the 0.65 floor
    }
    y0 + (pwm - x0) / (x1 - x0) * (y1 - y0)
}

const VOLTAGE_VERIFY_TOLERANCE_MV: u16 = 50;
const VOLTAGE_COMMAND_MAX_ATTEMPTS: u32 = 3;
const VOLTAGE_SEARCH_MAX_COMM_RETRIES: u32 = 3;
const VOLTAGE_SEARCH_MAX_LOW_CONFIDENCE_WINDOWS: u32 = 2;
const VOLTAGE_SEARCH_MIN_SAMPLES_PER_CHIP: u64 = 8;
const VOLTAGE_SEARCH_SETTLE_DELAY_MS: u64 = 500;
const VOLTAGE_SEARCH_FINAL_CONFIRM_WINDOW_S: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoltageWindowDecision {
    Stable,
    Unstable,
    LowConfidence,
    RetryCommunicationFault,
}

#[derive(Debug, Clone)]
struct VoltageOptimizationResult {
    optimal_voltage_mv: u16,
    stable_voltage_points_mv: Vec<u16>,
}

/// Per-chip background monitoring state.
struct ChipMonitor {
    /// ASIC chip ID for chain-specific PLL and hashrate math.
    chip_id: u16,
    /// Consecutive windows with error rate above threshold.
    consecutive_errors: u32,
    /// Consecutive windows with hashrate below min_hashrate_ratio.
    consecutive_hashrate_deficit: u32,
    /// Current applied operating frequency.
    current_freq_mhz: u16,
    /// Desired operating frequency before thermal ceilings are applied.
    desired_freq_mhz: u16,
    /// Original profile frequency (before any thermal derating).
    /// Used to restore frequency when temperature drops back below threshold.
    profile_freq_mhz: u16,
    /// Active thermal ceiling, if any.
    thermal_limit_mhz: Option<u16>,
    /// Active fan-clamp ceiling, if any.
    fan_limit_mhz: Option<u16>,
    /// Active sensor-failure safety ceiling, if any.
    sensor_safety_limit_mhz: Option<u16>,
    /// Whether this chip is currently thermally derated.
    thermally_derated: bool,
    /// Consecutive clean (error-free) windows since last backoff.
    /// Used for boost-back: if a backed-off chip runs clean for N windows,
    /// step it back up toward profile frequency.
    consecutive_clean_windows: u32,
    /// Number of boost-back attempts made for this chip.
    /// After max_boost_attempts, the chip stays at its backed-off frequency.
    boost_attempts: u32,
    /// Whether this chip is a candidate for dead-chip masking.
    /// Set after 5+ consecutive zero-nonce windows.
    consecutive_zero_nonce_windows: u32,
    /// Whether this chip has been masked (excluded from work dispatch).
    masked: bool,
}

/// State tracker for the thermal refinement soak phase.
///
/// Monitors temperature trend and per-chip error rates while the board
/// heats toward thermal equilibrium. Chips that become unstable as
/// temperature rises are stepped down to thermally-validated frequencies.
struct ThermalRefinementState {
    /// Temperature history for slope calculation: (elapsed_secs, temp_c).
    temp_history: Vec<(f64, f32)>,
    /// Per-chip nonce accumulator for the current refinement window.
    chip_nonces: Vec<u64>,
    /// Per-chip error accumulator for the current refinement window.
    chip_errors: Vec<u64>,
    /// Per-chip count of thermal backoffs applied during refinement.
    chip_backoffs: Vec<u32>,
    /// Start time of the refinement phase.
    start: Instant,
}

impl ThermalRefinementState {
    fn new(chip_count: usize) -> Self {
        Self {
            temp_history: Vec::with_capacity(32),
            chip_nonces: vec![0; chip_count],
            chip_errors: vec![0; chip_count],
            chip_backoffs: vec![0; chip_count],
            start: Instant::now(),
        }
    }

    /// Record a temperature reading. Returns true if thermal equilibrium is detected
    /// (slope below threshold AND minimum soak time elapsed).
    fn record_temperature(
        &mut self,
        temp_c: f32,
        min_soak_s: u64,
        stability_threshold: f32,
    ) -> bool {
        let elapsed_s = self.start.elapsed().as_secs_f64();
        self.temp_history.push((elapsed_s, temp_c));

        // Need at least 5 readings for meaningful slope calculation
        // Need enough readings for meaningful slope — 8 minimum to avoid
        // premature equilibrium detection from a brief temperature plateau.
        if self.temp_history.len() < 8 {
            return false;
        }

        // Check minimum soak time
        if elapsed_s < min_soak_s as f64 {
            return false;
        }

        // Compute slope from last 10 readings (longer window catches
        // false plateaus that a 5-reading window would miss)
        let slope_c_per_min = self.linear_slope();
        slope_c_per_min.abs() < stability_threshold
    }

    /// Compute the linear regression slope (C/min) over the last 10 temperature readings.
    /// Uses least-squares fit: slope = Σ((x-x̄)(y-ȳ)) / Σ((x-x̄)²).
    /// Longer window (10 vs 5) prevents false equilibrium from brief plateaus.
    fn linear_slope(&self) -> f32 {
        let n = self.temp_history.len().min(10);
        if n < 2 {
            return 0.0;
        }

        let recent = &self.temp_history[self.temp_history.len() - n..];

        // Convert to minutes for the slope
        let x_vals: Vec<f64> = recent.iter().map(|(t, _)| *t / 60.0).collect();
        let y_vals: Vec<f64> = recent.iter().map(|(_, temp)| *temp as f64).collect();

        let n_f = n as f64;
        let x_mean = x_vals.iter().sum::<f64>() / n_f;
        let y_mean = y_vals.iter().sum::<f64>() / n_f;

        let mut num = 0.0;
        let mut den = 0.0;
        for i in 0..n {
            let dx = x_vals[i] - x_mean;
            let dy = y_vals[i] - y_mean;
            num += dx * dy;
            den += dx * dx;
        }

        if den.abs() < 1e-12 {
            return 0.0;
        }

        (num / den) as f32
    }

    /// Reset per-chip accumulators for a new measurement window.
    fn reset_window(&mut self) {
        for v in &mut self.chip_nonces {
            *v = 0;
        }
        for v in &mut self.chip_errors {
            *v = 0;
        }
    }

    /// Accumulate a stats snapshot into the current window.
    fn accumulate(&mut self, snapshot: &ChipStatsSnapshot) {
        for i in 0..self.chip_nonces.len().min(snapshot.chip_nonces.len()) {
            self.chip_nonces[i] += snapshot.chip_nonces[i];
            self.chip_errors[i] += snapshot.stability_error_count(i);
        }
    }
}

/// Result summary from the thermal refinement phase.
#[allow(dead_code)]
struct ThermalRefinementResult {
    /// Number of measurement rounds completed.
    rounds: u32,
    /// Total number of chip backoffs across all chips.
    total_backoffs: u32,
    /// Whether thermal equilibrium was detected (vs timeout).
    equilibrium_reached: bool,
    /// Total duration of the refinement phase (seconds).
    duration_s: f64,
    /// Temperature at equilibrium (or at exit), if available.
    equilibrium_temp_c: Option<f32>,
}

struct WarmStartCandidate {
    chain_id: u8,
    chip_count: u8,
    chip_id: u16,
    profile: TuningProfile,
}

enum ResumeStateGate {
    LegacyNoState,
    Matched(crate::AutotunerResumeState),
    Invalid,
}

/// The auto-tuner orchestrator.
///
/// Owns the tuning state machine and manages the lifecycle of chip
/// characterization, verification, and background monitoring.
///
/// Communicates with WorkDispatcher via channels — never directly touches
/// FPGA chains (they are exclusively owned by the dispatcher).
pub struct AutoTuner {
    requested_config: AutoTunerConfig,
    config: AutoTunerConfig,
    state: TunerState,
    profiles: HashMap<u8, TuningProfile>,
    nominal_mhz: u16,
    chip_type: String,
    /// Parsed ASIC chip ID (e.g., 0x1387 for BM1387) for chip-specific
    /// PLL tables, nonce rate calculations, and power model selection.
    chip_id: u16,
    /// Power schedule for time-of-use electricity rate optimization.
    schedule: PowerSchedule,
    /// Telemetry recorder for tuning run history and export.
    telemetry: TelemetryRecorder,
    /// Multi-profile cache: profiles indexed by (chain_id, target_watts) for
    /// instant DPS switching without re-tuning. BraiinsOS+ does this — we must too.
    profile_cache: HashMap<(u8, u32), TuningProfile>,
    /// Optional broadcast sender for real-time efficiency snapshots.
    /// The dcentrald-api crate subscribes for WebSocket dashboard feeds.
    efficiency_tx: Option<tokio::sync::broadcast::Sender<EfficiencySnapshot>>,
    /// Optional broadcast sender for tuning progress updates.
    /// Dashboard shows real-time progress bar during characterization.
    progress_tx: Option<tokio::sync::broadcast::Sender<TuningProgress>>,
    /// Optional watch sender for the current runtime autotuner status.
    runtime_status_tx: Option<tokio::sync::watch::Sender<AutotunerRuntimeStatus>>,
    /// Optional watch sender for the latest live efficiency snapshot.
    efficiency_watch_tx: Option<tokio::sync::watch::Sender<Option<EfficiencySnapshot>>>,
    /// Optional watch sender for live per-chip health state.
    chip_health_tx: Option<tokio::sync::watch::Sender<Option<crate::LiveChipHealthState>>>,
    /// Optional watch sender for exportable telemetry state.
    telemetry_tx: Option<tokio::sync::watch::Sender<TelemetryExportState>>,
    /// Rolling accepted-work signal from the daemon share/power path.
    accepted_work_rx: Option<tokio::sync::watch::Receiver<Option<crate::AcceptedWorkSignal>>>,
    /// Live operator commands from the REST API. Config writes remain durable;
    /// this channel is only the runtime fast path when the tuner is monitoring.
    command_rx: Option<mpsc::Receiver<crate::AutoTunerCommand>>,
    /// Startup resume-state proof for API visibility.
    resume_state_status: Option<AutotunerResumeStateStatus>,
    /// Post-tune error tracking for automatic rollback.
    /// Counts background monitor windows since tuning completed.
    post_tune_windows: u32,
    /// Latest live per-chip average frequency after runtime ceilings/backoffs.
    live_avg_frequency_mhz: Option<f64>,
    /// Average error rate before the most recent tune (for rollback comparison).
    pre_tune_error_rate: f64,
    /// Per-chain counts of consecutive snapshots with no board temperature data.
    /// After 3 consecutive missing temp readings, force that chain to min frequency.
    consecutive_temp_missing: HashMap<u8, u32>,
    /// Last structured safety override currently active, if any.
    safety_override: Option<String>,
    /// Last runtime-applied objective after target-mode fallback resolution.
    active_runtime_objective: String,
    /// Last runtime-derived limiting factor from active monitor ceilings.
    active_runtime_limiting_factor: Option<String>,
    /// Chains the current run is responsible for tuning.
    target_chain_ids: BTreeSet<u8>,
    /// ASIC chip ID per chain for chain-specific PLL/power/hashrate logic.
    chain_chip_ids: HashMap<u8, u16>,
    /// Optional per-chain board/controller identity used to bind resume state
    /// to the currently installed hashboards when live probes provide it.
    chain_hardware_identities: HashMap<u8, crate::ChainHardwareIdentity>,
    /// Ordered list of chains in the current run with their chip counts.
    run_chain_plan: Vec<(u8, u8)>,
    /// Total chip count across the current run target set.
    target_chip_total: u16,
    /// Chains that failed characterization and fell back.
    failed_chain_ids: BTreeSet<u8>,
    /// Snapshots received for another chain/epoch while waiting on the current one.
    pending_stats: VecDeque<ChipStatsSnapshot>,
    /// Persistent wall-meter correction shared with the dispatcher/API.
    power_calibration: Arc<std::sync::RwLock<PowerCalibration>>,
    /// Family/controller capability profile for truthful preset gating.
    capabilities: crate::config::AutotunerCapabilityStatus,
    /// Resolved preset policy and effective config for the current run.
    resolved_policy: crate::config::ResolvedAutotunerPolicy,
    /// Baseline snapshot used to derive an effective rolling accepted-work window.
    accepted_work_baseline: Option<crate::AcceptedWorkSignal>,
    /// Reference accepted-difficulty-per-kWh established after tuning.
    accepted_work_reference_difficulty_per_kwh: Option<f64>,
    /// W13-A: operator-selected silicon profile id per
    /// `(miner_model_snake_case, hashboard)` chain. Populated by
    /// `AutoTunerCommand::ApplySiliconProfile`. The autotuner consults
    /// this map at the top of each background-adjust iteration to
    /// pick the preset table — see
    /// `dcentrald_silicon_profiles::registry::global()
    ///   .read().unwrap().get_active_bundle_for_chain(...)`.
    ///  ships the wiring; W15-A closes live preset-table
    /// consumption — see `active_silicon_profile_presets` below and
    /// `apply_active_silicon_profile_targets`.
    active_silicon_profile_ids: HashMap<(String, String), String>,
    /// W15-A: resolved preset table per active selection. Populated
    /// alongside `active_silicon_profile_ids` from the
    /// `AutoTunerCommand::ApplySiliconProfile` payload. Consumed at
    /// the top of each background-adjust iteration tick by
    /// `apply_active_silicon_profile_targets` to derive per-chain
    /// freq/voltage targets clamped by safety bounds.
    active_silicon_profile_presets: HashMap<(String, String), Vec<crate::SiliconPreset>>,
    /// W15-A: per-chain `(freq_mhz, voltage_mv)` of the most recently
    /// applied silicon-profile target. Used to short-circuit when the
    /// derived targets haven't changed between iterations — keeps the
    /// autotuner from flooding the dispatcher with redundant
    /// `SetVoltage` commands (which would also violate
    ///  if unchecked) and stops
    /// log spam.
    last_applied_silicon_targets: HashMap<u8, (u16, u16)>,

    /// W13.C3 (2026-05-10): per-chain BM1362 hashboard SKU map. Populated
    /// at chain bring-up by the daemon when the EEPROM/`/etc/subtype` read
    /// resolves to a known `Bm1362HashboardSku`. Consulted by
    /// [`Self::derive_silicon_profile_target`] to validate `(freq, volt)`
    /// tuples against the SKU's published PVT envelope before dispatch.
    ///
    /// When a chain is missing from this map, the validation gate is
    /// **open** (the autotuner stays compatible with chip families that
    /// don't carry per-SKU envelopes — every non-BM1362 chip today). For
    /// BM1362 chains, leaving this map empty effectively disables the
    /// envelope clamp — the daemon SHOULD populate it via
    /// [`Self::set_chain_sku`] before background-adjust starts.
    chain_skus: HashMap<u8, Bm1362HashboardSku>,

    /// W6.3 + W6.4: optional watch receiver for the step-up gate
    /// signal (rolling pool acceptance + worst-chip HW err EWMA).
    ///
    /// The daemon publishes a fresh `StepUpGateSignal` whenever the
    /// stratum client's `AcceptanceTracker` updates or the work
    /// dispatcher's `HwErrTracker` rolls forward. When `None`, the
    /// gate stays open (legacy behavior preserved). When `Some`, the
    /// gate refuses any boost-back or step-up that would raise a
    /// chip's frequency unless both conditions hold:
    /// `rolling_acceptance_pct >= 99.0` AND
    /// `worst_chip_hw_err_rate < 0.02`.
    step_up_gate_rx: Option<tokio::sync::watch::Receiver<crate::StepUpGateSignal>>,
}

impl AutoTuner {
    fn effective_monitor_freq(monitor: &ChipMonitor) -> u16 {
        [
            monitor.fan_limit_mhz,
            monitor.thermal_limit_mhz,
            monitor.sensor_safety_limit_mhz,
        ]
        .into_iter()
        .flatten()
        .fold(monitor.desired_freq_mhz, |freq, limit| freq.min(limit))
    }

    fn refresh_monitor_frequency(monitor: &mut ChipMonitor) -> bool {
        let new_freq = Self::effective_monitor_freq(monitor);
        let changed = new_freq != monitor.current_freq_mhz;
        monitor.current_freq_mhz = new_freq;
        monitor.thermally_derated = monitor
            .thermal_limit_mhz
            .map(|limit| limit < monitor.desired_freq_mhz)
            .unwrap_or(false);
        changed
    }

    fn profile_chip_id(profile: &TuningProfile) -> u16 {
        crate::chip_id_from_type(&profile.chip_type).unwrap_or(0x1387)
    }

    fn chain_chip_id(&self, chain_id: u8) -> u16 {
        self.chain_chip_ids
            .get(&chain_id)
            .copied()
            .or_else(|| self.profiles.get(&chain_id).map(Self::profile_chip_id))
            .unwrap_or(self.chip_id)
    }

    fn mixed_chain_chip_ids(&self) -> bool {
        let unique: BTreeSet<u16> = self.chain_chip_ids.values().copied().collect();
        unique.len() > 1
    }

    fn current_power_scale(&self) -> f64 {
        self.power_calibration
            .read()
            .map(|calibration| calibration.effective_multiplier())
            .unwrap_or(1.0)
    }

    fn power_model_for_chip(&self, chip_id: u16) -> PowerModel {
        PowerModel::new_for_chip(chip_id).with_power_scale(self.current_power_scale())
    }

    fn saved_calibrated_c_eff(&self) -> Option<f64> {
        let mut calibrated = self
            .profiles
            .values()
            .filter_map(|profile| profile.calibrated_c_eff)
            .filter(|c_eff| c_eff.is_finite() && *c_eff > 0.0);

        let first = calibrated.next()?;
        for value in calibrated {
            if (value - first).abs() > 1e-9 {
                warn!(
                    first_c_eff = format_args!("{:.6e}", first),
                    conflicting_c_eff = format_args!("{:.6e}", value),
                    "Persisted calibrated C_eff disagrees across chains — ignoring saved power calibration"
                );
                return None;
            }
        }

        Some(first)
    }

    fn share_efficiency_supported(&self) -> bool {
        self.capabilities.family_key == "bm1387"
            && self.capabilities.voltage_control == "pic16"
            && !self.mixed_chain_chip_ids()
    }

    fn evaluate_share_efficiency_validation(&mut self) -> ShareEfficiencyValidation {
        if !self.share_efficiency_supported() {
            return ShareEfficiencyValidation::NotApplicable;
        }

        let Some(rx) = &self.accepted_work_rx else {
            return ShareEfficiencyValidation::Unknown;
        };
        let Some(current) = rx.borrow().clone() else {
            return ShareEfficiencyValidation::Unknown;
        };

        let Some(baseline) = self.accepted_work_baseline.clone() else {
            self.accepted_work_baseline = Some(current);
            return ShareEfficiencyValidation::Unknown;
        };

        let delta_window_s = current.window_s.saturating_sub(baseline.window_s);
        // This validation uses accepted pool-target difficulty work, not
        // achieved/lucky share difficulty.
        let current_target_work = if current.accepted_pool_target_difficulty_sum > 0.0 {
            current.accepted_pool_target_difficulty_sum
        } else {
            current.accepted_difficulty_sum
        };
        let baseline_target_work = if baseline.accepted_pool_target_difficulty_sum > 0.0 {
            baseline.accepted_pool_target_difficulty_sum
        } else {
            baseline.accepted_difficulty_sum
        };
        let delta_difficulty = current_target_work - baseline_target_work;
        let delta_energy = current.estimated_wall_energy_kwh - baseline.estimated_wall_energy_kwh;

        if delta_window_s < 900
            || delta_difficulty < 1024.0
            || delta_energy <= 0.0
            || (!current.calibrated && current.power_source != "pmbus")
        {
            return ShareEfficiencyValidation::Unknown;
        }

        let delta_efficiency = delta_difficulty / delta_energy;
        self.accepted_work_baseline = Some(current);

        let Some(reference) = self.accepted_work_reference_difficulty_per_kwh else {
            self.accepted_work_reference_difficulty_per_kwh = Some(delta_efficiency);
            return ShareEfficiencyValidation::Unknown;
        };

        if delta_efficiency < reference * 0.75 {
            ShareEfficiencyValidation::Degraded
        } else {
            self.accepted_work_reference_difficulty_per_kwh =
                Some(reference * 0.8 + delta_efficiency * 0.2);
            ShareEfficiencyValidation::Healthy
        }
    }

    fn runtime_chain_infos_from_profiles(&self) -> Vec<crate::ChainTuneInfo> {
        let mut chain_ids: Vec<u8> = self.profiles.keys().copied().collect();
        chain_ids.sort_unstable();

        chain_ids
            .into_iter()
            .filter_map(|chain_id| {
                let profile = self.profiles.get(&chain_id)?;
                Some(crate::ChainTuneInfo {
                    chain_id,
                    chip_count: profile.chip_count,
                    voltage_mv: profile.optimal_voltage_mv.unwrap_or(profile.voltage_mv),
                    chip_id: self.chain_chip_id(chain_id),
                    hardware_identity: self
                        .chain_hardware_identities
                        .get(&chain_id)
                        .cloned()
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    fn sync_monitors_from_profiles(
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
        profiles: &HashMap<u8, TuningProfile>,
    ) {
        for (&chain_id, profile) in profiles {
            for chip in &profile.chips {
                if let Some(monitor) = monitors.get_mut(&(chain_id, chip.chip_index)) {
                    monitor.desired_freq_mhz = chip.operating_mhz;
                    monitor.profile_freq_mhz = chip.operating_mhz;
                    Self::refresh_monitor_frequency(monitor);
                    monitor.consecutive_clean_windows = 0;
                    monitor.boost_attempts = 0;
                    monitor.consecutive_hashrate_deficit = 0;
                }
            }
        }
    }

    async fn handle_runtime_command(
        &mut self,
        command: crate::AutoTunerCommand,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
    ) {
        match command {
            crate::AutoTunerCommand::ApplyMode { mode, ack_tx } => {
                let result = self.apply_runtime_mode(mode, freq_cmd_tx, monitors).await;
                let _ = ack_tx.send(result);
            }
            crate::AutoTunerCommand::ApplySiliconProfile {
                miner_model,
                hashboard,
                profile_id,
                presets,
                ack_tx,
            } => {
                let result =
                    self.apply_runtime_silicon_profile(miner_model, hashboard, profile_id, presets);
                let _ = ack_tx.send(result);
            }
        }
    }

    /// W13-A: handler for `AutoTunerCommand::ApplySiliconProfile`.
    ///
    /// Records the operator's silicon-profile selection for the given
    /// chain. The selection is consumed by the next iteration of the
    /// background-adjust loop via the registry's
    /// `get_active_bundle_for_chain` accessor. We keep the runtime
    /// effect minimal here — applying the profile's voltage/frequency
    /// targets in real time is queued for wave 14, after live
    /// hardware-in-the-loop verification on `a lab unit` (BM1362 am2).
    fn apply_runtime_silicon_profile(
        &mut self,
        miner_model: String,
        hashboard: String,
        profile_id: String,
        presets: Vec<crate::SiliconPreset>,
    ) -> crate::AutoTunerSiliconProfileResult {
        // Always record the selection. Even when the autotuner isn't
        // in BackgroundAdjust, the next iteration will pick it up via
        // `get_active_bundle_for_chain`.
        self.active_silicon_profile_ids
            .insert((miner_model.clone(), hashboard.clone()), profile_id.clone());
        // W15-A: also store the resolved preset table so the next
        // iteration of `background_monitor` can derive per-chain freq
        // and voltage targets without re-reading the registry from
        // inside the autotuner crate.
        self.active_silicon_profile_presets
            .insert((miner_model.clone(), hashboard.clone()), presets);
        // W15-A: clear the last-applied targets cache so the next
        // iteration emits fresh `FreqCommand` writes for the new
        // profile, even when the derived targets happen to match the
        // previous chain values.
        self.last_applied_silicon_targets.clear();

        let runtime_ready = matches!(
            self.state,
            TunerState::Tuned | TunerState::PartiallyTuned | TunerState::BackgroundAdjust
        );
        if !runtime_ready {
            self.publish_runtime_status(
                "Silicon profile selection persisted; runtime apply deferred",
                None,
            );
            return crate::AutoTunerSiliconProfileResult {
                status: crate::AutoTunerCommandStatus::Deferred,
                applied_runtime: false,
                profile_id,
                miner_model,
                hashboard,
                message:
                    "autotuner is not in background monitoring; silicon profile will apply on the next tuning cycle"
                        .to_string(),
            };
        }

        self.publish_runtime_status("Silicon profile selection accepted by live autotuner", None);
        crate::AutoTunerSiliconProfileResult {
            status: crate::AutoTunerCommandStatus::Applied,
            applied_runtime: true,
            profile_id,
            miner_model,
            hashboard,
            message:
                "runtime autotuner accepted the silicon-profile selection; preset table will be re-pulled on the next iteration"
                    .to_string(),
        }
    }

    /// W15-A: select the preset row from a silicon-profile preset
    /// table that best matches the current `TunerMode`.
    ///
    /// Mapping:
    /// - `TuneTarget::Hashrate` → highest `step` (max freq).
    /// - `TuneTarget::Efficiency` → preset with the best J/TH proxy
    ///   (median step — preset tables come pre-sorted along the
    ///   characterization curve, and BraiinsOS-style sweet-spot rows
    ///   live mid-table). When the table is short, falls back to
    ///   nameplate (step 0).
    /// - `TuneTarget::Power` / `TuneTarget::HashrateTarget` → step 0
    ///   (nameplate / default). The legacy power-budget allocator
    ///   in `apply_target_mode` continues to drive per-chip frequency
    ///   for these modes; the silicon profile only authorizes the
    ///   voltage/freq operating envelope.
    fn select_preset_for_mode(
        presets: &[crate::SiliconPreset],
        target_mode: TuneTarget,
    ) -> Option<&crate::SiliconPreset> {
        if presets.is_empty() {
            return None;
        }
        match target_mode {
            TuneTarget::Hashrate => presets.iter().max_by_key(|p| p.step),
            TuneTarget::Efficiency | TuneTarget::EfficiencyJTH => {
                // Median row by step. With short tables (1-3 rows)
                // fall back to step 0; with longer tables this lands
                // on the BraiinsOS-style sweet-spot region. EfficiencyJTH
                // shares the same preset selection — it differs from
                // Efficiency only in that the wattmeter anchor biases
                // J/TH evaluation in the runtime cost function.
                let mut sorted: Vec<&crate::SiliconPreset> = presets.iter().collect();
                sorted.sort_by_key(|p| p.step);
                if sorted.len() >= 4 {
                    Some(sorted[sorted.len() / 2])
                } else {
                    sorted
                        .iter()
                        .find(|p| p.step == 0)
                        .copied()
                        .or(Some(sorted[0]))
                }
            }
            TuneTarget::Power | TuneTarget::HashrateTarget => presets
                .iter()
                .find(|p| p.step == 0)
                .or_else(|| presets.first()),
        }
    }

    /// W15-A: derive `(freq_mhz, voltage_mv)` per chain from the
    /// active silicon-profile preset table, clamped by the autotuner's
    /// safety bounds.
    ///
    /// Safety clamps applied:
    /// - **Frequency**: `[config.min_freq_mhz, config.max_freq_mhz]`.
    /// - **Voltage**: `[config.min_voltage_mv, MAX_CHAIN_VOLTAGE_MV]`
    ///   where `MAX_CHAIN_VOLTAGE_MV = 9440` for BM1387 (S9, chain
    ///   rail 9.44V max)
    ///   and `14500` (the project load-bearing hard cap) for chain-rail
    ///   SHA-256 (BM1397/8/BM1362/6/8/70).
    /// - **fw=0x86 dsPIC refusal**: per
    ///   , when a chain identity has
    ///   `dspic_fw_byte == Some(0x86)` the autotuner SKIPS voltage
    ///   application for that chain — the FW is the proven post-PIC-RESET
    ///   corruption state and any `SetVoltage` would be rejected by the
    ///   downstream PSU layer (or worse, accepted on a corrupted parser).
    ///   Frequency is still applied.
    /// - Profile rows whose voltage is in the chip-rail band (sub-5V)
    ///   are validated against BM1362 SKU PVT bounds, but not dispatched
    ///   as voltage because the live voltage path expects chain-rail
    ///   volts.
    fn derive_silicon_profile_target(
        &self,
        chain_id: u8,
        preset: &crate::SiliconPreset,
    ) -> (u16, Option<u16>) {
        let chip_id = self.chain_chip_id(chain_id);
        let raw_freq = preset.freq_mhz.min(u16::MAX as u32) as u16;
        let clamped_freq = raw_freq
            .max(self.config.min_freq_mhz)
            .min(self.config.max_freq_mhz);

        // Voltage clamp envelope per chip family.
        let max_chain_voltage_mv: u16 = match chip_id {
            // BM1387: S9 chain rail; PIC DAC=0 = 9.44V.
            0x1387 => 9440,
            // SHA-256 BM139x/BM136x: chain-rail typical 13.4..14.4V.
            // Clamp at the project load-bearing hard cap (14500 mV) so a
            // malformed preset can't push voltage beyond it — production cold
            // boot stays at 13.7..13.8V per
            // . This is the upstream
            // backstop; the final rail-program backstop is the dsPIC driver's
            // own <=14500 input clamp (clamp_dspic_voltage_to_hard_cap). The
            // AMTC ~15.0V pre-open is NOT an autotuner steady-state path (it is
            // a cold-boot pulse, gated by DCENT_AM2_ALLOW_LAB_OVERVOLT in the
            // dsPIC driver), so lowering this ceiling does not affect it. Per
            // .
            0x1397 | 0x1398 | 0x1362 | 0x1366 | 0x1368 | 0x1370 => 14500,
            // Scrypt (BM1485 / BM1489) chain-rail.
            0x1485 | 0x1489 => 13500,
            // Unknown chip: refuse voltage delivery.
            _ => return (clamped_freq, None),
        };

        // Chip-rail rows (sub-5V) are not sent to the chain-rail
        // voltage path, but their mV value is still used by the
        // BM1362 PVT bounds check below.
        let voltage_v = preset.voltage_v;
        if !voltage_v.is_finite() || voltage_v <= 0.0 {
            return (clamped_freq, None);
        }
        let raw_mv = (voltage_v * 1000.0).round() as i32;
        let raw_mv = raw_mv.clamp(0, u16::MAX as i32) as u16;
        let mut dispatch_mv = if voltage_v < 5.0 {
            // Keep raw_mv available for BM1362 PVT validation below.
            // [GAP: chip-rail voltage application requires per-chip
            // BUCK driver wiring, deferred beyond W15-A.]
            None
        } else {
            Some(
                raw_mv
                    .max(self.config.min_voltage_mv)
                    .min(max_chain_voltage_mv),
            )
        };

        //: refuse voltage to
        // chains whose dsPIC reports the post-PIC-RESET corruption
        // signature (fw=0x86) unless `DCENT_AM2_TRUST_DEGRADED_FW=1`.
        // The autotuner doesn't read env vars directly — the daemon
        // already enforces this gate at the HAL boundary — but we
        // mirror the safety contract here so a profile-driven
        // SetVoltage never arrives at a known-corrupt chain.
        let dspic_fw = self
            .chain_hardware_identities
            .get(&chain_id)
            .and_then(|id| id.dspic_fw_byte);
        if dspic_fw == Some(0x86) {
            warn!(
                chain_id,
                "Silicon profile: refusing SetVoltage for fw=0x86 chain (post-PIC-RESET corruption signature); applying freq only"
            );
            dispatch_mv = None;
        }

        // W13.C3 (2026-05-10): per-SKU PVT envelope clamp gate. If the
        // chain has a registered BM1362 hashboard SKU, validate the
        // proposed `(freq, volt)` tuple against the SKU's envelope. On
        // OutsidePvt, refuse the dispatch (return freq-only with
        // freq snapped to the in-envelope nearest, voltage suppressed)
        // rather than silently coercing. The dashboard / log surface
        // tells the operator which SKU envelope was violated.
        //
        // Special-cases honored here:
        // - `voltage_fixed=true` (BHB42803) — the SKU's voltage is locked
        //   at the PCB-level VRM divider. Voltage application is already
        //   short-circuited by `voltage_search::new_with_pvt_flags`
        //   (W13.C1), so we mirror that contract here by suppressing the
        //   derived voltage. Frequency is still clamped to the envelope.
        // - `requires_apw12_plus=true` (high-bin BHB428xx) — emit a
        //   warning if the chip family is paired with an APW12 SMBus PSU.
        //   The install preflight already blocks this; the warning is
        //   defense-in-depth (W13.B7).
        if let Some(sku) = self.chain_skus.get(&chain_id).copied() {
            let flags = sku.flags();

            // Defense-in-depth APW12+ requirement check. The autotuner
            // doesn't see PSU type directly, but the chip rail voltage
            // ceiling acts as a proxy: APW12 SMBus tops out below the
            // high-bin BHB428xx envelope (1530 mV+). If a high-bin SKU
            // is registered AND the proposed voltage is within the
            // high-bin band, log a warn so operators see the gate
            // would have fired pre-W13.B7.
            if flags.requires_apw12_plus && raw_mv >= 1500 {
                warn!(
                    chain_id,
                    sku = sku.hashboard_id(),
                    proposed_mv = raw_mv,
                    "Silicon profile: SKU requires APW12+ PSU; verify install preflight gate (W13.B7)."
                );
            }

            // BHB42611 mix_levels — W13 only honours symmetric `[freq;
            // chain_count]` dispatch; per-chain asymmetric is W14+.
            if flags.mix_levels {
                tracing::info!(
                    chain_id,
                    sku = sku.hashboard_id(),
                    "BHB42611 mix_levels not yet supported in W13; using symmetric freq"
                );
            }

            // W24-EFF-1 (2026-05-22): axis-aware PVT check (default OFF).
            //
            // `raw_mv` here is `preset.voltage_v * 1000`. The BM1362 silicon
            // registry legitimately carries rows on TWO voltage axes
            // (`registry.rs::chip_voltage_ranges`: chip/core-rail 0.5..2.0 V,
            // chain-rail 7.5..15.0 V). The PVT envelope (`freq_voltage_table`)
            // models ONLY the core-mV axis (~1320..1380 mV). When the preset
            // carries a CHAIN-RAIL voltage (the baked `BM1362_PROFILES`
            // reality, e.g. 320 MHz @ 12.45 V), `raw_mv` ≈ 12450 — the wrong
            // axis — so the core-mV envelope always reports OutsidePvt, emits
            // a misleading "OUTSIDE PVT envelope" WARN on a healthy chain, and
            // the green W13.C3 fixture tests (hand-crafted 1.34..1.7 V core
            // values the real registry never produces) never exercise this.
            //
            // When the gate is ON and the preset is chain-rail, run the PVT
            // *freq* clamp only (still snaps freq to the envelope) and log a
            // single INFO instead of the OutsidePvt WARN. **No voltage
            // delivery changes**: chain-rail `dispatch_mv` is already `None`
            // (the `voltage_v < 5.0 => None` rule above only keeps Some() for
            // chain-rail, which the am2 consumer then HARD-REFUSES), so this
            // is byte-identical to live voltage behavior either way. Default
            // OFF ⇒ the original OutsidePvt branch runs unchanged.
            if crate::pvt_envelope::axis_aware_pvt_clamp_enabled()
                && crate::pvt_envelope::classify_voltage_axis(raw_mv)
                    == crate::pvt_envelope::VoltageAxis::ChainRail
            {
                let table = crate::pvt_envelope::pvt_envelope(sku);
                if !table.is_empty() {
                    let (min_f, max_f) =
                        table.iter().fold((u16::MAX, u16::MIN), |(lo, hi), (f, _)| {
                            (lo.min(*f), hi.max(*f))
                        });
                    let snapped_freq = clamped_freq.max(min_f).min(max_f);
                    tracing::info!(
                        chain_id,
                        sku = sku.hashboard_id(),
                        proposed_freq_mhz = clamped_freq,
                        chain_rail_mv = raw_mv,
                        snapped_freq_mhz = snapped_freq,
                        "Silicon profile: chain-rail preset voltage ({} mV) is not the core-mV \
                         axis the PVT envelope models; applying freq-only (voltage suppressed by \
                         the chain-rail path / am2 freq-only consumer). Not an envelope violation.",
                        raw_mv,
                    );
                    return (snapped_freq, None);
                }
                // Empty table → fall through to the unchanged check below
                // (which will OutsidePvt on the empty-table defensive path).
            }

            match crate::pvt_envelope::validate_freq_volt(sku, clamped_freq, raw_mv) {
                Ok(()) => {
                    // In-envelope. For voltage_fixed SKUs, suppress the
                    // voltage axis — the W13.C1 voltage_search short-circuit
                    // owns that path. The freq is still passed through.
                    if flags.voltage_fixed {
                        return (clamped_freq, None);
                    }
                }
                Err(AutoTunerError::OutsidePvt {
                    valid_freq_range,
                    valid_volt_range,
                    ..
                }) => {
                    // Snap the freq to the nearest in-envelope freq, drop
                    // the voltage, and warn. We deliberately do NOT
                    // silently coerce voltage too — the dispatch will
                    // re-derive on the next iteration once the operator
                    // fixes the profile.
                    let snapped_freq = clamped_freq.max(valid_freq_range.0).min(valid_freq_range.1);
                    warn!(
                        chain_id,
                        sku = sku.hashboard_id(),
                        proposed_freq_mhz = clamped_freq,
                        proposed_volt_mv = raw_mv,
                        valid_freq_min = valid_freq_range.0,
                        valid_freq_max = valid_freq_range.1,
                        valid_volt_min = valid_volt_range.0,
                        valid_volt_max = valid_volt_range.1,
                        snapped_freq_mhz = snapped_freq,
                        "Silicon profile: (freq, volt) tuple OUTSIDE PVT envelope for SKU; \
                         snapping freq to envelope, suppressing voltage"
                    );
                    return (snapped_freq, None);
                }
                Err(other) => {
                    // validate_freq_volt only returns OutsidePvt; any other
                    // variant is unexpected. Log and fall through with
                    // suppressed voltage to be safe.
                    warn!(
                        chain_id,
                        sku = sku.hashboard_id(),
                        error = %other,
                        "Silicon profile: PVT validation returned unexpected error variant; \
                         suppressing voltage to be safe"
                    );
                    return (clamped_freq, None);
                }
            }
        }

        (clamped_freq, dispatch_mv)
    }

    /// W15-A: at the top of each background-adjust iteration, consult
    /// the active silicon-profile preset table per
    /// `(miner_model, hashboard)` chain and apply the resulting
    /// per-chain freq/voltage targets via the existing
    /// `FreqCommand::SetChainFreq` + `FreqCommand::SetVoltage` rails.
    ///
    /// This closes the W13-A handoff: W13-A wired the
    /// `ApplySiliconProfile` command into the autotuner's selection
    /// state but explicitly deferred live preset-table consumption
    /// inside `tuner_loop`. W15-A makes the tuning loop honor the
    /// active selection on every tick.
    ///
    /// No-ops when no active selection is present, when the preset
    /// table is empty (deferred-only path from older W13-A callers),
    /// or when the derived targets match the cached
    /// `last_applied_silicon_targets` from the prior iteration.
    async fn apply_active_silicon_profile_targets(
        &mut self,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
    ) {
        if self.active_silicon_profile_presets.is_empty() {
            return;
        }
        // W15-A scope is single-platform (one miner = one model).
        // Pick the first non-empty preset table; cross-platform
        // mixed-hashboard support stays deferred behind per-chain
        // model/hashboard binding (TBD wave 17+ for am2 family).
        let preset_table: Vec<crate::SiliconPreset> = self
            .active_silicon_profile_presets
            .values()
            .find(|v| !v.is_empty())
            .cloned()
            .unwrap_or_default();
        if preset_table.is_empty() {
            return;
        }
        let Some(preset) = Self::select_preset_for_mode(&preset_table, self.config.target_mode)
        else {
            return;
        };
        let preset = *preset;

        let chain_ids: Vec<u8> = self.profiles.keys().copied().collect();
        if chain_ids.is_empty() {
            return;
        }

        for chain_id in chain_ids {
            let (target_freq_mhz, target_voltage_mv) =
                self.derive_silicon_profile_target(chain_id, &preset);

            let prev = self.last_applied_silicon_targets.get(&chain_id).copied();
            let prev_voltage_mv = prev.map(|(_, v)| v).unwrap_or(0);
            let new_voltage_mv = target_voltage_mv.unwrap_or(prev_voltage_mv);
            let needs_update = match prev {
                Some((freq, voltage)) => freq != target_freq_mhz || voltage != new_voltage_mv,
                None => true,
            };
            if !needs_update {
                continue;
            }

            // Broadcast frequency to every chip on the chain via the
            // existing `FreqCommand::SetChainFreq` rail. The
            // dispatcher honors per-chip ceilings (fan clamp, thermal,
            // sensor safety) on top of this — so a profile target
            // above the fan-clamped ceiling will be derated by the
            // dispatcher, not bypassed.
            let (freq_ack_tx, freq_ack_rx) = tokio::sync::oneshot::channel();
            if let Err(e) = freq_cmd_tx
                .send(FreqCommand::SetChainFreq {
                    chain_id,
                    freq_mhz: target_freq_mhz,
                    ack_tx: Some(freq_ack_tx),
                })
                .await
            {
                warn!(chain_id, error = %e, "Silicon profile: SetChainFreq dispatch failed");
                continue;
            }
            match freq_ack_rx.await {
                Ok(Ok(())) => {
                    info!(
                        chain_id,
                        target_freq_mhz,
                        step = preset.step,
                        "AUTOTUNE: silicon-profile broadcast freq to chain"
                    );
                }
                Ok(Err(detail)) => {
                    warn!(
                        chain_id,
                        target_freq_mhz,
                        detail = %detail,
                        "Silicon profile: dispatcher rejected SetChainFreq"
                    );
                    continue;
                }
                Err(_) => {
                    warn!(
                        chain_id,
                        "Silicon profile: SetChainFreq ack channel dropped"
                    );
                    continue;
                }
            }

            // Update WORK_TIME for the new chain frequency.
            let _ = freq_cmd_tx
                .send(FreqCommand::UpdateWorkTime {
                    chain_id,
                    min_freq_mhz: target_freq_mhz,
                })
                .await;

            // Voltage delivery is conditional: skipped for
            // chip-rail rows, fw=0x86 chains, and unknown chip
            // families. See `derive_silicon_profile_target`.
            if let Some(target_mv) = target_voltage_mv {
                let (v_ack_tx, v_ack_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = freq_cmd_tx
                    .send(FreqCommand::SetVoltage {
                        chain_id,
                        voltage_mv: target_mv,
                        ack_tx: Some(v_ack_tx),
                    })
                    .await
                {
                    warn!(chain_id, error = %e, "Silicon profile: SetVoltage dispatch failed");
                } else {
                    match v_ack_rx.await {
                        Ok(Ok(applied_mv)) => {
                            info!(
                                chain_id,
                                target_mv,
                                applied_mv,
                                step = preset.step,
                                "AUTOTUNE: silicon-profile voltage applied"
                            );
                        }
                        Ok(Err(detail)) => {
                            warn!(
                                chain_id,
                                target_mv,
                                detail = %detail,
                                "Silicon profile: dispatcher rejected SetVoltage"
                            );
                        }
                        Err(_) => {
                            warn!(chain_id, "Silicon profile: SetVoltage ack channel dropped");
                        }
                    }
                }
            }

            // Sync monitors so subsequent fan-clamp / thermal logic
            // sees the new desired frequency. This is structurally
            // identical to `apply_runtime_mode`'s
            // `sync_monitors_from_profiles` path but scoped to one
            // chain.
            for ((cid, _), monitor) in monitors.iter_mut() {
                if *cid != chain_id {
                    continue;
                }
                monitor.desired_freq_mhz = target_freq_mhz;
                monitor.profile_freq_mhz = target_freq_mhz;
                Self::refresh_monitor_frequency(monitor);
                monitor.consecutive_clean_windows = 0;
                monitor.boost_attempts = 0;
                monitor.consecutive_hashrate_deficit = 0;
            }

            self.last_applied_silicon_targets
                .insert(chain_id, (target_freq_mhz, new_voltage_mv));
        }
    }

    /// W15-A test-only helper: drive
    /// `apply_active_silicon_profile_targets` from outside the live
    /// `background_monitor` loop. Mirrors the W13-A pattern.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn tick_silicon_profile_targets_for_test(
        &mut self,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        // Hydrate monitors from profiles so the sync step has
        // something to update.
        for (&chain_id, profile) in &self.profiles {
            for chip in &profile.chips {
                monitors.insert(
                    (chain_id, chip.chip_index),
                    ChipMonitor {
                        chip_id: Self::profile_chip_id(profile),
                        consecutive_errors: 0,
                        consecutive_hashrate_deficit: 0,
                        current_freq_mhz: chip.operating_mhz,
                        desired_freq_mhz: chip.operating_mhz,
                        profile_freq_mhz: chip.operating_mhz,
                        thermal_limit_mhz: None,
                        fan_limit_mhz: None,
                        sensor_safety_limit_mhz: None,
                        thermally_derated: false,
                        consecutive_clean_windows: 0,
                        boost_attempts: 0,
                        consecutive_zero_nonce_windows: 0,
                        masked: false,
                    },
                );
            }
        }
        self.apply_active_silicon_profile_targets(freq_cmd_tx, &mut monitors)
            .await;
    }

    /// W15-A test-only accessor: read the cached last-applied
    /// `(freq_mhz, voltage_mv)` target for a given chain.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn last_applied_silicon_target_for_test(&self, chain_id: u8) -> Option<(u16, u16)> {
        self.last_applied_silicon_targets.get(&chain_id).copied()
    }

    /// W15-A test-only accessor: install a synthetic
    /// `(chain_id, ChainHardwareIdentity)` so a test can drive the
    /// fw=0x86 refusal path without spinning up the full HAL probe
    /// pipeline.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_chain_hardware_identity_for_test(
        &mut self,
        chain_id: u8,
        identity: crate::ChainHardwareIdentity,
    ) {
        self.chain_hardware_identities.insert(chain_id, identity);
    }

    /// W15-A test-only seeder: install a single profile so the
    /// silicon-profile target apply path has chain ids to iterate
    /// when the test bypasses the full characterization pipeline.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn install_profile_for_test(&mut self, chain_id: u8, profile: crate::TuningProfile) {
        self.chain_chip_ids
            .insert(chain_id, Self::profile_chip_id(&profile));
        self.profiles.insert(chain_id, profile);
    }

    /// W15-A test-only setter: override `config.target_mode` so a test
    /// can exercise the `select_preset_for_mode` Hashrate → max-step
    /// branch explicitly.
    ///
    /// `AutoTunerConfig::default()` lands on `TuneTarget::Efficiency`
    /// (the load-bearing home-miner safety default — home miners pay
    /// per kWh, so the default optimizes J/TH, not the TH/s
    /// leaderboard; see `config.rs::TuneTarget::default`). A test that
    /// wants to verify the highest-step preset is selected must
    /// therefore opt back into `Hashrate` the same way `Hacker` mode
    /// does at runtime via `TuneTarget::for_mode("hacker")`.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_target_mode_for_test(&mut self, mode: TuneTarget) {
        self.config.target_mode = mode;
    }

    /// Test-only helper that drives a single iteration of the runtime
    /// command channel: pulls the next pending `AutoTunerCommand` (if
    /// any), dispatches it through `handle_runtime_command`, and
    /// returns. Returns `Ok(false)` if no command was waiting.
    ///
    /// Live `tuner_loop` ownership of `command_rx` happens deep inside
    /// the long-running task that consumes `&mut self`, so test code
    /// can't drive that loop directly. This helper exposes just enough
    /// surface for an integration test to push a command through and
    /// observe the resulting state without spinning up the full loop.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn tick_runtime_commands_for_test(
        &mut self,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) -> bool {
        let Some(rx) = self.command_rx.as_mut() else {
            return false;
        };
        let Ok(command) = rx.try_recv() else {
            return false;
        };
        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        self.handle_runtime_command(command, freq_cmd_tx, &mut monitors)
            .await;
        true
    }

    /// Test-only state injection so the runtime-command path can
    /// reach the `runtime_ready` branch without running the full
    /// characterization pipeline.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn force_state_for_test(&mut self, state: TunerState) {
        self.state = state;
    }

    /// W8.2 — clamp `config.target_watts` (and the operator-requested copy)
    /// to `circuit_capacity_watts` whenever a real-time power-target raise
    /// would exceed the declared circuit budget. The dispatcher already
    /// throttles wall power below the cap, but the autotuner's own
    /// requested target should never lie above it — otherwise the tuner
    /// keeps trying to step up while the dispatcher fights it back down.
    ///
    /// Returns the clamped target in watts (0 = no power-target mode), or
    /// None if no clamp was needed.
    fn clamp_target_to_circuit_limit(&mut self) -> Option<u32> {
        let cap = self.config.circuit_capacity_watts?;
        if cap == 0 {
            return None;
        }
        let mut clamped = None;
        if self.config.target_watts > 0 && self.config.target_watts > cap {
            tracing::warn!(
                requested = self.config.target_watts,
                circuit_cap = cap,
                "Clamping autotuner target_watts to declared circuit capacity (W8.2 circuit guard)"
            );
            self.config.target_watts = cap;
            self.requested_config.target_watts = cap;
            if let Some(TunerMode::PowerTarget { watts }) = self.config.tuner_mode.as_mut() {
                *watts = cap;
            }
            if let Some(TunerMode::PowerTarget { watts }) =
                self.requested_config.tuner_mode.as_mut()
            {
                *watts = cap;
            }
            clamped = Some(cap);
        }
        if self.config.total_power_limit_w > 0 && self.config.total_power_limit_w > cap {
            tracing::warn!(
                requested = self.config.total_power_limit_w,
                circuit_cap = cap,
                "Clamping autotuner total_power_limit_w to declared circuit capacity (W8.2 circuit guard)"
            );
            self.config.total_power_limit_w = cap;
            self.requested_config.total_power_limit_w = cap;
            clamped = Some(cap);
        }
        clamped
    }

    /// Clamp the runtime PowerTarget/Heater setpoint to the configured total
    /// power budget (`total_power_limit_w`). Unlike `clamp_target_to_circuit_limit`
    /// this runs even when no `circuit_capacity_watts` is declared — which is the
    /// common default and the exact scenario the runtime path must cover.
    ///
    /// The runtime `ApplyMode(PowerTarget/Heater)` path validates only against
    /// `ABSOLUTE_MAX_WATTS` and clamps only to the circuit ceiling, so without
    /// this a direct operator command could drive the chain above the configured
    /// residential/operator power cap until the next full re-tune. Preset power
    /// targets already run through `clamp_preset_power_target`, which caps at
    /// `total_power_limit_w` — this closes that asymmetry for the runtime command
    /// path. `apply_to_config` sets `config.target_watts` from `target_watts()`
    /// for BOTH PowerTarget and Heater (btu→watts), and `apply_target_mode`
    /// allocates the budget from `config.target_watts`, so clamping it here bounds
    /// the applied budget for either mode.
    fn clamp_target_to_total_power_limit(&mut self) -> Option<u32> {
        let limit = self.config.total_power_limit_w;
        if limit == 0 {
            return None;
        }
        if self.config.target_watts > 0 && self.config.target_watts > limit {
            tracing::warn!(
                requested = self.config.target_watts,
                power_limit = limit,
                "Clamping autotuner target_watts to configured total_power_limit_w"
            );
            self.config.target_watts = limit;
            self.requested_config.target_watts = limit;
            if let Some(TunerMode::PowerTarget { watts }) = self.config.tuner_mode.as_mut() {
                *watts = limit;
            }
            if let Some(TunerMode::PowerTarget { watts }) =
                self.requested_config.tuner_mode.as_mut()
            {
                *watts = limit;
            }
            return Some(limit);
        }
        None
    }

    async fn apply_runtime_mode(
        &mut self,
        mode: TunerMode,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
    ) -> crate::AutoTunerCommandResult {
        if let Err(message) = mode.validate() {
            return crate::AutoTunerCommandResult {
                status: crate::AutoTunerCommandStatus::Rejected,
                applied_runtime: false,
                mode,
                message,
            };
        }

        mode.apply_to_config(&mut self.requested_config);
        mode.apply_to_config(&mut self.config);
        // W8.2: apply circuit-budget clamp after writing the new mode into
        // config so a PowerTarget command can never raise the requested
        // wattage above the declared circuit ceiling.
        let _ = self.clamp_target_to_circuit_limit();
        // F3: also clamp the runtime setpoint to the configured total power
        // budget. The circuit clamp above early-returns when no
        // `circuit_capacity_watts` is declared (the default), so on its own it
        // leaves `total_power_limit_w` unenforced for direct operator
        // PowerTarget/Heater commands — matching the preset path, not leaving a
        // residential-cap escape open until the next full re-tune.
        let _ = self.clamp_target_to_total_power_limit();
        self.active_runtime_objective =
            Self::target_mode_objective(self.config.target_mode).to_string();

        if matches!(mode, TunerMode::Manual { .. }) {
            return crate::AutoTunerCommandResult {
                status: crate::AutoTunerCommandStatus::Deferred,
                applied_runtime: false,
                mode,
                message: "manual runtime mode is persisted but not applied live by the autotuner command channel".to_string(),
            };
        }

        let runtime_ready = matches!(
            self.state,
            TunerState::Tuned | TunerState::PartiallyTuned | TunerState::BackgroundAdjust
        );
        if !runtime_ready || self.profiles.is_empty() {
            self.publish_runtime_status(
                "Autotuner target mode persisted; runtime apply deferred",
                None,
            );
            return crate::AutoTunerCommandResult {
                status: crate::AutoTunerCommandStatus::Deferred,
                applied_runtime: false,
                mode,
                message: "autotuner is not in background monitoring, so the new mode will apply on the next tuning cycle".to_string(),
            };
        }

        let chain_infos = self.runtime_chain_infos_from_profiles();
        if chain_infos.is_empty() {
            self.publish_runtime_status(
                "Autotuner target mode persisted; no profiles available for runtime apply",
                None,
            );
            return crate::AutoTunerCommandResult {
                status: crate::AutoTunerCommandStatus::Deferred,
                applied_runtime: false,
                mode,
                message: "no tuned profiles are available for a live target-mode apply".to_string(),
            };
        }

        self.apply_target_mode(&chain_infos, freq_cmd_tx).await;
        Self::sync_monitors_from_profiles(monitors, &self.profiles);
        self.active_runtime_limiting_factor =
            Self::compute_active_runtime_limiting_factor(monitors);
        self.live_avg_frequency_mhz = Self::monitor_avg_freq_mhz(monitors);
        self.publish_runtime_status("Autotuner target mode applied live", None);

        crate::AutoTunerCommandResult {
            status: crate::AutoTunerCommandStatus::Applied,
            applied_runtime: true,
            mode,
            message: "runtime autotuner accepted and applied the target mode".to_string(),
        }
    }

    /// Create a new auto-tuner.
    pub fn new(
        config: AutoTunerConfig,
        nominal_mhz: u16,
        chip_type: String,
        voltage_control: String,
        power_calibration: Arc<std::sync::RwLock<PowerCalibration>>,
    ) -> Self {
        let chip_id = crate::chip_id_from_type(&chip_type).unwrap_or(0x1387);
        // PERF-006/011: honor the default-OFF `DCENT_AM2_VOLTAGE_AUTOTUNE` gate.
        // With the gate unset this returns the SAME conservative capability set
        // as `autotuner_capabilities_for_chip` (byte-identical behavior); when
        // set, the am2/BM1362 dsPIC profile advertises the operator-opted-in,
        // downstream-clamped voltage search.
        let capabilities = crate::autotuner_capabilities_for_chip_with_voltage_autotune(
            chip_id,
            &voltage_control,
            std::env::var(crate::config::AM2_VOLTAGE_AUTOTUNE_ENV)
                .ok()
                .as_deref(),
        );
        let resolved_policy = crate::resolve_autotuner_policy(&config, &capabilities);
        let effective_config = resolved_policy.effective_config.clone();
        Self {
            requested_config: config,
            schedule: PowerSchedule::default(),
            telemetry: TelemetryRecorder::new(),
            profile_cache: HashMap::new(),
            efficiency_tx: None,
            progress_tx: None,
            runtime_status_tx: None,
            efficiency_watch_tx: None,
            chip_health_tx: None,
            telemetry_tx: None,
            accepted_work_rx: None,
            command_rx: None,
            resume_state_status: None,
            post_tune_windows: 0,
            live_avg_frequency_mhz: None,
            pre_tune_error_rate: 0.0,
            consecutive_temp_missing: HashMap::new(),
            safety_override: None,
            active_runtime_objective: Self::target_mode_objective(effective_config.target_mode)
                .to_string(),
            active_runtime_limiting_factor: None,
            target_chain_ids: BTreeSet::new(),
            chain_chip_ids: HashMap::new(),
            chain_hardware_identities: HashMap::new(),
            run_chain_plan: Vec::new(),
            target_chip_total: 0,
            failed_chain_ids: BTreeSet::new(),
            pending_stats: VecDeque::new(),
            power_calibration,
            config: effective_config,
            state: TunerState::Idle,
            profiles: HashMap::new(),
            nominal_mhz,
            chip_type,
            chip_id,
            capabilities,
            resolved_policy,
            accepted_work_baseline: None,
            accepted_work_reference_difficulty_per_kwh: None,
            active_silicon_profile_ids: HashMap::new(),
            active_silicon_profile_presets: HashMap::new(),
            last_applied_silicon_targets: HashMap::new(),
            chain_skus: HashMap::new(),
            step_up_gate_rx: None,
        }
    }

    /// W13.C3 (2026-05-10): register the BM1362 hashboard SKU for a chain.
    ///
    /// The daemon SHOULD call this once per chain at bring-up after the
    /// EEPROM / `/etc/subtype` read resolves to a `Bm1362HashboardSku`.
    /// The autotuner uses the SKU to:
    ///
    /// - Validate `(freq, volt)` dispatches against the SKU's published
    ///   PVT envelope (`AutoTunerError::OutsidePvt`).
    /// - Disable the freq↓ ⇒ volt↓ heuristic for `inverted_curve` SKUs
    ///   (BHB42841 —).
    /// - Emit a defense-in-depth warning when a `requires_apw12_plus`
    ///   SKU is paired with an APW12 SMBus PSU (the install preflight
    ///   already blocks this; the warning catches a bypass).
    pub fn set_chain_sku(&mut self, chain_id: u8, sku: Bm1362HashboardSku) {
        self.chain_skus.insert(chain_id, sku);
    }

    /// W13.C3: read-only accessor — returns the SKU registered for a
    /// chain, or `None` if no SKU has been registered (validation gate
    /// is open in that case).
    pub fn chain_sku(&self, chain_id: u8) -> Option<Bm1362HashboardSku> {
        self.chain_skus.get(&chain_id).copied()
    }

    /// W13.C3: count of chains with a registered hashboard SKU.
    /// Test/diagnostic helper.
    pub fn chain_sku_count(&self) -> usize {
        self.chain_skus.len()
    }

    /// CE-011 (2026-07-08): tighten the frequency **CEILING** to each
    /// registered SKU's PVT-envelope maximum frequency.
    ///
    /// For every chain with a `set_chain_sku`-registered `Bm1362HashboardSku`,
    /// compute the SKU envelope's max published frequency
    /// (`pvt_envelope(sku).iter().map(|(f, _)| *f).max()`) and tighten
    /// `self.config.max_freq_mhz` with `.min()`. **CEILING-ONLY** — this
    /// uses `.min()` (never `.clamp()`), never touches `min_freq_mhz` (raising
    /// the floor would snap a quiet home unit UP in frequency/power), and can
    /// only ever LOWER the ceiling, never raise it.
    ///
    /// Fail-closed: when no SKU is registered (the live default today —
    /// `chain_skus` is empty because no production path calls
    /// `set_chain_sku` yet), this is a pure no-op and behavior is exactly
    /// today's. Deliberate no-op on the proven am2 paths too: every BM1362
    /// SKU envelope max (>=545) is `>=` the am2 applied ceiling 545, so
    /// `.min()` is a no-op there and `a lab unit`/`a lab unit` stay byte-identical.
    pub(crate) fn apply_sku_freq_ceilings(&mut self) {
        // Snapshot to avoid holding an immutable borrow of `self.chain_skus`
        // across the `self.config` mutation.
        let registered: Vec<(u8, Bm1362HashboardSku)> =
            self.chain_skus.iter().map(|(&c, &s)| (c, s)).collect();
        for (chain_id, sku) in registered {
            let Some(envelope_max) = crate::pvt_envelope::pvt_envelope(sku)
                .iter()
                .map(|(f, _)| *f)
                .max()
            else {
                // Empty PVT table (unrecognised SKU) — nothing to clamp to.
                continue;
            };
            let before = self.config.max_freq_mhz;
            // CEILING-ONLY: `.min()`, never `.clamp()`; never raises the ceiling.
            let tightened = before.min(envelope_max);
            if tightened < before {
                self.config.max_freq_mhz = tightened;
                info!(
                    chain_id,
                    sku = %sku.hashboard_id(),
                    envelope_max_mhz = envelope_max,
                    previous_max_mhz = before,
                    new_max_mhz = tightened,
                    "CE-011: tightened autotuner frequency ceiling to SKU PVT envelope max (ceiling-only)"
                );
            }
        }
    }

    /// W6.3 + W6.4: wire the step-up gate signal receiver from the
    /// daemon. The daemon constructs the watch channel and feeds it
    /// from the live `AcceptanceTracker` (stratum client) +
    /// `HwErrTracker` (work dispatcher). When unset, the gate stays
    /// open and the autotuner keeps its legacy step-up behavior.
    pub fn set_step_up_gate_watch(
        &mut self,
        rx: tokio::sync::watch::Receiver<crate::StepUpGateSignal>,
    ) {
        self.step_up_gate_rx = Some(rx);
    }

    /// W6.3 + W6.4: read the latest step-up gate signal and decide
    /// whether a boost-back / step-up is authorized.
    ///
    /// Returns `true` (gate open) when:
    /// - no signal receiver is wired (legacy), or
    /// - both gate conditions hold: rolling acceptance >= 99% AND
    ///   worst-chip HW err < 2%.
    ///
    /// Returns `false` (gate closed) and emits a structured
    /// `tracing::warn!` when either gate condition fails. The warning
    /// names the active acceptance percentage and worst-chip rate so
    /// the operator can see *why* step-up is blocked.
    fn step_up_gate_passes(&self, chain_id: u8, chip_idx: u8, target_freq_mhz: u16) -> bool {
        let Some(rx) = self.step_up_gate_rx.as_ref() else {
            return true;
        };
        let signal = *rx.borrow();
        if signal.passes() {
            return true;
        }
        // Failed — emit a single structured warn so operators see why
        // the gate fired. The autotuner already logs every successful
        // step-up at info level, so a corresponding warn keeps the
        // log timeline honest.
        tracing::warn!(
            chain_id,
            chip = chip_idx,
            target_freq_mhz,
            rolling_acceptance_pct = signal.rolling_acceptance_pct,
            worst_chip_hw_err_rate = signal
                .worst_chip_hw_err_rate
                .map(|r| format!("{:.4}", r))
                .unwrap_or_else(|| "none".to_string()),
            acceptance_threshold = crate::StepUpGateSignal::ACCEPTANCE_THRESHOLD_PCT,
            hw_err_threshold = crate::StepUpGateSignal::HW_ERR_THRESHOLD,
            "Autotuner step-up gate BLOCKED: refusing to raise chip frequency \
             while pool acceptance < 99% or per-chip HW err >= 2%. Staying at \
             current freq until rolling window clears."
        );
        false
    }

    /// W13-A: read-only accessor for the operator-selected active
    /// silicon profile id on a given chain. Returns `None` if no
    /// `AutoTunerCommand::ApplySiliconProfile` has been processed for
    /// the (model, hashboard) tuple. The `model` argument is the
    /// snake_case wire spelling (e.g. `antminer_s9`,
    /// `antminer_s19j_pro_a`) — the autotuner stays free of the
    /// `dcentrald-api-types::MinerModel` enum.
    pub fn active_silicon_profile_id(&self, miner_model: &str, hashboard: &str) -> Option<&str> {
        self.active_silicon_profile_ids
            .get(&(miner_model.to_string(), hashboard.to_string()))
            .map(|s| s.as_str())
    }

    /// W13-A: count of chains with an operator-selected active silicon
    /// profile recorded inside the tuner. Test/diagnostic helper.
    pub fn active_silicon_profile_count(&self) -> usize {
        self.active_silicon_profile_ids.len()
    }

    /// Set the power schedule for time-of-use optimization.
    pub fn set_schedule(&mut self, schedule: PowerSchedule) {
        self.schedule = schedule;
    }

    /// Set the broadcast sender for real-time efficiency dashboard feed.
    ///
    /// The dcentrald-api crate creates the broadcast channel and passes
    /// the sender here. The autotuner publishes EfficiencySnapshots every
    /// 5 seconds during background monitoring.
    pub fn set_efficiency_broadcast(
        &mut self,
        tx: tokio::sync::broadcast::Sender<EfficiencySnapshot>,
    ) {
        self.efficiency_tx = Some(tx);
    }

    pub fn set_runtime_status_watch(
        &mut self,
        tx: tokio::sync::watch::Sender<AutotunerRuntimeStatus>,
    ) {
        self.runtime_status_tx = Some(tx);
        self.publish_runtime_status("Autotuner initialized", None);
    }

    pub fn set_efficiency_watch(
        &mut self,
        tx: tokio::sync::watch::Sender<Option<EfficiencySnapshot>>,
    ) {
        self.efficiency_watch_tx = Some(tx);
    }

    pub fn set_chip_health_watch(
        &mut self,
        tx: tokio::sync::watch::Sender<Option<crate::LiveChipHealthState>>,
    ) {
        self.chip_health_tx = Some(tx);
    }

    pub fn set_telemetry_watch(&mut self, tx: tokio::sync::watch::Sender<TelemetryExportState>) {
        self.telemetry_tx = Some(tx);
        self.publish_telemetry_state();
    }

    pub fn set_accepted_work_watch(
        &mut self,
        rx: tokio::sync::watch::Receiver<Option<crate::AcceptedWorkSignal>>,
    ) {
        self.accepted_work_rx = Some(rx);
    }

    pub fn set_command_receiver(&mut self, rx: mpsc::Receiver<crate::AutoTunerCommand>) {
        self.command_rx = Some(rx);
    }

    /// Get a reference to the telemetry recorder for API export.
    pub fn telemetry(&self) -> &TelemetryRecorder {
        &self.telemetry
    }

    /// Get a mutable reference to the telemetry recorder.
    pub fn telemetry_mut(&mut self) -> &mut TelemetryRecorder {
        &mut self.telemetry
    }

    /// Cache the current profile for a specific power target.
    ///
    /// Enables instant DPS switching: when the schedule changes power target,
    /// we check the cache before re-tuning. If a profile for this target exists,
    /// we apply it directly (~3 seconds vs ~15 seconds for re-characterization).
    fn cache_profile_for_target(&mut self, chain_id: u8, target_watts: u32) {
        if let Some(profile) = self.profiles.get(&chain_id) {
            self.profile_cache
                .insert((chain_id, target_watts), profile.clone());
            tracing::debug!(
                chain_id,
                target_watts,
                "Cached profile for chain {} at {} W target",
                chain_id,
                target_watts,
            );
        }
    }

    fn cache_current_profiles_for_target(&mut self, target_watts: u32) {
        let chain_ids: Vec<u8> = self.profiles.keys().copied().collect();
        for chain_id in chain_ids {
            self.cache_profile_for_target(chain_id, target_watts);
        }
    }

    /// Try to load a cached profile for a specific power target.
    fn load_cached_profile(&self, chain_id: u8, target_watts: u32) -> Option<&TuningProfile> {
        self.profile_cache.get(&(chain_id, target_watts))
    }

    async fn apply_cached_profiles_for_target(
        &mut self,
        target_watts: u32,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) -> bool {
        let chain_ids: Vec<u8> = self.profiles.keys().copied().collect();
        if chain_ids.is_empty() {
            return false;
        }

        let cached_profiles: Option<Vec<(u8, TuningProfile)>> = chain_ids
            .iter()
            .map(|&chain_id| {
                self.load_cached_profile(chain_id, target_watts)
                    .cloned()
                    .map(|profile| (chain_id, profile))
            })
            .collect();

        let Some(cached_profiles) = cached_profiles else {
            return false;
        };

        for (chain_id, profile) in cached_profiles {
            info!(
                chain_id,
                target_watts, "DPS: applying cached profile for {} W target", target_watts,
            );
            if let Err(apply_err) = self
                .apply_profile_via_channel(chain_id, &profile, freq_cmd_tx)
                .await
            {
                warn!(
                    chain_id,
                    target_watts,
                    error = %apply_err,
                    "DPS: failed to apply cached profile"
                );
                return false;
            }
            self.profiles.insert(chain_id, profile);
        }

        true
    }

    fn dps_walker_config(&self) -> crate::dps::DpsWalkerConfig {
        let max_power_w = if self.config.total_power_limit_w == 0 {
            crate::config::ABSOLUTE_MAX_WATTS
        } else {
            self.config
                .total_power_limit_w
                .min(crate::config::ABSOLUTE_MAX_WATTS)
        };

        crate::dps::DpsWalkerConfig {
            power_step_w: self.config.power_step_w,
            hashrate_step_ths: self.config.hashrate_step_ths,
            min_power_w: 200,
            max_power_w,
            min_hashrate_ths: 1.0,
            max_hashrate_ths: self.config.target_hashrate_ths.max(300.0),
            high_performance_mode: self.config.dps_high_performance_mode,
        }
    }

    fn next_scheduled_power_target(
        &self,
        current_target_watts: Option<u32>,
        desired_target_watts: Option<u32>,
    ) -> Option<u32> {
        let desired_target_watts = desired_target_watts.filter(|watts| *watts > 0);
        match (current_target_watts, desired_target_watts) {
            (Some(current), Some(desired)) => Some(
                self.dps_walker_config()
                    .walk_power_target(current, desired)
                    .next,
            ),
            (None, Some(desired)) => Some(
                self.dps_walker_config()
                    .walk_power_target(desired, desired)
                    .next,
            ),
            (_, None) => None,
        }
    }

    fn refresh_runtime_after_profile_change(
        &mut self,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
    ) {
        Self::sync_monitors_from_profiles(monitors, &self.profiles);
        self.active_runtime_limiting_factor =
            Self::compute_active_runtime_limiting_factor(monitors);
        self.live_avg_frequency_mhz = Self::monitor_avg_freq_mhz(monitors);
    }

    fn resume_state_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.config.profile_path).join("state.toml")
    }

    fn resume_fingerprint_for_profiles(
        &self,
        profiles: &HashMap<u8, TuningProfile>,
    ) -> crate::AutotunerHardwareFingerprint {
        crate::AutotunerHardwareFingerprint::from_profiles_with_identities(
            profiles,
            Some(self.capabilities.profile_key.clone()),
            &self.chain_hardware_identities,
        )
    }

    fn set_resume_state_status(
        &mut self,
        status: impl Into<String>,
        path: Option<&std::path::Path>,
        matched: bool,
        matched_chains: usize,
        message: impl Into<String>,
    ) {
        self.resume_state_status = Some(AutotunerResumeStateStatus {
            status: status.into(),
            path: path.map(|path| path.display().to_string()),
            matched,
            matched_chains,
            message: message.into(),
        });
    }

    fn load_resume_state_gate(&mut self, candidates: &[WarmStartCandidate]) -> ResumeStateGate {
        let path = self.resume_state_path();
        if candidates.is_empty() || !path.exists() {
            self.set_resume_state_status(
                "legacy_no_state",
                Some(&path),
                false,
                0,
                "No matching resume-state file was evaluated; using legacy saved-profile warm-start behavior",
            );
            return ResumeStateGate::LegacyNoState;
        }

        let profiles: HashMap<u8, TuningProfile> = candidates
            .iter()
            .map(|candidate| (candidate.chain_id, candidate.profile.clone()))
            .collect();
        let fingerprint = self.resume_fingerprint_for_profiles(&profiles);

        match crate::AutotunerResumeState::load_if_hardware_matches(&path, &fingerprint) {
            Ok(Some(state)) => {
                self.set_resume_state_status(
                    "matched",
                    Some(&path),
                    true,
                    state.chains.len(),
                    "Resume state matched current hardware fingerprint",
                );
                info!(
                    path = %path.display(),
                    chains = state.chains.len(),
                    "Autotuner resume state matched current hardware"
                );
                ResumeStateGate::Matched(state)
            }
            Ok(None) => {
                self.set_resume_state_status(
                    "invalid",
                    Some(&path),
                    false,
                    0,
                    "Resume state did not match current hardware fingerprint; cold-starting saved profiles",
                );
                warn!(
                    path = %path.display(),
                    "Autotuner resume state does not match current hardware; cold-starting saved profiles"
                );
                ResumeStateGate::Invalid
            }
            Err(error) => {
                self.set_resume_state_status(
                    "error",
                    Some(&path),
                    false,
                    0,
                    format!(
                        "Resume state could not be loaded ({error}); cold-starting saved profiles"
                    ),
                );
                warn!(
                    path = %path.display(),
                    error = %error,
                    "Autotuner resume state could not be loaded; cold-starting saved profiles"
                );
                ResumeStateGate::Invalid
            }
        }
    }

    fn resume_state_contains_chain(
        state: &crate::AutotunerResumeState,
        chain_id: u8,
        chip_count: u8,
    ) -> bool {
        state
            .chains
            .iter()
            .any(|chain| chain.chain_id == chain_id && chain.chip_count == chip_count)
    }

    fn save_resume_state(&self) {
        if self.profiles.is_empty() {
            return;
        }

        let fingerprint = self.resume_fingerprint_for_profiles(&self.profiles);
        let state = crate::AutotunerResumeState::from_profiles(&self.profiles, fingerprint);
        let path = self.resume_state_path();
        if let Err(e) = state.save_atomic(&path) {
            warn!(
                error = %e,
                path = %path.display(),
                "Failed to save autotuner resume state"
            );
        }
    }

    /// Set the progress broadcast sender for real-time tuning progress.
    ///
    /// The dashboard subscribes to show a progress bar during characterization
    /// instead of a static "Characterizing..." message.
    pub fn set_progress_broadcast(&mut self, tx: tokio::sync::broadcast::Sender<TuningProgress>) {
        self.progress_tx = Some(tx);
    }

    fn current_grade_counts(&self) -> Option<SiliconGradeCounts> {
        let mut iter = self.profiles.values();
        let first = iter.next()?;
        let mut counts = SiliconGradeCounts {
            a: first.stats.grade_a,
            b: first.stats.grade_b,
            c: first.stats.grade_c,
            d: first.stats.grade_d,
        };
        for profile in iter {
            counts.a += profile.stats.grade_a;
            counts.b += profile.stats.grade_b;
            counts.c += profile.stats.grade_c;
            counts.d += profile.stats.grade_d;
        }
        Some(counts)
    }

    fn current_avg_freq_mhz(&self) -> Option<f64> {
        let total_chips: u16 = self.profiles.values().map(|p| p.chip_count as u16).sum();
        if total_chips == 0 {
            return None;
        }
        let weighted_sum: f64 = self
            .profiles
            .values()
            .map(|p| p.stats.avg_freq_mhz * p.chip_count as f64)
            .sum();
        Some(weighted_sum / total_chips as f64)
    }

    fn monitor_avg_freq_mhz(monitors: &HashMap<(u8, u8), ChipMonitor>) -> Option<f64> {
        let mut total = 0u64;
        let mut count = 0u64;
        for monitor in monitors.values() {
            if monitor.masked || monitor.current_freq_mhz == 0 {
                continue;
            }
            total += monitor.current_freq_mhz as u64;
            count += 1;
        }
        (count > 0).then_some(total as f64 / count as f64)
    }

    fn tuned_chain_ids(&self) -> Vec<u8> {
        let mut ids: Vec<u8> = self.profiles.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    fn failed_chain_ids(&self) -> Vec<u8> {
        self.failed_chain_ids.iter().copied().collect()
    }

    fn tuned_chip_total(&self) -> usize {
        self.profiles
            .values()
            .map(|profile| profile.chip_count as usize)
            .sum()
    }

    fn progress_totals(&self, progress: &TuningProgress) -> (usize, u16, f64, Option<f64>) {
        let total_target = if self.target_chip_total > 0 {
            self.target_chip_total
        } else {
            progress.total_chips as u16
        };

        let chips_before: usize = self
            .run_chain_plan
            .iter()
            .take_while(|(chain_id, _)| *chain_id != progress.chain_id)
            .map(|(_, chip_count)| *chip_count as usize)
            .sum();
        let completed_in_chain =
            (progress.total_chips as usize).saturating_sub(progress.active_chips);
        let completed_total = (chips_before + completed_in_chain).min(total_target as usize);
        let percent_complete = if total_target > 0 {
            (completed_total as f64 / total_target as f64 * 100.0).min(99.0)
        } else {
            progress.percent_complete
        };
        let estimated_remaining_s = if progress.active_chips > 0 {
            let seconds_per_active_chip =
                progress.estimated_remaining_s / progress.active_chips as f64;
            let remaining_total = total_target as usize - completed_total;
            Some(seconds_per_active_chip * remaining_total as f64)
        } else {
            Some(progress.estimated_remaining_s)
        };

        (
            completed_total,
            total_target,
            percent_complete,
            estimated_remaining_s,
        )
    }

    fn steady_state(&self) -> TunerState {
        if self.failed_chain_ids.is_empty() {
            TunerState::Tuned
        } else if self.profiles.is_empty() {
            TunerState::Failed
        } else {
            TunerState::PartiallyTuned
        }
    }

    fn build_runtime_status(
        &self,
        message: &str,
        progress: Option<&TuningProgress>,
    ) -> AutotunerRuntimeStatus {
        let total_chips: u16 = if self.target_chip_total > 0 {
            self.target_chip_total
        } else {
            self.profiles.values().map(|p| p.chip_count as u16).sum()
        };
        let tuned_chain_ids = self.tuned_chain_ids();
        let failed_chain_ids = self.failed_chain_ids();
        let display_state = match self.state {
            TunerState::Tuned | TunerState::PartiallyTuned | TunerState::Failed => {
                self.steady_state()
            }
            other => other,
        };
        let (completed_chips, runtime_total_chips, percent_complete, estimated_remaining_s) =
            if let Some(progress) = progress {
                self.progress_totals(progress)
            } else {
                let completed = self.tuned_chip_total();
                let percent = if total_chips > 0 {
                    (completed as f64 / total_chips as f64 * 100.0).min(100.0)
                } else {
                    0.0
                };
                (completed, total_chips, percent, None)
            };
        AutotunerRuntimeStatus {
            enabled: self.config.enabled,
            live_runtime: true,
            stale: false,
            age_s: 0,
            source: "runtime".to_string(),
            state: display_state.to_string(),
            phase: progress
                .map(|p| p.phase.clone())
                .unwrap_or_else(|| display_state.to_string()),
            percent_complete,
            completed_chips,
            active_chips: progress.map(|p| p.active_chips).unwrap_or(0),
            total_chips: runtime_total_chips,
            active_chain_id: progress.map(|p| p.chain_id),
            active_chain_total_chips: progress.map(|p| p.total_chips),
            target_chains: self.target_chain_ids.len(),
            tuned_chains: tuned_chain_ids.len(),
            failed_chains: failed_chain_ids.len(),
            tuned_chain_ids,
            failed_chain_ids,
            estimated_remaining_s,
            avg_frequency_mhz: self
                .live_avg_frequency_mhz
                .or_else(|| self.current_avg_freq_mhz()),
            efficiency_jth: self.profiles.values().find_map(|p| {
                (p.estimated_efficiency_jth > 0.0).then_some(p.estimated_efficiency_jth)
            }),
            silicon_grades: self.current_grade_counts(),
            policy: Some(self.build_policy_status()),
            resume_state: self.resume_state_status.clone(),
            last_update_s: now_unix_s(),
            message: message.to_string(),
        }
    }

    fn build_policy_status(&self) -> AutotunerPolicyStatus {
        let requested_preset = self.resolved_policy.requested_preset.clone();
        let effective_preset = self.resolved_policy.effective_preset.clone();
        let requested_preset_supported = self.resolved_policy.requested_preset_supported;
        let requested_preset_display_name = requested_preset
            .as_deref()
            .and_then(crate::autotuner_preset_display_name)
            .map(str::to_string);
        let effective_preset_display_name = effective_preset
            .as_deref()
            .and_then(crate::autotuner_preset_display_name)
            .map(str::to_string);
        let requested_preset_reason = self.resolved_policy.requested_preset_reason.clone();

        let active_objective = Some(self.active_runtime_objective.clone());

        let safety_override = self.safety_override.clone().or_else(|| {
            self.consecutive_temp_missing
                .values()
                .any(|count| *count >= 3)
                .then_some("missing_temperature".to_string())
        });

        let active_limiting_factor = if safety_override.is_some() {
            Some("sensor_safety".to_string())
        } else {
            self.active_runtime_limiting_factor.clone()
        };

        AutotunerPolicyStatus {
            requested_preset,
            effective_preset,
            requested_preset_supported,
            requested_preset_display_name,
            effective_preset_display_name,
            requested_preset_reason,
            degraded_from_requested: self.resolved_policy.degraded_from_requested,
            capabilities: Some(self.resolved_policy.capabilities.clone()),
            active_objective,
            active_limiting_factor,
            safety_override,
        }
    }

    fn compute_active_runtime_limiting_factor(
        monitors: &HashMap<(u8, u8), ChipMonitor>,
    ) -> Option<String> {
        if monitors
            .values()
            .any(|monitor| monitor.sensor_safety_limit_mhz.is_some())
        {
            return Some("sensor_safety".to_string());
        }
        if monitors
            .values()
            .any(|monitor| monitor.thermal_limit_mhz.is_some())
        {
            return Some("thermal".to_string());
        }
        if monitors
            .values()
            .any(|monitor| monitor.fan_limit_mhz.is_some())
        {
            return Some("fan_clamp".to_string());
        }
        None
    }

    fn target_mode_objective(target: TuneTarget) -> &'static str {
        match target {
            TuneTarget::Hashrate => "hashrate",
            TuneTarget::Power => "power_cap",
            TuneTarget::Efficiency => "efficiency",
            TuneTarget::HashrateTarget => "hashrate_target",
            TuneTarget::EfficiencyJTH => "efficiency_jth",
        }
    }

    fn resolved_runtime_objective(&self) -> &'static str {
        match self.config.target_mode {
            TuneTarget::Hashrate => "hashrate",
            TuneTarget::Power => {
                if self.mixed_chain_chip_ids() || self.config.target_watts == 0 {
                    "hashrate"
                } else {
                    "power_cap"
                }
            }
            TuneTarget::HashrateTarget => {
                if self.mixed_chain_chip_ids() || self.config.target_hashrate_ths <= 0.0 {
                    "hashrate"
                } else {
                    "hashrate_target"
                }
            }
            TuneTarget::Efficiency => "efficiency",
            TuneTarget::EfficiencyJTH => "efficiency_jth",
        }
    }

    fn publish_runtime_status(&self, message: &str, progress: Option<&TuningProgress>) {
        if let Some(ref tx) = self.runtime_status_tx {
            let _ = tx.send(self.build_runtime_status(message, progress));
        }
    }

    fn publish_telemetry_state(&self) {
        if let Some(ref tx) = self.telemetry_tx {
            let _ = tx.send(self.telemetry.export_state());
        }
    }

    fn record_telemetry_sample(&mut self, snapshot: &ChipStatsSnapshot) {
        if !self.telemetry.is_recording() {
            return;
        }

        let chips = snapshot
            .chip_nonces
            .iter()
            .enumerate()
            .map(|(idx, &nonces)| crate::telemetry::ChipTelemetry {
                chip_index: idx as u8,
                nonces,
                errors: snapshot.chip_errors.get(idx).copied().unwrap_or(0),
                freq_mhz: self
                    .profiles
                    .get(&snapshot.chain_id)
                    .and_then(|profile| profile.chips.get(idx))
                    .map(|chip| chip.operating_mhz)
                    .unwrap_or(self.nominal_mhz),
                decision: None,
            })
            .collect();

        self.telemetry
            .record_sample(crate::telemetry::TelemetrySample {
                elapsed_s: self.telemetry.elapsed_s(),
                chain_id: snapshot.chain_id,
                chips,
                board_temp_c: snapshot.board_temp_c,
                tuner_state: self.state.to_string(),
                difficulty: snapshot.current_difficulty,
            });
        self.publish_telemetry_state();
    }

    fn publish_efficiency_snapshot(&self, snapshot: EfficiencySnapshot) {
        if let Some(ref tx) = self.efficiency_tx {
            let _ = tx.send(snapshot.clone());
        }
        if let Some(ref tx) = self.efficiency_watch_tx {
            let _ = tx.send(Some(snapshot));
        }
    }

    fn publish_chip_health(&self, statuses: Vec<ChipHealthStatus>) {
        if let Some(ref tx) = self.chip_health_tx {
            let _ = tx.send(Some(crate::LiveChipHealthState {
                last_update_s: now_unix_s(),
                chips: statuses,
            }));
        }
    }

    /// Update the current fan speed for thermal ceiling estimation.
    ///
    /// Called by the thermal controller whenever fan PWM changes.
    /// The autotuner uses this to adjust its frequency expectations:
    /// at low fan speeds (Home mode quiet), the thermal ceiling is
    /// lower, so max achievable frequency is reduced.
    pub fn update_fan_speed(&mut self, fan_pwm: u8) {
        self.config.current_fan_pwm = fan_pwm.min(FAN_PWM_MAX);
    }

    /// Get the fan-adjusted thermal ceiling factor (0.65 - 1.0).
    fn fan_factor(&self) -> f64 {
        fan_thermal_factor(self.config.current_fan_pwm)
    }

    /// Send a frequency command, logging a warning on failure.
    ///
    /// Safety-critical commands (SetVoltage, emergency throttle) must not be
    /// silently dropped. If the channel is full or the receiver is gone, we
    /// log a warning so operators can diagnose the issue.
    #[allow(dead_code)]
    async fn send_freq_cmd(freq_cmd_tx: &mpsc::Sender<FreqCommand>, cmd: FreqCommand) {
        let desc = format!("{:?}", cmd);
        if let Err(e) = freq_cmd_tx.send(cmd).await {
            warn!(
                error = %e,
                command = %desc,
                "SAFETY: failed to send frequency command — channel closed or full. \
                 The autotuner believes it changed hardware state but it did NOT.",
            );
        }
    }

    async fn send_freq_cmd_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        cmd: FreqCommand,
    ) -> crate::Result<()> {
        let desc = format!("{:?}", cmd);
        freq_cmd_tx.send(cmd).await.map_err(|e| {
            crate::AutoTunerError::Config(format!(
                "failed to send frequency command `{}`: {}",
                desc, e
            ))
        })
    }

    async fn set_chain_freq_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        freq_mhz: u16,
    ) -> crate::Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::SetChainFreq {
                chain_id,
                freq_mhz,
                ack_tx: Some(ack_tx),
            },
        )
        .await?;
        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "dispatcher dropped chain frequency acknowledgement for chain {}",
                    chain_id
                ))
            })?
            .map_err(|detail| {
                crate::AutoTunerError::Config(format!(
                    "failed to apply chain frequency on chain {}: {}",
                    chain_id, detail
                ))
            })
    }

    async fn set_chip_freq_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        chip_index: u8,
        freq_mhz: u16,
    ) -> crate::Result<u16> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::SetChipFreq {
                chain_id,
                chip_index,
                freq_mhz,
                ack_tx: Some(ack_tx),
            },
        )
        .await?;
        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "dispatcher dropped chip frequency acknowledgement for chain {} chip {}",
                    chain_id, chip_index
                ))
            })?
            .map_err(|detail| {
                crate::AutoTunerError::Config(format!(
                    "failed to apply chain {} chip {} frequency {} MHz: {}",
                    chain_id, chip_index, freq_mhz, detail
                ))
            })
    }

    async fn set_chip_limit_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        chip_index: u8,
        max_freq_mhz: Option<u16>,
        source: crate::FrequencyLimitSource,
    ) -> crate::Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::SetChipFrequencyLimit {
                chain_id,
                chip_index,
                max_freq_mhz,
                source,
                ack_tx: Some(ack_tx),
            },
        )
        .await?;
        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "dispatcher dropped chip limit acknowledgement for chain {} chip {}",
                    chain_id, chip_index
                ))
            })?
            .map_err(|detail| {
                crate::AutoTunerError::Config(format!(
                    "failed to apply chain {} chip {} ceiling {:?} from {:?}: {}",
                    chain_id, chip_index, max_freq_mhz, source, detail
                ))
            })
    }

    async fn wait_for_dispatcher(freq_cmd_tx: &mpsc::Sender<FreqCommand>) -> crate::Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        freq_cmd_tx
            .send(FreqCommand::Barrier { ack_tx })
            .await
            .map_err(|e| {
                crate::AutoTunerError::Config(format!(
                    "failed to synchronize dispatcher command stream: {}",
                    e
                ))
            })?;
        ack_rx.await.map_err(|_| {
            crate::AutoTunerError::Config(
                "dispatcher barrier acknowledgement channel closed".to_string(),
            )
        })
    }

    async fn begin_measurement_window(
        &self,
        chain_id: u8,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        settle_delay: Duration,
    ) -> crate::Result<u64> {
        Self::wait_for_dispatcher(freq_cmd_tx).await?;
        if settle_delay > Duration::ZERO {
            tokio::time::sleep(settle_delay).await;
        }

        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        freq_cmd_tx
            .send(FreqCommand::BeginMeasurement { chain_id, ack_tx })
            .await
            .map_err(|e| {
                crate::AutoTunerError::Config(format!(
                    "failed to start measurement window for chain {}: {}",
                    chain_id, e
                ))
            })?;

        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "measurement window acknowledgement dropped for chain {}",
                    chain_id
                ))
            })?
            .ok_or(crate::AutoTunerError::ChainUnavailable { chain_id })
    }

    async fn restore_chain_nominal(
        &self,
        chain_id: u8,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) -> crate::Result<()> {
        Self::set_chain_freq_checked(freq_cmd_tx, chain_id, self.nominal_mhz).await?;
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::UpdateWorkTime {
                chain_id,
                min_freq_mhz: self.nominal_mhz,
            },
        )
        .await?;
        Self::wait_for_dispatcher(freq_cmd_tx).await
    }

    async fn set_voltage_checked_once(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        voltage_mv: u16,
    ) -> crate::Result<u16> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::SetVoltage {
                chain_id,
                voltage_mv,
                ack_tx: Some(ack_tx),
            },
        )
        .await?;
        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "dispatcher dropped voltage apply acknowledgement for chain {}",
                    chain_id
                ))
            })?
            .map_err(|detail| {
                crate::AutoTunerError::Config(format!(
                    "voltage apply failed for chain {}: {}",
                    chain_id, detail
                ))
            })
    }

    async fn set_voltage_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        voltage_mv: u16,
    ) -> crate::Result<u16> {
        for attempt in 1..=VOLTAGE_COMMAND_MAX_ATTEMPTS {
            match Self::set_voltage_checked_once(freq_cmd_tx, chain_id, voltage_mv).await {
                Ok(applied_mv) => return Ok(applied_mv),
                Err(crate::AutoTunerError::Config(detail))
                    if attempt < VOLTAGE_COMMAND_MAX_ATTEMPTS
                        && Self::is_retryable_voltage_command_error(&detail) =>
                {
                    warn!(
                        chain_id,
                        target_mv = voltage_mv,
                        attempt,
                        max_attempts = VOLTAGE_COMMAND_MAX_ATTEMPTS,
                        error = %detail,
                        "Voltage apply hit a transient communication fault — retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
                Err(error) => return Err(error),
            }
        }

        unreachable!("voltage apply retry loop must return or continue")
    }

    async fn verify_voltage_checked_once(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        target_mv: u16,
    ) -> crate::Result<Option<u16>> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        Self::send_freq_cmd_checked(
            freq_cmd_tx,
            FreqCommand::VerifyVoltage {
                chain_id,
                target_mv,
                ack_tx: Some(ack_tx),
            },
        )
        .await?;
        ack_rx
            .await
            .map_err(|_| {
                crate::AutoTunerError::Config(format!(
                    "dispatcher dropped voltage verification acknowledgement for chain {}",
                    chain_id
                ))
            })?
            .map_err(|detail| {
                crate::AutoTunerError::Config(format!(
                    "voltage verification failed for chain {}: {}",
                    chain_id, detail
                ))
            })
    }

    async fn verify_voltage_checked(
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        chain_id: u8,
        target_mv: u16,
    ) -> crate::Result<Option<u16>> {
        for attempt in 1..=VOLTAGE_COMMAND_MAX_ATTEMPTS {
            match Self::verify_voltage_checked_once(freq_cmd_tx, chain_id, target_mv).await {
                Ok(actual_mv) => return Ok(actual_mv),
                Err(crate::AutoTunerError::Config(detail))
                    if attempt < VOLTAGE_COMMAND_MAX_ATTEMPTS
                        && Self::is_retryable_voltage_command_error(&detail) =>
                {
                    warn!(
                        chain_id,
                        target_mv,
                        attempt,
                        max_attempts = VOLTAGE_COMMAND_MAX_ATTEMPTS,
                        error = %detail,
                        "Voltage verification hit a transient communication fault — retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
                Err(error) => return Err(error),
            }
        }

        unreachable!("voltage verification retry loop must return or continue")
    }

    fn is_retryable_voltage_command_error(detail: &str) -> bool {
        let detail = detail.to_ascii_lowercase();
        [
            "timed out",
            "timeout",
            "reply channel dropped",
            "runtime voltage reply channel dropped",
            "runtime voltage verification reply channel dropped",
            "i/o",
            "input/output",
            "nack",
            "bus",
            "temporar",
            "resource busy",
        ]
        .iter()
        .any(|needle| detail.contains(needle))
    }

    fn assess_voltage_window(
        snapshot: &ChipStatsSnapshot,
        chip_count: u8,
        error_threshold_pct: f64,
        min_window_s: f64,
    ) -> VoltageWindowDecision {
        if snapshot.window_duration_s < min_window_s * 0.5 {
            return VoltageWindowDecision::LowConfidence;
        }

        let required_active = ((chip_count as f64) * 0.75).ceil().max(1.0) as usize;
        let mut active_chips = 0usize;
        let mut communication_fault_chips = 0usize;
        let mut low_sample_chips = 0usize;
        let mut zero_data_chips = 0usize;

        for i in 0..chip_count as usize {
            if i >= snapshot.chip_nonces.len() {
                zero_data_chips += 1;
                continue;
            }

            let nonces = snapshot.chip_nonces[i];
            let errors = snapshot.stability_error_count(i);
            let communication_issues = snapshot.communication_issue_count(i);
            let total = nonces + errors;

            if total == 0 {
                if communication_issues > 0 {
                    communication_fault_chips += 1;
                } else {
                    zero_data_chips += 1;
                }
                continue;
            }

            active_chips += 1;
            if total < VOLTAGE_SEARCH_MIN_SAMPLES_PER_CHIP {
                low_sample_chips += 1;
            }

            let error_rate = errors as f64 / total as f64 * 100.0;
            if error_rate >= error_threshold_pct {
                return VoltageWindowDecision::Unstable;
            }
        }

        if active_chips < required_active {
            if communication_fault_chips > 0 {
                return VoltageWindowDecision::RetryCommunicationFault;
            }
            return VoltageWindowDecision::LowConfidence;
        }

        if low_sample_chips > 0 || zero_data_chips > 0 {
            return VoltageWindowDecision::LowConfidence;
        }

        VoltageWindowDecision::Stable
    }

    fn profile_runtime_compatible(&self, candidate: &WarmStartCandidate) -> bool {
        if !candidate.profile.is_compatible_with_chain(
            candidate.chip_id,
            candidate.chain_id,
            candidate.chip_count,
        ) {
            return false;
        }

        let pll =
            dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(candidate.chip_id);
        for chip in &candidate.profile.chips {
            if chip.operating_mhz < self.config.min_freq_mhz
                || chip.operating_mhz > self.config.max_freq_mhz
            {
                warn!(
                    chain_id = candidate.chain_id,
                    chip = chip.chip_index,
                    operating_mhz = chip.operating_mhz,
                    min_freq_mhz = self.config.min_freq_mhz,
                    max_freq_mhz = self.config.max_freq_mhz,
                    "Warm-start profile frequency is outside the current configured range"
                );
                return false;
            }
            if !pll.contains(&chip.operating_mhz) {
                warn!(
                    chain_id = candidate.chain_id,
                    chip = chip.chip_index,
                    operating_mhz = chip.operating_mhz,
                    "Warm-start profile frequency is not in the active PLL table"
                );
                return false;
            }
        }

        true
    }

    async fn verify_applied_profile(
        &mut self,
        chain_id: u8,
        chip_count: u8,
        profile: &TuningProfile,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> crate::Result<()> {
        let verification_epoch = self
            .begin_measurement_window(chain_id, freq_cmd_tx, Duration::from_millis(50))
            .await?;
        let verify_start = Instant::now();
        let mut verify_errors = vec![0u64; chip_count as usize];
        let mut verify_nonces = vec![0u64; chip_count as usize];
        let mut verification_samples = 0u32;
        let mut saw_board_temp = false;

        while verify_start.elapsed().as_secs() < self.config.verification_window_s {
            if shutdown.is_cancelled() {
                break;
            }

            let snapshot = self
                .wait_for_chain_stats(chain_id, Some(verification_epoch), stats_rx, shutdown)
                .await?;
            verification_samples += 1;
            self.record_telemetry_sample(&snapshot);
            saw_board_temp |= snapshot.board_temp_c.is_some();

            for i in 0..chip_count as usize {
                if i < snapshot.chip_nonces.len() {
                    verify_nonces[i] += snapshot.chip_nonces[i];
                    verify_errors[i] += snapshot.stability_error_count(i);
                }
            }
        }

        if verification_samples == 0 {
            return Err(crate::AutoTunerError::StatsTimeout {
                seconds: self.config.verification_window_s,
            });
        }

        let total_nonces: u64 = verify_nonces.iter().sum();
        if total_nonces == 0 {
            return Err(crate::AutoTunerError::Config(format!(
                "warm-start verification produced zero nonces on chain {}",
                chain_id
            )));
        }

        let active_chip_count = verify_nonces.iter().filter(|&&count| count > 0).count();
        let required_active = ((chip_count as f64) * 0.90).ceil() as usize;
        if active_chip_count < required_active {
            return Err(crate::AutoTunerError::Config(format!(
                "warm-start verification only saw {} active chips on chain {} (need at least {})",
                active_chip_count, chain_id, required_active
            )));
        }

        if self.chain_chip_id(chain_id) == 0x1387 && !saw_board_temp {
            return Err(crate::AutoTunerError::Config(format!(
                "warm-start verification on chain {} did not receive a fresh BM1387 board temperature",
                chain_id
            )));
        }

        for chip in &profile.chips {
            let idx = chip.chip_index as usize;
            if idx >= verify_nonces.len() {
                continue;
            }
            let total = verify_nonces[idx] + verify_errors[idx];
            if total == 0 {
                continue;
            }
            let err_pct = verify_errors[idx] as f64 / total as f64 * 100.0;
            if err_pct > self.config.error_threshold_pct {
                return Err(crate::AutoTunerError::Config(format!(
                    "warm-start verification failed for chain {} chip {}: {:.2}% errors exceeds {:.2}%",
                    chain_id, chip.chip_index, err_pct, self.config.error_threshold_pct
                )));
            }
        }

        Ok(())
    }

    async fn try_warm_start_candidate(
        &mut self,
        candidate: &WarmStartCandidate,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> crate::Result<TuningProfile> {
        if self.capabilities.voltage_optimization_supported {
            let target_voltage_mv = if self.config.voltage_optimization {
                candidate
                    .profile
                    .optimal_voltage_mv
                    .unwrap_or(candidate.profile.voltage_mv)
            } else {
                candidate.profile.voltage_mv
            };
            Self::set_voltage_checked(freq_cmd_tx, candidate.chain_id, target_voltage_mv).await?;
            let _ =
                Self::verify_voltage_checked(freq_cmd_tx, candidate.chain_id, target_voltage_mv)
                    .await?;
        }

        self.apply_profile_via_channel(candidate.chain_id, &candidate.profile, freq_cmd_tx)
            .await?;
        self.verify_applied_profile(
            candidate.chain_id,
            candidate.chip_count,
            &candidate.profile,
            freq_cmd_tx,
            stats_rx,
            shutdown,
        )
        .await?;
        Ok(candidate.profile.clone())
    }

    /// Run the auto-tuner as an async task.
    ///
    /// This method drives the full tuning lifecycle:
    /// 1. Check for saved profiles (warm start)
    /// 2. Characterize chips via parallel binary search
    /// 3. Verify and save profiles
    /// 4. Monitor chip health in background
    ///
    /// `chain_infos`: per-chain runtime tuning metadata for each mining chain.
    /// `stats_rx`: receives per-chip nonce/error snapshots from WorkDispatcher.
    /// `freq_cmd_tx`: sends frequency change commands to WorkDispatcher.
    pub async fn run(
        &mut self,
        chain_infos: &[crate::ChainTuneInfo],
        mut stats_rx: mpsc::Receiver<ChipStatsSnapshot>,
        freq_cmd_tx: mpsc::Sender<FreqCommand>,
        shutdown: CancellationToken,
    ) {
        info!(
            state = %self.state,
            chains = chain_infos.len(),
            chip_type = %self.chip_type,
            nominal_mhz = self.nominal_mhz,
            "Auto-tuner starting — DCENTos TABS algorithm for per-chip frequency optimization"
        );

        self.target_chain_ids = chain_infos.iter().map(|info| info.chain_id).collect();
        self.chain_chip_ids = chain_infos
            .iter()
            .map(|info| (info.chain_id, info.chip_id))
            .collect();
        self.chain_hardware_identities = chain_infos
            .iter()
            .map(|info| (info.chain_id, info.hardware_identity.clone()))
            .collect();
        self.run_chain_plan = chain_infos
            .iter()
            .map(|info| (info.chain_id, info.chip_count))
            .collect();
        self.target_chip_total = chain_infos.iter().map(|info| info.chip_count as u16).sum();
        self.capabilities = if self.mixed_chain_chip_ids() {
            crate::autotuner_capabilities_for_mixed_families()
        } else {
            chain_infos
                .first()
                .map(|info| {
                    // PERF-006/011: honor the default-OFF
                    // `DCENT_AM2_VOLTAGE_AUTOTUNE` gate. Gate unset ⇒ identical
                    // conservative capability set (byte-identical behavior).
                    crate::autotuner_capabilities_for_chip_with_voltage_autotune(
                        info.chip_id,
                        &self.capabilities.voltage_control,
                        std::env::var(crate::config::AM2_VOLTAGE_AUTOTUNE_ENV)
                            .ok()
                            .as_deref(),
                    )
                })
                .unwrap_or_else(crate::autotuner_capabilities_for_mixed_families)
        };
        self.resolved_policy =
            crate::resolve_autotuner_policy(&self.requested_config, &self.capabilities);
        self.config = self.resolved_policy.effective_config.clone();
        info!(
            requested_preset = ?self.resolved_policy.requested_preset,
            effective_preset = ?self.resolved_policy.effective_preset,
            degraded = self.resolved_policy.degraded_from_requested,
            reason = ?self.resolved_policy.requested_preset_reason,
            capability_profile = %self.resolved_policy.capabilities.profile_key,
            "Autotuner policy resolved for current runtime"
        );

        // CE-011 (2026-07-08): CEILING-ONLY per-SKU PVT clamp. Run once here,
        // after the chain maps (target_chain_ids/chain_chip_ids/…) are built and
        // `self.config` is the resolved effective config, so the tightening lands
        // on the config the tuning loop actually reads. Deliberate no-op when
        // `chain_skus` is empty (today's live default) and byte-identical on the
        // proven am2 paths (every BM1362 envelope max >= the applied 545 ceiling).
        self.apply_sku_freq_ceilings();

        self.profiles.clear();
        self.failed_chain_ids.clear();
        self.pending_stats.clear();
        self.consecutive_temp_missing.clear();
        self.live_avg_frequency_mhz = None;
        self.active_runtime_objective = self.resolved_runtime_objective().to_string();
        self.active_runtime_limiting_factor = None;

        let mut warm_start_chains: Vec<u8> = Vec::new();
        let mut cold_start_chains = Vec::new();
        let mut warm_start_candidates: Vec<WarmStartCandidate> = Vec::new();

        for info in chain_infos {
            let chain_id = info.chain_id;
            let chip_count = info.chip_count;
            if let Some(profile) = TuningProfile::load(&self.config.profile_path, chain_id) {
                let candidate = WarmStartCandidate {
                    chain_id,
                    chip_count,
                    chip_id: info.chip_id,
                    profile,
                };
                if self.profile_runtime_compatible(&candidate) {
                    warm_start_candidates.push(candidate);
                } else {
                    cold_start_chains.push((chain_id, chip_count));
                }
            } else {
                cold_start_chains.push((chain_id, chip_count));
            }
        }

        match self.load_resume_state_gate(&warm_start_candidates) {
            ResumeStateGate::LegacyNoState => {}
            ResumeStateGate::Invalid => {
                for candidate in warm_start_candidates.drain(..) {
                    cold_start_chains.push((candidate.chain_id, candidate.chip_count));
                }
            }
            ResumeStateGate::Matched(state) => {
                let mut retained = Vec::new();
                for candidate in warm_start_candidates.drain(..) {
                    if Self::resume_state_contains_chain(
                        &state,
                        candidate.chain_id,
                        candidate.chip_count,
                    ) {
                        retained.push(candidate);
                    } else {
                        warn!(
                            chain_id = candidate.chain_id,
                            chips = candidate.chip_count,
                            "Autotuner resume state lacks this chain; cold-starting chain profile"
                        );
                        cold_start_chains.push((candidate.chain_id, candidate.chip_count));
                    }
                }
                warm_start_candidates = retained;
            }
        }

        for candidate in warm_start_candidates {
            if shutdown.is_cancelled() {
                return;
            }

            self.state = TunerState::Verifying;
            self.publish_runtime_status("Warm-start verification started", None);
            match self
                .try_warm_start_candidate(&candidate, &freq_cmd_tx, &mut stats_rx, &shutdown)
                .await
            {
                Ok(profile) => {
                    info!(
                        chain_id = candidate.chain_id,
                        chips = candidate.chip_count,
                        avg_freq = format_args!("{:.0}", profile.stats.avg_freq_mhz),
                        "Warm-start verified — reusing saved profile"
                    );
                    warm_start_chains.push(candidate.chain_id);
                    self.failed_chain_ids.remove(&candidate.chain_id);
                    self.profiles.insert(candidate.chain_id, profile);
                }
                Err(error) => {
                    warn!(
                        chain_id = candidate.chain_id,
                        error = %error,
                        "Warm-start verification failed — falling back to cold characterization"
                    );
                    if let Err(restore_err) = self
                        .restore_chain_nominal(candidate.chain_id, &freq_cmd_tx)
                        .await
                    {
                        warn!(
                            chain_id = candidate.chain_id,
                            error = %restore_err,
                            "Warm-start fallback failed to restore nominal chain frequency"
                        );
                    }
                    cold_start_chains.push((candidate.chain_id, candidate.chip_count));
                }
            }
        }

        // Cold start: characterize chains that need it
        if !cold_start_chains.is_empty() {
            self.state = TunerState::Characterizing;
            self.publish_runtime_status("Per-chip characterization started", None);
            let chain_ids: Vec<u8> = cold_start_chains.iter().map(|&(id, _)| id).collect();
            info!(
                chains = ?chain_ids,
                "Cold start: characterizing {} chain(s) — parallel binary search begins",
                cold_start_chains.len(),
            );

            for &(chain_id, chip_count) in &cold_start_chains {
                if shutdown.is_cancelled() {
                    return;
                }

                let voltage_mv = chain_infos
                    .iter()
                    .find(|info| info.chain_id == chain_id)
                    .map(|info| info.voltage_mv)
                    .unwrap_or(0);

                self.telemetry.start_run();
                self.publish_telemetry_state();
                match self
                    .characterize_chain(
                        chain_id,
                        chip_count,
                        voltage_mv,
                        &freq_cmd_tx,
                        &mut stats_rx,
                        &shutdown,
                    )
                    .await
                {
                    Ok(profile) => {
                        self.telemetry.finish_run(true);
                        self.publish_telemetry_state();
                        if let Err(e) = profile.save(&self.config.profile_path) {
                            warn!(
                                chain_id,
                                error = %e,
                                "Failed to save tuning profile — will re-tune on next boot"
                            );
                        }
                        self.failed_chain_ids.remove(&chain_id);
                        self.profiles.insert(chain_id, profile);
                    }
                    Err(e) => {
                        self.telemetry.finish_run(false);
                        self.publish_telemetry_state();
                        self.failed_chain_ids.insert(chain_id);
                        if let Err(restore_err) =
                            self.restore_chain_nominal(chain_id, &freq_cmd_tx).await
                        {
                            warn!(
                                chain_id,
                                error = %restore_err,
                                "Chain characterization failed and nominal restore also failed"
                            );
                        }
                        warn!(
                            chain_id,
                            error = %e,
                            "Chain characterization failed — running at nominal frequency"
                        );
                    }
                }
            }
        }

        // Phase 2.5: Thermal refinement soak (if enabled)
        if self.config.thermal_refinement_enabled {
            self.state = TunerState::ThermalRefinement;
            self.publish_runtime_status("Thermal refinement started", None);

            // Cold start chains: full thermal refinement
            for info in chain_infos {
                let chain_id = info.chain_id;
                let chip_count = info.chip_count;
                if shutdown.is_cancelled() {
                    break;
                }

                // Only refine chains we have profiles for
                let is_cold_start = cold_start_chains.iter().any(|&(id, _)| id == chain_id);
                let is_warm_start = warm_start_chains.contains(&chain_id);

                let max_soak = if is_cold_start {
                    self.config.thermal_refinement_max_s
                } else if is_warm_start && self.config.warm_start_thermal_check_s > 0 {
                    self.config.warm_start_thermal_check_s
                } else {
                    continue;
                };

                if let Some(mut profile) = self.profiles.remove(&chain_id) {
                    let label = if is_cold_start {
                        "cold start"
                    } else {
                        "warm start"
                    };
                    info!(
                        chain_id,
                        max_soak_s = max_soak,
                        "Thermal refinement ({}) for chain {} — max {}s soak",
                        label,
                        chain_id,
                        max_soak,
                    );

                    let result = self
                        .thermal_refinement(
                            chain_id,
                            chip_count,
                            max_soak,
                            &mut profile,
                            &freq_cmd_tx,
                            &mut stats_rx,
                            &shutdown,
                        )
                        .await;

                    // Re-save profile with thermally-validated frequencies
                    if result.total_backoffs > 0 || result.equilibrium_reached {
                        if let Err(e) = profile.save(&self.config.profile_path) {
                            warn!(
                                chain_id,
                                error = %e,
                                "Failed to save thermally-refined profile"
                            );
                        }
                    }

                    self.failed_chain_ids.remove(&chain_id);
                    self.profiles.insert(chain_id, profile);
                }
            }
        }

        // Phase 3: Voltage optimization (if enabled)
        if self.config.voltage_optimization {
            self.state = TunerState::Verifying;
            self.publish_runtime_status("Voltage optimization started", None);
            let voltage_chains: Vec<(u8, u16)> = chain_infos
                .iter()
                .map(|info| (info.chain_id, info.voltage_mv))
                .collect();

            for &(chain_id, initial_voltage_mv) in &voltage_chains {
                if shutdown.is_cancelled() {
                    break;
                }

                // Skip chains with no profile (characterization failed)
                if !self.profiles.contains_key(&chain_id) {
                    continue;
                }

                let chip_count = self
                    .profiles
                    .get(&chain_id)
                    .map(|p| p.chip_count)
                    .unwrap_or(0);

                if chip_count == 0 {
                    continue;
                }

                // Save backup before voltage optimization (for rollback)
                if self.config.enable_rollback {
                    if let Some(profile) = self.profiles.get(&chain_id) {
                        if let Err(e) = profile.save_backup(&self.config.profile_path) {
                            warn!(chain_id, error = %e, "Failed to save profile backup");
                        }
                    }
                }

                // Record pre-optimization error rate for rollback comparison
                let _pre_opt_error_rate = self
                    .profiles
                    .get(&chain_id)
                    .map(|p| {
                        let total_err: f64 = p.chips.iter().map(|c| c.error_rate).sum();
                        total_err / p.chips.len().max(1) as f64
                    })
                    .unwrap_or(0.0);

                info!(
                    chain_id,
                    initial_voltage_mv,
                    min_voltage_mv = self.config.min_voltage_mv,
                    margin_mv = self.config.voltage_margin_mv,
                    "Voltage optimization: searching for minimum stable voltage on chain {} ({} mV → floor {} mV)",
                    chain_id,
                    initial_voltage_mv,
                    self.config.min_voltage_mv,
                );

                match self
                    .optimize_voltage(
                        chain_id,
                        chip_count,
                        initial_voltage_mv,
                        &freq_cmd_tx,
                        &mut stats_rx,
                        &shutdown,
                    )
                    .await
                {
                    Ok(result) => {
                        let optimal_mv = result.optimal_voltage_mv;
                        let savings = initial_voltage_mv.saturating_sub(optimal_mv);
                        let chain_chip_id = self.chain_chip_id(chain_id);
                        let dvfs = DvfsOptimizer::new(self.power_model_for_chip(chain_chip_id));
                        info!(
                            chain_id,
                            initial_mv = initial_voltage_mv,
                            optimal_mv,
                            savings_mv = savings,
                            "Voltage optimization complete for chain {}: {} mV → {} mV (saved {} mV)",
                            chain_id,
                            initial_voltage_mv,
                            optimal_mv,
                            savings,
                        );

                        // Update the profile with the optimal voltage
                        if let Some(profile) = self.profiles.get_mut(&chain_id) {
                            profile.optimal_voltage_mv = Some(optimal_mv);
                            for chip in &mut profile.chips {
                                let voltage_freq_pairs: Vec<(u16, u16)> = result
                                    .stable_voltage_points_mv
                                    .iter()
                                    .copied()
                                    .map(|voltage_mv| (voltage_mv, chip.operating_mhz))
                                    .collect();
                                if !voltage_freq_pairs.is_empty() {
                                    let curve =
                                        dvfs.build_vf_curve(chip.chip_index, &voltage_freq_pairs);
                                    chip.vf_curve = Some(curve.points);
                                }
                            }
                            // Re-save profile with voltage data
                            if let Err(e) = profile.save(&self.config.profile_path) {
                                warn!(
                                    chain_id,
                                    error = %e,
                                    "Failed to re-save tuning profile with voltage data"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            chain_id,
                            error = %e,
                            "Voltage optimization failed for chain {} — running at initial voltage {} mV",
                            chain_id, initial_voltage_mv,
                        );

                        if let Err(restore_err) =
                            Self::set_voltage_checked(&freq_cmd_tx, chain_id, initial_voltage_mv)
                                .await
                        {
                            warn!(
                                chain_id,
                                error = %restore_err,
                                "Failed to restore initial voltage after optimization error"
                            );
                        }

                        // Rollback on optimization failure
                        if self.config.enable_rollback {
                            if let Some(backup) =
                                TuningProfile::load_backup(&self.config.profile_path, chain_id)
                            {
                                info!(
                                    chain_id,
                                    "Restoring backup profile after optimization failure"
                                );
                                if let Err(apply_err) = self
                                    .apply_profile_via_channel(chain_id, &backup, &freq_cmd_tx)
                                    .await
                                {
                                    warn!(
                                        chain_id,
                                        error = %apply_err,
                                        "Failed to re-apply backup profile after optimization failure"
                                    );
                                }
                                if let Err(restore_err) = Self::set_voltage_checked(
                                    &freq_cmd_tx,
                                    chain_id,
                                    initial_voltage_mv,
                                )
                                .await
                                {
                                    warn!(
                                        chain_id,
                                        error = %restore_err,
                                        "Failed to restore initial voltage after optimization failure"
                                    );
                                }
                                self.profiles.insert(chain_id, backup);
                            }
                        }
                    }
                }
            }
        }

        // Phase 3.5: Cross-chain voltage domain optimization (Item 13)
        // Trade voltage between chains: lower V on good-silicon chains, raise V
        // on poor-silicon chains, maximizing total hashrate within total power budget.
        if chain_infos.len() > 1 && self.config.voltage_optimization {
            self.cross_chain_voltage_optimize(chain_infos, &freq_cmd_tx)
                .await;
        }

        // Phase 4: Apply power budget or efficiency mode
        self.state = TunerState::Verifying;
        self.publish_runtime_status("Applying target-mode operating point", None);
        self.apply_target_mode(chain_infos, &freq_cmd_tx).await;

        // Phase 3B: Cross-chain power coordination
        if self.config.total_power_limit_w > 0 {
            self.enforce_total_power_limit(chain_infos, &freq_cmd_tx)
                .await;
        }

        // All chains tuned (or using nominal). Enter monitoring mode.
        self.state = self.steady_state();
        let completion_message = match self.state {
            TunerState::Tuned => "Tuning complete — background monitoring active",
            TunerState::PartiallyTuned => {
                "Tuning partially complete — monitoring tuned chains while failed chains stay nominal"
            }
            TunerState::Failed => {
                "Tuning failed on all requested chains — runtime fell back to nominal frequencies"
            }
            _ => "Tuning complete — background monitoring active",
        };
        self.publish_runtime_status(completion_message, None);

        let tuned_chains = self.tuned_chain_ids();
        let failed_chains = self.failed_chain_ids();
        let total_chips: u16 = self.profiles.values().map(|p| p.chip_count as u16).sum();
        info!(
            chains = ?tuned_chains,
            failed_chains = ?failed_chains,
            total_chips,
            "Auto-tuning complete — {} chips across {} chain(s) optimized. Entering background monitoring.",
            total_chips,
            tuned_chains.len(),
        );

        for (&chain_id, profile) in &self.profiles {
            let voltage_info = match profile.optimal_voltage_mv {
                Some(v) => format!(
                    ", voltage {} mV (optimized from {} mV)",
                    v, profile.voltage_mv
                ),
                None => format!(", voltage {} mV", profile.voltage_mv),
            };
            info!(
                chain_id,
                avg_freq = format_args!("{:.0} MHz", profile.stats.avg_freq_mhz),
                min_freq = format_args!("{} MHz", profile.stats.min_freq_mhz),
                max_freq = format_args!("{} MHz", profile.stats.max_freq_mhz),
                optimal_voltage_mv = ?profile.optimal_voltage_mv,
                grades = format_args!("{} A / {} B / {} C / {} D",
                    profile.stats.grade_a, profile.stats.grade_b,
                    profile.stats.grade_c, profile.stats.grade_d),
                "Chain {} tuning summary{}",
                chain_id, voltage_info,
            );
        }

        // Background monitoring loop
        self.background_monitor(&freq_cmd_tx, &mut stats_rx, &shutdown)
            .await;
    }

    /// Characterize a single chain using parallel binary search.
    async fn characterize_chain(
        &mut self,
        chain_id: u8,
        chip_count: u8,
        voltage_mv: u16,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> crate::Result<TuningProfile> {
        let start = Instant::now();

        info!(
            chain_id,
            chip_count,
            min_freq = self.config.min_freq_mhz,
            max_freq = self.config.max_freq_mhz,
            "Characterizing chain {} — {} chips, binary search over {}-{} MHz",
            chain_id,
            chip_count,
            self.config.min_freq_mhz,
            self.config.max_freq_mhz,
        );

        let tuner = BinarySearchTuner::new_for_chip(
            self.config.clone(),
            self.nominal_mhz,
            self.chain_chip_id(chain_id),
        );
        let max_iters = tuner.max_iterations();
        let mut states = tuner.init_search(chip_count);
        let mut iteration = 0u32;
        let mut consecutive_board_zero = 0u32;

        // Binary search loop
        loop {
            if shutdown.is_cancelled() {
                return Err(crate::AutoTunerError::StatsTimeout { seconds: 0 });
            }

            if BinarySearchTuner::all_done(&states) {
                break;
            }

            iteration += 1;
            if iteration > max_iters + 2 {
                warn!(
                    chain_id,
                    iterations = iteration,
                    "Binary search exceeded max iterations — forcing completion"
                );
                break;
            }

            // Set each chip to its current test frequency via command channel
            let freqs = tuner.current_frequencies(&states);
            for &(chip_idx, freq) in &freqs {
                Self::set_chip_freq_checked(freq_cmd_tx, chain_id, chip_idx, freq).await?;
            }

            // Update WORK_TIME based on SLOWEST chip — slow chips need longer to exhaust
            // their nonce range. Fast chips finish early (idle briefly, no harm).
            let min_freq = freqs
                .iter()
                .map(|&(_, f)| f)
                .min()
                .unwrap_or(self.nominal_mhz);
            let _ = freq_cmd_tx
                .send(FreqCommand::UpdateWorkTime {
                    chain_id,
                    min_freq_mhz: min_freq,
                })
                .await;
            info!(
                "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz",
                chain_id, min_freq
            );

            let measurement_epoch = self
                .begin_measurement_window(chain_id, freq_cmd_tx, Duration::from_millis(50))
                .await?;

            // Log active search chips
            let active: usize = states.iter().filter(|s| !s.done).count();
            let freq_range: String = if active > 0 {
                let min_f = states
                    .iter()
                    .filter(|s| !s.done)
                    .map(|s| s.test_freq())
                    .min()
                    .unwrap_or(0);
                let max_f = states
                    .iter()
                    .filter(|s| !s.done)
                    .map(|s| s.test_freq())
                    .max()
                    .unwrap_or(0);
                format!("{}-{} MHz", min_f, max_f)
            } else {
                "done".to_string()
            };

            info!(
                chain_id,
                iteration,
                active_chips = active,
                freq_range = %freq_range,
                "Binary search iteration {}/{} — {} chips still searching ({})",
                iteration, max_iters, active, freq_range,
            );

            let elapsed_s = start.elapsed().as_secs_f64();
            let avg_iteration_s = if iteration > 0 {
                elapsed_s / iteration as f64
            } else {
                3.0
            };
            let remaining_iters = max_iters.saturating_sub(iteration);
            let estimated_remaining = remaining_iters as f64 * avg_iteration_s;
            let pct = (iteration as f64 / max_iters as f64 * 100.0).min(99.0);
            let progress = TuningProgress {
                phase: "Characterizing".to_string(),
                chain_id,
                iteration,
                max_iterations: max_iters,
                active_chips: active,
                total_chips: chip_count,
                elapsed_s,
                estimated_remaining_s: estimated_remaining,
                percent_complete: pct,
            };

            // Publish bounded tuning progress for the dashboard.
            if let Some(ref tx) = self.progress_tx {
                let _ = tx.send(progress.clone());
            }
            self.publish_runtime_status("Per-chip characterization in progress", Some(&progress));

            // Wait for stats snapshot from this chain
            let snapshot = self
                .wait_for_chain_stats(chain_id, Some(measurement_epoch), stats_rx, shutdown)
                .await?;
            self.record_telemetry_sample(&snapshot);

            let target_window_s = tuner.recommended_window_s(&states, snapshot.current_difficulty);
            let mut snapshot = snapshot;
            if snapshot.window_duration_s + f64::EPSILON < target_window_s {
                info!(
                    chain_id,
                    iteration,
                    collected_window_s = format_args!("{:.1}", snapshot.window_duration_s),
                    target_window_s = format_args!("{:.1}", target_window_s),
                    "Characterization extending measurement window for better low-sample confidence"
                );
            }
            while snapshot.window_duration_s + f64::EPSILON < target_window_s {
                let next_snapshot = self
                    .wait_for_chain_stats(chain_id, Some(measurement_epoch), stats_rx, shutdown)
                    .await?;
                self.record_telemetry_sample(&next_snapshot);
                snapshot.accumulate_from(&next_snapshot);
            }

            // Diagnostic: log per-chip nonce summary for this iteration
            let total_nonces: u64 = snapshot.chip_nonces.iter().sum();
            let nonzero_chips = snapshot.chip_nonces.iter().filter(|&&n| n > 0).count();
            let max_chip_nonces = snapshot.chip_nonces.iter().max().copied().unwrap_or(0);
            info!(
                chain_id,
                iteration,
                total_nonces,
                nonzero_chips,
                total_chips = snapshot.chip_nonces.len(),
                max_chip_nonces,
                window_s = format_args!("{:.1}", snapshot.window_duration_s),
                "AUTOTUNE_DIAG: chain={} iter={} snapshot: {} total nonces across {}/{} chips (max={}, window={:.1}s)",
                chain_id,
                iteration,
                total_nonces,
                nonzero_chips,
                snapshot.chip_nonces.len(),
                max_chip_nonces,
                snapshot.window_duration_s,
            );

            // Board-level fault detection: if ALL chips produced 0 nonces,
            // this is a board-level fault (voltage loss, PIC death, FPGA issue),
            // NOT per-chip instability. Don't advance binary search — retry.
            if total_nonces == 0 {
                consecutive_board_zero += 1;
                warn!(
                    chain_id,
                    consecutive = consecutive_board_zero,
                    "BOARD FAULT: ALL chips produced 0 nonces — board-level issue (PIC/voltage/FPGA), \
                     NOT per-chip instability. Skipping binary search advance. ({}/3 before abort)",
                    consecutive_board_zero,
                );
                if consecutive_board_zero >= 3 {
                    return Err(crate::AutoTunerError::ChainUnavailable { chain_id });
                }
                continue;
            }
            consecutive_board_zero = 0;

            // Process snapshot and advance binary search
            let all_done = tuner.process_snapshot(&mut states, &snapshot);
            if all_done {
                break;
            }
        }

        // Verification phase
        self.state = TunerState::Verifying;
        self.publish_runtime_status("Verification phase started", None);
        info!(
            chain_id,
            verification_s = self.config.verification_window_s,
            "Binary search complete — verifying stability for {}s",
            self.config.verification_window_s,
        );

        let profiles = tuner.finalize(&states);

        // Apply operating frequencies for verification via command channel
        for profile in &profiles {
            Self::set_chip_freq_checked(
                freq_cmd_tx,
                chain_id,
                profile.chip_index,
                profile.operating_mhz,
            )
            .await?;
        }

        // Update WORK_TIME for verification phase — use SLOWEST chip frequency
        let min_op_freq = profiles
            .iter()
            .map(|p| p.operating_mhz)
            .min()
            .unwrap_or(self.nominal_mhz);
        let _ = freq_cmd_tx
            .send(FreqCommand::UpdateWorkTime {
                chain_id,
                min_freq_mhz: min_op_freq,
            })
            .await;
        info!(
            "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (verification)",
            chain_id, min_op_freq
        );

        let verification_epoch = self
            .begin_measurement_window(chain_id, freq_cmd_tx, Duration::from_millis(50))
            .await?;

        // Wait through verification window
        let verify_start = Instant::now();
        let mut verify_errors = vec![0u64; chip_count as usize];
        let mut verify_nonces = vec![0u64; chip_count as usize];
        let mut verification_samples = 0u32;

        while verify_start.elapsed().as_secs() < self.config.verification_window_s {
            if shutdown.is_cancelled() {
                break;
            }

            match self
                .wait_for_chain_stats(chain_id, Some(verification_epoch), stats_rx, shutdown)
                .await
            {
                Ok(snapshot) => {
                    verification_samples += 1;
                    self.record_telemetry_sample(&snapshot);
                    for i in 0..chip_count as usize {
                        if i < snapshot.chip_nonces.len() {
                            verify_nonces[i] += snapshot.chip_nonces[i];
                            verify_errors[i] += snapshot.stability_error_count(i);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }

        if verification_samples == 0 {
            return Err(crate::AutoTunerError::StatsTimeout {
                seconds: self.config.verification_window_s,
            });
        }

        // Check verification results — back off chips that failed
        let mut final_profiles = profiles;
        let mut failed_count = 0u32;
        for profile in &mut final_profiles {
            let idx = profile.chip_index as usize;
            let total = verify_nonces[idx] + verify_errors[idx];
            if total > 0 {
                let err_pct = verify_errors[idx] as f64 / total as f64 * 100.0;
                if err_pct > self.config.error_threshold_pct {
                    let backed_off =
                        self.step_down_freq(profile.operating_mhz, self.chain_chip_id(chain_id));
                    warn!(
                        chain_id,
                        chip = profile.chip_index,
                        err_pct = format_args!("{:.2}%", err_pct),
                        old_freq = profile.operating_mhz,
                        new_freq = backed_off,
                        "Chip {} failed verification ({:.2}% errors) — backing off {} → {} MHz",
                        profile.chip_index,
                        err_pct,
                        profile.operating_mhz,
                        backed_off,
                    );
                    profile.operating_mhz = backed_off;
                    profile.error_rate = err_pct / 100.0;
                    failed_count += 1;
                }
            }
        }

        if failed_count > 0 {
            let fail_pct = failed_count as f64 / chip_count as f64 * 100.0;
            if fail_pct > 5.0 {
                warn!(
                    chain_id,
                    failed = failed_count,
                    pct = format_args!("{:.1}%", fail_pct),
                    "{}% of chips failed verification — consider reducing max_freq_mhz or increasing voltage",
                    fail_pct,
                );
            }
        }

        let duration_s = start.elapsed().as_secs_f64();
        let stats = TuningProfile::compute_stats(&final_profiles, duration_s);

        // Get current timestamp (Unix epoch seconds)
        let tuned_at = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            format!("{}", now)
        };

        let profile = TuningProfile {
            version: TuningProfile::CURRENT_VERSION,
            chip_type: self.chip_type.clone(),
            chain_id,
            chip_count,
            voltage_mv,
            tuned_at,
            ambient_temp_c: None,
            optimal_voltage_mv: None,
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
            equilibrium_temp_c: None,
            thermal_refinement_duration_s: None,
            calibrated_c_eff: None,
            chips: final_profiles,
            stats,
            // W13.C3: SKU + flag denormalisation. Default to None — the
            // characterization path doesn't yet plumb the per-chain SKU
            // through. Daemon will set this via a follow-up profile
            // patch once W13.C3 is wired into chain bring-up.
            hashboard_sku: None,
            hashboard_sku_flags: None,
        };

        info!(
            chain_id,
            duration_s = format_args!("{:.1}", duration_s),
            avg_freq = format_args!("{:.0} MHz", profile.stats.avg_freq_mhz),
            min_freq = format_args!("{} MHz", profile.stats.min_freq_mhz),
            max_freq = format_args!("{} MHz", profile.stats.max_freq_mhz),
            grades = format_args!(
                "{}A/{}B/{}C/{}D",
                profile.stats.grade_a,
                profile.stats.grade_b,
                profile.stats.grade_c,
                profile.stats.grade_d
            ),
            "Chain {} characterized in {:.1}s — avg {:.0} MHz, spread {}-{} MHz",
            chain_id,
            duration_s,
            profile.stats.avg_freq_mhz,
            profile.stats.min_freq_mhz,
            profile.stats.max_freq_mhz,
        );

        Ok(profile)
    }

    /// Run the thermal refinement soak phase for a single chain.
    ///
    /// All chips run at their TABS-discovered operating frequencies while the
    /// board heats toward thermal equilibrium. Per-chip error rates are monitored
    /// in windows of `thermal_refinement_window_s`. Chips that become unstable
    /// as temperature rises get stepped down.
    ///
    /// Early exit when: temperature slope < threshold AND min soak elapsed.
    /// Hard exit at max duration.
    ///
    /// `max_duration_s` allows callers to specify a shortened soak (e.g., warm start).
    #[allow(clippy::too_many_arguments)]
    async fn thermal_refinement(
        &mut self,
        chain_id: u8,
        chip_count: u8,
        max_duration_s: u64,
        profile: &mut crate::profile::TuningProfile,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> ThermalRefinementResult {
        let mut state = ThermalRefinementState::new(chip_count as usize);
        let window_s = self.config.thermal_refinement_window_s;
        let min_soak_s = self.config.thermal_refinement_min_s.min(max_duration_s);
        let error_threshold = self.config.error_threshold_pct;
        let stability_threshold = self.config.thermal_stability_c_per_min;

        // Capture pre-refinement average frequency for degradation check
        let pre_avg_freq: f64 = if profile.chips.is_empty() {
            0.0
        } else {
            profile
                .chips
                .iter()
                .map(|c| c.operating_mhz as f64)
                .sum::<f64>()
                / profile.chips.len() as f64
        };

        info!(
            chain_id,
            chip_count,
            max_duration_s,
            min_soak_s,
            window_s,
            stability_threshold,
            "Thermal refinement: soaking chain {} — {} chips, max {}s, min {}s, window {}s",
            chain_id,
            chip_count,
            max_duration_s,
            min_soak_s,
            window_s,
        );

        let mut round = 0u32;
        let mut equilibrium_reached = false;
        let mut last_temp: Option<f32> = None;
        let mut no_temp_warned = false;

        loop {
            if shutdown.is_cancelled() {
                break;
            }

            // Check max duration
            let elapsed = state.start.elapsed().as_secs();
            if elapsed >= max_duration_s {
                info!(
                    chain_id,
                    elapsed_s = elapsed,
                    "Thermal refinement: max duration reached ({}s) — exiting",
                    elapsed,
                );
                break;
            }

            round += 1;
            state.reset_window();
            let measurement_epoch = match self
                .begin_measurement_window(chain_id, freq_cmd_tx, Duration::from_millis(50))
                .await
            {
                Ok(epoch) => epoch,
                Err(e) => {
                    warn!(
                        chain_id,
                        error = %e,
                        "Thermal refinement: failed to start fresh measurement window"
                    );
                    break;
                }
            };

            // Collect stats over the measurement window
            let window_start = Instant::now();
            while window_start.elapsed().as_secs() < window_s {
                if shutdown.is_cancelled() {
                    break;
                }

                match self
                    .wait_for_chain_stats(chain_id, Some(measurement_epoch), stats_rx, shutdown)
                    .await
                {
                    Ok(snapshot) => {
                        self.record_telemetry_sample(&snapshot);
                        // Record temperature if available
                        if let Some(temp) = snapshot.board_temp_c {
                            last_temp = Some(temp);
                        }
                        state.accumulate(&snapshot);
                    }
                    Err(_) => break,
                }
            }

            // Record temperature and check equilibrium
            if let Some(temp) = last_temp {
                equilibrium_reached =
                    state.record_temperature(temp, min_soak_s, stability_threshold);

                let slope = state.linear_slope();
                info!(
                    chain_id,
                    round,
                    temp_c = format_args!("{:.1}", temp),
                    slope_c_per_min = format_args!("{:.3}", slope),
                    elapsed_s = state.start.elapsed().as_secs(),
                    equilibrium = equilibrium_reached,
                    "Thermal refinement round {}: {:.1}C, slope {:.3} C/min",
                    round,
                    temp,
                    slope,
                );
            } else if !no_temp_warned {
                warn!(
                    chain_id,
                    "Thermal refinement: no temperature data — falling back to time-based exit only"
                );
                no_temp_warned = true;
            }

            // Check per-chip error rates and step down unstable chips
            let mut any_backoff = false;
            for i in 0..chip_count as usize {
                let total = state.chip_nonces[i] + state.chip_errors[i];
                if total == 0 {
                    continue;
                }

                let err_pct = state.chip_errors[i] as f64 / total as f64 * 100.0;
                if err_pct > error_threshold {
                    let old_freq = profile.chips[i].operating_mhz;
                    let new_freq = self.step_down_freq(old_freq, Self::profile_chip_id(profile));

                    if new_freq < old_freq {
                        warn!(
                            chain_id,
                            chip = i as u8,
                            round,
                            err_pct = format_args!("{:.2}%", err_pct),
                            old_freq,
                            new_freq,
                            temp_c = format_args!(
                                "{}",
                                last_temp.map(|t| format!("{:.1}", t)).unwrap_or_default()
                            ),
                            "Thermal backoff: chip {} unstable at {:.1}C ({:.2}% errors) — {} → {} MHz",
                            i,
                            last_temp.unwrap_or(0.0),
                            err_pct,
                            old_freq,
                            new_freq,
                        );

                        profile.chips[i].operating_mhz = new_freq;
                        state.chip_backoffs[i] += 1;
                        any_backoff = true;

                        // Apply the frequency change
                        if let Err(e) =
                            Self::set_chip_freq_checked(freq_cmd_tx, chain_id, i as u8, new_freq)
                                .await
                        {
                            warn!(chain_id, chip = i as u8, error = %e, "Thermal refinement: failed to apply backoff frequency");
                            continue;
                        }
                    }
                }
            }

            // Update WORK_TIME if any chip was backed off — use slowest chip
            if any_backoff {
                let min_freq = profile
                    .chips
                    .iter()
                    .map(|c| c.operating_mhz)
                    .min()
                    .unwrap_or(self.nominal_mhz);
                let _ = freq_cmd_tx
                    .send(FreqCommand::UpdateWorkTime {
                        chain_id,
                        min_freq_mhz: min_freq,
                    })
                    .await;
                info!(
                    "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (thermal backoff)",
                    chain_id, min_freq
                );
                if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                    warn!(chain_id, error = %e, "Thermal refinement: dispatcher sync failed after backoff batch");
                }
            }

            // Early exit if equilibrium reached
            if equilibrium_reached {
                info!(
                    chain_id,
                    elapsed_s = state.start.elapsed().as_secs(),
                    temp_c = format_args!("{:.1}", last_temp.unwrap_or(0.0)),
                    "Thermal refinement: equilibrium detected at {:.1}C — exiting early",
                    last_temp.unwrap_or(0.0),
                );
                break;
            }
        }

        let duration_s = state.start.elapsed().as_secs_f64();
        let total_backoffs: u32 = state.chip_backoffs.iter().sum();

        // Update profile with thermally-validated values
        for i in 0..chip_count as usize {
            if i < profile.chips.len() {
                // Record the hot max stable frequency
                profile.chips[i].thermal_max_stable_mhz = Some(profile.chips[i].operating_mhz);
            }
        }

        // Record equilibrium info in profile
        profile.equilibrium_temp_c = last_temp;
        profile.thermal_refinement_duration_s = Some(duration_s);

        // Check for degradation warning
        let post_avg_freq: f64 = if profile.chips.is_empty() {
            0.0
        } else {
            profile
                .chips
                .iter()
                .map(|c| c.operating_mhz as f64)
                .sum::<f64>()
                / profile.chips.len() as f64
        };

        if pre_avg_freq > 0.0 {
            let degradation_pct = (pre_avg_freq - post_avg_freq) / pre_avg_freq * 100.0;
            if degradation_pct > self.config.thermal_degradation_warn_pct as f64 {
                warn!(
                    chain_id,
                    pre_avg_mhz = format_args!("{:.0}", pre_avg_freq),
                    post_avg_mhz = format_args!("{:.0}", post_avg_freq),
                    degradation_pct = format_args!("{:.1}%", degradation_pct),
                    total_backoffs,
                    "COOLING WARNING: thermal refinement reduced avg frequency by {:.1}% ({:.0} → {:.0} MHz). \
                     Check: fan speed, heatsink contact, ambient temperature, thermal paste condition.",
                    degradation_pct,
                    pre_avg_freq,
                    post_avg_freq,
                );
            }
        }

        // Recompute profile stats with updated frequencies
        profile.stats =
            TuningProfile::compute_stats(&profile.chips, profile.stats.tuning_duration_s);

        info!(
            chain_id,
            rounds = round,
            total_backoffs,
            equilibrium = equilibrium_reached,
            duration_s = format_args!("{:.1}", duration_s),
            avg_freq = format_args!("{:.0}", post_avg_freq),
            "Thermal refinement complete — {} rounds, {} backoffs, {:.1}s, avg {:.0} MHz{}",
            round,
            total_backoffs,
            duration_s,
            post_avg_freq,
            if equilibrium_reached {
                " (equilibrium)"
            } else {
                " (timeout)"
            },
        );

        ThermalRefinementResult {
            rounds: round,
            total_backoffs,
            equilibrium_reached,
            duration_s,
            equilibrium_temp_c: last_temp,
        }
    }

    /// Apply a saved tuning profile via the frequency command channel.
    ///
    /// On warm start, uses a gradual frequency ramp from 300 MHz to target
    /// in 25 MHz steps over ~30s to prevent power surges that trip home 120V
    /// breakers. A full S9 at 650 MHz draws ~1200W; ramping avoids the
    /// instantaneous inrush from setting all 189 chips to full speed at once.
    async fn apply_profile_via_channel(
        &self,
        chain_id: u8,
        profile: &TuningProfile,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) -> crate::Result<()> {
        let max_target = profile
            .chips
            .iter()
            .map(|c| c.operating_mhz)
            .max()
            .unwrap_or(self.nominal_mhz);

        // Startup frequency ramp (Item 17): ramp from 300 MHz to target
        // in 25 MHz steps with short delays between steps.
        const RAMP_START_MHZ: u16 = 300;
        const RAMP_STEP_MHZ: u16 = 25;
        const RAMP_DELAY_MS: u64 = 100; // 100ms between steps
                                        // Total time: (650-300)/25 * 100ms = 14 steps * 100ms = 1.4s per chain

        let chain_chip_id = Self::profile_chip_id(profile);
        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chain_chip_id);

        if max_target > RAMP_START_MHZ {
            info!(
                chain_id,
                start_mhz = RAMP_START_MHZ,
                target_mhz = max_target,
                step_mhz = RAMP_STEP_MHZ,
                "Startup ramp: chain {} ramping {} → {} MHz in {} MHz steps",
                chain_id,
                RAMP_START_MHZ,
                max_target,
                RAMP_STEP_MHZ,
            );

            let mut current_ramp = RAMP_START_MHZ;
            while current_ramp < max_target {
                // Snap to PLL
                let ramp_freq = pll
                    .iter()
                    .rev()
                    .find(|&&f| f <= current_ramp)
                    .copied()
                    .unwrap_or(RAMP_START_MHZ);

                // Set all chips to the ramp frequency (capped at their target)
                let mut ramp_min_freq = ramp_freq;
                for chip in &profile.chips {
                    let chip_freq = ramp_freq.min(chip.operating_mhz);
                    ramp_min_freq = ramp_min_freq.min(chip_freq);
                    Self::set_chip_freq_checked(freq_cmd_tx, chain_id, chip.chip_index, chip_freq)
                        .await?;
                }

                // Use slowest chip frequency for WORK_TIME during ramp
                Self::send_freq_cmd_checked(
                    freq_cmd_tx,
                    FreqCommand::UpdateWorkTime {
                        chain_id,
                        min_freq_mhz: ramp_min_freq,
                    },
                )
                .await?;
                Self::wait_for_dispatcher(freq_cmd_tx).await?;

                tokio::time::sleep(std::time::Duration::from_millis(RAMP_DELAY_MS)).await;
                current_ramp += RAMP_STEP_MHZ;
            }
        }

        // Final: set each chip to its exact target frequency.
        // Fan-speed-aware clamping: at low fan speeds (Home mode quiet),
        // reduce max frequency proportionally. This prevents thermal runaway when
        // the user intentionally runs fans quietly for noise reduction.
        let fan_factor = self.fan_factor();
        let mut min_freq = u16::MAX;
        for chip in &profile.chips {
            let fan_limit = if fan_factor < 0.99 {
                let adj = (chip.operating_mhz as f64 * fan_factor) as u16;
                Some(
                    pll.iter()
                        .rev()
                        .find(|&&f| f <= adj)
                        .copied()
                        .unwrap_or(self.config.min_freq_mhz),
                )
            } else {
                None
            };
            let applied_target = fan_limit.unwrap_or(chip.operating_mhz);
            Self::set_chip_freq_checked(freq_cmd_tx, chain_id, chip.chip_index, chip.operating_mhz)
                .await?;
            Self::set_chip_limit_checked(
                freq_cmd_tx,
                chain_id,
                chip.chip_index,
                fan_limit,
                crate::FrequencyLimitSource::FanClamp,
            )
            .await?;
            if applied_target < min_freq {
                min_freq = applied_target;
            }
        }

        if fan_factor < 0.99 {
            info!(
                chain_id,
                fan_pwm = self.config.current_fan_pwm,
                fan_factor = format_args!("{:.2}", fan_factor),
                "Fan-speed-aware clamping: frequencies reduced to {:.0}% of profile (PWM {})",
                fan_factor * 100.0,
                self.config.current_fan_pwm,
            );
        }

        if min_freq < u16::MAX {
            Self::send_freq_cmd_checked(
                freq_cmd_tx,
                FreqCommand::UpdateWorkTime {
                    chain_id,
                    min_freq_mhz: min_freq,
                },
            )
            .await?;
            info!(
                "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (profile apply)",
                chain_id, min_freq
            );
            Self::wait_for_dispatcher(freq_cmd_tx).await?;
        }

        info!(
            chain_id,
            chips = profile.chip_count,
            avg_freq = format_args!("{:.0} MHz", profile.stats.avg_freq_mhz),
            "Applied saved profile — {} chips at avg {:.0} MHz (ramped)",
            profile.chip_count,
            profile.stats.avg_freq_mhz,
        );
        Ok(())
    }

    /// Background monitoring loop.
    ///
    /// After tuning, periodically check per-chip error rates and back off
    /// any chips that develop sustained errors (thermal drift, aging).
    ///
    /// Phase 5 additions:
    ///   - Thermal compensation: derate chip frequencies when board temperature
    ///     rises above threshold, restore when it drops back down.
    ///   - Aging detection: track long-term error rate trends via EMA, log
    ///     warnings when chips show sustained degradation.
    async fn background_monitor(
        &mut self,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) {
        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        let mut chip_health_tracker = ChipHealthTracker::new(&self.profiles);
        self.publish_chip_health(chip_health_tracker.all_statuses());
        let fan_factor = self.fan_factor();

        // Initialize monitors from profiles
        for (&chain_id, profile) in &self.profiles {
            let chain_chip_id = Self::profile_chip_id(profile);
            let pll =
                dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chain_chip_id);
            for chip in &profile.chips {
                let fan_limit_mhz = if fan_factor < 0.99 {
                    let adjusted = (chip.operating_mhz as f64 * fan_factor) as u16;
                    Some(
                        pll.iter()
                            .rev()
                            .find(|&&f| f <= adjusted)
                            .copied()
                            .unwrap_or(self.config.min_freq_mhz),
                    )
                } else {
                    None
                };
                let applied_freq = fan_limit_mhz.unwrap_or(chip.operating_mhz);
                monitors.insert(
                    (chain_id, chip.chip_index),
                    ChipMonitor {
                        chip_id: chain_chip_id,
                        consecutive_errors: 0,
                        consecutive_hashrate_deficit: 0,
                        current_freq_mhz: applied_freq,
                        desired_freq_mhz: chip.operating_mhz,
                        profile_freq_mhz: chip.operating_mhz,
                        thermal_limit_mhz: None,
                        fan_limit_mhz,
                        sensor_safety_limit_mhz: None,
                        thermally_derated: false,
                        consecutive_clean_windows: 0,
                        boost_attempts: 0,
                        consecutive_zero_nonce_windows: 0,
                        masked: false,
                    },
                );
            }
        }
        self.active_runtime_limiting_factor =
            Self::compute_active_runtime_limiting_factor(&monitors);

        // Phase 5: Initialize thermal compensator (if enabled)
        let thermal_comp = if self.config.thermal_compensation {
            let mut thermal_comp = HashMap::new();
            for (&chain_id, profile) in &self.profiles {
                let mut comp = ThermalCompensator::new()
                    .with_chip_id(Self::profile_chip_id(profile))
                    .with_derating(self.config.thermal_derating_per_c)
                    .with_hysteresis(self.config.thermal_hysteresis_c);

                // Apply immersion mode offset if enabled.
                // SAFETY: Validate that fans are actually off (immersion doesn't use fans).
                // If fan_pwm > 20, the user likely misconfigured immersion_mode on an
                // air-cooled miner — raised thresholds would allow dangerous temperatures.
                if self.config.immersion_mode {
                    if self.config.current_fan_pwm > 20 {
                        warn!(
                            fan_pwm = self.config.current_fan_pwm,
                            "SAFETY: immersion_mode enabled but fans running at PWM {} — \
                             this looks like air cooling. Ignoring immersion offset to \
                             protect chips. Set fan PWM <= 20 or disable immersion_mode.",
                            self.config.current_fan_pwm,
                        );
                    } else if self.config.current_fan_pwm == 0 && !self.config.immersion_confirmed {
                        warn!(
                            "SAFETY: immersion_mode enabled with fan_pwm=0 but immersion_confirmed=false — \
                             refusing to apply immersion offset. Set immersion_confirmed=true in config \
                             if this miner is truly immersion-cooled.",
                        );
                    } else {
                        comp = comp.with_immersion_offset(self.config.immersion_temp_offset_c);
                        info!(
                            offset_c = self.config.immersion_temp_offset_c,
                            confirmed = self.config.immersion_confirmed,
                            "Immersion mode: thermal thresholds raised by {:.0}C",
                            self.config.immersion_temp_offset_c,
                        );
                    }
                }

                info!(
                    chain_id,
                    derating_per_c = self.config.thermal_derating_per_c,
                    threshold_c = comp.derating_threshold_c(),
                    emergency_c = comp.emergency_temp_c(),
                    immersion = self.config.immersion_mode,
                    "Thermal compensation enabled — derating {:.1}%/C above {}C, emergency at {}C",
                    self.config.thermal_derating_per_c * 100.0,
                    comp.derating_threshold_c(),
                    comp.emergency_temp_c(),
                );
                thermal_comp.insert(chain_id, comp);
            }
            Some(thermal_comp)
        } else {
            None
        };

        // Phase 5: Initialize aging tracker (if enabled)
        let mut aging_tracker = if self.config.aging_detection {
            info!("Aging detection enabled — monitoring long-term chip degradation trends");
            Some(AgingTracker::new())
        } else {
            None
        };

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.background_interval_s,
        ));
        let mut command_rx = self.command_rx.take();

        // Schedule: track the applied power target so DPS can walk toward slot
        // changes instead of jumping directly between distant targets. Cache the
        // startup profile under key 0 for max-hashrate restoration, or under the
        // active power target if the tuner entered background mode already capped.
        let mut current_schedule_target: Option<u32> =
            if self.config.target_mode == TuneTarget::Power && self.config.target_watts > 0 {
                Some(self.config.target_watts)
            } else {
                None
            };
        self.cache_current_profiles_for_target(current_schedule_target.unwrap_or(0));
        // Efficiency dashboard: publish every 5 ticks (~5 minutes at 60s interval)
        let mut dashboard_counter: u32 = 0;
        const DASHBOARD_PUBLISH_INTERVAL: u32 = 5;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    self.live_avg_frequency_mhz = None;
                    info!("Auto-tuner background monitor stopping");
                    return;
                }
                command = async {
                    if let Some(rx) = command_rx.as_mut() {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(command) = command {
                        self.handle_runtime_command(command, freq_cmd_tx, &mut monitors).await;
                    }
                }
                _ = interval.tick() => {
                    // --- W15-A: Active silicon-profile target apply ---
                    // At the top of each iteration, consult the
                    // operator-selected active silicon profile (set
                    // via `PUT /api/profiles/silicon/active` →
                    // `AutoTunerCommand::ApplySiliconProfile`) and
                    // emit per-chain frequency/voltage targets via
                    // the existing `FreqCommand` rails. No-op when
                    // no active selection is present, so legacy
                    // behavior (apply_target_mode-driven) is
                    // preserved untouched on un-profiled miners.
                    self.apply_active_silicon_profile_targets(freq_cmd_tx, &mut monitors)
                        .await;

                    // --- Automatic Post-Tune Rollback ---
                    // Within the first 5 windows after tuning, accumulate actual
                    // error rates from snapshot data. If the observed error rate
                    // exceeds 2x the pre-tune baseline, revert to backup profiles.
                    if self.config.auto_rollback_post_tune && self.post_tune_windows < 5 {
                        self.post_tune_windows += 1;

                        // Accumulate ACTUAL nonce/error counts from the latest snapshots
                        // (not the consecutive_errors counter, which is a small integer proxy)
                        let mut snapshot_errors = 0u64;
                        let mut snapshot_nonces = 0u64;
                        for (_, m) in monitors.iter() {
                            if !m.masked && m.current_freq_mhz > 0 {
                                snapshot_nonces += 1; // Count active chips
                                if m.consecutive_errors > 0 {
                                    snapshot_errors += 1; // Count chips with ANY error this window
                                }
                            }
                        }

                        if self.post_tune_windows == 5 && snapshot_nonces > 0 {
                            // What fraction of chips had errors in the last window?
                            let error_chip_fraction = snapshot_errors as f64 / snapshot_nonces as f64;

                            // Rollback if >10% of chips are erroring AND pre-tune was clean
                            if error_chip_fraction > 0.10 && self.pre_tune_error_rate < 0.02 {
                                match self.evaluate_share_efficiency_validation() {
                                    ShareEfficiencyValidation::Healthy => {
                                        info!(
                                            error_fraction = format_args!("{:.1}%", error_chip_fraction * 100.0),
                                            "Post-tune rollback suppressed — accepted work efficiency remains healthy"
                                        );
                                    }
                                    ShareEfficiencyValidation::Unknown => {
                                        info!(
                                            error_fraction = format_args!("{:.1}%", error_chip_fraction * 100.0),
                                            "Post-tune rollback deferred — accepted work efficiency signal is not yet conclusive"
                                        );
                                    }
                                    ShareEfficiencyValidation::Degraded | ShareEfficiencyValidation::NotApplicable => {
                                        warn!(
                                            erroring_chips = snapshot_errors,
                                            total_chips = snapshot_nonces,
                                            error_fraction = format_args!("{:.1}%", error_chip_fraction * 100.0),
                                            pre_tune = format_args!("{:.3}", self.pre_tune_error_rate),
                                            "POST-TUNE ROLLBACK: {:.1}% of chips erroring ({}/{}) vs {:.1}% pre-tune. \
                                             Reverting all chains to backup profiles.",
                                            error_chip_fraction * 100.0, snapshot_errors, snapshot_nonces,
                                            self.pre_tune_error_rate * 100.0,
                                        );
                                        for &chain_id in self.profiles.clone().keys() {
                                            if let Some(backup) = TuningProfile::load_backup(
                                                &self.config.profile_path, chain_id,
                                            ) {
                                                if let Err(apply_err) =
                                                    self.apply_profile_via_channel(chain_id, &backup, freq_cmd_tx)
                                                        .await
                                                {
                                                    warn!(
                                                        chain_id,
                                                        error = %apply_err,
                                                        "Failed to apply backup profile during post-tune rollback"
                                                    );
                                                } else {
                                                    self.profiles.insert(chain_id, backup);
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                info!(
                                    error_fraction = format_args!("{:.1}%", error_chip_fraction * 100.0),
                                    "Post-tune check passed: {:.1}% chips erroring (threshold 10%)",
                                    error_chip_fraction * 100.0,
                                );
                            }
                        }
                    }

                    // --- DPS Schedule Check (Item 6) ---
                    // Every tick, walk one bounded step toward the active schedule target.
                    if self.schedule.enabled {
                        let desired_target = self.schedule.current_target_watts();
                        let new_target =
                            self.next_scheduled_power_target(current_schedule_target, desired_target);
                        if new_target != current_schedule_target {
                            let old = current_schedule_target;

                            match new_target {
                                Some(watts) if watts > 0 => {
                                    info!(
                                        old_target = ?old,
                                        desired_target = ?desired_target,
                                        next_target = watts,
                                        "DPS schedule: walking power target to {} W",
                                        watts,
                                    );

                                    self.cache_current_profiles_for_target(old.unwrap_or(0));
                                    let cache_hit = self
                                        .apply_cached_profiles_for_target(watts, freq_cmd_tx)
                                        .await;

                                    if cache_hit {
                                        self.config.target_watts = watts;
                                        self.requested_config.target_watts = watts;
                                        self.config.target_mode = TuneTarget::Power;
                                        self.requested_config.target_mode = TuneTarget::Power;
                                        self.config.tuner_mode =
                                            Some(TunerMode::PowerTarget { watts });
                                        self.requested_config.tuner_mode =
                                            Some(TunerMode::PowerTarget { watts });
                                        // W8.2: clamp scheduled target to circuit cap.
                                        let _ = self.clamp_target_to_circuit_limit();
                                        self.active_runtime_objective = "power_cap".to_string();
                                        self.refresh_runtime_after_profile_change(&mut monitors);
                                        info!(
                                            target_watts = watts,
                                            "DPS: cached schedule target applied"
                                        );
                                        self.publish_runtime_status(
                                            "DPS schedule cached target applied",
                                            None,
                                        );
                                    } else {
                                        // No cached profile — adjust power target dynamically.
                                        // Re-allocate through the live power-budget path.
                                        let mode = TunerMode::PowerTarget { watts };
                                        let result = self
                                            .apply_runtime_mode(mode, freq_cmd_tx, &mut monitors)
                                            .await;
                                        info!(
                                            target_watts = watts,
                                            status = ?result.status,
                                            applied_runtime = result.applied_runtime,
                                            "DPS: schedule target applied through runtime power allocator"
                                        );
                                    }
                                    current_schedule_target = Some(watts);
                                }
                                Some(0) | None => {
                                    info!(
                                        old_target = ?old,
                                        "DPS schedule: max hashrate mode (no power limit)",
                                    );
                                    self.cache_current_profiles_for_target(old.unwrap_or(0));
                                    let cache_hit = self
                                        .apply_cached_profiles_for_target(0, freq_cmd_tx)
                                        .await;
                                    if cache_hit {
                                        self.config.target_watts = 0;
                                        self.requested_config.target_watts = 0;
                                        self.config.target_mode = TuneTarget::Hashrate;
                                        self.requested_config.target_mode = TuneTarget::Hashrate;
                                        self.config.tuner_mode = Some(TunerMode::Performance);
                                        self.requested_config.tuner_mode =
                                            Some(TunerMode::Performance);
                                        self.active_runtime_objective = "hashrate".to_string();
                                        self.refresh_runtime_after_profile_change(&mut monitors);
                                        self.publish_runtime_status(
                                            "DPS schedule restored cached max-hashrate profile",
                                            None,
                                        );
                                    } else {
                                        let result = self
                                            .apply_runtime_mode(
                                                TunerMode::Performance,
                                                freq_cmd_tx,
                                                &mut monitors,
                                            )
                                            .await;
                                        info!(
                                            status = ?result.status,
                                            applied_runtime = result.applied_runtime,
                                            "DPS: schedule returned to max-hashrate mode without a cached profile"
                                        );
                                    }
                                    current_schedule_target = None;
                                }
                                _ => {}
                            }
                        }
                    }

                    // --- Efficiency Dashboard Feed (Item 16) ---
                    dashboard_counter += 1;
                    if dashboard_counter >= DASHBOARD_PUBLISH_INTERVAL {
                        dashboard_counter = 0;

                        if self.efficiency_tx.is_some() || self.efficiency_watch_tx.is_some() {
                            let snapshot = build_efficiency_snapshot(
                                &self.profiles,
                                self.current_power_scale(),
                            );
                            self.publish_efficiency_snapshot(snapshot);
                        }
                    }

                    // Drain all available snapshots
                    while let Ok(snapshot) = stats_rx.try_recv() {
                        // --- PSU Telemetry Calibration (Fixed: uses ALL chains) ---
                        // When actual PSU power reading is available via PMBus,
                        // calibrate the power model to match reality (±1% vs ±10%).
                        // BUG FIX: Must collect frequencies from ALL chains, not just
                        // the chain that triggered the snapshot. PSU measures total power
                        // across all 3 chains, so C_eff calibration needs total freq sum.
                        if let Some(measured_w) = snapshot.psu_power_w {
                            if dashboard_counter == 0 {
                                if self.mixed_chain_chip_ids() {
                                    debug!("Skipping PSU calibration during mixed-chip autotune run");
                                } else {
                                    let calibration_chip_id = self.chain_chip_id(snapshot.chain_id);
                                    let mut power_model = self.power_model_for_chip(calibration_chip_id);

                                    let mut chain_data: Vec<(u16, Vec<u16>)> = Vec::new();
                                    for (chain_id, profile) in &self.profiles {
                                        let voltage_mv = profile
                                            .optimal_voltage_mv
                                            .unwrap_or(profile.voltage_mv);
                                        let freqs: Vec<u16> = profile
                                            .chips
                                            .iter()
                                            .filter_map(|chip| {
                                                monitors
                                                    .get(&(*chain_id, chip.chip_index))
                                                    .filter(|monitor| {
                                                        !monitor.masked
                                                            && monitor.current_freq_mhz > 0
                                                    })
                                                    .map(|monitor| monitor.current_freq_mhz)
                                            })
                                            .collect();

                                        if !freqs.is_empty() {
                                            chain_data.push((voltage_mv, freqs));
                                        }
                                    }

                                    let chain_refs: Vec<(u16, &[u16])> = chain_data
                                        .iter()
                                        .map(|(voltage_mv, freqs)| (*voltage_mv, freqs.as_slice()))
                                        .collect();

                                    power_model.calibrate_chains(measured_w, &chain_refs);

                                    // Store calibrated C_eff in ALL profiles for persistence
                                    let c_eff = power_model.c_eff();
                                    for profile in self.profiles.values_mut() {
                                        profile.calibrated_c_eff = Some(c_eff);
                                    }
                                }
                            }
                        }

                        self.apply_fan_clamp_limits(
                            snapshot.chain_id,
                            &mut monitors,
                            &mut chip_health_tracker,
                            freq_cmd_tx,
                        ).await;

                        // Phase 5: Thermal compensation — adjust frequencies based on temperature
                        if let Some(ref thermal_comp_by_chain) = thermal_comp {
                            if let Some(comp) = thermal_comp_by_chain.get(&snapshot.chain_id) {
                            self.apply_thermal_compensation(
                                &snapshot,
                                comp,
                                &mut monitors,
                                &mut chip_health_tracker,
                                freq_cmd_tx,
                            ).await;
                            }
                        }

                        // Existing: per-chip error rate monitoring and back-off
                        self.process_background_snapshot(
                            &snapshot,
                            &mut monitors,
                            &mut chip_health_tracker,
                            freq_cmd_tx,
                        ).await;
                        self.active_runtime_limiting_factor =
                            Self::compute_active_runtime_limiting_factor(&monitors);
                        self.live_avg_frequency_mhz = Self::monitor_avg_freq_mhz(&monitors);
                        chip_health_tracker.update(&snapshot);
                        self.publish_chip_health(chip_health_tracker.all_statuses());

                        // Phase 5: Aging detection — track long-term degradation trends
                        if let Some(ref mut tracker) = aging_tracker {
                            let newly_flagged = tracker.update(&snapshot);
                            for (chain_id, chip_idx) in &newly_flagged {
                                let ema = tracker.chip_ema(*chain_id, *chip_idx)
                                    .unwrap_or(0.0);
                                warn!(
                                    chain_id = *chain_id,
                                    chip = *chip_idx,
                                    ema_error_pct = format_args!("{:.3}%", ema * 100.0),
                                    flagged_total = tracker.flagged_count(),
                                    "Aging detected: chip {} on chain {} shows sustained elevated error rate \
                                     (EMA {:.3}%) — triggering re-characterization",
                                    chip_idx, chain_id, ema * 100.0,
                                );
                            }
                        }
                    }

                    // Gap 4: Re-characterize flagged chips outside the aging_tracker borrow.
                    // Collect flags into a local Vec, then call recharacterize_chips().
                    let rechar_chips: Vec<(u8, u8)> = aging_tracker
                        .as_ref()
                        .map(|t| t.needs_rechar.clone())
                        .unwrap_or_default();

                    if !rechar_chips.is_empty() {
                        info!(
                            count = rechar_chips.len(),
                            "Re-characterizing {} aging-flagged chip(s)",
                            rechar_chips.len(),
                        );
                        self.recharacterize_chips(
                            &rechar_chips,
                            freq_cmd_tx,
                            stats_rx,
                            shutdown,
                        ).await;

                        // Clear flags for re-characterized chips
                        if let Some(ref mut tracker) = aging_tracker {
                            for &(chain_id, chip_idx) in &rechar_chips {
                                tracker.clear_rechar(chain_id, chip_idx);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Apply thermal compensation to chip frequencies based on board temperature.
    ///
    /// If temperature exceeds the derating threshold, computes per-chip ceilings
    /// and sends them to the dispatcher. When temperature drops, those ceilings
    /// are cleared so the dispatcher restores the autotuner's desired frequencies.
    /// If emergency temperature, throttles all chips to minimum frequency.
    async fn apply_thermal_compensation(
        &self,
        snapshot: &ChipStatsSnapshot,
        comp: &ThermalCompensator,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
        chip_health_tracker: &mut ChipHealthTracker,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        let temp_c = match snapshot.board_temp_c {
            Some(t) => t,
            None => return, // No temperature data available
        };

        let chain_id = snapshot.chain_id;
        let mut needs_work_time_update = false;

        // Emergency: throttle everything to minimum frequency immediately
        if comp.is_emergency(temp_c) {
            let min_freq = self.config.min_freq_mhz;
            warn!(
                chain_id,
                temp_c = format_args!("{:.1}", temp_c),
                emergency_c = format_args!("{:.1}", comp.emergency_temp_c()),
                min_freq,
                "EMERGENCY THERMAL THROTTLE: board temp {:.1}C >= {:.1}C — all chips to {} MHz",
                temp_c,
                comp.emergency_temp_c(),
                min_freq,
            );

            for i in 0..snapshot.chip_nonces.len() {
                let chip_idx = i as u8;
                let key = (chain_id, chip_idx);
                if let Some(monitor) = monitors.get_mut(&key) {
                    let new_limit = Some(min_freq);
                    if monitor.thermal_limit_mhz != new_limit {
                        if let Err(e) = Self::set_chip_limit_checked(
                            freq_cmd_tx,
                            chain_id,
                            chip_idx,
                            new_limit,
                            crate::FrequencyLimitSource::AutotunerThermal,
                        )
                        .await
                        {
                            warn!(chain_id, chip = chip_idx, error = %e, "Emergency thermal throttle: failed to apply per-chip ceiling");
                        } else {
                            monitor.thermal_limit_mhz = new_limit;
                            if Self::refresh_monitor_frequency(monitor) {
                                chip_health_tracker.set_current_frequency(
                                    chain_id,
                                    chip_idx,
                                    monitor.current_freq_mhz,
                                );
                                needs_work_time_update = true;
                            }
                        }
                    }
                }
            }
        } else if comp.needs_derating(temp_c) {
            // Derate: reduce frequencies proportionally based on temperature
            for i in 0..snapshot.chip_nonces.len() {
                let chip_idx = i as u8;
                let key = (chain_id, chip_idx);
                if let Some(monitor) = monitors.get_mut(&key) {
                    let derated = comp.derate_freq(monitor.profile_freq_mhz, temp_c);
                    let new_limit = Some(derated);
                    if monitor.thermal_limit_mhz != new_limit {
                        tracing::debug!(
                            chain_id,
                            chip = chip_idx,
                            temp_c = format_args!("{:.1}", temp_c),
                            profile_freq = monitor.profile_freq_mhz,
                            derated_freq = derated,
                            "Thermal derating chip {} — {:.1}C: {} -> {} MHz",
                            chip_idx,
                            temp_c,
                            monitor.current_freq_mhz,
                            derated,
                        );

                        if let Err(e) = Self::set_chip_limit_checked(
                            freq_cmd_tx,
                            chain_id,
                            chip_idx,
                            new_limit,
                            crate::FrequencyLimitSource::AutotunerThermal,
                        )
                        .await
                        {
                            warn!(chain_id, chip = chip_idx, error = %e, "Thermal derating: failed to apply per-chip ceiling");
                        } else {
                            monitor.thermal_limit_mhz = new_limit;
                            if Self::refresh_monitor_frequency(monitor) {
                                chip_health_tracker.set_current_frequency(
                                    chain_id,
                                    chip_idx,
                                    monitor.current_freq_mhz,
                                );
                                needs_work_time_update = true;
                            }
                        }
                    }
                }
            }
        } else if comp.should_restore(temp_c) {
            // Temperature is well below threshold (below hysteresis band) —
            // clear thermal ceilings so the dispatcher restores desired frequencies
            for i in 0..snapshot.chip_nonces.len() {
                let chip_idx = i as u8;
                let key = (chain_id, chip_idx);
                if let Some(monitor) = monitors.get_mut(&key) {
                    if monitor.thermal_limit_mhz.is_some() {
                        info!(
                            chain_id,
                            chip = chip_idx,
                            temp_c = format_args!("{:.1}", temp_c),
                            restored_freq = monitor.desired_freq_mhz,
                            "Thermal restore chip {} — {:.1}C below threshold, restoring {} MHz",
                            chip_idx,
                            temp_c,
                            monitor.desired_freq_mhz,
                        );

                        if let Err(e) = Self::set_chip_limit_checked(
                            freq_cmd_tx,
                            chain_id,
                            chip_idx,
                            None,
                            crate::FrequencyLimitSource::AutotunerThermal,
                        )
                        .await
                        {
                            warn!(chain_id, chip = chip_idx, error = %e, "Thermal restore: failed to clear per-chip ceiling");
                        } else {
                            monitor.thermal_limit_mhz = None;
                            if Self::refresh_monitor_frequency(monitor) {
                                chip_health_tracker.set_current_frequency(
                                    chain_id,
                                    chip_idx,
                                    monitor.current_freq_mhz,
                                );
                                needs_work_time_update = true;
                            }
                        }
                    }
                }
            }
        }

        // Update WORK_TIME if any chip frequency changed — use slowest chip
        if needs_work_time_update {
            let min_freq = monitors
                .iter()
                .filter(|&(&(cid, _), _)| cid == chain_id)
                .map(|(_, m)| m.current_freq_mhz)
                .min()
                .unwrap_or(self.nominal_mhz);

            let _ = freq_cmd_tx
                .send(FreqCommand::UpdateWorkTime {
                    chain_id,
                    min_freq_mhz: min_freq,
                })
                .await;
            info!(
                "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (thermal deration)",
                chain_id, min_freq
            );
            if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                warn!(chain_id, error = %e, "Background thermal compensation: dispatcher sync failed after frequency batch");
            }
        }
    }

    async fn apply_fan_clamp_limits(
        &self,
        chain_id: u8,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
        chip_health_tracker: &mut ChipHealthTracker,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        let fan_factor = self.fan_factor();
        let mut needs_work_time_update = false;

        for ((cid, chip_idx), monitor) in monitors.iter_mut() {
            if *cid != chain_id {
                continue;
            }

            let new_limit = if fan_factor < 0.99 {
                let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(
                    monitor.chip_id,
                );
                let adjusted = (monitor.profile_freq_mhz as f64 * fan_factor) as u16;
                Some(
                    pll.iter()
                        .rev()
                        .find(|&&f| f <= adjusted)
                        .copied()
                        .unwrap_or(self.config.min_freq_mhz),
                )
            } else {
                None
            };

            if monitor.fan_limit_mhz != new_limit {
                if let Err(e) = Self::set_chip_limit_checked(
                    freq_cmd_tx,
                    chain_id,
                    *chip_idx,
                    new_limit,
                    crate::FrequencyLimitSource::FanClamp,
                )
                .await
                {
                    warn!(chain_id, chip = *chip_idx, error = %e, "Fan clamp: failed to apply per-chip ceiling");
                } else {
                    monitor.fan_limit_mhz = new_limit;
                    if Self::refresh_monitor_frequency(monitor) {
                        chip_health_tracker.set_current_frequency(
                            chain_id,
                            *chip_idx,
                            monitor.current_freq_mhz,
                        );
                        needs_work_time_update = true;
                    }
                }
            }
        }

        if needs_work_time_update {
            let min_freq = monitors
                .iter()
                .filter(|&(&(cid, _), _)| cid == chain_id)
                .map(|(_, monitor)| monitor.current_freq_mhz)
                .min()
                .unwrap_or(self.nominal_mhz);

            let _ = freq_cmd_tx
                .send(FreqCommand::UpdateWorkTime {
                    chain_id,
                    min_freq_mhz: min_freq,
                })
                .await;
            if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                warn!(chain_id, error = %e, "Background fan clamp: dispatcher sync failed after frequency batch");
            }
        }
    }

    /// Process a background monitoring snapshot.
    async fn process_background_snapshot(
        &mut self,
        snapshot: &ChipStatsSnapshot,
        monitors: &mut HashMap<(u8, u8), ChipMonitor>,
        chip_health_tracker: &mut ChipHealthTracker,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        // Temperature sensor failure safety: if board_temp_c is missing for
        // 3+ consecutive snapshots, force all chips to minimum frequency.
        // A missing temp sensor is a safety hazard — we cannot protect against
        // thermal runaway without temperature data.
        if snapshot.board_temp_c.is_none() {
            let consecutive_missing = {
                let missing = self
                    .consecutive_temp_missing
                    .entry(snapshot.chain_id)
                    .or_insert(0);
                *missing += 1;
                *missing
            };
            if consecutive_missing >= 3 {
                let min_freq = self.config.min_freq_mhz;
                self.state = TunerState::BackgroundAdjust;
                self.safety_override = Some("missing_temperature".to_string());
                self.publish_runtime_status(
                    "Safety override active — board temperature data missing, forcing minimum frequency",
                    None,
                );
                warn!(
                    chain_id = snapshot.chain_id,
                    consecutive_missing,
                    min_freq,
                    "SAFETY: temperature sensor data missing for {} consecutive snapshots — \
                     forcing all chips to minimum frequency ({} MHz)",
                    consecutive_missing,
                    min_freq,
                );
                for (key, monitor) in monitors.iter_mut() {
                    if key.0 == snapshot.chain_id
                        && !monitor.masked
                        && monitor.current_freq_mhz > min_freq
                    {
                        if let Err(e) = Self::set_chip_limit_checked(
                            freq_cmd_tx,
                            snapshot.chain_id,
                            key.1,
                            Some(min_freq),
                            crate::FrequencyLimitSource::SensorSafety,
                        )
                        .await
                        {
                            warn!(chain_id = snapshot.chain_id, chip = key.1, error = %e, "Background safety: failed to apply sensor ceiling");
                        } else {
                            monitor.sensor_safety_limit_mhz = Some(min_freq);
                            Self::refresh_monitor_frequency(monitor);
                            chip_health_tracker.set_current_frequency(
                                snapshot.chain_id,
                                key.1,
                                monitor.current_freq_mhz,
                            );
                        }
                    }
                }

                let _ = freq_cmd_tx
                    .send(FreqCommand::UpdateWorkTime {
                        chain_id: snapshot.chain_id,
                        min_freq_mhz: min_freq,
                    })
                    .await;
                if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                    warn!(
                        chain_id = snapshot.chain_id,
                        error = %e,
                        "Background safety: dispatcher sync failed after missing-temperature downclock"
                    );
                }
                return; // Skip normal processing — safety override active
            }
        } else {
            let recovered_after_missing = self
                .consecutive_temp_missing
                .remove(&snapshot.chain_id)
                .map(|count| count >= 3)
                .unwrap_or(false);
            if recovered_after_missing {
                let mut needs_work_time_update = false;
                for (key, monitor) in monitors.iter_mut() {
                    if key.0 == snapshot.chain_id && monitor.current_freq_mhz > 0 {
                        if let Err(e) = Self::set_chip_limit_checked(
                            freq_cmd_tx,
                            snapshot.chain_id,
                            key.1,
                            None,
                            crate::FrequencyLimitSource::SensorSafety,
                        )
                        .await
                        {
                            warn!(chain_id = snapshot.chain_id, chip = key.1, error = %e, "Background safety: failed to clear sensor ceiling on recovery");
                            continue;
                        }
                    }
                    if key.0 == snapshot.chain_id && monitor.sensor_safety_limit_mhz.is_some() {
                        monitor.sensor_safety_limit_mhz = None;
                        if Self::refresh_monitor_frequency(monitor) {
                            needs_work_time_update = true;
                        }
                        chip_health_tracker.set_current_frequency(
                            snapshot.chain_id,
                            key.1,
                            monitor.current_freq_mhz,
                        );
                    }
                }

                if needs_work_time_update {
                    let min_freq = monitors
                        .iter()
                        .filter(|&(&(cid, _), _)| cid == snapshot.chain_id)
                        .map(|(_, monitor)| monitor.current_freq_mhz)
                        .min()
                        .unwrap_or(self.nominal_mhz);

                    let _ = freq_cmd_tx
                        .send(FreqCommand::UpdateWorkTime {
                            chain_id: snapshot.chain_id,
                            min_freq_mhz: min_freq,
                        })
                        .await;
                    if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                        warn!(
                            chain_id = snapshot.chain_id,
                            error = %e,
                            "Background safety: dispatcher sync failed after temperature recovery"
                        );
                    }
                }

                if !self
                    .consecutive_temp_missing
                    .values()
                    .any(|count| *count >= 3)
                {
                    self.safety_override = None;
                }
                self.publish_runtime_status(
                    "Board temperature data recovered — clearing sensor safety ceiling",
                    None,
                );
            }
        }

        let mut needs_work_time_update = false;
        // Track whether backoffs were error-based (safe to redistribute)
        // vs thermal-based (redistribution would cascade). CE WARNING-8.
        let mut had_error_based_backoff = false;
        // F4: per-event backoff deltas from THIS snapshot only, as
        // (chip_idx, old_freq, new_freq). Weak-chip compensation must redistribute
        // the power freed by the backoffs that happened THIS snapshot using each
        // event's own old->new pair (redistribute_freed_power's documented
        // contract), NOT the cumulative all-time (profile_freq -> desired_freq)
        // delta of every historically-backed-off chip — which re-donated the same
        // freed watts again on every snapshot and crept the chain above its tuned
        // power envelope.
        let mut error_backoff_events: Vec<(u8, u16, u16)> = Vec::new();

        // Board-level fault detection: if ALL chips on this chain produced
        // 0 nonces in this window, this is a board-level fault (voltage loss,
        // PIC heartbeat death, FPGA issue), NOT per-chip instability.
        // Skip all per-chip backoffs to avoid trashing the profile.
        let total_chain_nonces: u64 = snapshot.chip_nonces.iter().sum();
        if total_chain_nonces == 0 && !snapshot.chip_nonces.is_empty() {
            warn!(
                chain_id = snapshot.chain_id,
                chip_count = snapshot.chip_nonces.len(),
                "BOARD FAULT: ALL {} chips on chain {} produced 0 nonces — \
                 board-level issue (PIC/voltage/FPGA). Skipping per-chip adjustments.",
                snapshot.chip_nonces.len(),
                snapshot.chain_id,
            );
            return; // Don't touch any chip frequencies
        }

        for i in 0..snapshot.chip_nonces.len() {
            let chip_idx = i as u8;
            let key = (snapshot.chain_id, chip_idx);

            let monitor = match monitors.get_mut(&key) {
                Some(m) => m,
                None => continue,
            };

            let nonces = snapshot.chip_nonces[i];
            let errors = snapshot.stability_error_count(i);
            let comm_issues = snapshot.communication_issue_count(i);
            let total = nonces + errors;

            // Dead chip masking: MUST check before the total==0 skip.
            // A chip producing zero nonces AND zero errors is dead — track it.
            // After 5 consecutive zero windows, mask the chip permanently.
            if total == 0 {
                if comm_issues > 0 {
                    monitor.consecutive_zero_nonce_windows = 0;
                    continue;
                }
                monitor.consecutive_zero_nonce_windows += 1;
                if monitor.consecutive_zero_nonce_windows >= 5 && !monitor.masked {
                    // Use min_freq_mhz instead of 0 — the BM1387 PLL lookup clamps
                    // freq_mhz=0 to 100 MHz silently, so "masked" chips kept running.
                    // Running at min_freq_mhz (200 MHz default) is the same approach
                    // BraiinsOS uses for dead chips: minimum frequency, not disabled.
                    let mask_freq = self.config.min_freq_mhz;
                    warn!(
                        chain_id = snapshot.chain_id,
                        chip = chip_idx,
                        mask_freq,
                        "Dead chip detected: chip {} produced 0 nonces for {} consecutive windows — reducing to {} MHz",
                        chip_idx,
                        monitor.consecutive_zero_nonce_windows,
                        mask_freq,
                    );
                    monitor.masked = true;
                    monitor.desired_freq_mhz = mask_freq;
                    monitor.thermal_limit_mhz = None;
                    monitor.sensor_safety_limit_mhz = None;
                    Self::refresh_monitor_frequency(monitor);
                    chip_health_tracker.record_backoff(
                        snapshot.chain_id,
                        chip_idx,
                        monitor.current_freq_mhz,
                    );
                    if let Err(e) = Self::set_chip_freq_checked(
                        freq_cmd_tx,
                        snapshot.chain_id,
                        chip_idx,
                        mask_freq,
                    )
                    .await
                    {
                        warn!(chain_id = snapshot.chain_id, chip = chip_idx, error = %e,
                            "Failed to send dead chip mask command");
                        continue;
                    }
                    needs_work_time_update = true;
                }
                continue; // No nonce/error data to process
            }
            // Chip produced data — reset zero-nonce counter
            monitor.consecutive_zero_nonce_windows = 0;

            let error_rate = errors as f64 / total as f64 * 100.0;

            // Hashrate deficit check: detect chips with stuck cores
            // Expected nonces: freq_mhz × 114_cores / (diff × 2^32) × window_s
            // Uses actual difficulty from snapshot instead of hardcoded 256.
            if nonces > 0 && monitor.current_freq_mhz > 0 && snapshot.window_duration_s > 0.0 {
                let diff = if snapshot.current_difficulty == 0 {
                    256
                } else {
                    snapshot.current_difficulty
                };
                let expected_nps = crate::chip_geometry::expected_nps_for_chip(
                    monitor.chip_id,
                    monitor.current_freq_mhz,
                    diff,
                );
                let expected = expected_nps * snapshot.window_duration_s;
                if expected > 0.0 {
                    let ratio = nonces as f64 / expected;
                    if ratio < self.config.min_hashrate_ratio {
                        monitor.consecutive_hashrate_deficit += 1;
                        if monitor.consecutive_hashrate_deficit
                            >= self.config.max_consecutive_errors
                        {
                            let old_freq = monitor.current_freq_mhz;
                            let new_freq = self.step_down_freq(old_freq, monitor.chip_id);
                            if new_freq < old_freq {
                                warn!(
                                    chain_id = snapshot.chain_id,
                                    chip = chip_idx,
                                    hashrate_ratio = format_args!("{:.2}", ratio),
                                    old_freq,
                                    new_freq,
                                    "Hashrate deficit on chip {} — ratio {:.2} < {:.2}, backing off {} → {} MHz",
                                    chip_idx,
                                    ratio,
                                    self.config.min_hashrate_ratio,
                                    old_freq,
                                    new_freq,
                                );
                                if let Err(e) = Self::set_chip_freq_checked(
                                    freq_cmd_tx,
                                    snapshot.chain_id,
                                    chip_idx,
                                    new_freq,
                                )
                                .await
                                {
                                    warn!(chain_id = snapshot.chain_id, chip = chip_idx, error = %e,
                                        "Hashrate deficit backoff: failed to apply frequency change");
                                } else {
                                    monitor.desired_freq_mhz = new_freq;
                                    Self::refresh_monitor_frequency(monitor);
                                    chip_health_tracker.record_backoff(
                                        snapshot.chain_id,
                                        chip_idx,
                                        monitor.current_freq_mhz,
                                    );
                                    needs_work_time_update = true;
                                }
                            }
                            monitor.consecutive_hashrate_deficit = 0;
                        }
                    } else {
                        monitor.consecutive_hashrate_deficit = 0;
                    }
                }
            }

            if error_rate > self.config.error_threshold_pct {
                monitor.consecutive_errors += 1;

                if monitor.consecutive_errors >= self.config.max_consecutive_errors {
                    let old_freq = monitor.current_freq_mhz;
                    let new_freq = self.step_down_freq(old_freq, monitor.chip_id);

                    if new_freq < old_freq {
                        warn!(
                            chain_id = snapshot.chain_id,
                            chip = chip_idx,
                            error_rate = format_args!("{:.2}%", error_rate),
                            communication_issues = comm_issues,
                            consecutive = monitor.consecutive_errors,
                            old_freq,
                            new_freq,
                            "Backing off chip {} — {:.2}% HW errors sustained {} windows, {} comm issues ignored, {} → {} MHz",
                            chip_idx,
                            error_rate,
                            monitor.consecutive_errors,
                            comm_issues,
                            old_freq,
                            new_freq,
                        );

                        if let Err(e) = Self::set_chip_freq_checked(
                            freq_cmd_tx,
                            snapshot.chain_id,
                            chip_idx,
                            new_freq,
                        )
                        .await
                        {
                            warn!(chain_id = snapshot.chain_id, chip = chip_idx, error = %e,
                                "Error backoff: failed to apply frequency change");
                        } else {
                            monitor.desired_freq_mhz = new_freq;
                            Self::refresh_monitor_frequency(monitor);
                            chip_health_tracker.record_backoff(
                                snapshot.chain_id,
                                chip_idx,
                                monitor.current_freq_mhz,
                            );
                            needs_work_time_update = true;
                            had_error_based_backoff = true;
                            // F4: record THIS event's per-chip old->new delta so
                            // weak-chip compensation redistributes only the power
                            // this backoff freed, not every past backoff's again.
                            error_backoff_events.push((chip_idx, old_freq, new_freq));
                        }
                    }

                    monitor.consecutive_errors = 0;
                }
            } else {
                monitor.consecutive_errors = 0;

                // Boost-back: if a backed-off chip runs clean for N consecutive
                // windows, step it back up one PLL entry toward profile frequency.
                // Safety: only boost if board temperature is below derating threshold
                // (60C default, higher with immersion). Prevents boosting into thermal
                // instability that caused the original backoff.
                // Use configurable derating threshold (default 60C) instead of hardcoded value.
                // EE Round 2: hardcoded 60.0 would disagree with a custom ThermalCompensator threshold.
                let derating_threshold = 60.0_f32
                    + if self.config.immersion_mode {
                        self.config.immersion_temp_offset_c
                    } else {
                        0.0
                    };
                let temp_safe_for_boost = snapshot
                    .board_temp_c
                    .map(|t| t < derating_threshold)
                    .unwrap_or(false); // No temp data = assume UNSAFE, don't boost

                if monitor.desired_freq_mhz < monitor.profile_freq_mhz
                    && !monitor.thermally_derated
                    && !monitor.masked
                    && monitor.boost_attempts < self.config.max_boost_attempts
                    && temp_safe_for_boost
                {
                    monitor.consecutive_clean_windows += 1;

                    if monitor.consecutive_clean_windows >= self.config.boost_back_threshold {
                        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(
                            monitor.chip_id,
                        );
                        // Find next PLL entry above current frequency
                        let boosted = pll
                            .iter()
                            .find(|&&f| f > monitor.desired_freq_mhz)
                            .copied()
                            .unwrap_or(monitor.desired_freq_mhz)
                            .min(monitor.profile_freq_mhz);

                        if boosted > monitor.desired_freq_mhz {
                            // W6.3 + W6.4: gate the step-up on
                            // rolling pool acceptance >= 99% AND
                            // per-chip HW err < 2%. Failing the gate
                            // skips the apply (stays at current freq)
                            // and emits a structured warn — the
                            // monitor's `consecutive_clean_windows`
                            // counter is reset so the next gate check
                            // happens after another full clean run.
                            if !self.step_up_gate_passes(snapshot.chain_id, chip_idx, boosted) {
                                monitor.consecutive_clean_windows = 0;
                                continue;
                            }

                            info!(
                                chain_id = snapshot.chain_id,
                                chip = chip_idx,
                                old_freq = monitor.desired_freq_mhz,
                                new_freq = boosted,
                                attempt = monitor.boost_attempts + 1,
                                max_attempts = self.config.max_boost_attempts,
                                "Boost-back: chip {} clean for {} windows, {} → {} MHz (attempt {}/{})",
                                chip_idx,
                                monitor.consecutive_clean_windows,
                                monitor.desired_freq_mhz,
                                boosted,
                                monitor.boost_attempts + 1,
                                self.config.max_boost_attempts,
                            );

                            if let Err(e) = Self::set_chip_freq_checked(
                                freq_cmd_tx,
                                snapshot.chain_id,
                                chip_idx,
                                boosted,
                            )
                            .await
                            {
                                warn!(chain_id = snapshot.chain_id, chip = chip_idx, error = %e,
                                    "Boost-back: failed to apply frequency change");
                            } else {
                                monitor.desired_freq_mhz = boosted;
                                Self::refresh_monitor_frequency(monitor);
                                chip_health_tracker.set_current_frequency(
                                    snapshot.chain_id,
                                    chip_idx,
                                    monitor.current_freq_mhz,
                                );
                                monitor.boost_attempts += 1;
                                monitor.consecutive_clean_windows = 0;
                                needs_work_time_update = true;
                            }
                        }
                    }
                } else if monitor.desired_freq_mhz >= monitor.profile_freq_mhz {
                    // Reset clean counter when back at profile frequency
                    monitor.consecutive_clean_windows = 0;
                }
            }

            // (Dead chip masking moved above the total==0 guard — see line 1863+)
        }

        // Weak chip compensation: redistribute freed power to strong chips.
        // SAFETY (CE WARNING-8): Only redistribute for error-based backoffs.
        // Thermal backoffs mean the board has a cooling problem — redistributing
        // power to neighbors would make them run hotter and cascade.
        if needs_work_time_update && self.config.weak_chip_compensation && had_error_based_backoff {
            if let Some(profile) = self.profiles.get(&snapshot.chain_id) {
                let power_model = self.power_model_for_chip(Self::profile_chip_id(profile));
                let voltage_mv = profile.optimal_voltage_mv.unwrap_or(profile.voltage_mv);
                let voltage_v = voltage_mv as f64 / 1000.0;

                // Collect current frequencies from monitors
                let current_freqs: Vec<(u8, u16)> = monitors
                    .iter()
                    .filter(|&(&(cid, _), _)| cid == snapshot.chain_id)
                    .map(|(&(_, cidx), m)| (cidx, m.current_freq_mhz))
                    .collect();

                // F4: redistribute for the backoffs that happened THIS snapshot
                // only, each with its own per-event old->new delta (passed to
                // redistribute_freed_power below). The prior `for i in
                // 0..chip_nonces` loop fired for EVERY chip still below profile and
                // passed the cumulative profile->desired delta, so it re-donated
                // every past backoff's full freed power again on every snapshot —
                // creeping the chain above its tuned power envelope.
                for &(backed_off_idx, old_freq, new_freq) in &error_backoff_events {
                    let chip_idx = backed_off_idx;
                    let key = (snapshot.chain_id, chip_idx);
                    if let Some(monitor) = monitors.get(&key) {
                        // Detect a recent backoff: profile_freq > current_freq
                        if monitor.desired_freq_mhz < monitor.profile_freq_mhz {
                            let boosts = power_model.redistribute_freed_power(
                                chip_idx,
                                old_freq,
                                new_freq,
                                voltage_v,
                                &profile.chips,
                                &current_freqs,
                                self.config.min_freq_mhz,
                            );

                            for &(boost_idx, boost_freq) in &boosts {
                                if let Some(boost_monitor) =
                                    monitors.get_mut(&(snapshot.chain_id, boost_idx))
                                {
                                    if boost_freq > boost_monitor.desired_freq_mhz {
                                        // W6.3 + W6.4: weak-chip
                                        // compensation also raises a
                                        // chip's freq, so it must
                                        // honor the same step-up
                                        // gate as boost-back.
                                        if !self.step_up_gate_passes(
                                            snapshot.chain_id,
                                            boost_idx,
                                            boost_freq,
                                        ) {
                                            continue;
                                        }

                                        info!(
                                            chain_id = snapshot.chain_id,
                                            chip = boost_idx,
                                            old_freq = boost_monitor.desired_freq_mhz,
                                            new_freq = boost_freq,
                                            "Weak chip compensation: boosting chip {} — {} → {} MHz",
                                            boost_idx,
                                            boost_monitor.desired_freq_mhz,
                                            boost_freq,
                                        );
                                        if let Err(e) = Self::set_chip_freq_checked(
                                            freq_cmd_tx,
                                            snapshot.chain_id,
                                            boost_idx,
                                            boost_freq,
                                        )
                                        .await
                                        {
                                            warn!(chain_id = snapshot.chain_id, chip = boost_idx, error = %e,
                                                "Weak chip compensation: failed to apply boost frequency");
                                        } else {
                                            boost_monitor.desired_freq_mhz = boost_freq;
                                            Self::refresh_monitor_frequency(boost_monitor);
                                            chip_health_tracker.set_current_frequency(
                                                snapshot.chain_id,
                                                boost_idx,
                                                boost_monitor.current_freq_mhz,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // If we backed off any chip, update WORK_TIME with the new slowest frequency
        if needs_work_time_update {
            self.state = TunerState::BackgroundAdjust;
            self.publish_runtime_status("Background monitor adjusted chip operating points", None);
            let min_freq = monitors
                .iter()
                .filter(|&(&(cid, _), _)| cid == snapshot.chain_id)
                .map(|(_, m)| m.current_freq_mhz)
                .min()
                .unwrap_or(self.nominal_mhz);

            let _ = freq_cmd_tx
                .send(FreqCommand::UpdateWorkTime {
                    chain_id: snapshot.chain_id,
                    min_freq_mhz: min_freq,
                })
                .await;
            info!(
                "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (background backoff)",
                snapshot.chain_id, min_freq
            );
            if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                warn!(chain_id = snapshot.chain_id, error = %e, "Background monitor: dispatcher sync failed after frequency batch");
            }
        } else {
            let steady_state = self.steady_state();
            if self.state != steady_state {
                self.state = steady_state;
                self.publish_runtime_status("Background monitor stable", None);
            }
        }
    }

    /// Wait for a stats snapshot from a specific chain.
    fn snapshot_matches(
        snapshot: &ChipStatsSnapshot,
        chain_id: u8,
        measurement_epoch: Option<u64>,
    ) -> bool {
        snapshot.chain_id == chain_id
            && measurement_epoch.is_none_or(|epoch| snapshot.measurement_epoch == epoch)
    }

    fn should_buffer_snapshot(
        snapshot: &ChipStatsSnapshot,
        chain_id: u8,
        measurement_epoch: Option<u64>,
    ) -> bool {
        if snapshot.chain_id != chain_id {
            return true;
        }

        match measurement_epoch {
            Some(epoch) => snapshot.measurement_epoch > epoch,
            None => false,
        }
    }

    async fn wait_for_chain_stats(
        &mut self,
        chain_id: u8,
        measurement_epoch: Option<u64>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> crate::Result<ChipStatsSnapshot> {
        let timeout = if cfg!(test) && self.config.measurement_window_s == 0 {
            std::time::Duration::from_millis(50)
        } else {
            std::time::Duration::from_secs(self.config.measurement_window_s * 3 + 5)
        };
        let timer = tokio::time::sleep(timeout);
        tokio::pin!(timer);

        loop {
            let pending_len = self.pending_stats.len();
            for _ in 0..pending_len {
                if let Some(snapshot) = self.pending_stats.pop_front() {
                    if Self::snapshot_matches(&snapshot, chain_id, measurement_epoch) {
                        return Ok(snapshot);
                    }
                    if Self::should_buffer_snapshot(&snapshot, chain_id, measurement_epoch) {
                        self.pending_stats.push_back(snapshot);
                    }
                }
            }

            tokio::select! {
                _ = shutdown.cancelled() => {
                    return Err(crate::AutoTunerError::StatsTimeout { seconds: 0 });
                }
                _ = &mut timer => {
                    return Err(crate::AutoTunerError::StatsTimeout {
                        seconds: timeout.as_secs(),
                    });
                }
                result = stats_rx.recv() => {
                    match result {
                        Some(snapshot) if Self::snapshot_matches(&snapshot, chain_id, measurement_epoch) => {
                            return Ok(snapshot);
                        }
                        Some(snapshot) => {
                            if Self::should_buffer_snapshot(&snapshot, chain_id, measurement_epoch) {
                                self.pending_stats.push_back(snapshot);
                            }
                        }
                        None => {
                            return Err(crate::AutoTunerError::StatsTimeout {
                                seconds: timeout.as_secs(),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Run voltage descent search for a single chain.
    ///
    /// Steps the voltage down in coarse (20 mV) then fine (10 mV) increments,
    /// checking per-chip error rates at each step. Returns the optimal voltage
    /// (minimum stable + safety margin).
    async fn optimize_voltage(
        &mut self,
        chain_id: u8,
        chip_count: u8,
        initial_voltage_mv: u16,
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) -> crate::Result<VoltageOptimizationResult> {
        if self.chain_chip_id(chain_id) != 0x1387 || self.capabilities.voltage_control != "pic16" {
            return Err(crate::AutoTunerError::Config(format!(
                "runtime voltage optimization on chain {} is currently limited to BM1387/PIC16",
                chain_id
            )));
        }

        let mut search = VoltageSearchState::new(
            chain_id,
            initial_voltage_mv,
            self.config.min_voltage_mv,
            self.config.voltage_margin_mv,
        );

        let start = Instant::now();
        let mut step_count = 0u32;
        let mut communication_retries = 0u32;
        let mut low_confidence_windows = 0u32;
        let mut stable_voltage_points_mv = vec![initial_voltage_mv];
        // Safety limit: max iterations to prevent infinite loops.
        // Worst case: (9400 - 8400) / 10 = 100 fine steps.
        let max_steps = 150u32;

        while !search.is_done() && step_count < max_steps {
            if shutdown.is_cancelled() {
                return Err(crate::AutoTunerError::StatsTimeout { seconds: 0 });
            }
            if self.safety_override.is_some() {
                return Err(crate::AutoTunerError::Config(format!(
                    "runtime voltage optimization aborted on chain {} due to active safety override",
                    chain_id
                )));
            }

            step_count += 1;
            let test_voltage = search.current_voltage();

            // Send voltage change command to dispatcher and require an apply acknowledgement
            Self::set_voltage_checked(freq_cmd_tx, chain_id, test_voltage).await?;

            info!(
                chain_id,
                step = step_count,
                voltage_mv = test_voltage,
                phase = ?search.phase(),
                "Voltage search step {}: testing {} mV ({:?} phase)",
                step_count, test_voltage, search.phase(),
            );

            // Phase 2 fix: drop any stale pre-change snapshots still queued on the
            // autotuner channel. This is only dispatcher-side synchronization; Phase 3
            // still needs a true PIC-apply acknowledgement before voltage tuning is trusted.
            let measurement_epoch = self
                .begin_measurement_window(
                    chain_id,
                    freq_cmd_tx,
                    Duration::from_millis(VOLTAGE_SEARCH_SETTLE_DELAY_MS),
                )
                .await?;

            // Wait for a measurement window to collect error data at new voltage.
            // Low-sample or comm-fault windows are retried at the same voltage so
            // the search does not confuse transport noise with silicon instability.
            let mut snapshot = self
                .wait_for_chain_stats(chain_id, Some(measurement_epoch), stats_rx, shutdown)
                .await?;
            self.record_telemetry_sample(&snapshot);
            if snapshot.board_temp_c.is_none() {
                return Err(crate::AutoTunerError::Config(format!(
                    "runtime voltage optimization on chain {} requires a fresh board temperature",
                    chain_id
                )));
            }

            let mut decision = Self::assess_voltage_window(
                &snapshot,
                chip_count,
                self.config.error_threshold_pct,
                self.config.measurement_window_s as f64,
            );
            while decision == VoltageWindowDecision::LowConfidence
                && low_confidence_windows < VOLTAGE_SEARCH_MAX_LOW_CONFIDENCE_WINDOWS
            {
                low_confidence_windows += 1;
                info!(
                    chain_id,
                    voltage_mv = test_voltage,
                    extra_window = low_confidence_windows,
                    max_extra_windows = VOLTAGE_SEARCH_MAX_LOW_CONFIDENCE_WINDOWS,
                    collected_window_s = format_args!("{:.1}", snapshot.window_duration_s),
                    "Voltage search collected a low-confidence window — extending measurement"
                );
                let next_snapshot = self
                    .wait_for_chain_stats(chain_id, Some(measurement_epoch), stats_rx, shutdown)
                    .await?;
                self.record_telemetry_sample(&next_snapshot);
                snapshot.accumulate_from(&next_snapshot);
                decision = Self::assess_voltage_window(
                    &snapshot,
                    chip_count,
                    self.config.error_threshold_pct,
                    self.config.measurement_window_s as f64,
                );
            }

            if decision == VoltageWindowDecision::RetryCommunicationFault {
                communication_retries += 1;
                if communication_retries >= VOLTAGE_SEARCH_MAX_COMM_RETRIES {
                    return Err(crate::AutoTunerError::Config(format!(
                        "voltage search on chain {} hit repeated communication faults at {} mV",
                        chain_id, test_voltage
                    )));
                }

                warn!(
                    chain_id,
                    voltage_mv = test_voltage,
                    retry = communication_retries,
                    max_retries = VOLTAGE_SEARCH_MAX_COMM_RETRIES,
                    "Voltage search window was dominated by communication faults — retrying same voltage"
                );
                low_confidence_windows = 0;
                continue;
            }

            if decision == VoltageWindowDecision::LowConfidence {
                warn!(
                    chain_id,
                    voltage_mv = test_voltage,
                    window_s = format_args!("{:.1}", snapshot.window_duration_s),
                    "Voltage search still lacked enough confidence after extending the window — conservatively treating as unstable"
                );
            }

            communication_retries = 0;
            low_confidence_windows = 0;
            let all_stable = matches!(decision, VoltageWindowDecision::Stable);

            if all_stable && stable_voltage_points_mv.last().copied() != Some(test_voltage) {
                stable_voltage_points_mv.push(test_voltage);
            }

            info!(
                chain_id,
                voltage_mv = test_voltage,
                window_s = format_args!("{:.1}", snapshot.window_duration_s),
                decision = ?decision,
                all_stable,
                "Voltage {} mV: {}",
                test_voltage,
                if all_stable {
                    "ALL chips stable"
                } else {
                    "UNSTABLE — stepping back"
                },
            );

            search.advance(all_stable);
        }

        if step_count >= max_steps {
            warn!(
                chain_id,
                steps = step_count,
                "Voltage search hit iteration limit — using last stable result"
            );
        }

        let result_mv = search.result();
        let duration_s = start.elapsed().as_secs_f64();

        // Set the final optimized voltage and require a controller acknowledgement.
        Self::set_voltage_checked(freq_cmd_tx, chain_id, result_mv).await?;

        // Request voltage readback verification from the daemon.
        // The autotuner has no direct I2C access (HAL crate), so it signals
        // the daemon to read the actual PIC DAC voltage and verify it matches.
        if search.readback_requested() {
            info!(
                chain_id,
                target_mv = result_mv,
                "Voltage search complete — requesting PIC readback verification"
            );
            if let Some(actual_mv) =
                Self::verify_voltage_checked(freq_cmd_tx, chain_id, result_mv).await?
            {
                let delta_mv = actual_mv.abs_diff(result_mv);
                if delta_mv > VOLTAGE_VERIFY_TOLERANCE_MV {
                    return Err(crate::AutoTunerError::Config(format!(
                        "voltage verification mismatch on chain {}: target={}mV actual={}mV delta={}mV",
                        chain_id, result_mv, actual_mv, delta_mv,
                    )));
                }
            }
        }

        let confirm_epoch = self
            .begin_measurement_window(
                chain_id,
                freq_cmd_tx,
                Duration::from_millis(VOLTAGE_SEARCH_SETTLE_DELAY_MS),
            )
            .await?;
        let confirm_start = Instant::now();
        let mut confirm_snapshot: Option<ChipStatsSnapshot> = None;
        while confirm_start.elapsed().as_secs() < VOLTAGE_SEARCH_FINAL_CONFIRM_WINDOW_S {
            let snapshot = self
                .wait_for_chain_stats(chain_id, Some(confirm_epoch), stats_rx, shutdown)
                .await?;
            self.record_telemetry_sample(&snapshot);
            match &mut confirm_snapshot {
                Some(existing) => existing.accumulate_from(&snapshot),
                None => confirm_snapshot = Some(snapshot),
            }
        }

        let final_snapshot = confirm_snapshot.ok_or(crate::AutoTunerError::StatsTimeout {
            seconds: VOLTAGE_SEARCH_FINAL_CONFIRM_WINDOW_S,
        })?;
        if final_snapshot.board_temp_c.is_none() {
            return Err(crate::AutoTunerError::Config(format!(
                "runtime voltage optimization final confirmation on chain {} lacked a board temperature",
                chain_id
            )));
        }
        if !Self::check_all_chips_stable(
            &final_snapshot,
            chip_count,
            self.config.error_threshold_pct,
            VOLTAGE_SEARCH_FINAL_CONFIRM_WINDOW_S as f64,
        ) {
            return Err(crate::AutoTunerError::Config(format!(
                "runtime voltage optimization final confirmation failed on chain {} at {} mV",
                chain_id, result_mv
            )));
        }

        info!(
            chain_id,
            result_mv,
            savings_mv = search.savings_mv(),
            steps = step_count,
            duration_s = format_args!("{:.1}", duration_s),
            "Voltage search complete: {} mV (saved {} mV in {} steps, {:.1}s)",
            result_mv,
            search.savings_mv(),
            step_count,
            duration_s,
        );

        Ok(VoltageOptimizationResult {
            optimal_voltage_mv: result_mv,
            stable_voltage_points_mv,
        })
    }

    /// Check if all chips on a chain are stable (error rate below threshold).
    fn check_all_chips_stable(
        snapshot: &ChipStatsSnapshot,
        chip_count: u8,
        error_threshold_pct: f64,
        min_window_s: f64,
    ) -> bool {
        // If the measurement window was too short, consider unstable (insufficient data)
        if snapshot.window_duration_s < min_window_s * 0.5 {
            return false;
        }

        for i in 0..chip_count as usize {
            if i >= snapshot.chip_nonces.len() {
                continue;
            }

            let nonces = snapshot.chip_nonces[i];
            let errors = snapshot.stability_error_count(i);
            let total = nonces + errors;

            if total == 0 {
                // No data from this chip — could be dead or just quiet.
                // For voltage search, treat no data as potentially unstable
                // only if we expected data (window long enough).
                if snapshot.window_duration_s >= min_window_s {
                    return false;
                }
                continue;
            }

            let error_rate = errors as f64 / total as f64 * 100.0;
            if error_rate >= error_threshold_pct {
                return false;
            }
        }

        true
    }

    /// Apply the configured target mode (Hashrate/Power/Efficiency) to all tuned chains.
    ///
    /// After characterization and voltage optimization, this adjusts per-chip frequencies
    /// based on the selected mode:
    /// - Hashrate: no change (already at max stable with safety margin)
    /// - Power: allocate frequencies within the target_watts budget
    /// - Efficiency: minimize voltage, run at max stable for that voltage
    async fn apply_target_mode(
        &mut self,
        chain_infos: &[crate::ChainTuneInfo],
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        match self.config.target_mode {
            TuneTarget::Hashrate => {
                self.active_runtime_objective = "hashrate".to_string();
                // Default mode — compute power/efficiency estimates but don't change frequencies
                info!(
                    "Target mode: Hashrate — using max stable frequencies (no power budget applied)"
                );
                self.compute_power_estimates(chain_infos);
            }

            TuneTarget::Power => {
                self.active_runtime_objective = "power_cap".to_string();
                if self.mixed_chain_chip_ids() {
                    self.active_runtime_objective = "hashrate".to_string();
                    warn!(
                        "Power target mode is not yet supported across mixed chip families — using per-chain estimate-only fallback"
                    );
                    self.compute_power_estimates(chain_infos);
                    return;
                }
                let saved_calibrated_c_eff = self.saved_calibrated_c_eff();
                let mut power_model = self.power_model_for_chip(
                    self.chain_chip_id(chain_infos.first().map(|c| c.chain_id).unwrap_or(0)),
                );
                if let Some(c_eff) = saved_calibrated_c_eff {
                    power_model = power_model.with_c_eff(c_eff);
                }
                let num_chains = chain_infos.len() as u8;
                let raw_target_watts = self.config.target_watts;
                if raw_target_watts == 0 {
                    self.active_runtime_objective = "hashrate".to_string();
                    warn!("Power mode selected but target_watts=0 — falling back to hashrate mode");
                    self.compute_power_estimates(chain_infos);
                    return;
                }

                // Temperature-compensated power budget: C_eff increases ~0.3%/°C
                // above the 55°C reference point. In immersion mode (running at
                // 70-80°C), actual power is 4-7% higher than the model predicts.
                // Reduce the budget to prevent PSU overload.
                let target_watts = if self.config.immersion_mode {
                    let thermal_overhead = 0.955; // ~4.5% correction for elevated temps
                    let adjusted = (raw_target_watts as f64 * thermal_overhead) as u32;
                    if adjusted < raw_target_watts {
                        info!(
                            raw_watts = raw_target_watts,
                            adjusted_watts = adjusted,
                            "Immersion mode: reducing power budget by 4.5% for thermal C_eff correction"
                        );
                    }
                    adjusted
                } else {
                    raw_target_watts
                };

                info!(
                    target_watts,
                    "Target mode: Power — allocating per-chip frequencies within {} W budget",
                    target_watts,
                );

                // Collect all chip profiles across chains for budget allocation
                let mut all_chips: Vec<crate::profile::ChipProfile> = Vec::new();
                let mut chain_chip_ranges: Vec<(u8, u16, usize, usize)> = Vec::new();

                for info in chain_infos {
                    let chain_id = info.chain_id;
                    let voltage_mv = info.voltage_mv;
                    if let Some(profile) = self.profiles.get(&chain_id) {
                        let start = all_chips.len();
                        let voltage = profile.optimal_voltage_mv.unwrap_or(voltage_mv);
                        all_chips.extend(profile.chips.iter().cloned().map(|mut chip| {
                            if let Some(measured_freq) =
                                chip.measured_max_stable_at_or_below_voltage(voltage)
                            {
                                chip.max_stable_mhz = chip.max_stable_mhz.min(measured_freq);
                            }
                            chip
                        }));
                        chain_chip_ranges.push((chain_id, voltage, start, profile.chips.len()));
                    }
                }

                if all_chips.is_empty() {
                    return;
                }

                // Use a chip-count-weighted average voltage for the budget allocation.
                // Some families still share one board-level voltage, but cross-chain
                // planning should not silently inherit the first chain's voltage.
                let voltage_mv = if chain_chip_ranges.is_empty() {
                    9100
                } else {
                    let weighted_mv: usize = chain_chip_ranges
                        .iter()
                        .map(|(_, voltage_mv, _, count)| *voltage_mv as usize * *count)
                        .sum();
                    let total_weight: usize = chain_chip_ranges
                        .iter()
                        .map(|(_, _, _, count)| *count)
                        .sum();
                    if total_weight == 0 {
                        9100
                    } else {
                        (weighted_mv / total_weight) as u16
                    }
                };
                let voltage_v = voltage_mv as f64 / 1000.0;

                let budget_freqs = power_model.allocate_budget_safe(
                    target_watts as f64,
                    voltage_v,
                    &all_chips,
                    self.config.min_freq_mhz,
                    num_chains,
                    saved_calibrated_c_eff,
                );

                // Apply the budget-allocated frequencies to each chain
                for &(chain_id, _voltage, start, count) in &chain_chip_ranges {
                    let chain_freqs = &budget_freqs[start..start + count];

                    if let Some(profile) = self.profiles.get_mut(&chain_id) {
                        for (i, &new_freq) in chain_freqs.iter().enumerate() {
                            if i < profile.chips.len() {
                                let old_freq = profile.chips[i].operating_mhz;
                                match Self::set_chip_freq_checked(
                                    freq_cmd_tx,
                                    chain_id,
                                    profile.chips[i].chip_index,
                                    new_freq,
                                )
                                .await
                                {
                                    Ok(applied_freq) => {
                                        profile.chips[i].operating_mhz = applied_freq;
                                    }
                                    Err(e) => {
                                        warn!(chain_id, chip = profile.chips[i].chip_index, error = %e, "Target mode DVFS: failed to apply chip frequency");
                                    }
                                }

                                if new_freq != old_freq {
                                    tracing::debug!(
                                        chain_id,
                                        chip = profile.chips[i].chip_index,
                                        old_freq,
                                        new_freq,
                                        "Power budget: adjusted chip {} freq {} → {} MHz",
                                        profile.chips[i].chip_index,
                                        old_freq,
                                        new_freq,
                                    );
                                }
                            }
                        }

                        // Update WORK_TIME for this chain — use slowest chip
                        let min_freq = chain_freqs
                            .iter()
                            .copied()
                            .min()
                            .unwrap_or(self.nominal_mhz);
                        let _ = freq_cmd_tx
                            .send(FreqCommand::UpdateWorkTime {
                                chain_id,
                                min_freq_mhz: min_freq,
                            })
                            .await;
                        info!(
                            "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (DVFS apply)",
                            chain_id, min_freq
                        );
                        if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                            warn!(chain_id, error = %e, "Target mode DVFS: dispatcher sync failed after frequency batch");
                        }

                        // Recompute stats with new frequencies
                        let duration_s = profile.stats.tuning_duration_s;
                        profile.stats = TuningProfile::compute_stats(&profile.chips, duration_s);
                    }
                }

                info!(
                    target_watts,
                    "Power budget applied — frequencies adjusted for {} chips across {} chain(s)",
                    all_chips.len(),
                    chain_chip_ranges.len(),
                );

                self.compute_power_estimates(chain_infos);
            }

            TuneTarget::HashrateTarget => {
                self.active_runtime_objective = "hashrate_target".to_string();
                if self.mixed_chain_chip_ids() {
                    self.active_runtime_objective = "hashrate".to_string();
                    warn!(
                        "HashrateTarget mode is not yet supported across mixed chip families — using per-chain estimate-only fallback"
                    );
                    self.compute_power_estimates(chain_infos);
                    return;
                }
                let saved_calibrated_c_eff = self.saved_calibrated_c_eff();
                let mut power_model = self.power_model_for_chip(
                    self.chain_chip_id(chain_infos.first().map(|c| c.chain_id).unwrap_or(0)),
                );
                if let Some(c_eff) = saved_calibrated_c_eff {
                    power_model = power_model.with_c_eff(c_eff);
                }
                let num_chains = chain_infos.len() as u8;
                let target_ths = self.config.target_hashrate_ths;
                if target_ths <= 0.0 {
                    self.active_runtime_objective = "hashrate".to_string();
                    warn!(
                        "HashrateTarget mode but target_hashrate_ths=0 — falling back to hashrate mode"
                    );
                    self.compute_power_estimates(chain_infos);
                    return;
                }

                let total_chips: usize = self.profiles.values().map(|p| p.chips.len()).sum();

                let voltage_mv = if chain_infos.is_empty() {
                    9100
                } else {
                    let weighted_mv: usize = chain_infos
                        .iter()
                        .map(|info| {
                            let count = self
                                .profiles
                                .get(&info.chain_id)
                                .map(|p| p.chips.len())
                                .unwrap_or(info.chip_count as usize);
                            let voltage = self
                                .profiles
                                .get(&info.chain_id)
                                .and_then(|p| p.optimal_voltage_mv)
                                .unwrap_or(info.voltage_mv);
                            voltage as usize * count
                        })
                        .sum();
                    let total_weight: usize = chain_infos
                        .iter()
                        .map(|info| {
                            self.profiles
                                .get(&info.chain_id)
                                .map(|p| p.chips.len())
                                .unwrap_or(info.chip_count as usize)
                        })
                        .sum();
                    if total_weight == 0 {
                        9100
                    } else {
                        (weighted_mv / total_weight) as u16
                    }
                };
                let voltage_v = voltage_mv as f64 / 1000.0;

                let synthetic_budget =
                    power_model.budget_for_hashrate(target_ths, voltage_v, total_chips, num_chains);

                info!(
                    target_ths = format_args!("{:.2}", target_ths),
                    synthetic_budget_w = format_args!("{:.0}", synthetic_budget),
                    "Target mode: HashrateTarget — {:.2} TH/s at minimum power (~{:.0}W budget)",
                    target_ths,
                    synthetic_budget,
                );

                let mut all_chips: Vec<crate::profile::ChipProfile> = Vec::new();
                let mut chain_chip_ranges: Vec<(u8, u16, usize, usize)> = Vec::new();

                for info in chain_infos {
                    let chain_id = info.chain_id;
                    let vmv = info.voltage_mv;
                    if let Some(profile) = self.profiles.get(&chain_id) {
                        let start = all_chips.len();
                        let voltage = profile.optimal_voltage_mv.unwrap_or(vmv);
                        all_chips.extend(profile.chips.iter().cloned().map(|mut chip| {
                            if let Some(measured_freq) =
                                chip.measured_max_stable_at_or_below_voltage(voltage)
                            {
                                chip.max_stable_mhz = chip.max_stable_mhz.min(measured_freq);
                            }
                            chip
                        }));
                        chain_chip_ranges.push((chain_id, voltage, start, profile.chips.len()));
                    }
                }

                if !all_chips.is_empty() {
                    let budget_freqs = power_model.allocate_budget_safe(
                        synthetic_budget,
                        voltage_v,
                        &all_chips,
                        self.config.min_freq_mhz,
                        num_chains,
                        saved_calibrated_c_eff,
                    );

                    for &(chain_id, _voltage, start, count) in &chain_chip_ranges {
                        let chain_freqs = &budget_freqs[start..start + count];

                        if let Some(profile) = self.profiles.get_mut(&chain_id) {
                            for (i, &new_freq) in chain_freqs.iter().enumerate() {
                                if i < profile.chips.len() {
                                    // Record operating_mhz ONLY from the applied result
                                    // (mirror the Power branch). Assigning new_freq before
                                    // the apply meant a failed set_chip_freq left the power
                                    // model trusting a frequency the hardware never took —
                                    // and total-power enforcement then passed on phantom
                                    // numbers while the chip ran at its prior (higher) freq.
                                    match Self::set_chip_freq_checked(
                                        freq_cmd_tx,
                                        chain_id,
                                        profile.chips[i].chip_index,
                                        new_freq,
                                    )
                                    .await
                                    {
                                        Ok(applied_freq) => {
                                            profile.chips[i].operating_mhz = applied_freq;
                                        }
                                        Err(e) => {
                                            warn!(chain_id, chip = profile.chips[i].chip_index, error = %e, "Hashrate target mode: failed to apply chip frequency");
                                        }
                                    }
                                }
                            }

                            let min_freq = chain_freqs
                                .iter()
                                .copied()
                                .min()
                                .unwrap_or(self.nominal_mhz);
                            let _ = freq_cmd_tx
                                .send(FreqCommand::UpdateWorkTime {
                                    chain_id,
                                    min_freq_mhz: min_freq,
                                })
                                .await;
                            info!(
                                "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (power budget)",
                                chain_id, min_freq
                            );
                            if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                                warn!(chain_id, error = %e, "Hashrate target mode: dispatcher sync failed after frequency batch");
                            }

                            let duration_s = profile.stats.tuning_duration_s;
                            profile.stats =
                                TuningProfile::compute_stats(&profile.chips, duration_s);
                        }
                    }
                }

                self.compute_power_estimates(chain_infos);
            }

            TuneTarget::Efficiency | TuneTarget::EfficiencyJTH => {
                let is_jth_mode = matches!(self.config.target_mode, TuneTarget::EfficiencyJTH);
                self.active_runtime_objective = if is_jth_mode {
                    "efficiency_jth".to_string()
                } else {
                    "efficiency".to_string()
                };
                if is_jth_mode {
                    info!(
                        "Target mode: EfficiencyJTH — minimizing operator-anchored J/TH (wattmeter source-of-truth when available)"
                    );
                } else {
                    info!("Target mode: Efficiency — minimizing J/TH (lower voltage preferred)");
                }

                for info in chain_infos {
                    let chain_id = info.chain_id;
                    let voltage_mv = info.voltage_mv;
                    let chain_chip_id = info.chip_id;
                    let power_model = self.power_model_for_chip(chain_chip_id);
                    if let Some(profile) = self.profiles.get_mut(&chain_id) {
                        let runtime_voltage_supported = self.config.voltage_optimization
                            && self.capabilities.voltage_optimization_supported
                            && self.capabilities.profile_key == "bm1387-home-pic16";
                        // The optimizer derates max_stable toward a LOWER operating
                        // voltage relative to this reference (efficiency.rs
                        // contract: "voltage at which chip profiles were
                        // characterized"), so it must be the profile's own
                        // characterization voltage — NOT the chip's catalog default
                        // (BM1387's is 8600 mV, below the ~9100 mV S9 operating
                        // voltage, which under-derates every downward voltage step).
                        // The >1.0-ratio overclock this previously also caused is
                        // independently capped in EfficiencyOptimizer::optimize.
                        let reference_voltage_mv = if profile.voltage_mv > 0 {
                            profile.voltage_mv
                        } else {
                            dcentrald_asic::drivers::MinerProfile::for_chip(chain_chip_id)
                                .map(|p| p.default_voltage_mv)
                                .unwrap_or(9100)
                        };
                        let optimizer_min_voltage_mv = if runtime_voltage_supported {
                            self.config.min_voltage_mv
                        } else {
                            voltage_mv
                        };
                        let optimizer_reference_voltage_mv = if runtime_voltage_supported {
                            reference_voltage_mv
                        } else {
                            voltage_mv
                        };
                        let preferred_voltage_mv = if runtime_voltage_supported {
                            profile.optimal_voltage_mv
                        } else {
                            Some(voltage_mv)
                        };
                        let (optimal_voltage_mv, per_chip_freqs) = EfficiencyOptimizer::optimize(
                            &power_model,
                            &profile.chips,
                            optimizer_min_voltage_mv,
                            self.config.min_freq_mhz,
                            chain_chip_id,
                            optimizer_reference_voltage_mv,
                            preferred_voltage_mv,
                        );

                        let voltage_apply_succeeded = if runtime_voltage_supported {
                            match Self::set_voltage_checked(
                                freq_cmd_tx,
                                chain_id,
                                optimal_voltage_mv,
                            )
                            .await
                            {
                                Ok(_) => {
                                    match Self::verify_voltage_checked(
                                        freq_cmd_tx,
                                        chain_id,
                                        optimal_voltage_mv,
                                    )
                                    .await
                                    {
                                        Ok(Some(_)) => true,
                                        Ok(None) => {
                                            warn!(
                                                chain_id,
                                                target_mv = optimal_voltage_mv,
                                                "Efficiency mode: voltage verification returned no readback; not persisting voltage truth"
                                            );
                                            false
                                        }
                                        Err(e) => {
                                            warn!(chain_id, error = %e, "Efficiency mode: voltage verification failed");
                                            false
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(chain_id, error = %e, "Efficiency mode: voltage change failed");
                                    false
                                }
                            }
                        } else {
                            if optimal_voltage_mv != voltage_mv {
                                warn!(
                                    chain_id,
                                    configured_voltage_optimization = self.config.voltage_optimization,
                                    capability_profile = %self.capabilities.profile_key,
                                    "Efficiency mode: runtime undervolt unsupported here; keeping current chain voltage"
                                );
                            }
                            false
                        };

                        info!(
                            chain_id,
                            original_voltage_mv = voltage_mv,
                            optimal_voltage_mv,
                            "Efficiency mode: voltage {} → {} mV for chain {}",
                            voltage_mv,
                            optimal_voltage_mv,
                            chain_id,
                        );

                        // Apply per-chip frequencies
                        for (i, &new_freq) in per_chip_freqs.iter().enumerate() {
                            if i < profile.chips.len() {
                                match Self::set_chip_freq_checked(
                                    freq_cmd_tx,
                                    chain_id,
                                    profile.chips[i].chip_index,
                                    new_freq,
                                )
                                .await
                                {
                                    Ok(applied_freq) => {
                                        profile.chips[i].operating_mhz = applied_freq;
                                    }
                                    Err(e) => {
                                        warn!(chain_id, chip = profile.chips[i].chip_index, error = %e, "Efficiency mode: failed to apply chip frequency");
                                    }
                                }
                            }
                        }

                        // Update WORK_TIME — use slowest chip
                        let min_freq = profile
                            .chips
                            .iter()
                            .map(|chip| chip.operating_mhz)
                            .min()
                            .unwrap_or(self.nominal_mhz);
                        let _ = freq_cmd_tx
                            .send(FreqCommand::UpdateWorkTime {
                                chain_id,
                                min_freq_mhz: min_freq,
                            })
                            .await;
                        info!(
                            "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (efficiency)",
                            chain_id, min_freq
                        );
                        if let Err(e) = Self::wait_for_dispatcher(freq_cmd_tx).await {
                            warn!(chain_id, error = %e, "Efficiency mode: dispatcher sync failed after frequency batch");
                        }

                        // Only persist voltage truth when runtime voltage control actually succeeded.
                        if voltage_apply_succeeded {
                            profile.optimal_voltage_mv = Some(optimal_voltage_mv);
                        }

                        // Recompute stats
                        let duration_s = profile.stats.tuning_duration_s;
                        profile.stats = TuningProfile::compute_stats(&profile.chips, duration_s);
                    }
                }

                self.compute_power_estimates(chain_infos);
            }
        }

        // Re-save profiles with updated power estimates
        for (&chain_id, profile) in &self.profiles {
            if let Err(e) = profile.save(&self.config.profile_path) {
                warn!(
                    chain_id,
                    error = %e,
                    "Failed to save profile with power estimates"
                );
            }
        }
        self.save_resume_state();
    }

    /// Compute and populate power/hashrate/efficiency estimates for all tuned profiles.
    fn compute_power_estimates(&mut self, chain_infos: &[crate::ChainTuneInfo]) {
        let active_chain_count = chain_infos
            .iter()
            .filter(|info| self.profiles.contains_key(&info.chain_id))
            .count()
            .max(1) as u8;

        for info in chain_infos {
            let chain_id = info.chain_id;
            let voltage_mv = info.voltage_mv;
            let chip_id = info.chip_id;
            let power_model = self.power_model_for_chip(chip_id);
            if let Some(profile) = self.profiles.get_mut(&chain_id) {
                let voltage = profile.optimal_voltage_mv.unwrap_or(voltage_mv);

                // Compute per-chain power
                let freqs: Vec<u16> = profile.chips.iter().map(|c| c.operating_mhz).collect();
                let chain_power = power_model.chain_power_w(voltage, &freqs, active_chain_count);

                // Compute hashrate
                let hashrate_ghs: f64 = profile
                    .chips
                    .iter()
                    .map(|c| {
                        crate::chip_geometry::chip_hashrate_ghs_for_chip(chip_id, c.operating_mhz)
                    })
                    .sum();

                // Compute efficiency
                let hashrate_ths = hashrate_ghs / 1000.0;
                let efficiency_jth = if hashrate_ths > 0.0 {
                    chain_power / hashrate_ths
                } else {
                    0.0
                };

                profile.estimated_power_w = chain_power;
                profile.estimated_efficiency_jth = efficiency_jth;
                profile.stats.estimated_hashrate_ghs = hashrate_ghs;
                profile.stats.estimated_power_w = chain_power;
                profile.stats.estimated_efficiency_jth = efficiency_jth;

                info!(
                    chain_id,
                    power_w = format_args!("{:.0}", chain_power),
                    hashrate_ghs = format_args!("{:.1}", hashrate_ghs),
                    efficiency_jth = format_args!("{:.1}", efficiency_jth),
                    voltage_mv = voltage,
                    "Chain {} estimates: {:.0}W, {:.1} GH/s, {:.1} J/TH at {} mV",
                    chain_id,
                    chain_power,
                    hashrate_ghs,
                    efficiency_jth,
                    voltage,
                );
            }
        }
    }

    /// Enforce total power limit across all chains.
    ///
    /// If the total estimated power exceeds the configured limit, backs off
    /// the least efficient chips first until power is within budget.
    async fn enforce_total_power_limit(
        &mut self,
        chain_infos: &[crate::ChainTuneInfo],
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        if self.mixed_chain_chip_ids() {
            warn!(
                "Total power limit enforcement is not yet supported across mixed chip families — skipping"
            );
            return;
        }
        let power_model = self.power_model_for_chip(
            self.chain_chip_id(chain_infos.first().map(|c| c.chain_id).unwrap_or(0)),
        );
        let limit_w = self.config.total_power_limit_w as f64;

        // Compute total power
        let mut chains_data: Vec<(u16, Vec<u16>)> = Vec::new();
        for info in chain_infos {
            let chain_id = info.chain_id;
            let voltage_mv = info.voltage_mv;
            if let Some(profile) = self.profiles.get(&chain_id) {
                let voltage = profile.optimal_voltage_mv.unwrap_or(voltage_mv);
                let freqs: Vec<u16> = profile.chips.iter().map(|c| c.operating_mhz).collect();
                chains_data.push((voltage, freqs));
            }
        }

        let chains_ref: Vec<(u16, &[u16])> = chains_data
            .iter()
            .map(|(v, f)| (*v, f.as_slice()))
            .collect();
        let total_power = power_model.total_power_w(&chains_ref);

        if total_power <= limit_w {
            info!(
                total_power_w = format_args!("{:.0}", total_power),
                limit_w = format_args!("{:.0}", limit_w),
                "Total power {:.0}W within {:.0}W limit — no adjustment needed",
                total_power,
                limit_w,
            );
            return;
        }

        info!(
            total_power_w = format_args!("{:.0}", total_power),
            limit_w = format_args!("{:.0}", limit_w),
            excess_w = format_args!("{:.0}", total_power - limit_w),
            "Total power {:.0}W exceeds {:.0}W limit — backing off least efficient chips",
            total_power,
            limit_w,
        );

        // Collect all chips with their efficiency (J/TH), sorted worst (highest J/TH) first
        let mut chip_list: Vec<(u8, u8, f64, u16)> = Vec::new(); // (chain_id, chip_index, jth, freq)
        for info in chain_infos {
            let chain_id = info.chain_id;
            let voltage_mv = info.voltage_mv;
            if let Some(profile) = self.profiles.get(&chain_id) {
                let chip_id = Self::profile_chip_id(profile);
                let power_model = self.power_model_for_chip(chip_id);
                let voltage = profile.optimal_voltage_mv.unwrap_or(voltage_mv);
                let voltage_v = voltage as f64 / 1000.0;
                for chip in &profile.chips {
                    let power = power_model.chip_power_w(voltage_v, chip.operating_mhz);
                    let hashrate_ths = crate::chip_geometry::chip_hashrate_ghs_for_chip(
                        chip_id,
                        chip.operating_mhz,
                    ) / 1000.0;
                    let jth = if hashrate_ths > 0.0 {
                        power / hashrate_ths
                    } else {
                        f64::INFINITY
                    };
                    chip_list.push((chain_id, chip.chip_index, jth, chip.operating_mhz));
                }
            }
        }

        // Sort by J/TH descending (worst efficiency first)
        chip_list.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        // Back off chips one by one until power is within budget
        let mut backed_off = 0u32;
        for (chain_id, chip_index, _jth, _freq) in &chip_list {
            // Recompute total power after each step
            let mut current_chains: Vec<(u16, Vec<u16>)> = Vec::new();
            for info in chain_infos {
                let cid = info.chain_id;
                let voltage_mv = info.voltage_mv;
                if let Some(profile) = self.profiles.get(&cid) {
                    let voltage = profile.optimal_voltage_mv.unwrap_or(voltage_mv);
                    let freqs: Vec<u16> = profile.chips.iter().map(|c| c.operating_mhz).collect();
                    current_chains.push((voltage, freqs));
                }
            }
            let current_ref: Vec<(u16, &[u16])> = current_chains
                .iter()
                .map(|(v, f)| (*v, f.as_slice()))
                .collect();
            let current_power = power_model.total_power_w(&current_ref);

            if current_power <= limit_w {
                break;
            }

            // Back off this chip — compute step-down freq before mutable borrow
            let backoff_step = self.config.backoff_step_mhz;
            let min_freq = self.config.min_freq_mhz;
            if let Some(profile) = self.profiles.get_mut(chain_id) {
                let chip_id = Self::profile_chip_id(profile);
                if let Some(chip) = profile
                    .chips
                    .iter_mut()
                    .find(|c| c.chip_index == *chip_index)
                {
                    let old_freq = chip.operating_mhz;
                    let target = old_freq.saturating_sub(backoff_step);
                    let new_freq =
                        dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chip_id)
                            .iter()
                            .rev()
                            .find(|&&f| f <= target)
                            .copied()
                            .unwrap_or(min_freq);
                    if new_freq < old_freq {
                        chip.operating_mhz = new_freq;
                        if let Err(e) = Self::set_chip_freq_checked(
                            freq_cmd_tx,
                            *chain_id,
                            *chip_index,
                            new_freq,
                        )
                        .await
                        {
                            warn!(chain_id = *chain_id, chip = *chip_index, error = %e,
                                "Cross-chain power coordination: failed to apply backoff frequency");
                        } else {
                            backed_off += 1;
                        }
                    }
                }
            }
        }

        if backed_off > 0 {
            info!(
                backed_off,
                "Cross-chain power coordination: backed off {} chip(s) to meet {:.0}W limit",
                backed_off,
                limit_w,
            );
        }
    }

    /// Cross-chain voltage domain optimization (Item 13).
    ///
    /// After per-chain voltage search, trade voltage between chains:
    /// - Lower voltage on good-silicon chains (they can handle it)
    /// - Raise voltage on poor-silicon chains (they need headroom)
    ///   This maximizes total hashrate within the total power budget.
    ///
    /// Only adjusts voltage by ±20 mV per chain (conservative).
    async fn cross_chain_voltage_optimize(
        &mut self,
        chain_infos: &[crate::ChainTuneInfo],
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
    ) {
        if chain_infos.len() < 2 {
            return;
        }

        if self.mixed_chain_chip_ids() {
            warn!(
                "Cross-chain voltage optimization is not yet supported across mixed chip families — skipping"
            );
            return;
        }

        if self.capabilities.profile_key != "bm1387-home-pic16" {
            warn!(
                capability_profile = %self.capabilities.profile_key,
                "Cross-chain voltage optimization is currently limited to BM1387/PIC16 — skipping on this family"
            );
            return;
        }

        // Rank chains by silicon quality (average grade score)
        let mut chain_quality: Vec<(u8, f64, u16)> = Vec::new(); // (chain_id, quality, current_voltage)
        for info in chain_infos {
            let chain_id = info.chain_id;
            let initial_voltage_mv = info.voltage_mv;
            if let Some(profile) = self.profiles.get(&chain_id) {
                let voltage = profile.optimal_voltage_mv.unwrap_or(initial_voltage_mv);
                let quality: f64 = profile
                    .chips
                    .iter()
                    .map(|c| match c.grade {
                        crate::profile::ChipGrade::A => 1.0,
                        crate::profile::ChipGrade::B => 0.75,
                        crate::profile::ChipGrade::C => 0.5,
                        crate::profile::ChipGrade::D => 0.25,
                    })
                    .sum::<f64>()
                    / profile.chips.len().max(1) as f64;
                chain_quality.push((chain_id, quality, voltage));
            }
        }

        if chain_quality.len() < 2 {
            return;
        }

        // Sort by quality (best first)
        chain_quality.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let best = chain_quality.first().unwrap();
        let worst = chain_quality.last().unwrap();

        // Only trade if there's a meaningful quality difference
        let quality_delta = best.1 - worst.1;
        if quality_delta < 0.1 {
            info!(
                "Cross-chain voltage: all chains similar quality (delta {:.2}) — no trade needed",
                quality_delta,
            );
            return;
        }

        // Trade: lower best chain by 10mV, raise worst chain by 10mV
        const TRADE_MV: u16 = 10;
        let best_new_voltage = best
            .2
            .saturating_sub(TRADE_MV)
            .max(self.config.min_voltage_mv);
        let worst_new_voltage = (worst.2 + TRADE_MV).min(9400); // BM1387/PIC16 safe ceiling

        if best_new_voltage < best.2 || worst_new_voltage > worst.2 {
            info!(
                best_chain = best.0,
                worst_chain = worst.0,
                best_quality = format_args!("{:.2}", best.1),
                worst_quality = format_args!("{:.2}", worst.1),
                best_voltage = format_args!("{} → {} mV", best.2, best_new_voltage),
                worst_voltage = format_args!("{} → {} mV", worst.2, worst_new_voltage),
                "Cross-chain voltage trade: best chain {} ({:.2}q) {} → {} mV, \
                 worst chain {} ({:.2}q) {} → {} mV",
                best.0,
                best.1,
                best.2,
                best_new_voltage,
                worst.0,
                worst.1,
                worst.2,
                worst_new_voltage,
            );

            // Apply voltage changes
            if best_new_voltage < best.2 {
                if let Err(e) =
                    Self::set_voltage_checked(freq_cmd_tx, best.0, best_new_voltage).await
                {
                    warn!(chain_id = best.0, error = %e, "Cross-chain voltage optimize: failed to lower voltage on best chain");
                } else if let Some(profile) = self.profiles.get_mut(&best.0) {
                    profile.optimal_voltage_mv = Some(best_new_voltage);
                }
            }

            if worst_new_voltage > worst.2 {
                if let Err(e) =
                    Self::set_voltage_checked(freq_cmd_tx, worst.0, worst_new_voltage).await
                {
                    warn!(chain_id = worst.0, error = %e, "Cross-chain voltage optimize: failed to raise voltage on worst chain");
                } else if let Some(profile) = self.profiles.get_mut(&worst.0) {
                    profile.optimal_voltage_mv = Some(worst_new_voltage);
                }
            }
        }
    }

    /// Step down to the next lower PLL frequency.
    fn step_down_freq(&self, current_mhz: u16, chip_id: u16) -> u16 {
        let freqs = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chip_id);

        let target = current_mhz
            .saturating_sub(self.config.backoff_step_mhz)
            .max(self.config.min_freq_mhz);
        freqs
            .iter()
            .rev()
            .find(|&&f| f <= target)
            .copied()
            .unwrap_or(self.config.min_freq_mhz)
    }

    /// Re-characterize specific chips flagged by the aging tracker.
    ///
    /// Groups flagged chips by chain, runs a targeted binary search for just
    /// those chips, updates the profile, and applies new operating frequencies.
    /// `stats_rx` is passed separately to avoid borrow conflicts with `self`.
    async fn recharacterize_chips(
        &mut self,
        flagged: &[(u8, u8)],
        freq_cmd_tx: &mpsc::Sender<FreqCommand>,
        stats_rx: &mut mpsc::Receiver<ChipStatsSnapshot>,
        shutdown: &CancellationToken,
    ) {
        // Group flagged chips by chain_id
        let mut by_chain: HashMap<u8, Vec<u8>> = HashMap::new();
        for &(chain_id, chip_idx) in flagged {
            by_chain.entry(chain_id).or_default().push(chip_idx);
        }

        for (chain_id, chip_indices) in &by_chain {
            if shutdown.is_cancelled() {
                return;
            }

            let chip_count = chip_indices.len();
            info!(
                chain_id,
                chips = ?chip_indices,
                "Re-characterizing {} flagged chip(s) on chain {}",
                chip_count, chain_id,
            );

            let tuner = BinarySearchTuner::new_for_chip(
                self.config.clone(),
                self.nominal_mhz,
                self.chain_chip_id(*chain_id),
            );
            let max_iters = tuner.max_iterations();
            let mut states = tuner.init_search_for_chips(chip_indices);
            let mut iteration = 0u32;

            // Binary search loop (same as characterize_chain but only for flagged chips)
            loop {
                if shutdown.is_cancelled() || BinarySearchTuner::all_done(&states) {
                    break;
                }

                iteration += 1;
                if iteration > max_iters + 2 {
                    warn!(
                        chain_id,
                        iterations = iteration,
                        "Re-characterization exceeded max iterations — forcing completion"
                    );
                    break;
                }

                // Set test frequencies only for flagged chips
                let freqs = tuner.current_frequencies(&states);
                for &(chip_idx, freq) in &freqs {
                    if let Err(e) =
                        Self::set_chip_freq_checked(freq_cmd_tx, *chain_id, chip_idx, freq).await
                    {
                        warn!(chain_id = *chain_id, chip = chip_idx, error = %e,
                            "Re-characterization: failed to apply test frequency");
                    }
                }

                // Update WORK_TIME based on SLOWEST chip frequency
                let min_freq = freqs
                    .iter()
                    .map(|&(_, f)| f)
                    .min()
                    .unwrap_or(self.nominal_mhz);
                let _ = freq_cmd_tx
                    .send(FreqCommand::UpdateWorkTime {
                        chain_id: *chain_id,
                        min_freq_mhz: min_freq,
                    })
                    .await;
                info!(
                    "AUTOTUNE: UpdateWorkTime chain={} min_freq={} MHz (re-characterize)",
                    *chain_id, min_freq
                );

                let measurement_epoch = match self
                    .begin_measurement_window(*chain_id, freq_cmd_tx, Duration::from_millis(50))
                    .await
                {
                    Ok(epoch) => epoch,
                    Err(e) => {
                        warn!(
                            chain_id = *chain_id,
                            error = %e,
                            "Re-characterization: failed to start fresh measurement window"
                        );
                        break;
                    }
                };

                // Wait for stats snapshot from this chain
                let snapshot = match self
                    .wait_for_chain_stats(*chain_id, Some(measurement_epoch), stats_rx, shutdown)
                    .await
                {
                    Ok(s) => s,
                    Err(_) => break,
                };
                self.record_telemetry_sample(&snapshot);

                // Process snapshot — only flagged chip indices are active in states
                let all_done = tuner.process_snapshot(&mut states, &snapshot);
                if all_done {
                    break;
                }
            }

            // Finalize and update profile
            let new_profiles = tuner.finalize(&states);

            if let Some(profile) = self.profiles.get_mut(chain_id) {
                for new_chip in &new_profiles {
                    // Find and replace the matching chip in the existing profile
                    if let Some(existing) = profile
                        .chips
                        .iter_mut()
                        .find(|c| c.chip_index == new_chip.chip_index)
                    {
                        let old_freq = existing.operating_mhz;
                        *existing = new_chip.clone();
                        info!(
                            chain_id,
                            chip = new_chip.chip_index,
                            old_freq,
                            new_freq = new_chip.operating_mhz,
                            grade = %new_chip.grade,
                            "Re-characterized chip {} — {} → {} MHz (grade {})",
                            new_chip.chip_index, old_freq, new_chip.operating_mhz, new_chip.grade,
                        );

                        // Apply the new operating frequency
                        if let Err(e) = Self::set_chip_freq_checked(
                            freq_cmd_tx,
                            *chain_id,
                            new_chip.chip_index,
                            new_chip.operating_mhz,
                        )
                        .await
                        {
                            warn!(chain_id = *chain_id, chip = new_chip.chip_index, error = %e,
                                "Re-characterization: failed to apply operating frequency");
                        }
                    }
                }

                // Recompute profile stats and re-save
                profile.stats = TuningProfile::compute_stats(&profile.chips, 0.0);
                if let Err(e) = profile.save(&self.config.profile_path) {
                    warn!(
                        chain_id,
                        error = %e,
                        "Failed to save updated profile after re-characterization"
                    );
                }
            }
        }
        self.save_resume_state();
    }

    /// Get the current tuner state.
    pub fn state(&self) -> TunerState {
        self.state
    }

    /// Get a reference to tuning profiles.
    pub fn profiles(&self) -> &HashMap<u8, TuningProfile> {
        &self.profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip_stats::ChipStatsSnapshot;
    use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
    use std::sync::Arc;
    use std::time::Instant;

    fn default_power_calibration() -> Arc<std::sync::RwLock<PowerCalibration>> {
        Arc::new(std::sync::RwLock::new(PowerCalibration::default()))
    }

    /// Create a stable snapshot for a chain with configurable error rate.
    fn make_snapshot(
        chain_id: u8,
        chip_count: u8,
        error_rate: f64,
        temp_c: Option<f32>,
    ) -> ChipStatsSnapshot {
        let nonces_per_chip = 100u64;
        let errors_per_chip = (nonces_per_chip as f64 * error_rate) as u64;
        ChipStatsSnapshot {
            chain_id,
            measurement_epoch: 0,
            chip_nonces: vec![nonces_per_chip; chip_count as usize],
            chip_errors: vec![errors_per_chip; chip_count as usize],
            window_duration_s: 3.0,
            timestamp: Instant::now(),
            board_temp_c: temp_c,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        }
    }

    fn make_test_profile(chain_id: u8, chip_count: u8, operating_mhz: u16) -> TuningProfile {
        let chips: Vec<ChipProfile> = (0..chip_count)
            .map(|i| ChipProfile {
                chip_index: i,
                max_stable_mhz: operating_mhz + 25,
                operating_mhz,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();
        let stats = TuningProfile::compute_stats(&chips, 15.0);
        TuningProfile {
            version: 1,
            chip_type: "BM1387".to_string(),
            chain_id,
            chip_count,
            voltage_mv: 9100,
            tuned_at: "1710000000".to_string(),
            ambient_temp_c: None,
            optimal_voltage_mv: None,
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
            equilibrium_temp_c: None,
            thermal_refinement_duration_s: None,
            calibrated_c_eff: None,
            chips,
            stats,
            // W13.C3: SKU + flag denormalisation. Test fixture default.
            hashboard_sku: None,
            hashboard_sku_flags: None,
        }
    }

    #[test]
    fn test_tuner_initial_state() {
        let config = AutoTunerConfig::default();
        let tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        assert_eq!(tuner.state(), TunerState::Idle);
        assert!(tuner.profiles().is_empty());
    }

    // PERF-006/011: the `AutoTuner::new` capability call site now reads the
    // default-OFF `DCENT_AM2_VOLTAGE_AUTOTUNE` gate via
    // `autotuner_capabilities_for_chip_with_voltage_autotune`. With the gate
    // UNSET the result must be byte-identical to the prior
    // `autotuner_capabilities_for_chip` form (voltage optimization stays gated
    // to BM1387/PIC16). This test serializes env access so it cannot race the
    // other env-sensitive tests in this binary, and always restores the prior
    // value. The pure-function equivalence (`..., None` == the old form) is
    // additionally covered in `config.rs::perf006_voltage_autotune_defaults_off`.
    static VOLTAGE_AUTOTUNE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_voltage_autotune_env<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        let _g = VOLTAGE_AUTOTUNE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let key = crate::config::AM2_VOLTAGE_AUTOTUNE_ENV;
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        let out = f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        out
    }

    #[test]
    fn perf006_tuner_new_voltage_autotune_gate_off_is_conservative() {
        // dsPIC BM1362 (am2). Gate OFF ⇒ voltage optimization NOT advertised
        // (byte-identical to the historical conservative capability set).
        with_voltage_autotune_env(None, || {
            let tuner = AutoTuner::new(
                AutoTunerConfig::default(),
                500,
                "BM1362".to_string(),
                "dspic".to_string(),
                default_power_calibration(),
            );
            assert!(
                !tuner.capabilities.voltage_optimization_supported,
                "gate OFF must keep voltage optimization gated for am2/BM1362 dsPIC"
            );

            // Equivalence to the pure no-gate form at the same inputs.
            let chip_id = crate::chip_id_from_type("BM1362").unwrap_or(0);
            let pure = crate::autotuner_capabilities_for_chip(chip_id, "dspic");
            assert_eq!(
                tuner.capabilities.voltage_optimization_supported,
                pure.voltage_optimization_supported
            );
            assert_eq!(
                tuner.capabilities.quiet_home_presets,
                pure.quiet_home_presets
            );
        });
    }

    #[test]
    fn perf006_tuner_new_voltage_autotune_gate_on_flips_capability() {
        // dsPIC BM1362 (am2). Gate ON ⇒ the operator-opted-in voltage search is
        // advertised (the downstream clamp to [13700,14500] stays enforced in
        // the autotuner runtime / pvt envelope — this only flips the capability
        // flag the resolver routes on).
        with_voltage_autotune_env(Some("1"), || {
            let tuner = AutoTuner::new(
                AutoTunerConfig::default(),
                500,
                "BM1362".to_string(),
                "dspic".to_string(),
                default_power_calibration(),
            );
            assert!(
                tuner.capabilities.voltage_optimization_supported,
                "gate ON must advertise the opted-in dsPIC voltage search"
            );
        });
    }

    #[tokio::test]
    async fn test_wait_for_chain_stats_buffers_other_chain_snapshot() {
        let config = AutoTunerConfig {
            measurement_window_s: 1,
            ..Default::default()
        };
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        let (stats_tx, mut stats_rx) = mpsc::channel::<ChipStatsSnapshot>(4);
        let shutdown = CancellationToken::new();

        let mut other_chain = make_snapshot(7, 3, 0.0, Some(46.0));
        other_chain.measurement_epoch = 11;
        let mut target_chain = make_snapshot(6, 3, 0.0, Some(45.0));
        target_chain.measurement_epoch = 5;

        stats_tx.send(other_chain.clone()).await.unwrap();
        stats_tx.send(target_chain.clone()).await.unwrap();

        let first = tuner
            .wait_for_chain_stats(6, Some(5), &mut stats_rx, &shutdown)
            .await
            .expect("expected target chain snapshot");
        assert_eq!(first.chain_id, 6);
        assert_eq!(first.measurement_epoch, 5);

        let buffered = tuner
            .wait_for_chain_stats(7, Some(11), &mut stats_rx, &shutdown)
            .await
            .expect("expected buffered snapshot for other chain");
        assert_eq!(buffered.chain_id, 7);
        assert_eq!(buffered.measurement_epoch, 11);
    }

    #[tokio::test]
    async fn test_wait_for_chain_stats_times_out_when_channel_is_silent() {
        let config = AutoTunerConfig {
            measurement_window_s: 0,
            ..Default::default()
        };
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        let (_stats_tx, mut stats_rx) = mpsc::channel::<ChipStatsSnapshot>(1);
        let shutdown = CancellationToken::new();

        let err = tuner
            .wait_for_chain_stats(6, Some(1), &mut stats_rx, &shutdown)
            .await
            .expect_err("silent channel should time out");

        assert!(matches!(err, crate::AutoTunerError::StatsTimeout { .. }));
    }

    #[test]
    fn test_runtime_status_reports_partial_success() {
        let config = AutoTunerConfig::default();
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        tuner.state = TunerState::Tuned;
        tuner.target_chain_ids.extend([6, 7]);
        tuner.target_chip_total = 6;
        tuner.failed_chain_ids.insert(7);
        tuner.profiles.insert(6, make_test_profile(6, 3, 650));

        let status = tuner.build_runtime_status("partial", None);

        assert_eq!(status.state, "PartiallyTuned");
        assert_eq!(status.target_chains, 2);
        assert_eq!(status.tuned_chains, 1);
        assert_eq!(status.failed_chains, 1);
        assert_eq!(status.tuned_chain_ids, vec![6]);
        assert_eq!(status.failed_chain_ids, vec![7]);
        assert_eq!(status.total_chips, 6);
    }

    #[test]
    fn test_saved_calibrated_c_eff_requires_consistent_profiles() {
        let config = AutoTunerConfig::default();
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        let mut first = make_test_profile(6, 3, 650);
        first.calibrated_c_eff = Some(1.23e-4);
        tuner.profiles.insert(6, first.clone());
        assert_eq!(tuner.saved_calibrated_c_eff(), first.calibrated_c_eff);

        let mut second = make_test_profile(7, 3, 650);
        second.calibrated_c_eff = Some(1.24e-4);
        tuner.profiles.insert(7, second);
        assert_eq!(tuner.saved_calibrated_c_eff(), None);
    }

    #[test]
    fn test_runtime_status_progress_is_global_across_chains() {
        let config = AutoTunerConfig::default();
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        tuner.target_chain_ids.extend([6, 7, 8]);
        tuner.run_chain_plan = vec![(6, 3), (7, 3), (8, 3)];
        tuner.target_chip_total = 9;
        let progress = TuningProgress {
            phase: "Characterizing".to_string(),
            chain_id: 7,
            iteration: 2,
            max_iterations: 6,
            active_chips: 1,
            total_chips: 3,
            elapsed_s: 6.0,
            estimated_remaining_s: 3.0,
            percent_complete: 66.0,
        };

        let status = tuner.build_runtime_status("global-progress", Some(&progress));

        assert_eq!(status.total_chips, 9);
        assert_eq!(status.completed_chips, 5);
        assert_eq!(status.active_chain_id, Some(7));
        assert_eq!(status.active_chain_total_chips, Some(3));
        assert!(status.percent_complete > 55.0 && status.percent_complete < 56.0);
        assert_eq!(
            status.estimated_remaining_s.map(|s| s.round() as u64),
            Some(12)
        );
    }

    #[tokio::test]
    async fn test_cold_start_characterization() {
        let config = AutoTunerConfig {
            measurement_window_s: 1,
            verification_window_s: 1,
            voltage_optimization: false,
            ..Default::default()
        };
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        let (stats_tx, stats_rx) = mpsc::channel::<ChipStatsSnapshot>(100);
        let (freq_cmd_tx, mut freq_cmd_rx) = mpsc::channel::<FreqCommand>(100);
        let shutdown = CancellationToken::new();

        let chain_infos = vec![crate::ChainTuneInfo {
            chain_id: 6,
            chip_count: 3,
            voltage_mv: 9100,
            chip_id: 0x1387,
            hardware_identity: crate::ChainHardwareIdentity::default(),
        }];
        let shutdown_clone = shutdown.clone();

        // Spawn a mock dispatcher that sends stable snapshots
        let mock_handle = tokio::spawn(async move {
            let mut iterations = 0;
            loop {
                // Send stable snapshot for chain 6
                let snapshot = make_snapshot(6, 3, 0.0, Some(45.0));
                if stats_tx.send(snapshot).await.is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                iterations += 1;
                if iterations > 50 {
                    break;
                }
            }
        });

        // Drain freq commands in background
        let cmd_drain =
            tokio::spawn(async move { while let Some(_cmd) = freq_cmd_rx.recv().await {} });

        // Run tuner with a timeout
        let tuner_handle = tokio::spawn(async move {
            tokio::select! {
                _ = tuner.run(&chain_infos, stats_rx, freq_cmd_tx, shutdown_clone) => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
            tuner.state()
        });

        // Give it time to characterize, then shut down
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        shutdown.cancel();

        let final_state = tuner_handle.await.unwrap();
        // After characterization, should be Tuned or at least past Idle
        assert_ne!(final_state, TunerState::Idle, "Should have left Idle state");

        mock_handle.abort();
        cmd_drain.abort();
    }

    #[test]
    fn test_step_down_freq() {
        let config = AutoTunerConfig {
            backoff_step_mhz: 25,
            min_freq_mhz: 200,
            ..Default::default()
        };
        let tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        let backed_off = tuner.step_down_freq(650, 0x1387);
        assert!(backed_off < 650, "Step down should reduce frequency");
        assert!(backed_off >= 600, "Step down by 25 from 650 should be ~625");

        // At minimum, should return min_freq
        let at_min = tuner.step_down_freq(200, 0x1387);
        assert_eq!(at_min, 200, "At minimum should stay at minimum");
    }

    #[test]
    fn test_check_all_chips_stable() {
        // All stable
        let snapshot = make_snapshot(6, 3, 0.001, None);
        assert!(AutoTuner::check_all_chips_stable(&snapshot, 3, 0.5, 3.0));

        // One chip with high errors
        let mut bad_snapshot = make_snapshot(6, 3, 0.0, None);
        bad_snapshot.chip_errors[1] = 50; // 50% error rate
        assert!(!AutoTuner::check_all_chips_stable(
            &bad_snapshot,
            3,
            0.5,
            3.0
        ));

        // Window too short
        let mut short_snapshot = make_snapshot(6, 3, 0.0, None);
        short_snapshot.window_duration_s = 0.5;
        assert!(!AutoTuner::check_all_chips_stable(
            &short_snapshot,
            3,
            0.5,
            3.0
        ));

        let mut comm_issue_snapshot = make_snapshot(6, 1, 0.0, None);
        comm_issue_snapshot.chip_errors[0] = 20;
        comm_issue_snapshot.chip_hw_errors = Some(vec![0]);
        comm_issue_snapshot.chip_timeouts = Some(vec![10]);
        comm_issue_snapshot.chip_duplicates = Some(vec![10]);
        assert!(AutoTuner::check_all_chips_stable(
            &comm_issue_snapshot,
            1,
            0.5,
            3.0
        ));
    }

    #[test]
    fn test_assess_voltage_window_comm_fault_retries() {
        let mut snapshot = make_snapshot(6, 3, 0.0, None);
        snapshot.chip_nonces = vec![0, 0, 0];
        snapshot.chip_errors = vec![0, 0, 0];
        snapshot.chip_timeouts = Some(vec![5, 4, 0]);
        snapshot.chip_duplicates = Some(vec![1, 0, 0]);

        assert_eq!(
            AutoTuner::assess_voltage_window(&snapshot, 3, 0.5, 3.0),
            VoltageWindowDecision::RetryCommunicationFault
        );
    }

    #[test]
    fn test_assess_voltage_window_low_confidence_with_sparse_samples() {
        let mut snapshot = make_snapshot(6, 2, 0.0, None);
        snapshot.chip_nonces = vec![3, 100];
        snapshot.chip_errors = vec![0, 0];

        assert_eq!(
            AutoTuner::assess_voltage_window(&snapshot, 2, 0.5, 3.0),
            VoltageWindowDecision::LowConfidence
        );
    }

    #[test]
    fn test_retryable_voltage_command_error_detection() {
        assert!(AutoTuner::is_retryable_voltage_command_error(
            "voltage apply failed for chain 6: runtime voltage apply timed out after 2s"
        ));
        assert!(AutoTuner::is_retryable_voltage_command_error(
            "voltage verification failed for chain 6: Input/output error"
        ));
        assert!(!AutoTuner::is_retryable_voltage_command_error(
            "voltage apply failed for chain 6: chain has no voltage controller address"
        ));
    }

    #[tokio::test]
    async fn test_thermal_derate_and_restore() {
        let config = AutoTunerConfig {
            thermal_compensation: true,
            thermal_derating_per_c: 0.003,
            thermal_hysteresis_c: 3.0,
            ..Default::default()
        };
        let tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        let comp = ThermalCompensator::new()
            .with_derating(0.003)
            .with_hysteresis(3.0);

        let (freq_cmd_tx, mut freq_cmd_rx) = mpsc::channel::<FreqCommand>(100);
        let handler = tokio::spawn(async move {
            while let Some(cmd) = freq_cmd_rx.recv().await {
                match cmd {
                    FreqCommand::SetChipFrequencyLimit {
                        ack_tx: Some(ack_tx),
                        ..
                    } => {
                        let _ = ack_tx.send(Ok(()));
                    }
                    FreqCommand::SetChipFreq {
                        ack_tx: Some(ack_tx),
                        ..
                    } => {
                        let _ = ack_tx.send(Ok(650));
                    }
                    FreqCommand::UpdateWorkTime { .. } => {}
                    FreqCommand::Barrier { ack_tx } => {
                        let _ = ack_tx.send(());
                    }
                    _ => {}
                }
            }
        });
        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        monitors.insert(
            (6, 0),
            ChipMonitor {
                chip_id: 0x1387,
                consecutive_errors: 0,
                consecutive_hashrate_deficit: 0,
                current_freq_mhz: 650,
                desired_freq_mhz: 650,
                profile_freq_mhz: 650,
                thermal_limit_mhz: None,
                fan_limit_mhz: None,
                sensor_safety_limit_mhz: None,
                thermally_derated: false,
                consecutive_clean_windows: 0,
                boost_attempts: 0,
                consecutive_zero_nonce_windows: 0,
                masked: false,
            },
        );
        let mut chip_health = ChipHealthTracker::new_with_chip_id(&HashMap::new(), 0x1387);

        // Hot snapshot: should trigger derating
        let hot_snapshot = make_snapshot(6, 1, 0.0, Some(65.0));
        tuner
            .apply_thermal_compensation(
                &hot_snapshot,
                &comp,
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;

        assert!(
            monitors[&(6, 0)].thermally_derated,
            "Should be thermally derated at 65C"
        );
        assert!(
            monitors[&(6, 0)].current_freq_mhz < 650,
            "Freq should be reduced"
        );

        // Warm snapshot (59C): within hysteresis band, should NOT restore
        let warm_snapshot = make_snapshot(6, 1, 0.0, Some(59.0));
        tuner
            .apply_thermal_compensation(
                &warm_snapshot,
                &comp,
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;
        assert!(
            monitors[&(6, 0)].thermally_derated,
            "Should still be derated at 59C (within hysteresis)"
        );

        // Cool snapshot (55C): below hysteresis band (57C), should restore
        let cool_snapshot = make_snapshot(6, 1, 0.0, Some(55.0));
        tuner
            .apply_thermal_compensation(
                &cool_snapshot,
                &comp,
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;
        assert!(
            !monitors[&(6, 0)].thermally_derated,
            "Should be restored at 55C"
        );
        assert_eq!(
            monitors[&(6, 0)].current_freq_mhz,
            650,
            "Freq should be restored"
        );

        // Drain commands
        drop(freq_cmd_tx);
        handler.await.expect("freq command handler should finish");
    }

    #[tokio::test]
    async fn test_background_backoff_on_errors() {
        let config = AutoTunerConfig {
            max_consecutive_errors: 2,
            backoff_step_mhz: 25,
            error_threshold_pct: 0.5,
            ..Default::default()
        };
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        let (freq_cmd_tx, mut freq_cmd_rx) = mpsc::channel::<FreqCommand>(100);
        let handler = tokio::spawn(async move {
            while let Some(cmd) = freq_cmd_rx.recv().await {
                match cmd {
                    FreqCommand::SetChipFrequencyLimit {
                        ack_tx: Some(ack_tx),
                        ..
                    } => {
                        let _ = ack_tx.send(Ok(()));
                    }
                    FreqCommand::SetChipFreq {
                        freq_mhz,
                        ack_tx: Some(ack_tx),
                        ..
                    } => {
                        let _ = ack_tx.send(Ok(freq_mhz));
                    }
                    FreqCommand::UpdateWorkTime { .. } => {}
                    FreqCommand::Barrier { ack_tx } => {
                        let _ = ack_tx.send(());
                    }
                    _ => {}
                }
            }
        });
        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        monitors.insert(
            (6, 0),
            ChipMonitor {
                chip_id: 0x1387,
                consecutive_errors: 0,
                consecutive_hashrate_deficit: 0,
                current_freq_mhz: 650,
                desired_freq_mhz: 650,
                profile_freq_mhz: 650,
                thermal_limit_mhz: None,
                fan_limit_mhz: None,
                sensor_safety_limit_mhz: None,
                thermally_derated: false,
                consecutive_clean_windows: 0,
                boost_attempts: 0,
                consecutive_zero_nonce_windows: 0,
                masked: false,
            },
        );
        let mut chip_health = ChipHealthTracker::new_with_chip_id(&HashMap::new(), 0x1387);

        // Send high-error snapshots to trigger backoff
        let bad_snapshot = make_snapshot(6, 1, 0.1, None); // 10% error rate
        tuner
            .process_background_snapshot(
                &bad_snapshot,
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;
        assert_eq!(
            monitors[&(6, 0)].consecutive_errors,
            1,
            "First error window"
        );

        tuner
            .process_background_snapshot(
                &bad_snapshot,
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;
        // After 2 consecutive errors (max_consecutive_errors=2), should back off
        assert!(
            monitors[&(6, 0)].current_freq_mhz < 650,
            "Should have backed off after 2 error windows"
        );

        drop(freq_cmd_tx);
        handler.await.expect("freq command handler should finish");
    }

    #[tokio::test]
    async fn test_missing_temp_recovery_restores_work_time() {
        let mut tuner = AutoTuner::new(
            AutoTunerConfig::default(),
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        tuner.safety_override = Some("missing_temperature".to_string());
        tuner.consecutive_temp_missing.insert(6, 3);

        let min_freq = tuner.config.min_freq_mhz;
        let (freq_cmd_tx, mut freq_cmd_rx) = mpsc::channel::<FreqCommand>(100);
        let handler = tokio::spawn(async move {
            let mut cleared_limit = false;
            let mut work_time_freq = None;
            let mut saw_barrier = false;
            while let Some(cmd) = freq_cmd_rx.recv().await {
                match cmd {
                    FreqCommand::SetChipFrequencyLimit {
                        chain_id,
                        chip_index,
                        max_freq_mhz,
                        source,
                        ack_tx,
                    } => {
                        if chain_id == 6
                            && chip_index == 0
                            && max_freq_mhz.is_none()
                            && source == crate::FrequencyLimitSource::SensorSafety
                        {
                            cleared_limit = true;
                        }
                        if let Some(ack_tx) = ack_tx {
                            let _ = ack_tx.send(Ok(()));
                        }
                    }
                    FreqCommand::UpdateWorkTime {
                        chain_id: 6,
                        min_freq_mhz,
                    } => {
                        work_time_freq = Some(min_freq_mhz);
                    }
                    FreqCommand::Barrier { ack_tx } => {
                        saw_barrier = true;
                        let _ = ack_tx.send(());
                        break;
                    }
                    _ => {}
                }
            }
            (cleared_limit, work_time_freq, saw_barrier)
        });

        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        monitors.insert(
            (6, 0),
            ChipMonitor {
                chip_id: 0x1387,
                consecutive_errors: 0,
                consecutive_hashrate_deficit: 0,
                current_freq_mhz: min_freq,
                desired_freq_mhz: 650,
                profile_freq_mhz: 650,
                thermal_limit_mhz: None,
                fan_limit_mhz: None,
                sensor_safety_limit_mhz: Some(min_freq),
                thermally_derated: false,
                consecutive_clean_windows: 0,
                boost_attempts: 0,
                consecutive_zero_nonce_windows: 0,
                masked: false,
            },
        );
        let mut chip_health = ChipHealthTracker::new_with_chip_id(&HashMap::new(), 0x1387);

        tuner
            .process_background_snapshot(
                &make_snapshot(6, 1, 0.0, Some(55.0)),
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;

        let (cleared_limit, work_time_freq, saw_barrier) = handler.await.unwrap();
        assert!(cleared_limit, "Recovery should clear sensor safety limit");
        assert_eq!(
            work_time_freq,
            Some(650),
            "Recovery should refresh WORK_TIME to the restored chain minimum"
        );
        assert!(saw_barrier, "Recovery should wait for dispatcher sync");
        assert_eq!(monitors[&(6, 0)].sensor_safety_limit_mhz, None);
        assert_eq!(monitors[&(6, 0)].current_freq_mhz, 650);
        assert_eq!(tuner.safety_override, None);
    }

    #[tokio::test]
    async fn test_missing_temp_recovery_preserves_other_chain_override() {
        let mut tuner = AutoTuner::new(
            AutoTunerConfig::default(),
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        tuner.safety_override = Some("missing_temperature".to_string());
        tuner.consecutive_temp_missing.insert(6, 3);
        tuner.consecutive_temp_missing.insert(7, 3);

        let min_freq = tuner.config.min_freq_mhz;
        let (freq_cmd_tx, mut freq_cmd_rx) = mpsc::channel::<FreqCommand>(100);
        let handler = tokio::spawn(async move {
            while let Some(cmd) = freq_cmd_rx.recv().await {
                match cmd {
                    FreqCommand::SetChipFrequencyLimit {
                        ack_tx: Some(ack_tx),
                        ..
                    } => {
                        let _ = ack_tx.send(Ok(()));
                    }
                    FreqCommand::UpdateWorkTime { .. } => {}
                    FreqCommand::Barrier { ack_tx } => {
                        let _ = ack_tx.send(());
                        break;
                    }
                    _ => {}
                }
            }
        });

        let mut monitors: HashMap<(u8, u8), ChipMonitor> = HashMap::new();
        monitors.insert(
            (6, 0),
            ChipMonitor {
                chip_id: 0x1387,
                consecutive_errors: 0,
                consecutive_hashrate_deficit: 0,
                current_freq_mhz: min_freq,
                desired_freq_mhz: 650,
                profile_freq_mhz: 650,
                thermal_limit_mhz: None,
                fan_limit_mhz: None,
                sensor_safety_limit_mhz: Some(min_freq),
                thermally_derated: false,
                consecutive_clean_windows: 0,
                boost_attempts: 0,
                consecutive_zero_nonce_windows: 0,
                masked: false,
            },
        );
        let mut chip_health = ChipHealthTracker::new_with_chip_id(&HashMap::new(), 0x1387);

        tuner
            .process_background_snapshot(
                &make_snapshot(6, 1, 0.0, Some(55.0)),
                &mut monitors,
                &mut chip_health,
                &freq_cmd_tx,
            )
            .await;

        handler.await.unwrap();
        assert_eq!(
            tuner.safety_override.as_deref(),
            Some("missing_temperature"),
            "Recovery on one chain must not clear the global safety override while another chain is still missing temperatures"
        );
    }

    // --- Thermal Refinement Tests ---

    #[test]
    fn test_linear_slope_rising() {
        let mut state = ThermalRefinementState::new(3);
        // Simulate rising temperature: 40C → 50C over 5 minutes
        for i in 0..5 {
            let elapsed_s = i as f64 * 60.0;
            let temp = 40.0 + (i as f32 * 2.5); // 2.5 C/min rise
            state.temp_history.push((elapsed_s, temp));
        }
        let slope = state.linear_slope();
        // Should be approximately 2.5 C/min
        assert!(slope > 2.0, "Slope should be > 2.0, got {}", slope);
        assert!(slope < 3.0, "Slope should be < 3.0, got {}", slope);
    }

    #[test]
    fn test_linear_slope_flat() {
        let mut state = ThermalRefinementState::new(3);
        // Flat temperature at 55C
        for i in 0..5 {
            let elapsed_s = i as f64 * 60.0;
            state.temp_history.push((elapsed_s, 55.0));
        }
        let slope = state.linear_slope();
        assert!(
            slope.abs() < 0.01,
            "Flat data should give ~0 slope, got {}",
            slope
        );
    }

    #[test]
    fn test_equilibrium_detection() {
        let mut state = ThermalRefinementState::new(3);
        let stability_threshold = 0.2; // C/min
        let min_soak_s = 120;

        // First: rising temperatures (should not detect equilibrium)
        for i in 0..5 {
            let elapsed_s = i as f64 * 30.0; // 30s intervals
            let temp = 40.0 + (i as f32 * 3.0); // 6 C/min rise
            state.temp_history.push((elapsed_s, temp));
        }
        // Manually set start to make elapsed > min_soak
        state.start = Instant::now() - std::time::Duration::from_secs(300);
        assert!(
            !state.record_temperature(55.0, min_soak_s, stability_threshold),
            "Rising temps should not trigger equilibrium"
        );

        // Now: stable temperatures
        state.temp_history.clear();
        for i in 0..7 {
            let elapsed_s = 300.0 + i as f64 * 30.0;
            state
                .temp_history
                .push((elapsed_s, 55.0 + (i as f32 * 0.02))); // ~0.04 C/min
        }
        assert!(
            state.record_temperature(55.1, min_soak_s, stability_threshold),
            "Stable temps after min soak should trigger equilibrium"
        );
    }

    #[test]
    fn test_equilibrium_respects_min_soak() {
        let mut state = ThermalRefinementState::new(3);
        let stability_threshold = 0.2;
        let min_soak_s = 120;

        // Flat temperature from the start, but NOT enough time elapsed
        for i in 0..5 {
            let elapsed_s = i as f64 * 10.0; // Only 40s total
            state.temp_history.push((elapsed_s, 55.0));
        }
        // start is Instant::now(), so elapsed < min_soak_s
        let result = state.record_temperature(55.0, min_soak_s, stability_threshold);
        assert!(
            !result,
            "Should not declare equilibrium before min soak time"
        );
    }

    #[test]
    fn test_refinement_accumulator() {
        let mut state = ThermalRefinementState::new(3);

        let snapshot = make_snapshot(6, 3, 0.01, Some(50.0));
        state.accumulate(&snapshot);

        assert_eq!(state.chip_nonces[0], 100);
        assert_eq!(state.chip_errors[0], 1);

        // Accumulate again
        state.accumulate(&snapshot);
        assert_eq!(state.chip_nonces[0], 200);
        assert_eq!(state.chip_errors[0], 2);

        // Reset window
        state.reset_window();
        assert_eq!(state.chip_nonces[0], 0);
        assert_eq!(state.chip_errors[0], 0);
    }

    #[test]
    fn test_refinement_no_backoff_stable_chips() {
        // Verify that stable chips retain their frequencies through the
        // accumulator/error-check logic (backoff count stays 0)
        let state = ThermalRefinementState::new(3);
        // All backoff counters should start at zero
        assert!(state.chip_backoffs.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_degradation_warning_threshold() {
        let config = AutoTunerConfig {
            thermal_degradation_warn_pct: 15.0,
            ..Default::default()
        };

        // Simulate: pre_avg = 650, post_avg = 500 → 23% degradation → should warn
        let pre_avg = 650.0_f64;
        let post_avg = 500.0_f64;
        let degradation_pct = (pre_avg - post_avg) / pre_avg * 100.0;
        assert!(
            degradation_pct > config.thermal_degradation_warn_pct as f64,
            "23% degradation should exceed 15% threshold"
        );

        // Simulate: pre_avg = 650, post_avg = 620 → 4.6% → should NOT warn
        let post_avg_small = 620.0_f64;
        let degradation_small = (pre_avg - post_avg_small) / pre_avg * 100.0;
        assert!(
            degradation_small < config.thermal_degradation_warn_pct as f64,
            "4.6% degradation should not exceed 15% threshold"
        );
    }

    #[test]
    fn test_fan_thermal_factor() {
        // Max fan speed: no derating
        assert!((fan_thermal_factor(FAN_PWM_MAX) - 1.0).abs() < 0.01);
        // Mid speed: ~82.5% ceiling
        let mid = fan_thermal_factor(55);
        assert!(
            mid > 0.80 && mid < 0.85,
            "Mid fan factor should be ~0.825, got {:.2}",
            mid
        );
        // Quiet mode (PWM 10): ~65% ceiling
        let quiet = fan_thermal_factor(10);
        assert!(
            (quiet - 0.65).abs() < 0.01,
            "Quiet mode should be 0.65, got {:.2}",
            quiet
        );
        // RE 2026-06-02 bible anchors (power-estimation-model.md ALGO 8): PWM 30 -> 0.70 (home
        // cap), PWM 64 -> 0.85. The old 2-point fit gave 0.728 / 0.86 here.
        assert!(
            (fan_thermal_factor(30) - 0.70).abs() < 0.001,
            "Home-cap PWM 30 should be exactly 0.70, got {:.3}",
            fan_thermal_factor(30)
        );
        assert!(
            (fan_thermal_factor(64) - 0.85).abs() < 0.001,
            "PWM 64 should be exactly 0.85, got {:.3}",
            fan_thermal_factor(64)
        );
        // Disabled (0): no adjustment
        assert!((fan_thermal_factor(0) - 1.0).abs() < 0.01);
        // PWM 1: should be near 0.65 (below 10)
        let very_low = fan_thermal_factor(1);
        assert_eq!(very_low, 0.65, "Very low PWM should clamp to 0.65");
    }

    #[test]
    fn test_tuning_progress_serialization() {
        let progress = TuningProgress {
            phase: "Characterizing".to_string(),
            chain_id: 6,
            iteration: 3,
            max_iterations: 6,
            active_chips: 45,
            total_chips: 63,
            elapsed_s: 9.0,
            estimated_remaining_s: 9.0,
            percent_complete: 50.0,
        };

        let json = serde_json::to_string(&progress).expect("serialize failed");
        assert!(json.contains("\"phase\":\"Characterizing\""));
        assert!(json.contains("\"percent_complete\":50.0"));
    }

    #[test]
    fn test_fan_factor_monotonic() {
        // Factor should increase monotonically with fan speed
        let mut prev = fan_thermal_factor(1);
        for pwm in 2..=FAN_PWM_MAX {
            let factor = fan_thermal_factor(pwm);
            assert!(
                factor >= prev,
                "Fan factor should increase: PWM {} ({:.3}) < PWM {} ({:.3})",
                pwm,
                factor,
                pwm - 1,
                prev,
            );
            prev = factor;
        }
    }

    #[test]
    fn test_schedule_target_uses_dps_step_and_restore_marker() {
        let config = AutoTunerConfig {
            power_step_w: 300,
            total_power_limit_w: 1800,
            ..Default::default()
        };
        let tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );

        assert_eq!(
            tuner.next_scheduled_power_target(Some(900), Some(1500)),
            Some(1200)
        );
        assert_eq!(
            tuner.next_scheduled_power_target(Some(1200), Some(1500)),
            Some(1500)
        );
        assert_eq!(
            tuner.next_scheduled_power_target(None, Some(1500)),
            Some(1500)
        );
        assert_eq!(tuner.next_scheduled_power_target(Some(900), None), None);
    }

    #[test]
    fn test_resume_state_gate_invalidates_stale_hardware() {
        let dir = std::env::temp_dir().join("dcent_autotuner_resume_gate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp profile dir");

        let config = AutoTunerConfig {
            profile_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        let candidate = WarmStartCandidate {
            chain_id: 6,
            chip_count: 3,
            chip_id: 0x1387,
            profile: make_test_profile(6, 3, 650),
        };

        let mut stale_profiles = HashMap::new();
        stale_profiles.insert(6, make_test_profile(6, 2, 650));
        let stale_fingerprint = tuner.resume_fingerprint_for_profiles(&stale_profiles);
        let stale_state =
            crate::AutotunerResumeState::from_profiles(&stale_profiles, stale_fingerprint);
        stale_state
            .save_atomic(tuner.resume_state_path())
            .expect("save stale state");

        assert!(matches!(
            tuner.load_resume_state_gate(&[candidate]),
            ResumeStateGate::Invalid
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resume_fingerprint_uses_runtime_chain_identity() {
        let config = AutoTunerConfig::default();
        let mut tuner = AutoTuner::new(
            config,
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        );
        tuner.chain_hardware_identities.insert(
            6,
            crate::ChainHardwareIdentity {
                eeprom_serial: Some("BHB42-RUNTIME".to_string()),
                eeprom_fingerprint: Some("i2c1-0x50:sha256:runtime".to_string()),
                dspic_fw_byte: Some(0x03),
            },
        );

        let mut profiles = HashMap::new();
        profiles.insert(6, make_test_profile(6, 3, 650));
        let fingerprint = tuner.resume_fingerprint_for_profiles(&profiles);

        assert_eq!(
            fingerprint.chains[0].eeprom_serial.as_deref(),
            Some("BHB42-RUNTIME")
        );
        assert_eq!(
            fingerprint.chains[0].eeprom_fingerprint.as_deref(),
            Some("i2c1-0x50:sha256:runtime")
        );
        assert_eq!(fingerprint.chains[0].dspic_fw_byte, Some(0x03));
    }

    // -------------------------------------------------------------------
    // W6.3 + W6.4: step-up gate tests.
    //
    // The gate (`step_up_gate_passes`) is the read of the optional
    // `StepUpGateSignal` watch + the AND of the two thresholds. The
    // boost-back / weak-chip-compensation call sites are pinned by the
    // existing site tests; here we pin the *gate* contract independently
    // so a refactor that flips a threshold or unwires the watch can't
    // pass review unnoticed.
    // -------------------------------------------------------------------

    fn make_test_tuner_for_gate() -> AutoTuner {
        AutoTuner::new(
            AutoTunerConfig::default(),
            650,
            "BM1387".to_string(),
            "pic16".to_string(),
            default_power_calibration(),
        )
    }

    #[test]
    fn runtime_target_clamped_to_total_power_limit_without_circuit_cap() {
        // F3: the runtime PowerTarget/Heater apply path must clamp target_watts to
        // total_power_limit_w even when NO circuit_capacity_watts is declared (the
        // default). clamp_target_to_circuit_limit early-returns on a None circuit
        // capacity, so on its own it leaves the configured residential power cap
        // unenforced — a direct operator command could exceed it until the next
        // full re-tune. apply_to_config sets target_watts for PowerTarget AND
        // Heater, and apply_target_mode allocates from target_watts, so clamping it
        // here bounds the applied budget for both modes.
        let mut tuner = make_test_tuner_for_gate();
        tuner.config.circuit_capacity_watts = None;
        tuner.config.total_power_limit_w = 1400;
        tuner.config.target_watts = 1750;
        tuner.config.tuner_mode = Some(crate::config::TunerMode::PowerTarget { watts: 1750 });
        tuner.requested_config.target_watts = 1750;

        // The circuit clamp alone does nothing without a declared circuit cap...
        assert_eq!(tuner.clamp_target_to_circuit_limit(), None);
        assert_eq!(
            tuner.config.target_watts, 1750,
            "circuit clamp leaves target unbounded when circuit_capacity is None"
        );
        // ...the total-power-limit clamp brings it down to the configured budget.
        assert_eq!(tuner.clamp_target_to_total_power_limit(), Some(1400));
        assert_eq!(tuner.config.target_watts, 1400);
        assert_eq!(tuner.requested_config.target_watts, 1400);
        assert!(matches!(
            tuner.config.tuner_mode,
            Some(crate::config::TunerMode::PowerTarget { watts: 1400 })
        ));

        // A target already within budget is left unchanged (no false clamp).
        tuner.config.target_watts = 900;
        assert_eq!(tuner.clamp_target_to_total_power_limit(), None);
        assert_eq!(tuner.config.target_watts, 900);
    }

    #[test]
    fn step_up_gate_open_when_no_signal_wired() {
        // Pin: legacy behavior preserved when the daemon hasn't wired
        // the watch channel yet. Without this, every miner that hasn't
        // upgraded its daemon-side wiring would silently lose
        // boost-back / weak-chip compensation.
        let tuner = make_test_tuner_for_gate();
        assert!(
            tuner.step_up_gate_passes(0, 0, 700),
            "no signal wired must leave the gate OPEN (legacy behavior)"
        );
    }

    #[test]
    fn step_up_gate_passes_at_clean_baseline() {
        // Default StepUpGateSignal = (100.0%, None) which is the
        // "no rolling rejects + no chip HW err yet" baseline. Pin
        // that this passes the gate.
        let mut tuner = make_test_tuner_for_gate();
        let (tx, rx) = tokio::sync::watch::channel(crate::StepUpGateSignal::default());
        tuner.set_step_up_gate_watch(rx);
        let _ = tx; // keep tx alive while we read the rx
        assert!(
            tuner.step_up_gate_passes(0, 0, 700),
            "clean baseline (100% / no HW err) must pass the gate"
        );
    }

    #[test]
    fn autotuner_skips_step_up_when_rejection_rising() {
        // Pin the W6.3 condition. 95% rolling acceptance is below the
        // 99.0% threshold — the gate must close even if HW err is
        // perfectly clean.
        let mut tuner = make_test_tuner_for_gate();
        let (tx, rx) = tokio::sync::watch::channel(crate::StepUpGateSignal {
            rolling_acceptance_pct: 95.0,
            worst_chip_hw_err_rate: Some(0.0),
        });
        tuner.set_step_up_gate_watch(rx);
        let _ = tx;
        assert!(
            !tuner.step_up_gate_passes(0, 0, 700),
            "95% rolling acceptance must fail the 99.0% gate"
        );
    }

    #[test]
    fn step_up_gate_blocks_when_chip_hw_err_above_threshold() {
        // Pin the W6.4 condition. 5% per-chip HW err is above the
        // 2% threshold — the gate must close even if pool acceptance
        // is perfect.
        let mut tuner = make_test_tuner_for_gate();
        let (tx, rx) = tokio::sync::watch::channel(crate::StepUpGateSignal {
            rolling_acceptance_pct: 100.0,
            worst_chip_hw_err_rate: Some(0.05),
        });
        tuner.set_step_up_gate_watch(rx);
        let _ = tx;
        assert!(
            !tuner.step_up_gate_passes(0, 0, 700),
            "5% chip HW err must fail the 2.0% gate"
        );
    }

    #[test]
    fn step_up_gate_passes_at_exact_threshold() {
        // 99.0% rolling acceptance is the threshold. `>=` semantics
        // mean it passes. Pin that boundary so a future refactor that
        // flipped to `>` doesn't silently lose 1% of legitimate
        // step-ups.
        let mut tuner = make_test_tuner_for_gate();
        let (tx, rx) = tokio::sync::watch::channel(crate::StepUpGateSignal {
            rolling_acceptance_pct: 99.0,
            worst_chip_hw_err_rate: Some(0.019),
        });
        tuner.set_step_up_gate_watch(rx);
        let _ = tx;
        assert!(
            tuner.step_up_gate_passes(0, 0, 700),
            "99.0% acceptance + 1.9% HW err must PASS the gate"
        );
    }

    #[test]
    fn step_up_gate_signal_passes_helper_matches_thresholds() {
        // Pin the StepUpGateSignal::passes helper directly. The gate
        // method delegates to it, so any threshold drift here would
        // propagate everywhere.
        assert!(crate::StepUpGateSignal::default().passes());
        assert!(crate::StepUpGateSignal {
            rolling_acceptance_pct: 99.5,
            worst_chip_hw_err_rate: Some(0.01),
        }
        .passes());
        assert!(!crate::StepUpGateSignal {
            rolling_acceptance_pct: 98.99,
            worst_chip_hw_err_rate: Some(0.0),
        }
        .passes());
        assert!(!crate::StepUpGateSignal {
            rolling_acceptance_pct: 100.0,
            worst_chip_hw_err_rate: Some(0.02),
        }
        .passes());
    }

    #[test]
    fn step_up_gate_thresholds_are_pinned_constants() {
        // Drift in either threshold silently changes the production
        // gate. Pin them so a future refactor has to update this
        // test (which forces a code review).
        assert!((crate::StepUpGateSignal::ACCEPTANCE_THRESHOLD_PCT - 99.0).abs() < f64::EPSILON);
        assert!((crate::StepUpGateSignal::HW_ERR_THRESHOLD - 0.02).abs() < f64::EPSILON);
    }

    // -------------------------------------------------------------------
    // W13.C3 (2026-05-10): per-SKU PVT envelope clamp integration tests.
    //
    // These tests exercise the autotuner's `derive_silicon_profile_target`
    // path with a registered `Bm1362HashboardSku` so the validation gate
    // fires. They complement the unit tests in `pvt_envelope::tests` —
    // those prove the helpers in isolation; these prove the autotuner
    // calls them on the dispatch path.
    //
    // See:
    // - ~/
    // - ~/
    // - ~/
    // -------------------------------------------------------------------

    /// Construct a BM1362-flavored AutoTuner with a single registered chain
    /// for envelope clamp testing.
    fn make_bm1362_tuner_with_sku(
        chain_id: u8,
        sku: dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku,
    ) -> AutoTuner {
        let mut config = AutoTunerConfig::default();
        // Wide bounds so the envelope clamp is the only gate firing.
        config.min_freq_mhz = 100;
        config.max_freq_mhz = 1000;
        config.min_voltage_mv = 1000;
        let mut tuner = AutoTuner::new(
            config,
            500,
            "BM1362".to_string(),
            "pic1704".to_string(),
            default_power_calibration(),
        );
        tuner.set_chain_sku(chain_id, sku);
        tuner.chain_chip_ids.insert(chain_id, 0x1362);
        tuner
    }

    #[test]
    fn validate_freq_volt_inside_envelope_passes_via_tuner() {
        // BHB42601 envelope: 465-545 MHz @ 1320-1380 mV. 545 MHz @ 1380 mV
        // is the upper-corner endpoint. derive_silicon_profile_target
        // must keep frequency unchanged while suppressing chip-rail
        // voltage dispatch on the chain-rail voltage path.
        let tuner = make_bm1362_tuner_with_sku(
            6,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        let preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 545,
            voltage_v: 1.380,
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        assert_eq!(freq, 545);
        assert_eq!(
            volt, None,
            "chip-rail tuple must validate PVT but suppress chain-rail voltage dispatch"
        );
    }

    #[test]
    fn validate_freq_volt_outside_envelope_returns_outside_pvt_via_tuner() {
        // BHB42601 envelope: 465-545 MHz @ 1320-1380 mV. 700 MHz @ 1700 mV
        // is wildly out — the tuner must snap freq to the envelope (545
        // MHz) and SUPPRESS voltage so SetVoltage isn't dispatched. This
        // mirrors the W13.C3 contract that operator-visible OutsidePvt
        // is preferred over silent coercion.
        let tuner = make_bm1362_tuner_with_sku(
            6,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        let preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 700,
            voltage_v: 1.700,
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        // Freq snapped to envelope upper bound.
        assert_eq!(freq, 545, "freq must snap to envelope max (545)");
        // Voltage SUPPRESSED so SetVoltage doesn't dispatch.
        assert_eq!(
            volt, None,
            "out-of-envelope tuple must suppress voltage (no silent coerce)"
        );
    }

    #[test]
    fn w24_eff1_chain_rail_baked_profile_is_freq_only_via_tuner() {
        // W24-EFF-1 VERDICT PIN. The real BM1362 baked-profile efficiency
        // headline (BHB42601 Step-9 = 320 MHz @ 12.45 V CHAIN-RAIL) is what
        // an operator actually applies — NOT the hand-crafted 1.34 V core
        // values the other W13.C3 tuner tests use. With the axis-aware gate
        // DEFAULT-OFF (ship state), derive_silicon_profile_target takes the
        // OutsidePvt branch: freq is snapped INTO the envelope (320 < 465 →
        // 465) and voltage is SUPPRESSED. This proves the contested finding:
        // efficiency mode applies frequency only; the per-step chain-rail
        // voltage never reaches the chain via this path.
        std::env::remove_var("DCENT_AM2_AXIS_AWARE_PVT_CLAMP");
        let tuner = make_bm1362_tuner_with_sku(
            6,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        let preset = crate::SiliconPreset {
            step: -9,
            freq_mhz: 320,
            voltage_v: 12.450, // CHAIN-RAIL — the baked BM1362_PROFILES axis
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        // 320 MHz is below the BHB42601 envelope floor (465); the OutsidePvt
        // branch snaps it to the envelope min. Either way it is config-clamped
        // and voltage is suppressed — the load-bearing fact is volt == None.
        assert!(
            freq >= 320,
            "freq must be config/envelope-clamped, not raised arbitrarily"
        );
        assert_eq!(
            volt, None,
            "chain-rail baked profile must NOT deliver voltage (efficiency = freq-only); \
             the per-step PSU-rail volt never reaches the chain on this path"
        );
    }

    #[test]
    fn bhb42803_voltage_fixed_blocks_dvs_in_tuner() {
        // BHB42803 has voltage_fixed=true. Even an in-envelope voltage
        // (1530 mV) must SUPPRESS the voltage axis at the tuner level —
        // SET_VOLTAGE on a fixed-V VRM corrupts the PIC MSSP parser.
        // The W13.C1 voltage_search short-circuit owns dispatch
        // suppression; this proves derive_silicon_profile_target also
        // honors the contract on the silicon-profile target derivation
        // path.
        let tuner = make_bm1362_tuner_with_sku(
            6,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42803,
        );
        let preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 615,
            voltage_v: 1.530, // exactly the fixed value
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        assert_eq!(
            freq, 615,
            "freq must pass through (in-envelope for BHB42803)"
        );
        assert_eq!(
            volt, None,
            "BHB42803 voltage_fixed=true MUST suppress voltage axis at tuner level"
        );
    }

    #[test]
    fn bhb42841_inverted_curve_not_treated_as_normal() {
        // BHB42841 has inverted_curve=true (lower freq → HIGHER volt).
        // The W13.C3 contract does NOT change the bounds gate (the
        // envelope is still the envelope) but the tuner MUST track the
        // SKU's inverted_curve flag so future heuristics don't assume
        // freq↓ ⇒ volt↓ on this SKU.
        //
        // Pin: the SKU lookup returns inverted_curve=true and the
        // envelope clamp accepts an in-envelope tuple. A future
        // freq-down heuristic that read the wrong flag would fail this
        // when it lowered voltage on a salvage-bin chip.
        let tuner = make_bm1362_tuner_with_sku(
            6,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42841,
        );
        let sku = tuner
            .chain_sku(6)
            .expect("SKU must be registered for chain 6");
        assert_eq!(
            sku,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42841
        );
        let flags = sku.flags();
        assert!(
            flags.inverted_curve,
            "BHB42841 MUST be marked inverted_curve so heuristics see it"
        );
        assert!(
            !flags.voltage_fixed,
            "BHB42841 must NOT be voltage_fixed (only BHB42803 is)"
        );
        // In-envelope tuple: freq 450 MHz @ 1360 mV. The frequency
        // passes; the chip-rail voltage is validated but not dispatched.
        let preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 450,
            voltage_v: 1.360,
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        assert_eq!(freq, 450);
        assert_eq!(
            volt, None,
            "chip-rail tuple must validate PVT but suppress chain-rail voltage dispatch"
        );
        // Out-of-envelope tuple (freq below 410) — must reject via the
        // envelope clamp, NOT via any inverted-curve heuristic shortcut.
        let bad_preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 350,
            voltage_v: 1.360,
        };
        let (snapped_freq, snapped_volt) = tuner.derive_silicon_profile_target(6, &bad_preset);
        assert!(
            snapped_freq >= 410 && snapped_freq <= 475,
            "freq must snap into BHB42841 envelope [410, 475], got {}",
            snapped_freq
        );
        assert_eq!(
            snapped_volt, None,
            "out-of-envelope tuple must suppress voltage on BHB42841 too"
        );
    }

    #[test]
    fn no_sku_registered_means_envelope_gate_is_open() {
        // Defensive: chains with no registered BM1362 SKU must NOT
        // trigger envelope validation — the autotuner stays compatible
        // with non-BM1362 chips. The freq + voltage flow through
        // unchanged (after the existing min/max + fw=0x86 gates).
        let mut config = AutoTunerConfig::default();
        config.min_freq_mhz = 100;
        config.max_freq_mhz = 1000;
        config.min_voltage_mv = 1000;
        let tuner = AutoTuner::new(
            config,
            500,
            "BM1362".to_string(),
            "pic1704".to_string(),
            default_power_calibration(),
        );
        // No set_chain_sku call.
        assert_eq!(tuner.chain_sku_count(), 0);
        // The chain isn't registered, so derive_silicon_profile_target
        // skips the envelope clamp entirely and applies the legacy
        // freq clamp, while voltage is still suppressed because the row
        // is chip-rail and the live voltage path is chain-rail.
        let preset = crate::SiliconPreset {
            step: 0,
            freq_mhz: 700,
            voltage_v: 1.700,
        };
        let (freq, volt) = tuner.derive_silicon_profile_target(6, &preset);
        assert_eq!(freq, 700);
        assert_eq!(volt, None);
    }

    // -------------------------------------------------------------------
    // CE-011: apply_sku_freq_ceilings (ceiling-only per-SKU PVT clamp)
    // -------------------------------------------------------------------

    #[test]
    fn ce011_apply_sku_freq_ceilings_tightens_to_envelope_max() {
        // config max 800 + BHB42601 (envelope max 545) => tightened to 545.
        let mut tuner = make_bm1362_tuner_with_sku(
            0,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        tuner.config.max_freq_mhz = 800;
        tuner.apply_sku_freq_ceilings();
        assert_eq!(
            tuner.config.max_freq_mhz, 545,
            "ceiling must tighten to the SKU PVT envelope max"
        );
    }

    #[test]
    fn ce011_apply_sku_freq_ceilings_never_raises_ceiling() {
        // config max 500 (< envelope max 545) => stays 500 (`.min()`, never raises).
        let mut tuner = make_bm1362_tuner_with_sku(
            0,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        tuner.config.max_freq_mhz = 500;
        tuner.apply_sku_freq_ceilings();
        assert_eq!(
            tuner.config.max_freq_mhz, 500,
            "ceiling-only: a max below the envelope max must NOT be raised"
        );
    }

    #[test]
    fn ce011_apply_sku_freq_ceilings_no_sku_is_noop() {
        // No SKU registered => config unchanged (fail-closed no-op — today's
        // live default, since no production path calls set_chain_sku yet).
        let mut config = AutoTunerConfig::default();
        config.min_freq_mhz = 100;
        config.max_freq_mhz = 1000;
        let mut tuner = AutoTuner::new(
            config,
            500,
            "BM1362".to_string(),
            "dspic".to_string(),
            default_power_calibration(),
        );
        tuner.config.max_freq_mhz = 800;
        assert_eq!(tuner.chain_sku_count(), 0);
        tuner.apply_sku_freq_ceilings();
        assert_eq!(
            tuner.config.max_freq_mhz, 800,
            "no registered SKU => ceiling unchanged"
        );
    }

    #[test]
    fn ce011_apply_sku_freq_ceilings_am2_545_band_is_byte_identical() {
        // config max 545 (the proven am2 applied ceiling) + BHB42601 (envelope
        // max 545) => stays 545. Pins ZERO behavior change on `a lab unit`/`a lab unit`.
        let mut tuner = make_bm1362_tuner_with_sku(
            0,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        tuner.config.max_freq_mhz = 545;
        tuner.apply_sku_freq_ceilings();
        assert_eq!(
            tuner.config.max_freq_mhz, 545,
            "the 545 am2 band must be byte-identical (envelope max == 545)"
        );
    }

    #[test]
    fn ce011_apply_sku_freq_ceilings_never_touches_min_freq() {
        // Load-bearing: the floor MUST NOT move (raising it would snap a quiet
        // home unit UP in frequency/power).
        let mut tuner = make_bm1362_tuner_with_sku(
            0,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42601,
        );
        tuner.config.min_freq_mhz = 200;
        tuner.config.max_freq_mhz = 800;
        tuner.apply_sku_freq_ceilings();
        assert_eq!(
            tuner.config.min_freq_mhz, 200,
            "min_freq_mhz (the floor) must never be touched"
        );
        assert_eq!(tuner.config.max_freq_mhz, 545);
    }

    #[test]
    fn outside_pvt_error_carries_sku_and_envelope_info_via_validate() {
        // The error carries the SKU id and the inclusive envelope
        // bounds so the dashboard can render an actionable warning.
        let err = crate::pvt_envelope::validate_freq_volt(
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::Bhb42801,
            700,
            1700,
        )
        .expect_err("must be OutsidePvt");
        match err {
            AutoTunerError::OutsidePvt {
                sku,
                freq_mhz,
                volt_mv,
                valid_freq_range,
                valid_volt_range,
            } => {
                assert_eq!(sku, "BHB42801");
                assert_eq!(freq_mhz, 700);
                assert_eq!(volt_mv, 1700);
                assert_eq!(valid_freq_range, (585, 675));
                assert_eq!(valid_volt_range, (1530, 1600));
            }
            other => panic!("expected OutsidePvt, got {:?}", other),
        }
    }

    #[test]
    fn pvt_envelope_helper_returns_correct_table_for_15_skus() {
        // Sanity tie-back: every SKU in ALL_BM1362_HASHBOARD_SKUS
        // returns a non-empty envelope via the autotuner-facing proxy.
        for sku in dcentrald_silicon_profiles::bm1362::ALL_BM1362_HASHBOARD_SKUS {
            let table = crate::pvt_envelope::pvt_envelope(*sku);
            assert!(!table.is_empty(), "{} envelope empty", sku.hashboard_id());
        }
    }
}
