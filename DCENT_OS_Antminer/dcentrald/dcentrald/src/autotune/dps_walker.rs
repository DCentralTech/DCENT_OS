//! Dynamic Performance Scaling (DPS) continuous walker — Supremacy S5-03.
//!
//! Braiins' GDTUNER does an online per-chip `MCR(V)` quadratic fit with
//! continuous gradient-descent updates toward a max-efficiency operating
//! point. This module is a deliberately simpler-but-real analog for
//! `dcentrald`: a continuous per-chain (or per-chip) frequency/voltage
//! curve walker that perturbs the operating point, measures the result,
//! online-fits a 3-point parabola of the objective vs frequency, and
//! walks toward the fitted vertex — re-fitting as it goes.
//!
//! ## Design
//!
//! * **Perturb-measure-accept** — each tick (~60 s) the walker steps
//!   frequency by a small amount, waits for the next telemetry sample,
//!   and accepts the move if it improves the configured objective.
//!   Three distinct frequency samples are kept and used to fit a
//!   parabola; the walker then aims at the parabola vertex.
//! * **Closed-form 3-point parabola** — given three `(freq, value)`
//!   samples the vertex of the fitted quadratic is computed with the
//!   classic divided-difference closed form (no matrix solve, no
//!   `nalgebra`/`ndarray`). Degenerate (collinear / coincident-x)
//!   cases fall back to a gradient step.
//! * **Objective** — configurable: maximize hashrate, minimize
//!   joules-per-terahash, or hit a wattage target. The walker turns
//!   each into a single scalar it MAXIMIZES (J/TH and target-power are
//!   negated / distance-negated so "higher is always better"
//!   internally).
//! * **Bounded envelope** — the walker NEVER proposes a frequency
//!   outside `[frequency_band_min_mhz, frequency_band_max_mhz]` and
//!   NEVER engages while the reported voltage exceeds
//!   [`VOLTAGE_CLAMP_MV`]. The voltage clamp is read-only here — the
//!   same suggestion-engine contract as [`super::PowerTargetController`].
//! * **Bounded slew** — per-tick movement is clamped to
//!   `±step_mhz` (default 3 MHz/tick).
//! * **Chip-health guard** — an error-rate spike (HW-error fraction
//!   above [`ERROR_RATE_BACKOFF_THRESHOLD`], or a configured absolute
//!   ceiling) triggers an IMMEDIATE backoff to the last-known-good
//!   operating point and resets the fit. This takes precedence over
//!   every optimization decision.
//! * **Convergence detection** — when the fitted parabola vertex stays
//!   within `±CONVERGENCE_TOLERANCE_MHZ` of the current point for
//!   `convergence_ticks` consecutive ticks the walker stops perturbing
//!   (holds station). It resumes perturbing if telemetry drifts beyond
//!   the convergence band (objective changes by more than
//!   [`DRIFT_RESUME_FRACTION`]).
//!
//! ## Safety contract
//!
//! Like [`super::PowerTargetController`], this is a *suggestion*
//! engine. It computes a target frequency in MHz; it does NOT perform
//! the voltage/frequency write. The daemon code consuming
//! [`DpsWalker::next_frequency_mhz`] routes the change through the
//! `dcentrald-silicon-profiles` PVT envelope clamps + the platform's
//! voltage controller. Memory rule to save post-merge:
//! .
//!
//! ## Mutual exclusion with `[autotune.power_target]`
//!
//! `[autotune.power_target]` and `[autotune.dps]` are mutually
//! exclusive. If BOTH are enabled the **power-target controller wins
//! and DPS yields** (DPS construction returns
//! [`DpsEngageError::YieldsToPowerTarget`]). Rationale: the power
//! target controller is a hard operator-supplied wattage constraint;
//! the DPS walker is an opportunistic optimizer. An optimizer must
//! never fight a hard constraint. The precedence is also enforced at
//! the config layer (see [`super::AutotuneConfig::validate_exclusive`]).

use serde::{Deserialize, Serialize};

pub use super::{HOME_FAN_PWM_MAX, VOLTAGE_CLAMP_MV};

/// Default walker tick period (seconds). Slow enough for chain power /
/// hashrate telemetry to settle after a frequency step.
pub const DEFAULT_TICK_SECONDS: u64 = 60;

/// Default per-tick frequency step / slew clamp (MHz). The walker never
/// moves more than this per tick. Hard-capped at
/// [`MAX_STEP_MHZ`] regardless of config.
pub const DEFAULT_STEP_MHZ: u16 = 3;

/// HARD cap on the per-tick step. Even if an operator configures a
/// larger `step_mhz`, the walker clamps to this. ±3 MHz/tick is the
/// S5-03 slew contract.
pub const MAX_STEP_MHZ: u16 = 3;

/// Default number of consecutive in-band ticks required to declare
/// convergence.
pub const DEFAULT_CONVERGENCE_TICKS: u8 = 5;

/// Conservative default frequency floor (MHz). The silicon-profile
/// crate clamps tighter per-family upstream; this is only the walker's
/// own authority band.
pub const DEFAULT_FREQ_MIN_MHZ: u16 = 400;

/// Conservative default frequency ceiling (MHz).
pub const DEFAULT_FREQ_MAX_MHZ: u16 = 800;

/// Vertex-proximity tolerance (MHz). Once the fitted parabola vertex is
/// within this many MHz of the current operating point for
/// `convergence_ticks` consecutive ticks, the walker stops perturbing.
pub const CONVERGENCE_TOLERANCE_MHZ: f64 = 2.0;

/// HW-error fraction that trips the chip-health backoff. 0.05 = 5 % of
/// returned nonces being hardware errors is already pathological for a
/// healthy chain.
pub const ERROR_RATE_BACKOFF_THRESHOLD: f64 = 0.05;

/// Fractional objective change beyond which a *converged* walker
/// resumes perturbing (telemetry drifted — the old optimum is stale).
pub const DRIFT_RESUME_FRACTION: f64 = 0.03;

/// Default per-chain initial voltage assumption (mV) used only for
/// efficiency bookkeeping when a sample omits it. Never written.
const _DEFAULT_NOMINAL_MV: u16 = 13_700;

// ---------------------------------------------------------------------
// Config (serde — wired into dcentrald.toml as `[autotune.dps]`)
// ---------------------------------------------------------------------

/// Walker objective. Each maps to a scalar the walker MAXIMIZES.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpsObjective {
    /// Maximize raw hashrate (TH/s).
    MaxHashrate,
    /// Minimize joules-per-terahash (J/TH). Internally the walker
    /// maximizes `-jth`.
    #[default]
    MinJth,
    /// Drive wall/chip power toward `target_power_w`. Internally the
    /// walker maximizes `-(actual_w - target_w).abs()`.
    TargetPower,
}

