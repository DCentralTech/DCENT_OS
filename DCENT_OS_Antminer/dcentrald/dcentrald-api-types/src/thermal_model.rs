//!  therm-A — DCENT_OS + VNish thermal pull-back model (HAL-free).
//!
//! Source RE evidence:
//!
//! (237 lines).
//!
//! Two thermal control flavors live behind the same DTO surface:
//! 1. **DCENT_OS continuous derating (ALGO 7)** — per-tick frequency scale
//!    factor as a smooth function of board temp. Used by the runtime
//!    autotuner.
//! 2. **VNish profile auto-switching** — discrete preset stepping when
//!    crossing temp/fan thresholds for sustained windows. Used by VNish
//!    1.2.x firmwares we observe in the wild.
//!
//! The fan PWM mode-cap table is the load-bearing safety contract. Per
//!  and , every
//! safety path (sensor error, fan failure, EmergencyShutdown, daemon crash)
//! MUST cap fan PWM at the active mode's cap — NEVER 127. Violating this
//! has burned the user repeatedly. The cap table is pinned by tests.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// DCENT_OS continuous derating (ALGO 7)
// ---------------------------------------------------------------------------

/// DCENT_OS canonical derating thresholds per
/// `thermal-control-model.md` table.
pub const DEFAULT_REFERENCE_TEMP_C: f32 = 55.0;
pub const DEFAULT_DERATING_THRESHOLD_C: f32 = 60.0;
pub const DEFAULT_DERATING_PER_C: f32 = 0.003; // 0.3% per °C above threshold
pub const DEFAULT_EMERGENCY_TEMP_C: f32 = 75.0;
pub const DEFAULT_HYSTERESIS_BAND_C: f32 = 3.0;
pub const DEFAULT_MIN_SCALE: f32 = 0.30; // ~30% of nominal frequency
pub const DEFAULT_IMMERSION_OFFSET_C: f32 = 20.0;

/// Configuration for the continuous derating loop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalCompConfig {
    pub reference_temp_c: f32,
    pub derating_threshold_c: f32,
    pub derating_per_c: f32,
    pub emergency_temp_c: f32,
    pub hysteresis_band_c: f32,
    pub min_scale: f32,
}

impl Default for ThermalCompConfig {
    fn default() -> Self {
        Self {
            reference_temp_c: DEFAULT_REFERENCE_TEMP_C,
            derating_threshold_c: DEFAULT_DERATING_THRESHOLD_C,
            derating_per_c: DEFAULT_DERATING_PER_C,
            emergency_temp_c: DEFAULT_EMERGENCY_TEMP_C,
            hysteresis_band_c: DEFAULT_HYSTERESIS_BAND_C,
            min_scale: DEFAULT_MIN_SCALE,
        }
    }
}

impl ThermalCompConfig {
    /// Apply the +20 °C immersion offset across all thresholds when
    /// confirmed-immersion is signaled at platform startup.
    pub fn with_immersion_offset(self) -> Self {
        Self {
            reference_temp_c: self.reference_temp_c + DEFAULT_IMMERSION_OFFSET_C,
            derating_threshold_c: self.derating_threshold_c + DEFAULT_IMMERSION_OFFSET_C,
            emergency_temp_c: self.emergency_temp_c + DEFAULT_IMMERSION_OFFSET_C,
            ..self
        }
    }
}

/// Verdict from `compute_scale`. The runtime adapter takes one of four
/// downstream actions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ScaleAction {
    /// Board temp ≥ emergency_temp. Snap to min scale and trigger event.
    Emergency { scale: f32 },
    /// Board temp ≥ derating_threshold. Linearly derate scale.
    Derate { scale: f32 },
    /// Board temp dropped below threshold-hysteresis. Restore scale = 1.0.
    Restore,
    /// Board temp inside hysteresis band. Hold last computed scale.
    HoldCurrent { scale: f32 },
}

impl ScaleAction {
    /// Return the scale factor implied by this action, normalized so
    /// `Restore` reads as 1.0.
    pub fn scale(&self) -> f32 {
        match *self {
            ScaleAction::Emergency { scale } => scale,
            ScaleAction::Derate { scale } => scale,
            ScaleAction::Restore => 1.0,
            ScaleAction::HoldCurrent { scale } => scale,
        }
    }

