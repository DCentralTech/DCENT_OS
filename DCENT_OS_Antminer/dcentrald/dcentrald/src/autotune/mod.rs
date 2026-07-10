//! Closed-loop power-target controller (Supremacy S4-03).
//!
//! LuxOS + Braiins both ship power-target autotuners that drive chip
//! frequency to hit an operator-supplied wattage target. This module
//! mirrors that capability for `dcentrald` with a deliberately
//! conservative PI controller and HARD safety clamps.
//!
//! ## Design
//!
//! * **PI controller** — `error = target_watts - actual_watts`,
//!   `freq_delta = Kp * error + Ki * integral`. The integral term is
//!   bounded so it cannot wind up past the slew-rate authority of the
//!   controller.
//! * **Bounded slew** — `freq_delta` is clamped to `±slew_rate_mhz`
//!   per tick (default 5 MHz).
//! * **30 s tick** — slow enough to let chain power telemetry settle
//!   after each frequency step before the next correction is computed.
//! * **HARD voltage clamp** — the controller refuses to engage at any
//!   voltage above 14_500 mV. This matches the am2 BM1362 envelope and
//!   is intentional dead code for any frequency adjustment that would
//!   require lifting voltage past the clamp. The voltage clamp is
//!   read-only at this layer; an upstream caller is responsible for
//!   refusing voltage writes that exceed the clamp.
//! * **Fan PWM clamp** — when `home_mode` is true, the controller
//!   refuses to engage if the resolved `fan_max_pwm > 30`. Per
//!   , fan blast is unacceptable on a home/space-heater
//!   profile.
//! * **Steady-state gate** — the controller waits for three
//!   consecutive ticks of stable hashrate (within a small tolerance)
//!   before starting to integrate error. A miner that has not yet
//!   converged to mining steady-state would otherwise feed garbage
//!   into the integrator.
//!
//! ## Safety contract
//!
//! The controller computes a *target* frequency in MHz. It does NOT
//! perform the voltage/frequency write itself — the daemon code that
//! consumes `PowerTargetController::next_frequency_mhz()` is
//! responsible for routing the change through the
//! `dcentrald-silicon-profiles` PVT envelope clamps + the platform's
//! voltage controller (which itself enforces fw=0x89 / fw=0x71 /
//! PIC1704 whitelists). This module is intentionally a *suggestion*
//! engine. Memory rule to save post-merge:
//! .

use serde::{Deserialize, Serialize};

/// Supremacy S5-03: Dynamic Performance Scaling continuous walker
/// (Braiins GDTUNER analog). Lives in `[autotune.dps]`. Mutually
/// exclusive with `[autotune.power_target]` — DPS yields.
pub mod dps_walker;

/// Supremacy S5-02: TunerMode 6-variant strategy enum. Re-exported
/// at module level for downstream callers.
pub mod tuner_mode;

pub use tuner_mode::{ManualSettings, TunerDriver, TunerMode, TunerOutcome, SLEW_MHZ_PER_TICK};

/// Hard upper bound on chip voltage (millivolts). Used as a refuse-to-
/// engage gate when the operator-supplied configuration would imply
/// any voltage write above this value. am2 voltage
/// envelope.
pub const VOLTAGE_CLAMP_MV: u16 = 14_500;

/// Home-mode fan PWM ceiling.: "NEVER allow fans above
/// PWM 30 for home mining."
pub const HOME_FAN_PWM_MAX: u8 = 30;

/// Default proportional gain (MHz per Watt error).
pub const DEFAULT_KP_MHZ_PER_W: f64 = 2.0;

/// Default integral gain (MHz per Watt·second of accumulated error).
pub const DEFAULT_KI_MHZ_PER_W_S: f64 = 0.05;

/// Default slew-rate clamp (MHz per tick).
pub const DEFAULT_SLEW_MHZ: u16 = 5;

/// Default controller tick period (seconds).
pub const DEFAULT_TICK_SECONDS: u64 = 30;

/// Steady-state hashrate tolerance (fraction). Two ticks whose
/// hashrate differs by less than this fraction count as "stable".
const STEADY_STATE_TOLERANCE: f64 = 0.05;

/// Number of consecutive stable ticks required before the controller
/// will start adjusting frequency.
const STEADY_STATE_REQUIRED_TICKS: u8 = 3;

/// Anti-windup ceiling on the integrator (in MHz). Bounded to the
/// slew rate × a small number of ticks so the integrator cannot
/// dominate the slew-rate authority.
const INTEGRAL_AUTHORITY_TICKS: u32 = 6;

// ---------------------------------------------------------------------
// Config (serde — wired into dcentrald.toml as `[autotune.power_target]`)
// ---------------------------------------------------------------------