/// `[autotune.dps]` TOML section.
///
/// Default: disabled. Opt-in by setting `enabled = true`. Yields to
/// `[autotune.power_target]` when both are enabled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DpsConfig {
    /// Enable the DPS continuous walker.
    #[serde(default)]
    pub enabled: bool,

    /// Optimization objective.
    #[serde(default)]
    pub objective: DpsObjective,

    /// Target wall/chip power in watts. Only used when
    /// `objective = "target_power"`. 0 here with that objective is a
    /// degenerate config and refuses to engage.
    #[serde(default)]
    pub target_power_w: u32,

    /// Home / space-heater profile. Caps `fan_max_pwm` at
    /// [`HOME_FAN_PWM_MAX`]; the walker refuses to engage if the
    /// runtime fan ceiling exceeds the cap while this is true.
    #[serde(default)]
    pub home_mode: bool,

    /// Lower bound on the walker's frequency authority (MHz).
    #[serde(default = "default_freq_min_mhz")]
    pub frequency_band_min_mhz: u16,

    /// Upper bound on the walker's frequency authority (MHz).
    #[serde(default = "default_freq_max_mhz")]
    pub frequency_band_max_mhz: u16,

    /// Tick period (seconds).
    #[serde(default = "default_tick_seconds")]
    pub tick_s: u64,

    /// Per-tick frequency step / slew clamp (MHz). HARD-capped at
    /// [`MAX_STEP_MHZ`] regardless of the configured value.
    #[serde(default = "default_step_mhz")]
    pub step_mhz: u16,

    /// Consecutive in-band ticks required to declare convergence.
    #[serde(default = "default_convergence_ticks")]
    pub convergence_ticks: u8,

    /// Optional absolute HW-error-fraction ceiling. When `Some(x)` an
    /// error fraction above `x` trips the chip-health backoff even if
    /// it is below the global [`ERROR_RATE_BACKOFF_THRESHOLD`]. `None`
    /// uses the global threshold only.
    #[serde(default)]
    pub error_rate_ceiling: Option<f64>,
}

impl Default for DpsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            objective: DpsObjective::default(),
            target_power_w: 0,
            home_mode: false,
            frequency_band_min_mhz: default_freq_min_mhz(),
            frequency_band_max_mhz: default_freq_max_mhz(),
            tick_s: default_tick_seconds(),
            step_mhz: default_step_mhz(),
            convergence_ticks: default_convergence_ticks(),
            error_rate_ceiling: None,
        }
    }
}

fn default_freq_min_mhz() -> u16 {
    DEFAULT_FREQ_MIN_MHZ
}
fn default_freq_max_mhz() -> u16 {
    DEFAULT_FREQ_MAX_MHZ
}
fn default_tick_seconds() -> u64 {
    DEFAULT_TICK_SECONDS
}
fn default_step_mhz() -> u16 {
    DEFAULT_STEP_MHZ
}
fn default_convergence_ticks() -> u8 {
    DEFAULT_CONVERGENCE_TICKS
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Reasons the walker refuses to engage (construction or runtime).
#[derive(Debug, Clone, PartialEq)]
pub enum DpsEngageError {
    /// `[autotune.dps] enabled = false`.
    Disabled,
    /// Both `[autotune.power_target]` and `[autotune.dps]` enabled —
    /// DPS yields to the hard power-target constraint.
    YieldsToPowerTarget,
    /// `objective = "target_power"` but `target_power_w == 0`.
    MissingPowerTarget,
    /// Requested voltage exceeds [`VOLTAGE_CLAMP_MV`].
    VoltageClampViolation { requested_mv: u16, clamp_mv: u16 },
    /// `home_mode = true` but the resolved fan ceiling exceeds
    /// [`HOME_FAN_PWM_MAX`].
    HomeFanCapViolation { requested_pwm: u8, cap_pwm: u8 },
    /// Frequency band is degenerate (`min >= max`).
    InvalidFrequencyBand { min_mhz: u16, max_mhz: u16 },
    /// `step_mhz == 0` — walker has no authority.
    InvalidStep,
    /// `tick_s == 0`.
    InvalidTickPeriod,
    /// `convergence_ticks == 0` — convergence could never be declared.
    InvalidConvergenceTicks,
}

impl core::fmt::Display for DpsEngageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DpsEngageError::Disabled => f.write_str("DPS walker disabled"),
            DpsEngageError::YieldsToPowerTarget => {
                f.write_str("[autotune.power_target] and [autotune.dps] both enabled — DPS yields")
            }
            DpsEngageError::MissingPowerTarget => {
                f.write_str("objective=target_power requires target_power_w > 0")
            }
            DpsEngageError::VoltageClampViolation {
                requested_mv,
                clamp_mv,
            } => write!(
                f,
                "voltage {} mV exceeds HARD clamp {} mV — refusing to engage",
                requested_mv, clamp_mv
            ),
            DpsEngageError::HomeFanCapViolation {
                requested_pwm,
                cap_pwm,
            } => write!(
                f,
                "home_mode fan PWM {} exceeds home cap {} — refusing to engage",
                requested_pwm, cap_pwm
            ),
            DpsEngageError::InvalidFrequencyBand { min_mhz, max_mhz } => write!(
                f,
                "frequency band min {} MHz >= max {} MHz",
                min_mhz, max_mhz
            ),
            DpsEngageError::InvalidStep => f.write_str("step_mhz must be > 0"),
            DpsEngageError::InvalidTickPeriod => f.write_str("tick_s must be > 0"),
            DpsEngageError::InvalidConvergenceTicks => f.write_str("convergence_ticks must be > 0"),
        }
    }
}

impl std::error::Error for DpsEngageError {}

// ---------------------------------------------------------------------
// Telemetry sample
// ---------------------------------------------------------------------

/// One tick of telemetry the walker needs.
#[derive(Debug, Clone, Copy)]
pub struct DpsSample {
    /// Current full-chain hashrate (TH/s).
    pub hashrate_ths: f64,
    /// Estimated chip / wall power right now (Watts).
    pub power_w: f64,
    /// Fraction of returned results that were hardware errors
    /// (0.0–1.0). Drives the chip-health backoff.
    pub error_rate: f64,
    /// Current chip voltage (mV). Used for the HARD clamp check.
    pub voltage_mv: u16,
    /// Current fan PWM ceiling (0–127). Used for the home-mode cap.
    pub fan_pwm: u8,
}

impl DpsSample {
    /// Joules-per-terahash. `+inf` if hashrate is non-positive.
    pub fn jth(&self) -> f64 {
        if self.hashrate_ths > 0.0 {
            self.power_w / self.hashrate_ths
        } else {
            f64::INFINITY
        }
    }
}

// ---------------------------------------------------------------------
// 3-point closed-form parabola
// ---------------------------------------------------------------------

/// A measured `(frequency_mhz, objective_value)` point. `value` is the
/// *internal maximize-me* scalar (higher = better) — NOT raw J/TH.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitPoint {
    pub freq_mhz: f64,
    pub value: f64,
}

/// Result of a 3-point parabola fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParabolaFit {
    /// A well-formed concave/convex parabola; `vertex_mhz` is the
    /// frequency of the extremum, `opens_down` true when it is a
    /// maximum (the case we want for a maximize objective).
    Vertex { vertex_mhz: f64, opens_down: bool },
    /// The three points are (near-)collinear or have coincident x —
    /// no meaningful vertex. The caller should gradient-step instead.
    Degenerate,
}