    /// True iff this verdict represents a thermal alarm (Emergency).
    pub fn is_emergency(&self) -> bool {
        matches!(self, ScaleAction::Emergency { .. })
    }
}

/// Compute the next scale action for a board-temp sample.
///
/// `current_scale` is the last applied scale (carried across ticks for
/// the hysteresis-hold case).
pub fn compute_scale(
    config: &ThermalCompConfig,
    board_temp_c: f32,
    current_scale: f32,
) -> ScaleAction {
    if board_temp_c >= config.emergency_temp_c {
        return ScaleAction::Emergency {
            scale: config.min_scale,
        };
    }
    if board_temp_c >= config.derating_threshold_c {
        let delta = board_temp_c - config.derating_threshold_c;
        let computed = (1.0 - delta * config.derating_per_c).max(config.min_scale);
        return ScaleAction::Derate { scale: computed };
    }
    if board_temp_c < config.derating_threshold_c - config.hysteresis_band_c {
        return ScaleAction::Restore;
    }
    ScaleAction::HoldCurrent {
        scale: current_scale,
    }
}

// ---------------------------------------------------------------------------
// VNish profile auto-switching
// ---------------------------------------------------------------------------

/// VNish 1.2.x canonical thresholds for discrete profile stepping.
pub const VNISH_LOWER_PROFILE_TEMP_C: f32 = 85.0;
pub const VNISH_LOWER_PROFILE_FAN_PWM_PERCENT: u8 = 90;
pub const VNISH_RAISE_PROFILE_TEMP_C: f32 = 60.0;
pub const VNISH_RAISE_PROFILE_FAN_PWM_PERCENT: u8 = 50;
pub const VNISH_SUSTAIN_WINDOW_SECONDS: u32 = 60;

/// Per-tick verdict for the VNish profile auto-switcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishProfileAction {
    /// Step the active preset DOWN one rung (less aggressive).
    StepDown,
    /// Step the active preset UP one rung (more aggressive).
    StepUp,
    /// Stay on the current preset.
    Hold,
}

/// Decide whether VNish should step the preset based on current samples.
///
/// Returns `StepDown` if either temp ≥ `lower_temp` OR fan_pwm_percent ≥
/// `lower_fan_pwm` for the sustained window. Returns `StepUp` only when
/// BOTH temp < `raise_temp` AND fan_pwm < `raise_fan_pwm` (and the held
/// window has elapsed).
pub fn vnish_profile_decision(
    temp_c: f32,
    fan_pwm_percent: u8,
    sustained_above_seconds: u32,
    sustained_below_seconds: u32,
) -> VnishProfileAction {
    let above = temp_c >= VNISH_LOWER_PROFILE_TEMP_C
        || fan_pwm_percent >= VNISH_LOWER_PROFILE_FAN_PWM_PERCENT;
    if above && sustained_above_seconds >= VNISH_SUSTAIN_WINDOW_SECONDS {
        return VnishProfileAction::StepDown;
    }
    let below = temp_c < VNISH_RAISE_PROFILE_TEMP_C
        && fan_pwm_percent < VNISH_RAISE_PROFILE_FAN_PWM_PERCENT;
    if below && sustained_below_seconds >= VNISH_SUSTAIN_WINDOW_SECONDS {
        return VnishProfileAction::StepUp;
    }
    VnishProfileAction::Hold
}

// ---------------------------------------------------------------------------
// Fan PWM mode caps (HARD SAFETY RULE)
// ---------------------------------------------------------------------------

/// Operator-selected fan-mode cap policy. Each variant pins a maximum PWM
/// the runtime is allowed to write to fan controllers. The loudest any mode
/// may reach is the fan_ctrl FPGA IP ceiling (100) — even the explicit
/// `HashrateMax` user opt-in is capped at 100, never 127 (w24-thermal-safety
/// F-2; the IP rejects PWM > 100).
///
/// + : every
/// safety path (sensor error, fan failure, EmergencyShutdown, daemon
/// crash, stale-temp) MUST cap at the mode's cap, and no mode cap exceeds
/// the IP ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanMode {
    /// Low-power home use. Boot default; acoustic proof comes from tach/RPM.
    QuietHome,
    /// Standard home mining; the canonical safety mode.
    Home,
    /// Balanced ambient handling.
    Balanced,
    /// Advanced / industrial mode.
    Advanced,
    /// Explicit user opt-in for full PWM range.
    HashrateMax,
}