/// `[autotune.power_target]` TOML section.
///
/// Default: disabled. The operator must opt-in by setting
/// `enabled = true` AND a positive `target_watts`. Engineer-tunable
/// `kp` / `ki` / `slew_rate_mhz` keep their conservative defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PowerTargetConfig {
    /// Enable the closed-loop power-target controller.
    #[serde(default)]
    pub enabled: bool,

    /// Target wall/chip power in watts. 0 = no target (controller
    /// stays disengaged).
    #[serde(default)]
    pub target_watts: u32,

    /// Home / space-heater profile. Caps `fan_max_pwm` at
    /// `HOME_FAN_PWM_MAX`. The controller refuses to engage if the
    /// runtime fan ceiling exceeds the cap while `home_mode` is true.
    #[serde(default)]
    pub home_mode: bool,

    /// Lower bound on the controller's frequency authority (MHz).
    /// Defaults to a conservative 400 MHz floor; per-family the
    /// silicon profile crate should clamp tighter.
    #[serde(default = "default_freq_min_mhz")]
    pub frequency_band_min_mhz: u16,

    /// Upper bound on the controller's frequency authority (MHz).
    /// Defaults to 800 MHz; the silicon profile crate clamps tighter.
    #[serde(default = "default_freq_max_mhz")]
    pub frequency_band_max_mhz: u16,

    /// Proportional gain — MHz per Watt of error.
    #[serde(default = "default_kp")]
    pub kp: f64,

    /// Integral gain — MHz per Watt·second of accumulated error.
    #[serde(default = "default_ki")]
    pub ki: f64,

    /// Maximum frequency change per tick (MHz, applied symmetrically
    /// as `±slew_rate_mhz`).
    #[serde(default = "default_slew")]
    pub slew_rate_mhz: u16,

    /// Tick period (seconds). The integral is computed in
    /// Watt·seconds so the tick period scales the integral
    /// contribution.
    #[serde(default = "default_tick_seconds")]
    pub tick_seconds: u64,
}

impl Default for PowerTargetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_watts: 0,
            home_mode: false,
            frequency_band_min_mhz: default_freq_min_mhz(),
            frequency_band_max_mhz: default_freq_max_mhz(),
            kp: default_kp(),
            ki: default_ki(),
            slew_rate_mhz: default_slew(),
            tick_seconds: default_tick_seconds(),
        }
    }
}

fn default_freq_min_mhz() -> u16 {
    400
}
fn default_freq_max_mhz() -> u16 {
    800
}
fn default_kp() -> f64 {
    DEFAULT_KP_MHZ_PER_W
}
fn default_ki() -> f64 {
    DEFAULT_KI_MHZ_PER_W_S
}
fn default_slew() -> u16 {
    DEFAULT_SLEW_MHZ
}
fn default_tick_seconds() -> u64 {
    DEFAULT_TICK_SECONDS
}

/// `[autotune]` parent section. Hosts `power_target` (S4-03 closed-loop
/// PI controller) and `dps` (S5-03 continuous walker, Braiins GDTUNER
/// analog). The existing `[autotuner]` section (per-chip frequency
/// search) is distinct and lives in
/// `dcentrald-autotuner::AutoTunerConfig`.
///
/// ## Precedence
///
/// `[autotune.power_target]` and `[autotune.dps]` are **mutually
/// exclusive**. If both `enabled`, the power-target controller wins
/// and the DPS walker yields — see [`AutotuneConfig::dps_yields`] and
/// [`dps_walker::DpsWalker::new`], which returns
/// [`dps_walker::DpsEngageError::YieldsToPowerTarget`]. Rationale: the
/// power-target controller is a hard operator wattage constraint; the
/// DPS walker is an opportunistic optimizer. An optimizer must never
/// fight a hard constraint.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AutotuneConfig {
    #[serde(default)]
    pub power_target: PowerTargetConfig,

    /// S5-03 Dynamic Performance Scaling continuous walker.
    #[serde(default)]
    pub dps: dps_walker::DpsConfig,

    /// W24-BC-1 (): cross-firmware bad-chip / degraded-chip
    /// supervisor (`[autotune.bad_chip]`). **Default-OFF.**
    ///
    /// Hosts the RE-004 per-chip fault FSM owned by
    /// `dcentrald_autotuner::bad_chip_supervisor::BadChipConfig`
    /// (4-state Healthy/Degraded/Bad/Missing classifier + the
    /// downclock / blacklist / ReduceBoardProfile / bounded BoardReset
    /// / HaltMining action ladder). The struct's own `enabled` field
    /// defaults to `false` via serde; an entirely-absent
    /// `[autotune.bad_chip]` block deserializes to
    /// `BadChipConfig::default()` (also disabled). Either way the
    /// daemon NEVER constructs the supervisor and NEVER calls
    /// `observe()` until an operator explicitly sets
    /// `[autotune.bad_chip] enabled = true` — exactly mirroring the
    /// `[thermal.supervisor].enabled` / `[thermal].dps_enabled` /
    /// `[autotune.power_target].enabled` opt-in pattern.
    ///
    /// This is a SAFETY-CRITICAL surface: when enabled the supervisor
    /// can request per-chip downclock/blacklist and a bounded board
    /// reset / halt-mining. Live-HW enablement is Wave-H gated
    /// (operator per-action authorization). Default-off keeps the
    /// proven live `a lab unit` / `a lab unit` am2 path byte-identical.
    #[serde(default)]
    pub bad_chip: dcentrald_autotuner::bad_chip_supervisor::BadChipConfig,

    /// S5-02 TunerMode 6-variant selector. Defaults to a neutral
    /// `Manual { freq: 0, voltage: 0 }` so the daemon can re-seed
    /// with current on-chip values via
    /// [`TunerMode::default_manual_at`] at startup. The enum
    /// discriminant is the TOML `mode = "..."` field.
    #[serde(default)]
    pub mode: tuner_mode::TunerMode,
}