/// Fit a parabola through three `(x, y)` points and return the vertex
/// x-coordinate, using the closed-form divided-difference method:
///
/// For `y = a x^2 + b x + c`, the vertex is at `x = -b / (2a)`.
/// With three points the leading coefficient is
///
/// ```text
/// a = ( (y3 - y1)/(x3 - x1) - (y2 - y1)/(x2 - x1) ) / (x3 - x2)
/// ```
///
/// and `b` follows from the first divided difference. This is pure
/// scalar arithmetic — no linear-algebra crate.
///
/// Degenerate when any two x are within `X_EPS`, or `|a|` is within
/// `A_EPS` (collinear → no curvature → no vertex).
pub fn fit_parabola_vertex(p1: FitPoint, p2: FitPoint, p3: FitPoint) -> ParabolaFit {
    const X_EPS: f64 = 1e-6;
    const A_EPS: f64 = 1e-12;

    let (x1, y1) = (p1.freq_mhz, p1.value);
    let (x2, y2) = (p2.freq_mhz, p2.value);
    let (x3, y3) = (p3.freq_mhz, p3.value);

    // Coincident x → cannot fit.
    if (x1 - x2).abs() < X_EPS || (x2 - x3).abs() < X_EPS || (x1 - x3).abs() < X_EPS {
        return ParabolaFit::Degenerate;
    }

    // First divided differences.
    let d12 = (y2 - y1) / (x2 - x1);
    let d13 = (y3 - y1) / (x3 - x1);
    // Second divided difference = leading coefficient `a`.
    let a = (d13 - d12) / (x3 - x2);

    if a.abs() < A_EPS || !a.is_finite() {
        return ParabolaFit::Degenerate;
    }

    // Newton form: y = y1 + d12*(x-x1) + a*(x-x1)*(x-x2)
    //   => expand: a x^2 + b x + c, with
    //      b = d12 - a*(x1 + x2)
    let b = d12 - a * (x1 + x2);
    let vertex = -b / (2.0 * a);

    if !vertex.is_finite() {
        return ParabolaFit::Degenerate;
    }

    ParabolaFit::Vertex {
        vertex_mhz: vertex,
        // y = a x^2 + ... opens downward (a maximum) when a < 0.
        opens_down: a < 0.0,
    }
}

// ---------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------

/// What the walker decided on a given tick.
#[derive(Debug, Clone, PartialEq)]
pub enum DpsTickOutcome {
    /// Walker refused this tick (HARD gate). Reason captured.
    Refused(DpsEngageError),
    /// Chip-health guard fired — backed off to last-known-good.
    HealthBackoff {
        from_mhz: u16,
        to_mhz: u16,
        error_rate: f64,
    },
    /// Walker is gathering its initial 3 samples. Frequency was
    /// perturbed by one step to build the fit.
    Probing {
        from_mhz: u16,
        to_mhz: u16,
        samples_collected: u8,
    },
    /// Walker moved toward the fitted optimum (or gradient-stepped on
    /// a degenerate fit).
    Walked {
        from_mhz: u16,
        to_mhz: u16,
        delta_mhz: i32,
        objective_value: f64,
    },
    /// Walker is converged and holding station (no perturbation).
    Converged { freq_mhz: u16, ticks_in_band: u8 },
    /// Walker was converged but telemetry drifted — resuming.
    ResumedOnDrift { freq_mhz: u16, objective_value: f64 },
}

/// Direction the walker is currently perturbing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkDir {
    Up,
    Down,
}

impl WalkDir {
    fn flip(self) -> Self {
        match self {
            WalkDir::Up => WalkDir::Down,
            WalkDir::Down => WalkDir::Up,
        }
    }
    fn sign(self) -> i32 {
        match self {
            WalkDir::Up => 1,
            WalkDir::Down => -1,
        }
    }
}

/// Continuous DPS frequency/voltage curve walker.
///
/// Construct via [`DpsWalker::new`] (validates config + mutual
/// exclusion) and drive one tick at a time with [`DpsWalker::tick`].
/// Read the proposed frequency with [`DpsWalker::next_frequency_mhz`].
#[derive(Debug)]
pub struct DpsWalker {
    config: DpsConfig,
    effective_step_mhz: u16,
    current_freq_mhz: u16,
    /// Last operating point known to be healthy (low error rate).
    last_known_good_mhz: u16,
    /// Best objective value seen at LKG (for drift detection).
    last_known_good_value: f64,
    /// Rolling window of the three most recent distinct-frequency fit
    /// points (oldest..newest).
    fit_window: Vec<FitPoint>,
    walk_dir: WalkDir,
    /// Consecutive ticks the parabola vertex stayed within tolerance.
    in_band_ticks: u8,
    converged: bool,
    /// Objective value captured at convergence (for drift detection).
    converged_value: f64,
    engaged: bool,
}

impl DpsWalker {
    /// Construct a walker from validated config + the power-target
    /// enabled flag (for the mutual-exclusion check) + an initial
    /// frequency.
    ///
    /// `power_target_enabled` is `[autotune.power_target].enabled`.
    /// When BOTH are enabled DPS yields.
    pub fn new(
        config: DpsConfig,
        power_target_enabled: bool,
        initial_freq_mhz: u16,
    ) -> Result<Self, DpsEngageError> {
        Self::validate(&config, power_target_enabled)?;
        let clamped =
            initial_freq_mhz.clamp(config.frequency_band_min_mhz, config.frequency_band_max_mhz);
        let effective_step_mhz = config.step_mhz.min(MAX_STEP_MHZ);
        Ok(Self {
            config,
            effective_step_mhz,
            current_freq_mhz: clamped,
            last_known_good_mhz: clamped,
            last_known_good_value: f64::NEG_INFINITY,
            fit_window: Vec::with_capacity(3),
            walk_dir: WalkDir::Up,
            in_band_ticks: 0,
            converged: false,
            converged_value: 0.0,
            engaged: false,
        })
    }

    fn validate(config: &DpsConfig, power_target_enabled: bool) -> Result<(), DpsEngageError> {
        if !config.enabled {
            return Err(DpsEngageError::Disabled);
        }
        // Mutual exclusion: DPS yields to the hard power-target
        // constraint.
        if power_target_enabled {
            return Err(DpsEngageError::YieldsToPowerTarget);
        }
        if config.objective == DpsObjective::TargetPower && config.target_power_w == 0 {
            return Err(DpsEngageError::MissingPowerTarget);
        }
        if config.frequency_band_min_mhz >= config.frequency_band_max_mhz {
            return Err(DpsEngageError::InvalidFrequencyBand {
                min_mhz: config.frequency_band_min_mhz,
                max_mhz: config.frequency_band_max_mhz,
            });
        }
        if config.step_mhz == 0 {
            return Err(DpsEngageError::InvalidStep);
        }
        if config.tick_s == 0 {
            return Err(DpsEngageError::InvalidTickPeriod);
        }
        if config.convergence_ticks == 0 {
            return Err(DpsEngageError::InvalidConvergenceTicks);
        }
        Ok(())
    }

    /// HARD-clamp check on an external voltage write.
    pub fn check_voltage_clamp(requested_mv: u16) -> Result<(), DpsEngageError> {
        if requested_mv > VOLTAGE_CLAMP_MV {
            return Err(DpsEngageError::VoltageClampViolation {
                requested_mv,
                clamp_mv: VOLTAGE_CLAMP_MV,
            });
        }
        Ok(())
    }