impl FanMode {
    /// Maximum PWM duty cycle the runtime may write in this mode, including
    /// from safety paths.
    ///
    /// The BraiinsOS fan_ctrl FPGA IP ceiling is **100** (`dcentrald-hal`
    /// `fan.rs` `clamp_pwm` caps at 100; writing >100 to the IP is invalid),
    /// so NO mode may exceed 100 — `HashrateMax` is therefore capped at 100,
    /// not 127 (w24-thermal-safety F-2: a public `safe_fan_pwm(HashrateMax,..)`
    /// caller could otherwise hand a >ceiling value straight to
    /// `FanController::set_speed`). The home PWM ≤ 30 cap (`QuietHome`/`Home`)
    /// is unchanged — defense-in-depth here, NOT a relaxation of the home cap.
    pub fn max_pwm(&self) -> u8 {
        match self {
            FanMode::QuietHome => 10,
            FanMode::Home => 30,
            FanMode::Balanced => 64,
            // Advanced and HashrateMax both sit at the FPGA IP ceiling (100).
            // HashrateMax is the explicit user opt-in for "as loud as the IP
            // allows" — but the IP allows at most 100, never 127.
            FanMode::Advanced => 100,
            FanMode::HashrateMax => 100,
        }
    }

    /// Mode-cap PWM that ALL safety overrides MUST clamp to. Identical to
    /// `max_pwm` — kept as a separate accessor so the call-site reads as
    /// "I am applying a SAFETY cap" rather than a normal max.
    pub fn safety_cap_pwm(&self) -> u8 {
        self.max_pwm()
    }

    /// Friendly display name.
    pub fn display(&self) -> &'static str {
        match self {
            FanMode::QuietHome => "Quiet Home",
            FanMode::Home => "Home",
            FanMode::Balanced => "Balanced",
            FanMode::Advanced => "Advanced",
            FanMode::HashrateMax => "Hashrate Max",
        }
    }
}

/// Possible reasons fan logic must override the temp-curve output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanSafetyTrigger {
    /// Board temp sensor read returned no data.
    SensorError,
    /// Fan tachometer reads zero for >5 s while PWM > 0.
    FanFailure,
    /// Operator-triggered emergency shutdown (still need fans for cool-down).
    EmergencyShutdown,
    /// dcentrald crashed; safety wrapper engaged.
    DaemonCrash,
    /// Stale temp reading (older than tolerated tick window).
    StaleTemp,
}

/// Compute the safe fan PWM target given the current mode + a possible
/// safety trigger. Returns the mode-cap PWM in every safety case (always at
/// or below the fan_ctrl IP ceiling of 100 — never 127, even for
/// HashrateMax).
pub fn safe_fan_pwm(mode: FanMode, trigger: Option<FanSafetyTrigger>, requested: u8) -> u8 {
    let cap = mode.safety_cap_pwm();
    if trigger.is_some() {
        return cap;
    }
    requested.min(cap)
}

// ---------------------------------------------------------------------------
// Thermal-input assembly (S9 2026-04-19 LOAD-BEARING die-temp fallback)
// ---------------------------------------------------------------------------