impl AutotuneConfig {
    /// True when the DPS walker must yield because the power-target
    /// controller is also enabled (mutual-exclusion precedence). When
    /// this is true, callers should NOT construct a
    /// [`dps_walker::DpsWalker`] (it would return
    /// [`dps_walker::DpsEngageError::YieldsToPowerTarget`] anyway —
    /// this is the cheap pre-check).
    pub fn dps_yields(&self) -> bool {
        self.dps.enabled && self.power_target.enabled
    }

    /// Returns `true` if exactly zero or one of the two autotune
    /// controllers is enabled (the always-valid case), or if both are
    /// enabled (still valid — DPS deterministically yields, it is not
    /// a config error). Provided for callers that want an explicit
    /// "is the resolved controller set well-defined?" predicate. It is
    /// intentionally always `true`: the precedence rule means there is
    /// no invalid combination, only a documented winner.
    pub fn exclusive_resolution_is_defined(&self) -> bool {
        // power_target wins ties → always resolvable.
        true
    }
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Errors that cause the controller to refuse to engage. None of these
/// are recoverable mid-loop — the controller bails to disengaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngageError {
    /// Operator config disabled or `target_watts == 0`.
    Disabled,
    /// Requested voltage exceeds `VOLTAGE_CLAMP_MV`.
    VoltageClampViolation { requested_mv: u16, clamp_mv: u16 },
    /// `home_mode = true` but the resolved fan ceiling exceeds
    /// `HOME_FAN_PWM_MAX`.
    HomeFanCapViolation { requested_pwm: u8, cap_pwm: u8 },
    /// Frequency band is degenerate (`min >= max`).
    InvalidFrequencyBand { min_mhz: u16, max_mhz: u16 },
    /// `slew_rate_mhz == 0` — controller has no authority.
    InvalidSlewRate,
    /// `tick_seconds == 0` — division by zero in the integral term.
    InvalidTickPeriod,
}

impl core::fmt::Display for EngageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngageError::Disabled => f.write_str("power-target controller disabled"),
            EngageError::VoltageClampViolation {
                requested_mv,
                clamp_mv,
            } => write!(
                f,
                "voltage {} mV exceeds HARD clamp {} mV — refusing to engage",
                requested_mv, clamp_mv
            ),
            EngageError::HomeFanCapViolation {
                requested_pwm,
                cap_pwm,
            } => write!(
                f,
                "home_mode fan PWM {} exceeds home cap {} — refusing to engage",
                requested_pwm, cap_pwm
            ),
            EngageError::InvalidFrequencyBand { min_mhz, max_mhz } => write!(
                f,
                "frequency band min {} MHz >= max {} MHz",
                min_mhz, max_mhz
            ),
            EngageError::InvalidSlewRate => f.write_str("slew_rate_mhz must be > 0"),
            EngageError::InvalidTickPeriod => f.write_str("tick_seconds must be > 0"),
        }
    }
}

impl std::error::Error for EngageError {}

// ---------------------------------------------------------------------
// Telemetry sample
// ---------------------------------------------------------------------

/// One steady-state tick of telemetry the controller needs.
#[derive(Debug, Clone, Copy)]
pub struct TelemetrySample {
    /// Estimated chip / wall power right now (Watts).
    pub actual_watts: f64,
    /// Current full-miner hashrate (TH/s — only used to detect
    /// steady-state; the controller does not optimize on it).
    pub hashrate_ths: f64,
    /// Current chip voltage (mV). Used for the HARD clamp check.
    pub voltage_mv: u16,
    /// Current fan PWM ceiling (0–127). Used for the home-mode cap.
    pub fan_pwm: u8,
}

// ---------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------

/// Closed-loop power-target PI controller.
///
/// Construct via [`PowerTargetController::new`] (validates config) and
/// drive forward one tick at a time with [`PowerTargetController::tick`].
/// The controller reports its proposed next frequency via
/// [`PowerTargetController::next_frequency_mhz`].
#[derive(Debug)]
pub struct PowerTargetController {
    config: PowerTargetConfig,
    current_freq_mhz: u16,
    integral_mhz: f64,
    last_hashrate_ths: Option<f64>,
    stable_tick_count: u8,
    engaged: bool,
}

/// What the controller decided to do on a given tick.
#[derive(Debug, Clone, PartialEq)]
pub enum TickOutcome {
    /// Controller refused to engage this tick. Reason captured.
    Refused(EngageError),
    /// Controller is waiting for steady-state. Frequency unchanged.
    WaitingForSteadyState { ticks_observed: u8 },
    /// Controller acted. Frequency moved from `from_mhz` to `to_mhz`.
    /// `delta_mhz` is signed (i32 to allow negative).
    Adjusted {
        from_mhz: u16,
        to_mhz: u16,
        delta_mhz: i32,
        error_w: f64,
    },
    /// Controller engaged but at-target — no movement this tick.
    AtTarget { freq_mhz: u16, error_w: f64 },
}