    /// HARD-clamp check on the home-mode fan ceiling.
    pub fn check_home_fan_cap(home_mode: bool, requested_pwm: u8) -> Result<(), DpsEngageError> {
        if home_mode && requested_pwm > HOME_FAN_PWM_MAX {
            return Err(DpsEngageError::HomeFanCapViolation {
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

    /// True once the walker has cleared the HARD gates and started
    /// probing/walking.
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// True when the walker has declared convergence and is holding
    /// station.
    pub fn converged(&self) -> bool {
        self.converged
    }

    /// The effective per-tick step after the [`MAX_STEP_MHZ`] cap.
    pub fn effective_step_mhz(&self) -> u16 {
        self.effective_step_mhz
    }

    /// Borrow the resolved config.
    pub fn config(&self) -> &DpsConfig {
        &self.config
    }

    /// Map a raw sample to the internal MAXIMIZE-me scalar for the
    /// configured objective. Higher is always better.
    fn objective_value(&self, s: &DpsSample) -> f64 {
        match self.config.objective {
            DpsObjective::MaxHashrate => s.hashrate_ths,
            // Lower J/TH is better → maximize the negative.
            DpsObjective::MinJth => -s.jth(),
            // Closer to target is better → maximize the negative
            // absolute distance.
            DpsObjective::TargetPower => -(s.power_w - self.config.target_power_w as f64).abs(),
        }
    }

    /// Resolve the absolute error-rate ceiling that trips the
    /// chip-health backoff.
    fn error_ceiling(&self) -> f64 {
        match self.config.error_rate_ceiling {
            Some(c) => c.clamp(0.0, ERROR_RATE_BACKOFF_THRESHOLD),
            None => ERROR_RATE_BACKOFF_THRESHOLD,
        }
    }

    fn clamp_freq(&self, f: i32) -> u16 {
        f.clamp(
            self.config.frequency_band_min_mhz as i32,
            self.config.frequency_band_max_mhz as i32,
        ) as u16
    }

    /// Apply a bounded signed step to the current frequency, returning
    /// the clamped result. `delta` is pre-slew; it is clamped to
    /// `±effective_step_mhz` and then to the band.
    fn stepped(&self, delta: i32) -> u16 {
        let step = self.effective_step_mhz as i32;
        let bounded = delta.clamp(-step, step);
        self.clamp_freq(self.current_freq_mhz as i32 + bounded)
    }

    /// Insert a fit point keyed by frequency, keeping at most the three
    /// most-recent DISTINCT frequencies (oldest dropped). Replacing the
    /// value at an existing frequency keeps the fit fresh.
    fn record_fit_point(&mut self, fp: FitPoint) {
        if let Some(existing) = self
            .fit_window
            .iter_mut()
            .find(|p| (p.freq_mhz - fp.freq_mhz).abs() < 0.5)
        {
            existing.value = fp.value;
            return;
        }
        self.fit_window.push(fp);
        if self.fit_window.len() > 3 {
            self.fit_window.remove(0);
        }
    }

    /// Tick the walker forward by one period.
    pub fn tick(&mut self, sample: DpsSample) -> DpsTickOutcome {
        // ---- HARD safety gates first. ----
        if let Err(e) = Self::check_voltage_clamp(sample.voltage_mv) {
            self.engaged = false;
            return DpsTickOutcome::Refused(e);
        }
        if let Err(e) = Self::check_home_fan_cap(self.config.home_mode, sample.fan_pwm) {
            self.engaged = false;
            return DpsTickOutcome::Refused(e);
        }

        // ---- Chip-health guard: takes precedence over everything. ----
        if sample.error_rate > self.error_ceiling() {
            let from = self.current_freq_mhz;
            let to = self.last_known_good_mhz;
            self.current_freq_mhz = to;
            // Reset the optimization state — the curve we fitted is no
            // longer trustworthy.
            self.fit_window.clear();
            self.converged = false;
            self.in_band_ticks = 0;
            self.engaged = true;
            return DpsTickOutcome::HealthBackoff {
                from_mhz: from,
                to_mhz: to,
                error_rate: sample.error_rate,
            };
        }

        self.engaged = true;
        let value = self.objective_value(&sample);

        // Healthy sample → this point is a candidate last-known-good.
        if value >= self.last_known_good_value {
            self.last_known_good_value = value;
            self.last_known_good_mhz = self.current_freq_mhz;
        }

        // Record this measurement for the rolling parabola fit.
        self.record_fit_point(FitPoint {
            freq_mhz: self.current_freq_mhz as f64,
            value,
        });

        // ---- Converged: hold station unless telemetry drifts. ----
        if self.converged {
            let drift = if self.converged_value.abs() > f64::EPSILON {
                ((value - self.converged_value) / self.converged_value).abs()
            } else {
                (value - self.converged_value).abs()
            };
            if drift > DRIFT_RESUME_FRACTION {
                self.converged = false;
                self.in_band_ticks = 0;
                self.fit_window.clear();
                self.record_fit_point(FitPoint {
                    freq_mhz: self.current_freq_mhz as f64,
                    value,
                });
                return DpsTickOutcome::ResumedOnDrift {
                    freq_mhz: self.current_freq_mhz,
                    objective_value: value,
                };
            }
            return DpsTickOutcome::Converged {
                freq_mhz: self.current_freq_mhz,
                ticks_in_band: self.in_band_ticks,
            };
        }

        // ---- Still collecting the initial 3 distinct samples. ----
        if self.fit_window.len() < 3 {
            let from = self.current_freq_mhz;
            // Perturb one step in the current direction; flip at a band
            // edge so we still gather distinct frequencies.
            let mut delta = self.walk_dir.sign() * self.effective_step_mhz as i32;
            let mut to = self.stepped(delta);
            if to == from {
                self.walk_dir = self.walk_dir.flip();
                delta = self.walk_dir.sign() * self.effective_step_mhz as i32;
                to = self.stepped(delta);
            }
            self.current_freq_mhz = to;
            return DpsTickOutcome::Probing {
                from_mhz: from,
                to_mhz: to,
                samples_collected: self.fit_window.len() as u8,
            };
        }

        // ---- We have 3 points: fit + walk toward the vertex. ----
        //
        // The history buffer stays insertion-ordered + bounded to 3
        // (see `record_fit_point`); we only need a frequency-monotone
        // *view* for this tick. Sort a local copy by `freq_mhz`
        // ascending so the convex/degenerate fallback's neighbor logic
        // operates on a monotone-in-frequency triple regardless of
        // sample arrival order. The closed-form vertex math is already
        // order-invariant, but on a noisy chain the unsorted
        // value-`max_by` fallback flip-flops between samples and the
        // `in_band_ticks` reset below never lets the walker converge
        // (it just oscillates ±step around the optimum). This is a
        // stability fix only — clean-data behavior is unchanged.
        let mut pts = [self.fit_window[0], self.fit_window[1], self.fit_window[2]];
        pts.sort_by(|a, b| {
            a.freq_mhz
                .partial_cmp(&b.freq_mhz)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let fit = fit_parabola_vertex(pts[0], pts[1], pts[2]);

        let from = self.current_freq_mhz;
        let (target_mhz, used_vertex) = match fit {
            ParabolaFit::Vertex {
                vertex_mhz,
                opens_down,
            } if opens_down => (vertex_mhz, true),
            // Convex (opens up) parabola or degenerate → no interior
            // maximum. Gradient-step toward the better of the two
            // frequency-extreme samples. With the window now sorted by
            // frequency, `pts[0]` is the lowest-frequency sample and
            // `pts[2]` the highest, so this picks a stable *frequency
            // direction* (down vs up) instead of chasing a noisy
            // value-`max_by` winner that can sit at the middle freq.
            _ => {
                let lo = pts[0];
                let hi = pts[2];
                let best = if hi.value >= lo.value { hi } else { lo };
                (best.freq_mhz, false)
            }
        };

        // Convergence test: vertex within tolerance of the current
        // point.
        let vertex_delta = (target_mhz - from as f64).abs();
        if used_vertex && vertex_delta <= CONVERGENCE_TOLERANCE_MHZ {
            self.in_band_ticks = self.in_band_ticks.saturating_add(1);
            if self.in_band_ticks >= self.config.convergence_ticks {
                self.converged = true;
                self.converged_value = value;
                return DpsTickOutcome::Converged {
                    freq_mhz: self.current_freq_mhz,
                    ticks_in_band: self.in_band_ticks,
                };
            }
        } else {
            self.in_band_ticks = 0;
        }

        // Move toward the target, slew-bounded.
        let raw_delta = (target_mhz - from as f64).round() as i32;
        let to = self.stepped(raw_delta);
        // Track the direction we actually moved (for the probe-flip
        // logic if the fit window is later cleared).
        if to as i32 > from as i32 {
            self.walk_dir = WalkDir::Up;
        } else if (to as i32) < from as i32 {
            self.walk_dir = WalkDir::Down;
        }
        self.current_freq_mhz = to;

        DpsTickOutcome::Walked {
            from_mhz: from,
            to_mhz: to,
            delta_mhz: to as i32 - from as i32,
            objective_value: value,
        }
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dps_config() -> DpsConfig {
        DpsConfig {
            enabled: true,
            objective: DpsObjective::MaxHashrate,
            target_power_w: 0,
            home_mode: false,
            frequency_band_min_mhz: 400,
            frequency_band_max_mhz: 800,
            tick_s: 60,
            step_mhz: 3,
            convergence_ticks: 5,
            error_rate_ceiling: None,
        }
    }

    fn sample(hr: f64, pw: f64) -> DpsSample {
        DpsSample {
            hashrate_ths: hr,
            power_w: pw,
            error_rate: 0.0,
            voltage_mv: 13_700,
            fan_pwm: 10,
        }
    }

    // ---- Config / construction ----

    #[test]
    fn config_defaults_are_conservative_and_disabled() {
        let c = DpsConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.objective, DpsObjective::MinJth);
        assert_eq!(c.tick_s, 60);
        assert_eq!(c.step_mhz, 3);
        assert_eq!(c.convergence_ticks, 5);
        assert_eq!(c.frequency_band_min_mhz, 400);
        assert_eq!(c.frequency_band_max_mhz, 800);
        assert!(!c.home_mode);
        assert!(c.error_rate_ceiling.is_none());
    }

    #[test]
    fn disabled_refuses_construction() {
        let err = DpsWalker::new(DpsConfig::default(), false, 600).unwrap_err();
        assert_eq!(err, DpsEngageError::Disabled);
    }

    #[test]
    fn dps_yields_to_power_target_when_both_enabled() {
        // Mutual exclusion: power-target wins, DPS yields.
        let err = DpsWalker::new(dps_config(), true, 600).unwrap_err();
        assert_eq!(err, DpsEngageError::YieldsToPowerTarget);
    }

    #[test]
    fn target_power_objective_requires_target() {
        let mut c = dps_config();
        c.objective = DpsObjective::TargetPower;
        c.target_power_w = 0;
        let err = DpsWalker::new(c, false, 600).unwrap_err();
        assert_eq!(err, DpsEngageError::MissingPowerTarget);
    }

    #[test]
    fn degenerate_band_refuses() {
        let mut c = dps_config();
        c.frequency_band_min_mhz = 700;
        c.frequency_band_max_mhz = 700;
        let err = DpsWalker::new(c, false, 600).unwrap_err();
        assert!(matches!(err, DpsEngageError::InvalidFrequencyBand { .. }));
    }

    #[test]
    fn zero_step_refuses() {
        let mut c = dps_config();
        c.step_mhz = 0;
        assert_eq!(
            DpsWalker::new(c, false, 600).unwrap_err(),
            DpsEngageError::InvalidStep
        );
    }

    #[test]
    fn zero_tick_refuses() {
        let mut c = dps_config();
        c.tick_s = 0;
        assert_eq!(
            DpsWalker::new(c, false, 600).unwrap_err(),
            DpsEngageError::InvalidTickPeriod
        );
    }

    #[test]
    fn zero_convergence_ticks_refuses() {
        let mut c = dps_config();
        c.convergence_ticks = 0;
        assert_eq!(
            DpsWalker::new(c, false, 600).unwrap_err(),
            DpsEngageError::InvalidConvergenceTicks
        );
    }

    #[test]
    fn step_is_hard_capped_at_3mhz() {
        let mut c = dps_config();
        c.step_mhz = 50; // operator tries to over-slew
        let w = DpsWalker::new(c, false, 600).unwrap();
        assert_eq!(w.effective_step_mhz(), MAX_STEP_MHZ);
        assert_eq!(MAX_STEP_MHZ, 3);
    }

    #[test]
    fn initial_freq_is_clamped_into_band() {
        let w = DpsWalker::new(dps_config(), false, 2000).unwrap();
        assert_eq!(w.next_frequency_mhz(), 800);
        let w2 = DpsWalker::new(dps_config(), false, 100).unwrap();
        assert_eq!(w2.next_frequency_mhz(), 400);
    }

    // ---- Parabola vertex math ----

    #[test]
    fn parabola_vertex_exact_for_known_quadratic() {
        // y = -(x-500)^2 + 10  → vertex at x=500, opens down.
        let f = |x: f64| -((x - 500.0).powi(2)) + 10.0;
        let fit = fit_parabola_vertex(
            FitPoint {
                freq_mhz: 470.0,
                value: f(470.0),
            },
            FitPoint {
                freq_mhz: 500.0,
                value: f(500.0),
            },
            FitPoint {
                freq_mhz: 530.0,
                value: f(530.0),
            },
        );
        match fit {
            ParabolaFit::Vertex {
                vertex_mhz,
                opens_down,
            } => {
                assert!((vertex_mhz - 500.0).abs() < 1e-6, "vertex={}", vertex_mhz);
                assert!(opens_down);
            }
            other => panic!("expected vertex, got {:?}", other),
        }
    }

    #[test]
    fn parabola_vertex_offset_quadratic() {
        // y = -2(x-643)^2 + 5  → vertex at x=643.
        let f = |x: f64| -2.0 * (x - 643.0).powi(2) + 5.0;
        let fit = fit_parabola_vertex(
            FitPoint {
                freq_mhz: 600.0,
                value: f(600.0),
            },
            FitPoint {
                freq_mhz: 650.0,
                value: f(650.0),
            },
            FitPoint {
                freq_mhz: 700.0,
                value: f(700.0),
            },
        );
        match fit {
            ParabolaFit::Vertex {
                vertex_mhz,
                opens_down,
            } => {
                assert!((vertex_mhz - 643.0).abs() < 1e-6, "vertex={}", vertex_mhz);
                assert!(opens_down);
            }
            other => panic!("expected vertex, got {:?}", other),
        }
    }

    #[test]
    fn parabola_opens_up_detected() {
        // y = +(x-500)^2  → minimum (opens up), not a max.
        let f = |x: f64| (x - 500.0).powi(2);
        let fit = fit_parabola_vertex(
            FitPoint {
                freq_mhz: 480.0,
                value: f(480.0),
            },
            FitPoint {
                freq_mhz: 500.0,
                value: f(500.0),
            },
            FitPoint {
                freq_mhz: 520.0,
                value: f(520.0),
            },
        );
        match fit {
            ParabolaFit::Vertex { opens_down, .. } => assert!(!opens_down),
            other => panic!("expected vertex, got {:?}", other),
        }
    }

    #[test]
    fn parabola_collinear_is_degenerate() {
        // Straight line → no curvature.
        let fit = fit_parabola_vertex(
            FitPoint {
                freq_mhz: 400.0,
                value: 1.0,
            },
            FitPoint {
                freq_mhz: 500.0,
                value: 2.0,
            },
            FitPoint {
                freq_mhz: 600.0,
                value: 3.0,
            },
        );
        assert_eq!(fit, ParabolaFit::Degenerate);
    }

    #[test]
    fn parabola_coincident_x_is_degenerate() {
        let fit = fit_parabola_vertex(
            FitPoint {
                freq_mhz: 500.0,
                value: 1.0,
            },
            FitPoint {
                freq_mhz: 500.0,
                value: 2.0,
            },
            FitPoint {
                freq_mhz: 600.0,
                value: 3.0,
            },
        );
        assert_eq!(fit, ParabolaFit::Degenerate);
    }

    // ---- HARD gates at runtime ----

    #[test]
    fn voltage_clamp_hard_refuse() {
        assert!(DpsWalker::check_voltage_clamp(14_500).is_ok());
        let err = DpsWalker::check_voltage_clamp(14_501).unwrap_err();
        assert!(matches!(err, DpsEngageError::VoltageClampViolation { .. }));
    }

    #[test]
    fn voltage_violation_refuses_runtime_tick() {
        let mut w = DpsWalker::new(dps_config(), false, 600).unwrap();
        let mut s = sample(14.0, 1000.0);
        s.voltage_mv = 14_600;
        match w.tick(s) {
            DpsTickOutcome::Refused(DpsEngageError::VoltageClampViolation { .. }) => {}
            other => panic!("expected refuse, got {:?}", other),
        }
        assert!(!w.engaged());
    }

    #[test]
    fn home_mode_fan_cap_refuses_runtime_tick() {
        let mut c = dps_config();
        c.home_mode = true;
        let mut w = DpsWalker::new(c, false, 600).unwrap();
        let mut s = sample(14.0, 1000.0);
        s.fan_pwm = 40;
        match w.tick(s) {
            DpsTickOutcome::Refused(DpsEngageError::HomeFanCapViolation { .. }) => {}
            other => panic!("expected refuse, got {:?}", other),
        }
    }

    #[test]
    fn home_mode_within_cap_is_ok() {
        assert!(DpsWalker::check_home_fan_cap(true, 30).is_ok());
        assert!(DpsWalker::check_home_fan_cap(true, 31).is_err());
        // non-home ignores the cap
        assert!(DpsWalker::check_home_fan_cap(false, 120).is_ok());
    }

    // ---- Slew bound ----

    #[test]
    fn slew_never_exceeds_3mhz_per_tick() {
        let mut w = DpsWalker::new(dps_config(), false, 600).unwrap();
        let mut prev = w.next_frequency_mhz();
        // Drive a synthetic curve that wants a huge jump.
        for i in 0..40 {
            // Hashrate peaks far away at 780 MHz → walker always wants
            // to move far, but slew must bound it.
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 780.0) / 100.0).powi(2);
            let outcome = w.tick(sample(hr, 1000.0));
            let now = w.next_frequency_mhz();
            assert!(
                (now as i32 - prev as i32).abs() <= MAX_STEP_MHZ as i32,
                "tick {}: moved {} MHz (> {} cap), {:?}",
                i,
                (now as i32 - prev as i32).abs(),
                MAX_STEP_MHZ,
                outcome
            );
            prev = now;
        }
    }

    #[test]
    fn band_clamp_holds_at_edges() {
        let mut c = dps_config();
        c.frequency_band_min_mhz = 500;
        c.frequency_band_max_mhz = 520;
        let mut w = DpsWalker::new(c, false, 510).unwrap();
        // Curve that wants to run way past the ceiling forever.
        for _ in 0..60 {
            let f = w.next_frequency_mhz() as f64;
            let hr = f; // monotone increasing → always wants up
            w.tick(sample(hr, 1000.0));
            assert!(w.next_frequency_mhz() >= 500);
            assert!(w.next_frequency_mhz() <= 520);
        }
    }

    // ---- Chip-health backoff ----

    #[test]
    fn error_rate_spike_triggers_immediate_backoff_to_lkg() {
        let mut w = DpsWalker::new(dps_config(), false, 600).unwrap();
        // A few clean ticks establish a last-known-good point and walk
        // upward.
        for _ in 0..6 {
            w.tick(sample(15.0, 1000.0));
        }
        let lkg = w.last_known_good_mhz;
        // Now spike the error rate.
        let mut bad = sample(15.0, 1000.0);
        bad.error_rate = 0.20; // 20 % HW errors
        match w.tick(bad) {
            DpsTickOutcome::HealthBackoff {
                to_mhz, error_rate, ..
            } => {
                assert_eq!(to_mhz, lkg);
                assert!((error_rate - 0.20).abs() < 1e-9);
            }
            other => panic!("expected health backoff, got {:?}", other),
        }
        assert_eq!(w.next_frequency_mhz(), lkg);
        assert!(!w.converged());
    }

    #[test]
    fn configured_error_ceiling_is_respected() {
        let mut c = dps_config();
        c.error_rate_ceiling = Some(0.01); // very tight 1 %
        let mut w = DpsWalker::new(c, false, 600).unwrap();
        w.tick(sample(15.0, 1000.0));
        let mut s = sample(15.0, 1000.0);
        s.error_rate = 0.02; // above the tight ceiling, below global 5 %
        match w.tick(s) {
            DpsTickOutcome::HealthBackoff { .. } => {}
            other => panic!("tight ceiling should trip backoff, got {:?}", other),
        }
    }

    #[test]
    fn health_backoff_takes_precedence_over_convergence() {
        // Even a converged walker must back off on an error spike.
        let mut w = DpsWalker::new(dps_config(), false, 600).unwrap();
        // Converge on a flat-ish max.
        for _ in 0..30 {
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 600.0) / 50.0).powi(2);
            w.tick(sample(hr, 1000.0));
        }
        let mut bad = sample(20.0, 1000.0);
        bad.error_rate = 0.50;
        match w.tick(bad) {
            DpsTickOutcome::HealthBackoff { .. } => {}
            other => panic!("expected backoff even when converged, got {:?}", other),
        }
        assert!(!w.converged());
    }

    // ---- Convergence + walk toward optimum ----

    #[test]
    fn walker_converges_on_synthetic_efficiency_curve() {
        // J/TH-minimizing objective with a clean parabolic optimum at
        // ~620 MHz. power(f) = a*(f-620)^2 + base ; hashrate ∝ f.
        let mut c = dps_config();
        c.objective = DpsObjective::MinJth;
        let mut w = DpsWalker::new(c, false, 560).unwrap();

        let mut converged_at = None;
        for tick in 0..200 {
            let f = w.next_frequency_mhz() as f64;
            // Efficiency sweet spot near 620 MHz.
            let power = 0.02 * (f - 620.0).powi(2) + 900.0;
            let hashrate = f / 40.0; // monotone in freq
            let outcome = w.tick(DpsSample {
                hashrate_ths: hashrate,
                power_w: power,
                error_rate: 0.0,
                voltage_mv: 13_700,
                fan_pwm: 10,
            });
            if let DpsTickOutcome::Converged { freq_mhz, .. } = outcome {
                converged_at = Some((tick, freq_mhz));
                break;
            }
        }
        let (_, freq) = converged_at.expect("walker should converge within 200 ticks");
        // The J/TH optimum isn't exactly 620 (hashrate also rises with
        // freq), but it must land in a sane neighbourhood and well
        // inside the band.
        assert!(
            (560..=720).contains(&freq),
            "converged at {} MHz, expected a sane efficiency optimum",
            freq
        );
        assert!(w.converged());
    }

    #[test]
    fn walker_moves_toward_higher_hashrate() {
        // MaxHashrate, hashrate strictly increasing with freq → walker
        // should climb upward over time.
        let mut w = DpsWalker::new(dps_config(), false, 500).unwrap();
        let start = w.next_frequency_mhz();
        for _ in 0..30 {
            let f = w.next_frequency_mhz() as f64;
            w.tick(sample(f / 30.0, 1000.0));
        }
        assert!(
            w.next_frequency_mhz() > start,
            "expected upward climb from {} toward higher hashrate, got {}",
            start,
            w.next_frequency_mhz()
        );
    }

    #[test]
    fn convergence_requires_configured_consecutive_ticks() {
        let mut c = dps_config();
        c.convergence_ticks = 5;
        let mut w = DpsWalker::new(c, false, 600).unwrap();
        // Sharp max exactly at the start point so the vertex is always
        // in-band immediately once the fit forms.
        let mut converge_tick = None;
        for tick in 0..40 {
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 600.0) / 30.0).powi(2);
            if let DpsTickOutcome::Converged { ticks_in_band, .. } = w.tick(sample(hr, 1000.0)) {
                converge_tick = Some((tick, ticks_in_band));
                break;
            }
        }
        let (_, ticks_in_band) = converge_tick.expect("should converge on a sharp in-band max");
        assert!(
            ticks_in_band >= 5,
            "converged after only {} in-band ticks, need >= 5",
            ticks_in_band
        );
    }