/// Assemble the thermal controller's input temperature vector from the
/// already-validated per-chain board temps plus the control-board XADC die
/// temp.
///
/// **S9 2026-04-19 root cause — LOAD-BEARING, do NOT regress.** When board
/// temperatures disappear (the `board_temps` slice carries no `Some(_)`
/// values), the assembled input ALWAYS falls back to the XADC die temp —
/// for BOTH `skip_board_temp` values. The result is NEVER empty when a valid
/// `die_temp` is supplied. An empty board-temp set is normal on S9 (the
/// BM1387 I²C-passthrough board sensors don't always respond), so it must
/// never be allowed to reach the controller as an empty set (which an earlier
/// daemon version mistakenly relied on to trigger an EmergencyShutdown). The
/// die temp (~45 °C at 500 MHz) is the safe thermal input in that case.
///
/// `skip_board_temp` does NOT change the assembled vector — it only changes
/// the log line at the call site (kept inline in `daemon.rs`). It is accepted
/// here so the single source of truth is unambiguous: the fallback is the
/// same regardless of the flag.
///
/// `always_include_die` is an explicit SAF-2 cross-check opt-in for platforms
/// with a real XADC read: when board temps exist, append die temp too so a
/// stuck-cool board sensor cannot hide a rising die temp.
///
/// `board_temps` is the per-chain validated set (the daemon's
/// `per_chain_board_temps`): each entry is `Some(temp)` only after the
/// freshness/range/non-zero gating already applied at the call site, or
/// `None` for a missing/stale/out-of-range chain. This helper performs NO
/// gating itself — it only collects the valid samples and applies the
/// die-temp fallback, so the gating stays where the HAL atomics live.
pub fn assemble_thermal_input(
    board_temps: &[Option<f32>],
    die_temp: f32,
    skip_board_temp: bool,
    always_include_die: bool,
) -> Vec<f32> {
    // `skip_board_temp` is intentionally not branched on: the assembled
    // vector is identical for both values (the flag only gates a log line at
    // the call site). Bind it to silence the unused-param lint while keeping
    // the signature self-documenting.
    let _ = skip_board_temp;
    let mut temps: Vec<f32> = board_temps.iter().filter_map(|t| *t).collect();
    if temps.is_empty() {
        // LOAD-BEARING: die temp is ALWAYS the fallback when no board temp is
        // present — never leave this empty (S9 2026-04-19).
        temps.push(die_temp);
    } else if always_include_die {
        // SAF-2: include a real XADC die reading alongside board sensors when
        // the runtime opts into cross-checking. This lets the controller see a
        // rising die temp even if a plausible board sensor is stuck cool.
        temps.push(die_temp);
    }
    temps
}

// ---------------------------------------------------------------------------
// Per-chain published-temperature assembly (BUG-11 S9 telemetry)
// ---------------------------------------------------------------------------

/// Canonical `temp_source` string values for a published per-chain
/// temperature. Mirrors `dcentrald_api::ChainTempSource` (kept here too so the
/// no-HAL crate + its host tests don't depend on the HAL-bound `dcentrald-api`
/// crate). The wire field is a plain string so unknown future sources don't
/// break older clients.
pub mod chain_temp_source {
    /// Real on-board hashboard sensor reading (TMP451 / ADT7461 / NCT218 via
    /// BM1387 passthrough on Zynq, or the platform's direct board sensor).
    pub const BOARD_SENSOR: &str = "board_sensor";
    /// XADC SoC die-temp fallback — published when the hashboard board sensors
    /// return no data (the normal S9 case). An honest enclosure/board proxy,
    /// NOT a per-board sensor reading.
    pub const SOC_DIE_FALLBACK: &str = "soc_die_fallback";
}

/// Decide the per-chain temperature value + provenance string to PUBLISH to the
/// API/dashboard for one chain.
///
/// **BUG-11 (S9 board+chip temp missing from telemetry).** Distinct from
/// [`assemble_thermal_input`], which builds the *controller's* input vector:
/// this builds the *operator-facing* snapshot. The rule is the same honest
/// die-temp fallback, but here we also return a source label so the dashboard
/// can present a die-temp fallback as such (never as a board sensor, never as
/// an "unpowered board").
///
/// - `board_temp`: the chain's validated board-sensor reading (`Some` only
///   after freshness/range gating at the call site), or `None` when the board
///   sensor returned no usable data.
/// - `die_temp`: the control-board XADC SoC die temperature (a real reading on
///   Zynq; may be 0/invalid on platforms without an XADC).
///
/// Returns `(temp_c, Some(source))` when a real value is available, or
/// `(0.0, None)` when neither a board sensor nor a valid die temp exists (the
/// UI then shows "no telemetry" instead of a fabricated number).
pub fn assemble_chain_published_temp(
    board_temp: Option<f32>,
    die_temp: f32,
) -> (f32, Option<&'static str>) {
    let die_temp_valid = die_temp > 0.0 && die_temp < 125.0;
    match board_temp {
        Some(t) => (t, Some(chain_temp_source::BOARD_SENSOR)),
        None if die_temp_valid => (die_temp, Some(chain_temp_source::SOC_DIE_FALLBACK)),
        None => (0.0, None),
    }
}