impl PowerTargetController {
    /// Construct a controller from validated config and an initial
    /// frequency. Returns `Err` if config is degenerate.
    pub fn new(config: PowerTargetConfig, initial_freq_mhz: u16) -> Result<Self, EngageError> {
        Self::validate(&config)?;
        let clamped =
            initial_freq_mhz.clamp(config.frequency_band_min_mhz, config.frequency_band_max_mhz);
        Ok(Self {
            config,
            current_freq_mhz: clamped,
            integral_mhz: 0.0,
            last_hashrate_ths: None,
            stable_tick_count: 0,
            engaged: false,
        })
    }

    fn validate(config: &PowerTargetConfig) -> Result<(), EngageError> {
        if !config.enabled || config.target_watts == 0 {
            return Err(EngageError::Disabled);
        }
        if config.frequency_band_min_mhz >= config.frequency_band_max_mhz {
            return Err(EngageError::InvalidFrequencyBand {
                min_mhz: config.frequency_band_min_mhz,
                max_mhz: config.frequency_band_max_mhz,
            });
        }
        if config.slew_rate_mhz == 0 {
            return Err(EngageError::InvalidSlewRate);
        }
        if config.tick_seconds == 0 {
            return Err(EngageError::InvalidTickPeriod);
        }
        Ok(())
    }

    /// HARD-clamp check on an external voltage write. Callers should
    /// invoke this before any voltage write the controller is
    /// upstream of. Returns `Err` if the requested mV exceeds the
    /// clamp.
    pub fn check_voltage_clamp(requested_mv: u16) -> Result<(), EngageError> {
        if requested_mv > VOLTAGE_CLAMP_MV {
            return Err(EngageError::VoltageClampViolation {
                requested_mv,
                clamp_mv: VOLTAGE_CLAMP_MV,
            });
        }
        Ok(())
    }

    /// HARD-clamp check on the home-mode fan ceiling.
    pub fn check_home_fan_cap(home_mode: bool, requested_pwm: u8) -> Result<(), EngageError> {
        if home_mode && requested_pwm > HOME_FAN_PWM_MAX {
            return Err(EngageError::HomeFanCapViolation {
                requested_pwm,
                cap_pwm: HOME_FAN_PWM_MAX,
            });
        }
        Ok(())
    }

    /// Currently proposed frequency (MHz).
    pub fn next_frequency_mhz(&self) -> u16 {
        self.current_freq_mhz
    }

    /// Tick the controller forward by one period. Returns the
    /// decision taken.
    pub fn tick(&mut self, sample: TelemetrySample) -> TickOutcome {
        // HARD safety gates first — these refuse to engage at all.
        if let Err(e) = Self::check_voltage_clamp(sample.voltage_mv) {
            self.engaged = false;
            self.integral_mhz = 0.0;
            return TickOutcome::Refused(e);
        }
        if let Err(e) = Self::check_home_fan_cap(self.config.home_mode, sample.fan_pwm) {
            self.engaged = false;
            self.integral_mhz = 0.0;
            return TickOutcome::Refused(e);
        }

        // Steady-state detection: require N consecutive ticks within
        // tolerance.
        let stable = match self.last_hashrate_ths {
            Some(prev) if prev > 0.0 => {
                let rel = (sample.hashrate_ths - prev).abs() / prev;
                rel <= STEADY_STATE_TOLERANCE
            }
            // First tick: record and report waiting.
            _ => false,
        };
        self.last_hashrate_ths = Some(sample.hashrate_ths);
        if stable {
            self.stable_tick_count = self.stable_tick_count.saturating_add(1);
        } else {
            self.stable_tick_count = 0;
        }
        if self.stable_tick_count < STEADY_STATE_REQUIRED_TICKS {
            return TickOutcome::WaitingForSteadyState {
                ticks_observed: self.stable_tick_count,
            };
        }

        // PI math.
        let target = self.config.target_watts as f64;
        // Guard: a single non-finite telemetry sample (NaN/Inf actual_watts) would
        // otherwise permanently poison the integrator — `next_integral` becomes NaN,
        // `NaN.clamp()` returns NaN, and every subsequent tick then computes NaN and
        // freezes frequency forever. Treat a non-finite sample as zero error this tick
        // (hold), leaving the integrator finite and the frequency unchanged.
        let error_w = target - sample.actual_watts;
        let error_w = if error_w.is_finite() { error_w } else { 0.0 };
        let kp = self.config.kp;
        let ki = self.config.ki;
        let tick_s = self.config.tick_seconds as f64;
        let slew = self.config.slew_rate_mhz as f64;

        // Anti-windup: only integrate if we have authority left.
        let integral_ceiling = slew * INTEGRAL_AUTHORITY_TICKS as f64;
        let next_integral = self.integral_mhz + ki * error_w * tick_s;
        self.integral_mhz = next_integral.clamp(-integral_ceiling, integral_ceiling);

        let raw_delta_mhz = kp * error_w + self.integral_mhz;
        let clamped_delta = raw_delta_mhz.clamp(-slew, slew);
        let delta_rounded = clamped_delta.round() as i32;

        let from = self.current_freq_mhz;
        let proposed = (from as i32 + delta_rounded).clamp(
            self.config.frequency_band_min_mhz as i32,
            self.config.frequency_band_max_mhz as i32,
        ) as u16;
        self.current_freq_mhz = proposed;
        self.engaged = true;

        if proposed == from {
            TickOutcome::AtTarget {
                freq_mhz: proposed,
                error_w,
            }
        } else {
            TickOutcome::Adjusted {
                from_mhz: from,
                to_mhz: proposed,
                delta_mhz: proposed as i32 - from as i32,
                error_w,
            }
        }
    }