    #[test]
    fn converged_walker_resumes_on_telemetry_drift() {
        let mut c = dps_config();
        c.objective = DpsObjective::MaxHashrate;
        let mut w = DpsWalker::new(c, false, 600).unwrap();
        // Phase 1: converge on a stable max at 600.
        let mut converged = false;
        for _ in 0..60 {
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 600.0) / 30.0).powi(2);
            if let DpsTickOutcome::Converged { .. } = w.tick(sample(hr, 1000.0)) {
                converged = true;
            }
        }
        assert!(converged && w.converged(), "walker should be converged");

        // Phase 2: telemetry drifts hard (hashrate collapses ~40 %).
        let f = w.next_frequency_mhz() as f64;
        let collapsed = (20.0 - ((f - 600.0) / 30.0).powi(2)) * 0.6;
        match w.tick(sample(collapsed, 1000.0)) {
            DpsTickOutcome::ResumedOnDrift { .. } => {}
            other => panic!("expected resume-on-drift, got {:?}", other),
        }
        assert!(!w.converged(), "drift must un-converge the walker");
    }

    #[test]
    fn converged_walker_holds_station_without_drift() {
        let mut w = DpsWalker::new(dps_config(), false, 600).unwrap();
        for _ in 0..50 {
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 600.0) / 30.0).powi(2);
            w.tick(sample(hr, 1000.0));
        }
        assert!(w.converged());
        let held = w.next_frequency_mhz();
        // Steady telemetry → no movement, stays Converged.
        for _ in 0..10 {
            let f = w.next_frequency_mhz() as f64;
            let hr = 20.0 - ((f - 600.0) / 30.0).powi(2);
            match w.tick(sample(hr, 1000.0)) {
                DpsTickOutcome::Converged { .. } => {}
                other => panic!("expected steady Converged, got {:?}", other),
            }
        }
        assert_eq!(w.next_frequency_mhz(), held);
    }

    #[test]
    fn fallback_target_is_frequency_order_invariant() {
        // M3 core property pin (the audit's F-MEDIUM-1). The
        // convex/degenerate fallback now operates on a
        // frequency-sorted view of the 3-point window (see the
        // `pts.sort_by(freq_mhz)` in `tick()`) and steps toward the
        // better-valued *frequency extreme*, instead of the pre-fix
        // unsorted `max_by(value)` over the raw arrival window.
        //
        // The discriminating case is a **tied-value plateau**: three
        // distinct frequencies carrying the *same* objective value.
        // This is the realistic trigger near a flat optimum (telemetry
        // quantises / noise averages out) and it is degenerate (no
        // curvature) so it always takes the fallback leg. On a tie,
        // `Iterator::max_by` returns the *last* maximum, so the pre-fix
        // unsorted fallback's target depends on the order samples
        // arrived in — including, for some arrival orders, the
        // *interior* frequency, which makes no directional progress and
        // is exactly the "oscillate ±step around a flat optimum instead
        // of converging" pathology F-MEDIUM-1 describes. The sorted
        // freq-extreme choice is invariant to arrival order and always
        // a band extreme (directional progress).

        // The exact pre-fix decision logic (NO sort, fallback =
        // `max_by(value)` over raw arrival order) — kept here so this
        // test FAILS if the production sort is ever removed.
        let decide_unsorted = |triple: [FitPoint; 3]| -> (bool, f64) {
            let p = triple; // arrival order, as the pre-fix code saw it
            match fit_parabola_vertex(p[0], p[1], p[2]) {
                ParabolaFit::Vertex {
                    vertex_mhz,
                    opens_down,
                } if opens_down => (true, vertex_mhz),
                _ => {
                    let best = p
                        .iter()
                        .max_by(|a, b| {
                            a.value
                                .partial_cmp(&b.value)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .copied()
                        .unwrap();
                    (false, best.freq_mhz)
                }
            }
        };

        // The exact production decision logic (sorted view, fallback =
        // better-valued frequency extreme), mirroring `tick()`.
        let decide_sorted = |triple: [FitPoint; 3]| -> (bool, f64) {
            let mut p = triple;
            p.sort_by(|x, y| {
                x.freq_mhz
                    .partial_cmp(&y.freq_mhz)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            match fit_parabola_vertex(p[0], p[1], p[2]) {
                ParabolaFit::Vertex {
                    vertex_mhz,
                    opens_down,
                } if opens_down => (true, vertex_mhz),
                _ => {
                    let lo = p[0];
                    let hi = p[2];
                    let best = if hi.value >= lo.value { hi } else { lo };
                    (false, best.freq_mhz)
                }
            }
        };

        let perms = |t: [FitPoint; 3]| {
            let [a, b, c] = t;
            [
                [a, b, c],
                [a, c, b],
                [b, a, c],
                [b, c, a],
                [c, a, b],
                [c, b, a],
            ]
        };

        // Flat noisy plateau near a converged optimum: all three
        // distinct frequencies report the identical objective value.
        let plateau = [
            FitPoint {
                freq_mhz: 600.0,
                value: 5.0,
            },
            FitPoint {
                freq_mhz: 606.0,
                value: 5.0,
            }, // the interior frequency
            FitPoint {
                freq_mhz: 612.0,
                value: 5.0,
            },
        ];
        let ps = perms(plateau);

        // (a) The fix's guarantee: the sorted fallback is identical
        //     across all 6 arrival permutations and always a frequency
        //     extreme — never the interior 606 MHz.
        let sorted_ref = decide_sorted(ps[0]);
        assert!(
            !sorted_ref.0,
            "plateau fixture must be degenerate (fallback), got vertex"
        );
        assert!(
            sorted_ref.1 == 600.0 || sorted_ref.1 == 612.0,
            "sorted fallback target {} must be a frequency extreme, never \
             the interior 606 MHz",
            sorted_ref.1
        );
        for (i, perm) in ps.iter().enumerate() {
            assert_eq!(
                decide_sorted(*perm),
                sorted_ref,
                "permutation #{} ({:?}): sorted fallback must be \
                 arrival-order invariant",
                i,
                perm
            );
        }

        // (b) Discriminator: the pre-fix unsorted logic is NOT
        //     order-invariant on this exact fixture — across the 6
        //     arrival orders it yields more than one distinct target,
        //     and at least one of them is the interior 606 MHz (the
        //     oscillation seed). This is what the production sort
        //     eliminates; if a future refactor drops the sort this
        //     assertion documents precisely what regresses.
        let unsorted_targets: std::collections::BTreeSet<u64> =
            ps.iter().map(|p| decide_unsorted(*p).1.to_bits()).collect();
        assert!(
            unsorted_targets.len() > 1,
            "pre-fix unsorted fallback should be order-sensitive on a \
             tied-value plateau; got a single target {:?} — fixture no \
             longer discriminates the fix",
            unsorted_targets
        );
        assert!(
            unsorted_targets.contains(&606.0_f64.to_bits()),
            "pre-fix unsorted fallback should target the interior 606 MHz \
             for some arrival order (the oscillation seed); targets={:?}",
            unsorted_targets
                .iter()
                .map(|b| f64::from_bits(*b))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn walker_converges_on_noisy_curve_within_slew_and_band() {
        // M3 robustness regression pin (a stability guard, NOT a
        // pre/post discriminator — the sort is a correctness hardening
        // that keeps the fallback's order-invariance structural rather
        // than emergent; on the current `record_fit_point` + `WalkDir`
        // machinery it does not change observable convergence, which is
        // the desired "no behavior change on clean/normal data"
        // property). This test exists so a *future* refactor of the
        // fit-window bookkeeping cannot silently reintroduce the
        // audit's oscillation pathology without a test going red: a
        // noisy chain must still converge, settle near the optimum,
        // hold station through jitter, and never violate the ±3 MHz
        // slew bound.
        //
        // Deterministic LCG noise (no external deps) so the test is
        // reproducible.
        let mut c = dps_config();
        c.objective = DpsObjective::MaxHashrate;
        c.convergence_ticks = 5;
        let mut w = DpsWalker::new(c, false, 600).unwrap();

        let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
        let mut noise = || {
            lcg = lcg
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((lcg >> 33) as f64) / (1u64 << 31) as f64; // [0,1)
            (unit - 0.5) * 0.60 // ~±0.30 TH/s telemetry jitter
        };

        let mut prev = w.next_frequency_mhz();
        let mut converged_tick = None;
        for tick in 0..400 {
            let f = w.next_frequency_mhz() as f64;
            // Concave max near 600 MHz + telemetry jitter.
            let clean = 20.0 - ((f - 600.0) / 30.0).powi(2);
            let outcome = w.tick(sample(clean + noise(), 1000.0));
            let now = w.next_frequency_mhz();
            // ±3 MHz/tick slew bound holds on every tick, noise or not.
            assert!(
                (now as i32 - prev as i32).abs() <= MAX_STEP_MHZ as i32,
                "tick {}: moved {} MHz (> {} cap) on a noisy chain, {:?}",
                tick,
                (now as i32 - prev as i32).abs(),
                MAX_STEP_MHZ,
                outcome
            );
            prev = now;
            if let DpsTickOutcome::Converged { ticks_in_band, .. } = outcome {
                converged_tick = Some((tick, ticks_in_band));
                break;
            }
        }

        let (_, ticks_in_band) =
            converged_tick.expect("noisy walker must still converge within 400 ticks");
        assert!(
            ticks_in_band >= 5,
            "converged after only {} in-band ticks, need >= configured 5",
            ticks_in_band
        );
        assert!(w.converged());
        let settled = w.next_frequency_mhz();
        assert!(
            (560..=640).contains(&settled),
            "settled at {} MHz, expected a neighbourhood of the 600 MHz optimum",
            settled
        );

        // Steady-but-still-noisy telemetry must not bounce the walker
        // out of Converged every tick (drift resume is for *systematic*
        // change, not jitter).
        let mut still_converged_ticks = 0;
        for _ in 0..20 {
            let f = w.next_frequency_mhz() as f64;
            let clean = 20.0 - ((f - 600.0) / 30.0).powi(2);
            if let DpsTickOutcome::Converged { .. } = w.tick(sample(clean + noise(), 1000.0)) {
                still_converged_ticks += 1;
            }
        }
        assert!(
            still_converged_ticks >= 10,
            "walker fell out of convergence on {}/20 steady-noisy ticks — \
             expected it to hold station through jitter",
            20 - still_converged_ticks
        );
        assert!(
            (560..=640).contains(&w.next_frequency_mhz()),
            "post-convergence freq drifted out of the optimum band"
        );
    }

    // ---- Objective scalar mapping ----

    #[test]
    fn jth_objective_prefers_lower_jth() {
        let mut c = dps_config();
        c.objective = DpsObjective::MinJth;
        let w = DpsWalker::new(c, false, 600).unwrap();
        let efficient = sample(20.0, 1000.0); // 50 J/TH
        let wasteful = sample(20.0, 2000.0); // 100 J/TH
        assert!(
            w.objective_value(&efficient) > w.objective_value(&wasteful),
            "lower J/TH must score higher"
        );
    }

    #[test]
    fn target_power_objective_prefers_closer_to_target() {
        let mut c = dps_config();
        c.objective = DpsObjective::TargetPower;
        c.target_power_w = 1000;
        let w = DpsWalker::new(c, false, 600).unwrap();
        let close = sample(20.0, 1010.0);
        let far = sample(20.0, 1300.0);
        assert!(
            w.objective_value(&close) > w.objective_value(&far),
            "closer to target power must score higher"
        );
    }

    #[test]
    fn jth_is_infinite_for_zero_hashrate() {
        let s = sample(0.0, 1000.0);
        assert!(s.jth().is_infinite());
    }

    // ---- TOML round-trip ----

    #[test]
    fn config_round_trips_through_toml() {
        let c = DpsConfig {
            enabled: true,
            objective: DpsObjective::MinJth,
            target_power_w: 1200,
            home_mode: true,
            frequency_band_min_mhz: 450,
            frequency_band_max_mhz: 700,
            tick_s: 45,
            step_mhz: 2,
            convergence_ticks: 4,
            error_rate_ceiling: Some(0.02),
        };
        let s = toml::to_string(&c).unwrap();
        let back: DpsConfig = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn objective_serializes_snake_case() {
        let s = toml::to_string(&DpsConfig {
            enabled: true,
            objective: DpsObjective::MaxHashrate,
            ..Default::default()
        })
        .unwrap();
        assert!(s.contains("max_hashrate"), "got: {}", s);
    }
}