/// Amlogic spurious-RPM-zero guard. The Amlogic A113D fan tach driver
/// occasionally returns 0 RPM while the fan is physically spinning. The
/// runtime must keep the last-good RPM in this case to avoid spurious
/// FanFailure trips (15-s window).
pub fn amlogic_observed_rpm(raw_rpm: u16, last_good_rpm: u16, current_pwm: u8) -> u16 {
    if raw_rpm == 0 && current_pwm > 0 {
        last_good_rpm
    } else {
        raw_rpm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- DCENT_OS ALGO 7 ---------------------------------------------------

    #[test]
    fn default_thermal_config_matches_re_doc() {
        let cfg = ThermalCompConfig::default();
        assert!((cfg.reference_temp_c - 55.0).abs() < 1e-3);
        assert!((cfg.derating_threshold_c - 60.0).abs() < 1e-3);
        assert!((cfg.derating_per_c - 0.003).abs() < 1e-6);
        assert!((cfg.emergency_temp_c - 75.0).abs() < 1e-3);
        assert!((cfg.hysteresis_band_c - 3.0).abs() < 1e-3);
        assert!((cfg.min_scale - 0.30).abs() < 1e-3);
    }

    #[test]
    fn immersion_offset_shifts_thresholds_by_20c() {
        let cfg = ThermalCompConfig::default().with_immersion_offset();
        assert!((cfg.reference_temp_c - 75.0).abs() < 1e-3);
        assert!((cfg.derating_threshold_c - 80.0).abs() < 1e-3);
        assert!((cfg.emergency_temp_c - 95.0).abs() < 1e-3);
        // Per-C and hysteresis are unchanged
        assert!((cfg.derating_per_c - 0.003).abs() < 1e-6);
        assert!((cfg.hysteresis_band_c - 3.0).abs() < 1e-3);
    }

    #[test]
    fn cool_temp_returns_restore_below_threshold_minus_hysteresis() {
        let cfg = ThermalCompConfig::default();
        let action = compute_scale(&cfg, 50.0, 0.85);
        assert_eq!(action, ScaleAction::Restore);
        assert!((action.scale() - 1.0).abs() < 1e-6);
        assert!(!action.is_emergency());
    }

    #[test]
    fn temp_inside_hysteresis_band_holds_current_scale() {
        let cfg = ThermalCompConfig::default();
        // 58 °C is between (60 - 3) and 60 — inside the hysteresis band
        let action = compute_scale(&cfg, 58.0, 0.92);
        assert_eq!(action, ScaleAction::HoldCurrent { scale: 0.92 });
    }

    #[test]
    fn temp_above_threshold_derates_linearly() {
        let cfg = ThermalCompConfig::default();
        // delta = 5 °C → scale = 1.0 - 5*0.003 = 0.985
        let action = compute_scale(&cfg, 65.0, 1.0);
        match action {
            ScaleAction::Derate { scale } => assert!((scale - 0.985).abs() < 1e-3),
            other => panic!("expected Derate, got {:?}", other),
        }
    }

    #[test]
    fn temp_at_emergency_snaps_to_min_scale() {
        let cfg = ThermalCompConfig::default();
        let action = compute_scale(&cfg, 75.0, 0.5);
        assert_eq!(
            action,
            ScaleAction::Emergency {
                scale: cfg.min_scale,
            }
        );
        assert!(action.is_emergency());
    }

    #[test]
    fn extreme_temp_clamps_at_min_scale_not_below() {
        let cfg = ThermalCompConfig::default();
        // Way past emergency threshold — still floored at min_scale.
        let action = compute_scale(&cfg, 200.0, 0.5);
        assert_eq!(
            action,
            ScaleAction::Emergency {
                scale: cfg.min_scale,
            }
        );
    }

    // -- VNish profile auto-switching --------------------------------------

    #[test]
    fn vnish_holds_when_window_not_yet_elapsed() {
        // 86 °C exceeds the lower threshold but window is only 30 s.
        let action = vnish_profile_decision(86.0, 50, 30, 0);
        assert_eq!(action, VnishProfileAction::Hold);
    }

    #[test]
    fn vnish_steps_down_when_temp_high_for_sustain_window() {
        let action = vnish_profile_decision(86.0, 50, 60, 0);
        assert_eq!(action, VnishProfileAction::StepDown);
    }

    #[test]
    fn vnish_steps_down_when_fan_pwm_high_for_sustain_window() {
        // Temp OK, but fan PWM ≥ 90% sustained.
        let action = vnish_profile_decision(70.0, 90, 60, 0);
        assert_eq!(action, VnishProfileAction::StepDown);
    }

    #[test]
    fn vnish_steps_up_only_when_both_temp_and_fan_low() {
        // Both below raise thresholds, sustained.
        let action = vnish_profile_decision(55.0, 40, 0, 60);
        assert_eq!(action, VnishProfileAction::StepUp);
    }

    #[test]
    fn vnish_holds_when_only_temp_low_but_fan_high() {
        // Temp = 55 °C OK, but fan PWM = 60% NOT below 50% threshold
        let action = vnish_profile_decision(55.0, 60, 0, 60);
        assert_eq!(action, VnishProfileAction::Hold);
    }

    // -- Fan PWM mode caps (HARD SAFETY) -----------------------------------

    #[test]
    fn fan_mode_caps_match_re_doc_table() {
        // Per thermal-control-model.md §"Fan PWM mode caps"
        assert_eq!(FanMode::QuietHome.max_pwm(), 10);
        assert_eq!(FanMode::Home.max_pwm(), 30);
        assert_eq!(FanMode::Balanced.max_pwm(), 64);
        assert_eq!(FanMode::Advanced.max_pwm(), 100);
        // w24-thermal-safety F-2: HashrateMax is the FPGA IP ceiling (100),
        // NOT 127. The fan_ctrl IP rejects PWM > 100; no mode may exceed it.
        assert_eq!(FanMode::HashrateMax.max_pwm(), 100);
        // Defense-in-depth invariant: NO mode cap may exceed the IP ceiling.
        for mode in [
            FanMode::QuietHome,
            FanMode::Home,
            FanMode::Balanced,
            FanMode::Advanced,
            FanMode::HashrateMax,
        ] {
            assert!(
                mode.max_pwm() <= 100,
                "{} max_pwm must not exceed the fan_ctrl IP ceiling (100)",
                mode.display()
            );
        }
    }

    #[test]
    fn safety_cap_never_exceeds_mode_cap() {
        // Hard-rule pin: ALL safety paths must
        // cap at mode_cap, and no mode cap EXCEEDS the IP ceiling (100) — i.e.
        // never 127. Advanced and HashrateMax deliberately sit AT the ceiling
        // (100), so the bound is `<= 100`, not `< 100`. (Bug-hunt 2026-05-28:
        // wave24-crash `5211e079` over-tightened this from the original
        // `< 127` to `< 100`, which broke the test against the deliberate,
        // documented `Advanced => 100` — a since- green invariant. The
        // home PWM<=30 cap for QuietHome/Home is pinned separately by
        // `safe_fan_pwm_clamps_at_mode_cap_in_normal_path` + the max_pwm tests.)
        for mode in [
            FanMode::QuietHome,
            FanMode::Home,
            FanMode::Balanced,
            FanMode::Advanced,
        ] {
            assert!(
                mode.safety_cap_pwm() <= 100,
                "{} safety cap must NOT exceed the IP ceiling (100) — never 127",
                mode.display()
            );
        }
        // Home modes must additionally honor the load-bearing fan-never-blast
        // home cap (PWM <= 30) — strengthen the pin so a future bump can't slip
        // a HOME mode up to the IP ceiling unnoticed.
        assert!(FanMode::QuietHome.safety_cap_pwm() <= 30);
        assert!(FanMode::Home.safety_cap_pwm() <= 30);
        // HashrateMax is the explicit opt-in for the loudest the IP allows —
        // which is 100 (the fan_ctrl IP ceiling), NOT 127 (w24-thermal-safety
        // F-2). Even the opt-in max never exceeds the IP ceiling.
        assert_eq!(FanMode::HashrateMax.safety_cap_pwm(), 100);
    }

    #[test]
    fn safe_fan_pwm_clamps_at_mode_cap_in_normal_path() {
        // In Home mode, a curve request of PWM 80 must clamp to 30.
        assert_eq!(safe_fan_pwm(FanMode::Home, None, 80), 30);
        // Below cap passes through unchanged.
        assert_eq!(safe_fan_pwm(FanMode::Home, None, 20), 20);
    }

    #[test]
    fn safe_fan_pwm_returns_mode_cap_on_any_safety_trigger() {
        // EVERY safety trigger drives PWM to the mode cap. NEVER 127.
        for trigger in [
            FanSafetyTrigger::SensorError,
            FanSafetyTrigger::FanFailure,
            FanSafetyTrigger::EmergencyShutdown,
            FanSafetyTrigger::DaemonCrash,
            FanSafetyTrigger::StaleTemp,
        ] {
            // In Home mode, every trigger returns 30 (NOT 127).
            assert_eq!(
                safe_fan_pwm(FanMode::Home, Some(trigger), 0),
                30,
                "{:?} must drive Home-mode PWM to 30, not 127",
                trigger
            );
            // Same for QuietHome — returns 10.
            assert_eq!(safe_fan_pwm(FanMode::QuietHome, Some(trigger), 0), 10);
        }
    }

    #[test]
    fn safe_fan_pwm_ignores_requested_value_when_safety_triggered() {
        // Even if some other path requested 100, a safety trigger forces
        // back to mode cap 30.
        assert_eq!(
            safe_fan_pwm(FanMode::Home, Some(FanSafetyTrigger::FanFailure), 100),
            30
        );
    }

    // -- Amlogic RPM=0 spurious guard --------------------------------------

    #[test]
    fn amlogic_rpm_keeps_last_good_when_zero_with_active_pwm() {
        // raw_rpm=0 + current_pwm=20 → spurious zero, return last_good
        let observed = amlogic_observed_rpm(0, 4500, 20);
        assert_eq!(observed, 4500);
    }

    #[test]
    fn amlogic_rpm_passes_through_when_pwm_is_zero() {
        // PWM=0 means fan should genuinely be stopped — accept rpm=0.
        let observed = amlogic_observed_rpm(0, 4500, 0);
        assert_eq!(observed, 0);
    }

    #[test]
    fn amlogic_rpm_passes_through_real_reading() {
        // Non-zero raw reading is always honored.
        let observed = amlogic_observed_rpm(3200, 4500, 50);
        assert_eq!(observed, 3200);
    }

    // -- Thermal-input assembly (S9 2026-04-19 die-temp fallback) ----------

    #[test]
    fn empty_board_temps_fall_back_to_die_temp_never_empty_s9_2026_04_19() {
        // LOAD-BEARING S9 invariant: when the board-temp set is empty, the
        // assembled thermal input must ALWAYS contain the die temp (never be
        // empty) — for BOTH skip_board_temp values. An empty input previously
        // mis-triggered EmergencyShutdown.
        let die_temp = 45.0_f32;

        // No board temps at all (zero-length slice).
        for skip in [true, false] {
            let out = assemble_thermal_input(&[], die_temp, skip, false);
            assert_eq!(
                out,
                vec![die_temp],
                "empty board-temp slice must fall back to [die_temp] (skip_board_temp={skip})"
            );
            assert!(
                !out.is_empty(),
                "assembled thermal input must NEVER be empty (skip_board_temp={skip})"
            );
        }

        // All-None board temps (chains present but all stale/missing).
        for skip in [true, false] {
            let out = assemble_thermal_input(&[None, None, None], die_temp, skip, false);
            assert_eq!(
                out,
                vec![die_temp],
                "all-None board temps must fall back to [die_temp] (skip_board_temp={skip})"
            );
            assert!(
                !out.is_empty(),
                "assembled thermal input must NEVER be empty (skip_board_temp={skip})"
            );
        }
    }

    #[test]
    fn valid_board_temps_are_passed_through_in_order() {
        let die_temp = 45.0_f32;
        // A mix of valid + missing chains; only the valid samples come
        // through, in order, and the die temp is NOT appended when at least
        // one valid board temp exists.
        for skip in [true, false] {
            let out =
                assemble_thermal_input(&[Some(62.5), None, Some(58.0)], die_temp, skip, false);
            assert_eq!(
                out,
                vec![62.5, 58.0],
                "valid board temps must pass through in order, no die-temp append \
                 (skip_board_temp={skip})"
            );
        }

        // Fully populated three-chain set.
        let out = assemble_thermal_input(
            &[Some(60.0), Some(61.0), Some(59.5)],
            die_temp,
            false,
            false,
        );
        assert_eq!(out, vec![60.0, 61.0, 59.5]);
        // die_temp must not appear when board temps are present.
        assert!(!out.contains(&die_temp));
    }

    #[test]
    fn single_valid_board_temp_does_not_trigger_die_fallback() {
        // One valid chain among several missing → that one value, no fallback.
        let out = assemble_thermal_input(&[None, Some(66.0), None], 45.0, false, false);
        assert_eq!(out, vec![66.0]);
    }

    #[test]
    fn always_include_die_appends_xadc_to_board_temp_inputs() {
        let out = assemble_thermal_input(&[Some(62.5), None, Some(58.0)], 70.0, false, true);
        assert_eq!(
            out,
            vec![62.5, 58.0, 70.0],
            "SAF-2 XADC cross-check must keep valid board temps and append die temp"
        );

        let fallback = assemble_thermal_input(&[None, None], 70.0, false, true);
        assert_eq!(
            fallback,
            vec![70.0],
            "empty board-temp fallback must not duplicate die temp"
        );
    }

    // -- BUG-11: per-chain PUBLISHED temp + source ------------------------

    #[test]
    fn published_temp_uses_board_sensor_when_present() {
        // A real board sensor reading is published as-is, labeled board_sensor —
        // even if a die temp is also available.
        let (t, src) = assemble_chain_published_temp(Some(62.5), 45.0);
        assert!((t - 62.5).abs() < 1e-6);
        assert_eq!(src, Some(chain_temp_source::BOARD_SENSOR));
    }

    #[test]
    fn published_temp_falls_back_to_die_when_board_silent_s9() {
        // S9 case: board sensors return nothing, XADC die temp is valid →
        // publish the die temp labeled soc_die_fallback (NOT 0.0, NOT
        // board_sensor). This is the fix for the operator's "no board/chip
        // temp on the S9" report — the value reaches the dashboard with an
        // honest source label instead of a blank/N-A.
        let (t, src) = assemble_chain_published_temp(None, 45.0);
        assert!((t - 45.0).abs() < 1e-6);
        assert_eq!(src, Some(chain_temp_source::SOC_DIE_FALLBACK));
        assert_ne!(t, 0.0, "die fallback must publish a real number, never 0.0");
    }

    #[test]
    fn published_temp_is_none_source_when_no_board_and_no_valid_die() {
        // Amlogic / failed-XADC: no board sensor AND no valid die temp →
        // (0.0, None) so the UI shows "no telemetry", never a fabricated number.
        for die in [0.0_f32, -5.0, 130.0, 999.0] {
            let (t, src) = assemble_chain_published_temp(None, die);
            assert_eq!(t, 0.0, "invalid die temp {die} must not be published");
            assert_eq!(src, None, "no source label when there is no real temp");
        }
    }

    #[test]
    fn published_temp_die_must_be_in_valid_range() {
        // Boundary: 0 < die < 125. Exactly 0 and exactly 125 are rejected.
        assert_eq!(assemble_chain_published_temp(None, 0.0), (0.0, None));
        assert_eq!(assemble_chain_published_temp(None, 125.0), (0.0, None));
        let (t, src) = assemble_chain_published_temp(None, 124.9);
        assert!((t - 124.9).abs() < 1e-3);
        assert_eq!(src, Some(chain_temp_source::SOC_DIE_FALLBACK));
    }

    // -- Serialization pinning ---------------------------------------------

    #[test]
    fn fan_mode_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&FanMode::QuietHome).unwrap(),
            "\"quiet_home\""
        );
        assert_eq!(serde_json::to_string(&FanMode::Home).unwrap(), "\"home\"");
        assert_eq!(
            serde_json::to_string(&FanMode::Balanced).unwrap(),
            "\"balanced\""
        );
        assert_eq!(
            serde_json::to_string(&FanMode::Advanced).unwrap(),
            "\"advanced\""
        );
        assert_eq!(
            serde_json::to_string(&FanMode::HashrateMax).unwrap(),
            "\"hashrate_max\""
        );
    }

    #[test]
    fn vnish_profile_action_round_trips_through_json() {
        for action in [
            VnishProfileAction::StepDown,
            VnishProfileAction::StepUp,
            VnishProfileAction::Hold,
        ] {
            let json = serde_json::to_string(&action).unwrap();
            let recovered: VnishProfileAction = serde_json::from_str(&json).unwrap();
            assert_eq!(action, recovered);
        }
    }

    #[test]
    fn scale_action_serializes_with_kind_tag() {
        let json = serde_json::to_string(&ScaleAction::Emergency { scale: 0.30 }).unwrap();
        assert!(json.contains("\"kind\":\"emergency\""));
        assert!(json.contains("\"scale\":0.3"));
    }
}