    /// True once the controller has cleared the steady-state gate and
    /// started adjusting frequency.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Borrow the resolved config.
    pub fn config(&self) -> &PowerTargetConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn engaged_config() -> PowerTargetConfig {
        PowerTargetConfig {
            enabled: true,
            target_watts: 1000,
            home_mode: false,
            frequency_band_min_mhz: 400,
            frequency_band_max_mhz: 800,
            kp: DEFAULT_KP_MHZ_PER_W,
            ki: DEFAULT_KI_MHZ_PER_W_S,
            slew_rate_mhz: DEFAULT_SLEW_MHZ,
            tick_seconds: DEFAULT_TICK_SECONDS,
        }
    }

    fn stable_sample(actual_watts: f64) -> TelemetrySample {
        TelemetrySample {
            actual_watts,
            hashrate_ths: 14.0,
            voltage_mv: 13_700,
            fan_pwm: 10,
        }
    }

    /// Push three identical hashrate ticks to clear the steady-state
    /// gate.
    fn warm_up(controller: &mut PowerTargetController, watts: f64) {
        for _ in 0..STEADY_STATE_REQUIRED_TICKS + 1 {
            controller.tick(stable_sample(watts));
        }
    }

    #[test]
    fn config_defaults_are_conservative() {
        let cfg = PowerTargetConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.target_watts, 0);
        assert!((cfg.kp - 2.0).abs() < f64::EPSILON);
        assert!((cfg.ki - 0.05).abs() < f64::EPSILON);
        assert_eq!(cfg.slew_rate_mhz, 5);
        assert_eq!(cfg.tick_seconds, 30);
        assert_eq!(cfg.frequency_band_min_mhz, 400);
        assert_eq!(cfg.frequency_band_max_mhz, 800);
        assert!(!cfg.home_mode);
    }

    #[test]
    fn disabled_config_refuses_construction() {
        let cfg = PowerTargetConfig::default();
        let err = PowerTargetController::new(cfg, 600).unwrap_err();
        assert_eq!(err, EngageError::Disabled);
    }

    #[test]
    fn zero_target_refuses_construction() {
        let mut cfg = engaged_config();
        cfg.target_watts = 0;
        let err = PowerTargetController::new(cfg, 600).unwrap_err();
        assert_eq!(err, EngageError::Disabled);
    }

    #[test]
    fn degenerate_band_refuses_construction() {
        let mut cfg = engaged_config();
        cfg.frequency_band_min_mhz = 700;
        cfg.frequency_band_max_mhz = 700;
        let err = PowerTargetController::new(cfg, 600).unwrap_err();
        assert!(matches!(err, EngageError::InvalidFrequencyBand { .. }));
    }

    #[test]
    fn zero_slew_refuses_construction() {
        let mut cfg = engaged_config();
        cfg.slew_rate_mhz = 0;
        let err = PowerTargetController::new(cfg, 600).unwrap_err();
        assert_eq!(err, EngageError::InvalidSlewRate);
    }

    #[test]
    fn zero_tick_refuses_construction() {
        let mut cfg = engaged_config();
        cfg.tick_seconds = 0;
        let err = PowerTargetController::new(cfg, 600).unwrap_err();
        assert_eq!(err, EngageError::InvalidTickPeriod);
    }

    #[test]
    fn non_finite_watts_sample_does_not_poison_the_integrator() {
        // Regression (R-03): a single NaN/Inf actual_watts must not permanently
        // freeze the controller. The bug: NaN error -> NaN integral -> NaN.clamp()
        // stays NaN -> every later tick computes NaN and freezes frequency forever.
        // A controller that receives one NaN tick must still track one that never did.
        let mut poisoned = PowerTargetController::new(engaged_config(), 600).unwrap();
        let mut clean = PowerTargetController::new(engaged_config(), 600).unwrap();
        warm_up(&mut poisoned, 1000.0);
        warm_up(&mut clean, 1000.0);

        // One NaN tick to `poisoned`; the matching real tick to `clean`.
        poisoned.tick(stable_sample(f64::NAN));
        clean.tick(stable_sample(1000.0));

        // Drive both below target (1000 W) so the controller seeks more power by
        // ramping frequency up toward the band max.
        for _ in 0..40 {
            poisoned.tick(stable_sample(600.0));
            clean.tick(stable_sample(600.0));
        }

        let pf = poisoned.next_frequency_mhz() as i32;
        let cf = clean.next_frequency_mhz() as i32;
        // The `clean` controller must actually have ramped (proves the PI path ran).
        assert!(cf > 610, "clean controller should have ramped up, got {cf}");
        // Without the guard `poisoned` freezes at ~600 while `clean` ramps toward
        // 800 — a large gap. With the guard they track within a couple of slew steps.
        assert!(
            (pf - cf).abs() <= 20,
            "NaN tick poisoned the controller: poisoned={pf} clean={cf}"
        );
    }

    #[test]
    fn voltage_clamp_hard_refuse() {
        assert!(PowerTargetController::check_voltage_clamp(13_700).is_ok());
        assert!(PowerTargetController::check_voltage_clamp(14_500).is_ok());
        let err = PowerTargetController::check_voltage_clamp(14_501).unwrap_err();
        match err {
            EngageError::VoltageClampViolation {
                requested_mv,
                clamp_mv,
            } => {
                assert_eq!(requested_mv, 14_501);
                assert_eq!(clamp_mv, VOLTAGE_CLAMP_MV);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn home_mode_caps_fan_at_30() {
        assert!(PowerTargetController::check_home_fan_cap(true, 10).is_ok());
        assert!(PowerTargetController::check_home_fan_cap(true, 30).is_ok());
        let err = PowerTargetController::check_home_fan_cap(true, 31).unwrap_err();
        match err {
            EngageError::HomeFanCapViolation {
                requested_pwm,
                cap_pwm,
            } => {
                assert_eq!(requested_pwm, 31);
                assert_eq!(cap_pwm, HOME_FAN_PWM_MAX);
            }
            other => panic!("unexpected error: {:?}", other),
        }
        // Non-home mode does not enforce the cap.
        assert!(PowerTargetController::check_home_fan_cap(false, 100).is_ok());
    }

    #[test]
    fn voltage_violation_refuses_runtime_tick() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        let mut sample = stable_sample(900.0);
        sample.voltage_mv = 14_600;
        match c.tick(sample) {
            TickOutcome::Refused(EngageError::VoltageClampViolation { .. }) => {}
            other => panic!("expected refuse, got {:?}", other),
        }
        // After a refused tick, the controller drops back to
        // disengaged + zero integral.
        assert!(!c.engaged());
    }

    #[test]
    fn home_mode_violation_refuses_runtime_tick() {
        let mut cfg = engaged_config();
        cfg.home_mode = true;
        let mut c = PowerTargetController::new(cfg, 600).unwrap();
        let mut sample = stable_sample(900.0);
        sample.fan_pwm = 40;
        match c.tick(sample) {
            TickOutcome::Refused(EngageError::HomeFanCapViolation { .. }) => {}
            other => panic!("expected refuse, got {:?}", other),
        }
    }

    #[test]
    fn steady_state_gate_waits_three_ticks() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        // First tick has no prior sample → not stable.
        match c.tick(stable_sample(900.0)) {
            TickOutcome::WaitingForSteadyState { ticks_observed: 0 } => {}
            other => panic!("tick 1 got {:?}", other),
        }
        match c.tick(stable_sample(900.0)) {
            TickOutcome::WaitingForSteadyState { ticks_observed: 1 } => {}
            other => panic!("tick 2 got {:?}", other),
        }
        match c.tick(stable_sample(900.0)) {
            TickOutcome::WaitingForSteadyState { ticks_observed: 2 } => {}
            other => panic!("tick 3 got {:?}", other),
        }
        // 4th tick clears the gate (stable_tick_count just reached 3).
        let outcome = c.tick(stable_sample(900.0));
        assert!(
            matches!(
                outcome,
                TickOutcome::Adjusted { .. } | TickOutcome::AtTarget { .. }
            ),
            "tick 4 should engage, got {:?}",
            outcome
        );
    }

    #[test]
    fn pi_proposes_freq_up_when_undershooting_target() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        warm_up(&mut c, 900.0);
        // After warm-up, controller has already moved once. Check the
        // direction is positive (we want more power).
        let after = c.next_frequency_mhz();
        assert!(
            after >= 600,
            "expected freq to rise from 600 when target>actual, got {}",
            after
        );
    }

    #[test]
    fn pi_proposes_freq_down_when_overshooting_target() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        warm_up(&mut c, 1200.0);
        let after = c.next_frequency_mhz();
        assert!(
            after <= 600,
            "expected freq to fall from 600 when target<actual, got {}",
            after
        );
    }

    #[test]
    fn slew_rate_bounds_per_tick_movement() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        // Warm up.
        for _ in 0..STEADY_STATE_REQUIRED_TICKS {
            c.tick(stable_sample(1000.0));
        }
        let before = c.next_frequency_mhz();
        // Massive error.
        let outcome = c.tick(stable_sample(100.0));
        match outcome {
            TickOutcome::Adjusted { delta_mhz, .. } => {
                assert!(
                    delta_mhz.abs() <= DEFAULT_SLEW_MHZ as i32,
                    "delta {} MHz exceeded slew rate {} MHz",
                    delta_mhz,
                    DEFAULT_SLEW_MHZ
                );
            }
            TickOutcome::AtTarget { .. } => {
                // Acceptable if before was already at band edge.
            }
            other => panic!("expected adjustment, got {:?}", other),
        }
        let after = c.next_frequency_mhz();
        assert!((after as i32 - before as i32).abs() <= DEFAULT_SLEW_MHZ as i32);
    }

    #[test]
    fn band_clamp_holds_at_edges() {
        let mut cfg = engaged_config();
        cfg.frequency_band_min_mhz = 500;
        cfg.frequency_band_max_mhz = 550;
        let mut c = PowerTargetController::new(cfg, 510).unwrap();
        warm_up(&mut c, 100.0);
        for _ in 0..20 {
            c.tick(stable_sample(100.0));
        }
        assert!(c.next_frequency_mhz() <= 550);
        // Hammer the other side.
        let mut cfg2 = engaged_config();
        cfg2.frequency_band_min_mhz = 500;
        cfg2.frequency_band_max_mhz = 550;
        let mut c2 = PowerTargetController::new(cfg2, 540).unwrap();
        warm_up(&mut c2, 3000.0);
        for _ in 0..20 {
            c2.tick(stable_sample(3000.0));
        }
        assert!(c2.next_frequency_mhz() >= 500);
    }

    #[test]
    fn error_stays_bounded_under_noise() {
        // Around-target noise should not push the integrator out of
        // its authority band. We pump 100 ticks of zero-mean ±10W
        // noise and check the frequency stays within slew*N of the
        // initial.
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        warm_up(&mut c, 1000.0);
        let baseline = c.next_frequency_mhz();
        let noise = [10.0_f64, -10.0, 5.0, -5.0, 0.0];
        for i in 0..100 {
            let n = noise[i % noise.len()];
            c.tick(stable_sample(1000.0 + n));
        }
        let drift = (c.next_frequency_mhz() as i32 - baseline as i32).abs();
        assert!(
            drift <= (DEFAULT_SLEW_MHZ as i32) * (INTEGRAL_AUTHORITY_TICKS as i32 + 2),
            "freq drifted {} MHz under zero-mean noise, expected bounded by integrator authority",
            drift
        );
    }

    #[test]
    fn instability_resets_steady_state_counter() {
        let mut c = PowerTargetController::new(engaged_config(), 600).unwrap();
        // Two stable ticks.
        c.tick(stable_sample(900.0));
        c.tick(stable_sample(900.0));
        // A big hashrate jump breaks steady-state.
        let mut unstable = stable_sample(900.0);
        unstable.hashrate_ths = 28.0;
        match c.tick(unstable) {
            TickOutcome::WaitingForSteadyState { ticks_observed: 0 } => {}
            other => panic!("expected counter reset, got {:?}", other),
        }
    }

    #[test]
    fn config_round_trips_through_toml() {
        let cfg = PowerTargetConfig {
            enabled: true,
            target_watts: 750,
            home_mode: true,
            frequency_band_min_mhz: 450,
            frequency_band_max_mhz: 700,
            kp: 1.5,
            ki: 0.04,
            slew_rate_mhz: 4,
            tick_seconds: 20,
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: PowerTargetConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn autotune_parent_section_round_trips() {
        let parent = AutotuneConfig {
            power_target: PowerTargetConfig {
                enabled: true,
                target_watts: 1200,
                ..Default::default()
            },
            dps: dps_walker::DpsConfig::default(),
            bad_chip: Default::default(),
            mode: tuner_mode::TunerMode::default(),
        };
        let s = toml::to_string(&parent).unwrap();
        let back: AutotuneConfig = toml::from_str(&s).unwrap();
        assert_eq!(parent, back);
    }

    #[test]
    fn autotune_parent_defaults_have_both_controllers_disabled() {
        let p = AutotuneConfig::default();
        assert!(!p.power_target.enabled);
        assert!(!p.dps.enabled);
        assert!(!p.dps_yields());
        assert!(p.exclusive_resolution_is_defined());
    }

    #[test]
    fn dps_yields_when_power_target_also_enabled() {
        let p = AutotuneConfig {
            power_target: PowerTargetConfig {
                enabled: true,
                target_watts: 1000,
                ..Default::default()
            },
            dps: dps_walker::DpsConfig {
                enabled: true,
                ..Default::default()
            },
            bad_chip: Default::default(),
            mode: tuner_mode::TunerMode::default(),
        };
        assert!(
            p.dps_yields(),
            "with both enabled DPS must yield to power-target"
        );
        // The walker constructor agrees with the cheap pre-check.
        let err =
            dps_walker::DpsWalker::new(p.dps.clone(), p.power_target.enabled, 600).unwrap_err();
        assert_eq!(err, dps_walker::DpsEngageError::YieldsToPowerTarget);
    }

    #[test]
    fn dps_runs_when_power_target_disabled() {
        let p = AutotuneConfig {
            power_target: PowerTargetConfig::default(), // disabled
            dps: dps_walker::DpsConfig {
                enabled: true,
                ..Default::default()
            },
            bad_chip: Default::default(),
            mode: tuner_mode::TunerMode::default(),
        };
        assert!(!p.dps_yields());
        assert!(
            dps_walker::DpsWalker::new(p.dps.clone(), p.power_target.enabled, 600).is_ok(),
            "DPS should construct when power-target is disabled"
        );
    }

    #[test]
    fn autotune_parent_with_dps_only_round_trips() {
        let parent = AutotuneConfig {
            power_target: PowerTargetConfig::default(),
            dps: dps_walker::DpsConfig {
                enabled: true,
                objective: dps_walker::DpsObjective::MinJth,
                tick_s: 60,
                step_mhz: 3,
                convergence_ticks: 5,
                ..Default::default()
            },
            bad_chip: Default::default(),
            mode: tuner_mode::TunerMode::default(),
        };
        let s = toml::to_string(&parent).unwrap();
        let back: AutotuneConfig = toml::from_str(&s).unwrap();
        assert_eq!(parent, back);
    }

    // -----------------------------------------------------------------
    // W24-BC-1 (): bad-chip supervisor config gate (DEFAULT-OFF)
    // -----------------------------------------------------------------

    /// An ENTIRELY ABSENT `[autotune.bad_chip]` block must deserialize to
    /// the disabled default — the daemon must not construct the supervisor.
    #[test]
    fn bad_chip_absent_section_is_disabled() {
        // No `[autotune.bad_chip]` table at all, only an unrelated key.
        let toml_src = r#"
            [power_target]
            enabled = false
        "#;
        let cfg: AutotuneConfig = toml::from_str(toml_src).unwrap();
        assert!(
            !cfg.bad_chip.enabled,
            "absent [autotune.bad_chip] must default to disabled"
        );
        // The whole struct equals the default's bad_chip sub-config.
        assert_eq!(cfg.bad_chip, AutotuneConfig::default().bad_chip);
    }

    /// `[autotune.bad_chip] enabled = false` must stay disabled (and thus the
    /// daemon's `enabled`-gated tee never constructs a supervisor / calls
    /// `observe()`).
    #[test]
    fn bad_chip_enabled_false_is_disabled() {
        let toml_src = r#"
            [bad_chip]
            enabled = false
        "#;
        let cfg: AutotuneConfig = toml::from_str(toml_src).unwrap();
        assert!(!cfg.bad_chip.enabled);
        // The defaulted thresholds are still present (serde defaults filled in).
        assert_eq!(
            cfg.bad_chip.degraded_threshold_pct,
            dcentrald_autotuner::BadChipConfig::default().degraded_threshold_pct
        );
    }

    /// `[autotune.bad_chip] enabled = true` flips the gate on, the threshold
    /// fields deserialize, and a `BadChipSupervisor` constructs reporting
    /// `is_enabled() == true`. (This is the only path on which the daemon
    /// spawns the telemetry-first observer.)
    #[test]
    fn bad_chip_enabled_true_constructs_supervisor() {
        let toml_src = r#"
            [bad_chip]
            enabled = true
            degraded_threshold_pct = 80.0
            min_operational_chips_per_chain = 2
        "#;
        let cfg: AutotuneConfig = toml::from_str(toml_src).unwrap();
        assert!(cfg.bad_chip.enabled, "enabled=true must parse as enabled");
        assert_eq!(cfg.bad_chip.degraded_threshold_pct, 80.0);
        assert_eq!(cfg.bad_chip.min_operational_chips_per_chain, 2);

        // The daemon constructs the supervisor only when enabled; prove it
        // reports enabled and would observe (the daemon never builds it when
        // the gate is false).
        let sup = dcentrald_autotuner::BadChipSupervisor::new(cfg.bad_chip.clone(), Vec::new());
        assert!(sup.is_enabled());

        let disabled = dcentrald_autotuner::BadChipSupervisor::new(
            AutotuneConfig::default().bad_chip,
            Vec::new(),
        );
        assert!(
            !disabled.is_enabled(),
            "default-off config must construct a dormant supervisor"
        );
    }

    /// Full `[autotune.bad_chip]` round-trips through TOML inside the parent
    /// `AutotuneConfig` without disturbing the other controllers' defaults.
    #[test]
    fn bad_chip_round_trips_inside_parent() {
        let parent = AutotuneConfig {
            power_target: PowerTargetConfig::default(),
            dps: dps_walker::DpsConfig::default(),
            bad_chip: dcentrald_autotuner::BadChipConfig {
                enabled: true,
                repeated_bad_windows: 3,
                ..Default::default()
            },
            mode: tuner_mode::TunerMode::default(),
        };
        let s = toml::to_string(&parent).unwrap();
        let back: AutotuneConfig = toml::from_str(&s).unwrap();
        assert_eq!(parent, back);
        assert!(back.bad_chip.enabled);
        assert_eq!(back.bad_chip.repeated_bad_windows, 3);
        // Other controllers untouched.
        assert!(!back.power_target.enabled);
        assert!(!back.dps.enabled);
    }
}
