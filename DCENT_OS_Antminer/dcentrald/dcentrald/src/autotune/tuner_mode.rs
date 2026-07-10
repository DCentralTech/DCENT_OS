//! Supremacy S5-02 — TunerMode 6-variant redesign.
//!
//! A unified strategy enum for the autotuner. Six modes:
//!
//! * `Performance` — push frequency to the top of the envelope.
//! * `PowerTarget(watts)` — *delegates* to the S4-03
//!   [`PowerTargetController`](super::PowerTargetController). Do NOT
//!   reimplement the PI controller here; this variant just constructs
//!   it and forwards `tick()` outcomes.
//! * `HashrateTarget(ths)` — proportional adjust to hit a target TH/s.
//! * `Manual { freq, voltage }` — operator-pinned, still envelope-
//!   clamped to the platform PVT cap + the 14_500 mV HARD voltage cap.
//! * `Efficiency` — DCENT_OS-unique. Walks frequency *down* and stops
//!   when J/TH stops improving for three ticks. Braiins has nothing
//!   like this.
//! * `Heater(watts)` — DCENT_OS-unique. Space-heater profile.
//!   Maximises thermal output for a given wall wattage budget while
//!   keeping fan PWM at or below the home cap (≤30 PWM). Braiins has
//!   nothing like this.
//! * `HashrateQuota { fraction | absolute_ths }` — ePIC UMC OS
//!   V1.18.2 "hashrate split quota" analog (demand-response + the home
//!   "% of max heat" story). Allocates a *fraction* of the miner's
//!   rated max hashrate. It does NOT add any new control law: it
//!   resolves the quota to an equivalent wattage target on the
//!   platform's `(max_ths, max_watts)` envelope and then **delegates
//!   to the exact same [`PowerTargetController`] path
//!   `PowerTarget` uses**. Every HARD clamp (14_500 mV voltage,
//!   ≤30 PWM home fan, ±5 MHz/tick slew, steady-state gate) is
//!   inherited unchanged through that delegation — there is no new
//!   voltage or clamp bypass anywhere in this variant.
//!
//! ## Safety contract
//!
//! Every mode enforces the same three load-bearing clamps before
//! emitting a `Converged` outcome:
//!
//! 1. `voltage_mv` must be `<=` [`VOLTAGE_CLAMP_MV`] (14_500 mV).
//! 2. If the mode is `Heater`, `fan_pwm` must be `<=`
//!    [`HOME_FAN_PWM_MAX`] (30) — Heater is intrinsically home-mode.
//! 3. Frequency moves are bounded by `±SLEW_MHZ_PER_TICK` (5 MHz) per
//!    tick. Envelope `[freq_min_mhz, freq_max_mhz]` is the outer
//!    clamp.
//!
//! Any violation is reported as `RefusedSafety { reason }` with the
//! voltage/fan-cap reasons matching the
//! [`PowerTargetController`](super::PowerTargetController) phrasing.
//!
//! ## Memory rule to save post-merge
//!
//!  — TunerMode is the
//! canonical autotuner strategy surface. PowerTarget MUST delegate to
//! the S4-03 controller. Efficiency + Heater are DCENT_OS-unique
//! capabilities (Braiins lacks them). Never bypass the voltage/fan/
//! slew clamps; never reimplement PowerTarget here.

use serde::{Deserialize, Serialize};

use super::{
    PowerTargetConfig, PowerTargetController, TelemetrySample, TickOutcome, HOME_FAN_PWM_MAX,
    VOLTAGE_CLAMP_MV,
};

/// Default per-tick frequency slew clamp (MHz). Matches the S4-03
/// `DEFAULT_SLEW_MHZ`. Each non-pinned mode caps frequency movement at
/// this magnitude per tick.
pub const SLEW_MHZ_PER_TICK: u16 = 5;

/// Default frequency-envelope minimum used when a mode has no
/// platform-specific override (MHz).
pub const DEFAULT_FREQ_MIN_MHZ: u16 = 400;

/// Default frequency-envelope maximum used when a mode has no
/// platform-specific override (MHz).
pub const DEFAULT_FREQ_MAX_MHZ: u16 = 800;

/// Efficiency walker step (MHz). Each tick the walker drops frequency
/// by this much until J/TH stops improving for
/// [`EFFICIENCY_CONVERGENCE_TICKS`] consecutive ticks.
pub const EFFICIENCY_STEP_MHZ: u16 = 5;

/// Number of consecutive non-improving ticks before the efficiency
/// walker locks in.
pub const EFFICIENCY_CONVERGENCE_TICKS: u8 = 3;

/// Heater target tolerance — once the controller is within this
/// fraction of `target_watts`, it considers itself converged.
pub const HEATER_TARGET_TOLERANCE: f64 = 0.05;

/// HashrateTarget proportional gain (MHz per TH/s error).
pub const HASHRATE_GAIN_MHZ_PER_THS: f64 = 1.5;

/// HashrateTarget tolerance — fraction of target hashrate within
/// which we declare convergence.
pub const HASHRATE_TARGET_TOLERANCE: f64 = 0.02;

/// Default platform rated max hashrate (TH/s) used when the operator
/// configures a `HashrateQuota` by `fraction` without an explicit
/// `rated_max_ths`. Conservative — an S9-class floor; the operator (or
/// a future per-SKU silicon-profile lookup) should override via the
/// `rated_max_ths` field. The value only affects the fraction→absolute
/// resolution; it can never lift any clamp.
pub const DEFAULT_RATED_MAX_THS: f64 = 14.0;

/// Default platform rated max wall/chip wattage (W) used to map a
/// resolved absolute-TH/s quota onto the `PowerTargetController`'s
/// wattage axis. Conservative S9-class default; override via
/// `rated_max_watts`. This is purely the linear `ths→watts` slope used
/// to construct the *target* the existing PI controller chases — it is
/// NOT a power limit and grants no new authority.
pub const DEFAULT_RATED_MAX_WATTS: u32 = 1400;

/// Lower bound on the resolved quota fraction. A 0 (or negative)
/// fraction would resolve to a 0 W target which `PowerTargetController`
/// rejects as `Disabled`; we refuse earlier with a clear reason.
pub const HASHRATE_QUOTA_MIN_FRACTION: f64 = 0.01;

// ---------------------------------------------------------------------
// Outcome type — what a `step()` call decided to do.
// ---------------------------------------------------------------------

/// Outcome of one [`TunerMode::step`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum TunerOutcome {
    /// Controller is intentionally idle this tick (e.g. waiting for
    /// steady-state, or a `Manual` variant that has already settled).
    Disengaged,
    /// Still gathering telemetry / probing. No new freq/voltage yet.
    Probing,
    /// Converged on a `(freq, voltage)` pair this tick.
    Converged { freq_mhz: u16, voltage_mv: u16 },
    /// Refused to engage because of a HARD safety clamp. The string
    /// matches the [`super::EngageError`] phrasing for the relevant
    /// gate so downstream logging stays uniform.
    RefusedSafety { reason: String },
}

// ---------------------------------------------------------------------
// Manual pinned settings (data carried by `TunerMode::Manual`).
// ---------------------------------------------------------------------

/// Operator-pinned frequency + voltage for `TunerMode::Manual`. The
/// envelope clamp + 14_500 mV voltage cap still apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualSettings {
    pub freq_mhz: u16,
    pub voltage_mv: u16,
}

// ---------------------------------------------------------------------
// TunerMode enum
// ---------------------------------------------------------------------

/// Tagged-union TOML representation of `TunerMode`. Selected via the
/// `mode` discriminant string ("performance" / "power-target" / ...).
///
/// Defaults to `Manual { freq_mhz: 0, voltage_mv: 0 }` — the
/// "neutral / least-surprise" choice: the caller is expected to
/// upgrade this with the *current* on-chip values via
/// [`TunerMode::default_manual_at`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum TunerMode {
    /// Push frequency to `frequency_band.max` within the envelope.
    /// `step()` slews up by ±[`SLEW_MHZ_PER_TICK`] each tick.
    Performance {
        #[serde(default = "default_freq_min")]
        freq_min_mhz: u16,
        #[serde(default = "default_freq_max")]
        freq_max_mhz: u16,
        #[serde(default = "default_voltage")]
        voltage_mv: u16,
    },

    /// Delegate to the S4-03 closed-loop PI controller. The variant
    /// carries the `PowerTargetConfig`; `step()` builds (or reuses)
    /// the controller and forwards `tick()` outcomes.
    #[serde(rename = "power-target")]
    PowerTarget {
        target_watts: u32,
        #[serde(default)]
        home_mode: bool,
    },

    /// Proportional-only adjust to hit a target TH/s. Bounded by the
    /// same slew + envelope rules as `Performance`. Voltage stays at
    /// the platform default.
    #[serde(rename = "hashrate-target")]
    HashrateTarget {
        target_ths: f64,
        #[serde(default = "default_freq_min")]
        freq_min_mhz: u16,
        #[serde(default = "default_freq_max")]
        freq_max_mhz: u16,
        #[serde(default = "default_voltage")]
        voltage_mv: u16,
    },

    /// Operator-pinned settings. Still envelope-clamped at output
    /// time so a bad TOML value can't push the chip past the platform
    /// PVT cap.
    Manual {
        freq_mhz: u16,
        voltage_mv: u16,
        #[serde(default = "default_freq_min")]
        freq_min_mhz: u16,
        #[serde(default = "default_freq_max")]
        freq_max_mhz: u16,
    },

    /// DCENT_OS-unique. Walks frequency down by
    /// [`EFFICIENCY_STEP_MHZ`] each tick until J/TH stops improving
    /// for [`EFFICIENCY_CONVERGENCE_TICKS`] consecutive ticks.
    Efficiency {
        #[serde(default = "default_freq_min")]
        freq_min_mhz: u16,
        #[serde(default = "default_freq_max")]
        freq_max_mhz: u16,
        #[serde(default = "default_voltage")]
        voltage_mv: u16,
    },

    /// DCENT_OS-unique space-heater mode. Hold a target wall wattage
    /// while keeping commanded fans at the home cap (`fan_pwm <= 30`). Refuses to
    /// engage at any sample where `fan_pwm > 30` — fan blast is
    /// incompatible with space-heater use.
    Heater {
        target_watts: u32,
        #[serde(default = "default_freq_min")]
        freq_min_mhz: u16,
        #[serde(default = "default_freq_max")]
        freq_max_mhz: u16,
        #[serde(default = "default_voltage")]
        voltage_mv: u16,
    },

    /// ePIC UMC OS V1.18.2 "hashrate split quota" analog. Allocate a
    /// fraction of the miner's rated max hashrate (demand-response /
    /// the home "% of max heat" story). The operator supplies EITHER
    /// `fraction` (0.01..=1.0 of `rated_max_ths`) OR `absolute_ths`
    /// (an explicit TH/s ceiling). The resolved TH/s is mapped linearly
    /// onto `(rated_max_ths, rated_max_watts)` to produce a wattage
    /// target, and `step()` then **delegates to the exact same
    /// [`PowerTargetController`] path `PowerTarget` uses** — inheriting
    /// every HARD clamp (14_500 mV / ≤30 PWM home fan / ±5 MHz slew /
    /// steady-state gate) with NO new bypass. Defaults to `home_mode`
    /// because a quota cap is overwhelmingly a home/space-heater
    /// demand-response use; the operator can opt out.
    #[serde(rename = "hashrate-quota")]
    HashrateQuota {
        /// Fraction of `rated_max_ths` to allocate (0.01..=1.0).
        /// Mutually exclusive with `absolute_ths`; if both are set,
        /// `absolute_ths` wins (explicit beats relative) and `fraction`
        /// is ignored.
        #[serde(default)]
        fraction: Option<f32>,
        /// Explicit absolute hashrate ceiling in TH/s. Mutually
        /// exclusive with `fraction`.
        #[serde(default)]
        absolute_ths: Option<f32>,
        /// Platform rated max hashrate (TH/s) — the 100% reference for
        /// `fraction`. Defaults to a conservative S9-class floor.
        #[serde(default = "default_rated_max_ths")]
        rated_max_ths: f32,
        /// Platform rated max wall/chip wattage — the linear `ths→watts`
        /// slope endpoint used to construct the delegated PI target.
        #[serde(default = "default_rated_max_watts")]
        rated_max_watts: u32,
        /// Home / space-heater posture. Forwarded verbatim to the
        /// delegated `PowerTargetController` (caps fan ≤30). Defaults
        /// to `true` — a quota cap is a home demand-response feature.
        #[serde(default = "default_true")]
        home_mode: bool,
    },
}

fn default_freq_min() -> u16 {
    DEFAULT_FREQ_MIN_MHZ
}
fn default_freq_max() -> u16 {
    DEFAULT_FREQ_MAX_MHZ
}
fn default_voltage() -> u16 {
    13_700
}
fn default_rated_max_ths() -> f32 {
    DEFAULT_RATED_MAX_THS as f32
}
fn default_rated_max_watts() -> u32 {
    DEFAULT_RATED_MAX_WATTS
}
fn default_true() -> bool {
    true
}

impl Default for TunerMode {
    fn default() -> Self {
        // Least-surprise default per scope: pinned to a neutral value,
        // still envelope-clamped on emit. Callers should call
        // [`TunerMode::default_manual_at`] to seed with live values.
        TunerMode::Manual {
            freq_mhz: 0,
            voltage_mv: 0,
            freq_min_mhz: DEFAULT_FREQ_MIN_MHZ,
            freq_max_mhz: DEFAULT_FREQ_MAX_MHZ,
        }
    }
}

impl TunerMode {
    /// Convenience: build a `Manual` mode pinned at the current
    /// on-chip `(freq, voltage)`.
    pub fn default_manual_at(freq_mhz: u16, voltage_mv: u16) -> Self {
        TunerMode::Manual {
            freq_mhz,
            voltage_mv,
            freq_min_mhz: DEFAULT_FREQ_MIN_MHZ,
            freq_max_mhz: DEFAULT_FREQ_MAX_MHZ,
        }
    }
}

// ---------------------------------------------------------------------
// Stateful driver — wraps `TunerMode` with the running state each
// variant needs (the PowerTarget controller, the efficiency walker
// counter, etc.).
// ---------------------------------------------------------------------

/// Driver that turns a [`TunerMode`] into a step-by-step controller.
/// Each variant gets its own internal state slot; [`step`] dispatches.
///
/// Construct with [`TunerDriver::new`].
///
/// [`step`]: TunerDriver::step
#[derive(Debug)]
pub struct TunerDriver {
    mode: TunerMode,
    current_freq_mhz: u16,
    current_voltage_mv: u16,
    // PowerTarget delegate (lazily constructed; rebuilt on mode swap).
    power_controller: Option<PowerTargetController>,
    // Efficiency walker bookkeeping.
    eff_last_jth: Option<f64>,
    eff_non_improving_ticks: u8,
    eff_converged_at: Option<u16>,
    // Heater bookkeeping.
    heater_last_watts: Option<f64>,
}

impl TunerDriver {
    /// Build a fresh driver. `current_freq_mhz` / `current_voltage_mv`
    /// seed the controller with the chip's actual on-chip state so
    /// `Performance` / `Efficiency` / etc. don't slam the frequency
    /// at the band edge on tick 1.
    pub fn new(mode: TunerMode, current_freq_mhz: u16, current_voltage_mv: u16) -> Self {
        Self {
            mode,
            current_freq_mhz,
            current_voltage_mv,
            power_controller: None,
            eff_last_jth: None,
            eff_non_improving_ticks: 0,
            eff_converged_at: None,
            heater_last_watts: None,
        }
    }

    /// Borrow the active mode.
    pub fn mode(&self) -> &TunerMode {
        &self.mode
    }

    /// Swap modes. Resets per-variant state so the new mode starts
    /// clean (no stale PowerTarget integrator, no carried-over
    /// efficiency window).
    pub fn set_mode(&mut self, new_mode: TunerMode) {
        self.mode = new_mode;
        self.power_controller = None;
        self.eff_last_jth = None;
        self.eff_non_improving_ticks = 0;
        self.eff_converged_at = None;
        self.heater_last_watts = None;
    }

    /// Currently proposed frequency (MHz).
    pub fn current_freq_mhz(&self) -> u16 {
        self.current_freq_mhz
    }

    /// Currently proposed voltage (mV).
    pub fn current_voltage_mv(&self) -> u16 {
        self.current_voltage_mv
    }

    /// Drive one tick. Each variant enforces voltage ≤ 14_500 mV,
    /// fan ≤ 30 PWM (when applicable), and ±5 MHz/tick slew.
    pub fn step(&mut self, sample: TelemetrySample) -> TunerOutcome {
        // Universal voltage clamp — every mode honours this.
        if let Err(reason) = check_voltage_clamp(sample.voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }

        // Take ownership of the mode briefly so we can match by value
        // without partial-move headaches with `&mut self`.
        let mode = self.mode.clone();
        match mode {
            TunerMode::Performance {
                freq_min_mhz,
                freq_max_mhz,
                voltage_mv,
            } => self.step_performance(freq_min_mhz, freq_max_mhz, voltage_mv),
            TunerMode::PowerTarget {
                target_watts,
                home_mode,
            } => self.step_power_target(target_watts, home_mode, sample),
            TunerMode::HashrateTarget {
                target_ths,
                freq_min_mhz,
                freq_max_mhz,
                voltage_mv,
            } => self.step_hashrate_target(
                target_ths,
                freq_min_mhz,
                freq_max_mhz,
                voltage_mv,
                sample,
            ),
            TunerMode::Manual {
                freq_mhz,
                voltage_mv,
                freq_min_mhz,
                freq_max_mhz,
            } => self.step_manual(freq_mhz, voltage_mv, freq_min_mhz, freq_max_mhz),
            TunerMode::Efficiency {
                freq_min_mhz,
                freq_max_mhz,
                voltage_mv,
            } => self.step_efficiency(freq_min_mhz, freq_max_mhz, voltage_mv, sample),
            TunerMode::Heater {
                target_watts,
                freq_min_mhz,
                freq_max_mhz,
                voltage_mv,
            } => self.step_heater(target_watts, freq_min_mhz, freq_max_mhz, voltage_mv, sample),
            TunerMode::HashrateQuota {
                fraction,
                absolute_ths,
                rated_max_ths,
                rated_max_watts,
                home_mode,
            } => self.step_hashrate_quota(
                fraction,
                absolute_ths,
                rated_max_ths,
                rated_max_watts,
                home_mode,
                sample,
            ),
        }
    }

    /// Resolve a `HashrateQuota` to a wattage target and delegate to
    /// the **exact same** [`PowerTargetController`] path
    /// [`TunerMode::PowerTarget`] uses. This function deliberately owns
    /// ZERO control law and ZERO clamp logic: it only does the
    /// quota→watts arithmetic, then calls [`Self::step_power_target`].
    /// Every safety clamp (14_500 mV voltage, ≤30 PWM home fan, ±5
    /// MHz/tick slew, 3-tick steady-state gate) is enforced inside the
    /// delegated controller, unchanged. No new voltage/clamp bypass is
    /// introduced here — that property is regression-pinned by
    /// `hashrate_quota_delegates_through_gated_power_target_path` and
    /// `hashrate_quota_inherits_voltage_and_fan_clamps`.
    #[allow(clippy::too_many_arguments)]
    fn step_hashrate_quota(
        &mut self,
        fraction: Option<f32>,
        absolute_ths: Option<f32>,
        rated_max_ths: f32,
        rated_max_watts: u32,
        home_mode: bool,
        sample: TelemetrySample,
    ) -> TunerOutcome {
        let rated_ths = rated_max_ths as f64;
        if !(rated_ths.is_finite()) || rated_ths <= 0.0 {
            return TunerOutcome::RefusedSafety {
                reason: "hashrate-quota rated_max_ths must be > 0 TH/s".to_string(),
            };
        }
        if rated_max_watts == 0 {
            return TunerOutcome::RefusedSafety {
                reason: "hashrate-quota rated_max_watts must be > 0".to_string(),
            };
        }

        // Explicit absolute beats relative fraction (explicit wins).
        let resolved_ths = match (absolute_ths, fraction) {
            (Some(abs), _) => {
                let abs = abs as f64;
                if !abs.is_finite() || abs <= 0.0 {
                    return TunerOutcome::RefusedSafety {
                        reason: "hashrate-quota absolute_ths must be > 0 TH/s".to_string(),
                    };
                }
                // Never resolve above the rated envelope — a quota is a
                // *cap*, not an overclock authority.
                abs.min(rated_ths)
            }
            (None, Some(frac)) => {
                let frac = frac as f64;
                // `fraction` is an f32 field: f32(0.01) widens to
                // ~0.00999999978 in f64, which would spuriously fail a
                // bare `< 0.01` check. Tolerate one f32-epsilon on both
                // bounds so an operator who literally types the minimum
                // (or 1.0) is accepted; genuinely out-of-range values
                // (<=0, >1) still fail.
                let eps = f32::EPSILON as f64;
                if !frac.is_finite() || frac < HASHRATE_QUOTA_MIN_FRACTION - eps || frac > 1.0 + eps
                {
                    return TunerOutcome::RefusedSafety {
                        reason: format!(
                            "hashrate-quota fraction must be in \
                             [{:.2}, 1.0], got {}",
                            HASHRATE_QUOTA_MIN_FRACTION, frac
                        ),
                    };
                }
                // Clamp into the canonical band so the downstream
                // ths→watts map can't see a sub-epsilon under/overshoot.
                frac.clamp(HASHRATE_QUOTA_MIN_FRACTION, 1.0) * rated_ths
            }
            (None, None) => {
                return TunerOutcome::RefusedSafety {
                    reason: "hashrate-quota requires exactly one of \
                             `fraction` or `absolute_ths`"
                        .to_string(),
                };
            }
        };

        // Linear map onto the platform's (max_ths, max_watts) line to
        // produce the wattage target the existing PI controller will
        // chase. Quota is a cap → clamp the derived watts to the rated
        // envelope so the delegated controller never even *aims* above
        // the platform max. (It would refuse to lift voltage past the
        // HARD clamp anyway — this is defense-in-depth, not the gate.)
        let watts_per_ths = rated_max_watts as f64 / rated_ths;
        let target_watts_f = (resolved_ths * watts_per_ths).clamp(1.0, rated_max_watts as f64);
        let target_watts = target_watts_f.round() as u32;

        // DELEGATE — identical construction + tick path as PowerTarget.
        // step_power_target builds/owns the S4-03 PowerTargetController
        // (which enforces every clamp) and forwards its TickOutcome.
        self.step_power_target(target_watts, home_mode, sample)
    }

    // -----------------------------------------------------------------
    // Per-variant step bodies
    // -----------------------------------------------------------------

    fn step_performance(
        &mut self,
        freq_min_mhz: u16,
        freq_max_mhz: u16,
        voltage_mv: u16,
    ) -> TunerOutcome {
        if let Err(reason) = check_voltage_clamp(voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }
        if freq_min_mhz >= freq_max_mhz {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "frequency band min {} MHz >= max {} MHz",
                    freq_min_mhz, freq_max_mhz
                ),
            };
        }
        // Slew toward the band's max.
        let target = freq_max_mhz;
        let next = slew_toward(self.current_freq_mhz, target, freq_min_mhz, freq_max_mhz);
        self.current_freq_mhz = next;
        self.current_voltage_mv = voltage_mv;
        TunerOutcome::Converged {
            freq_mhz: next,
            voltage_mv,
        }
    }

    fn step_power_target(
        &mut self,
        target_watts: u32,
        home_mode: bool,
        sample: TelemetrySample,
    ) -> TunerOutcome {
        // Build the S4-03 controller on first use (or after a mode
        // swap). This keeps PI integrator state across ticks.
        if self.power_controller.is_none() {
            let cfg = PowerTargetConfig {
                enabled: true,
                target_watts,
                home_mode,
                ..Default::default()
            };
            match PowerTargetController::new(cfg, self.current_freq_mhz) {
                Ok(c) => self.power_controller = Some(c),
                Err(e) => {
                    return TunerOutcome::RefusedSafety {
                        reason: e.to_string(),
                    };
                }
            }
        }
        // Delegate the actual tick to the S4-03 controller. We don't
        // duplicate its PI math.
        let controller = self
            .power_controller
            .as_mut()
            .expect("controller constructed above");
        let outcome = controller.tick(sample);
        match outcome {
            TickOutcome::Refused(e) => TunerOutcome::RefusedSafety {
                reason: e.to_string(),
            },
            TickOutcome::WaitingForSteadyState { .. } => TunerOutcome::Probing,
            TickOutcome::Adjusted { to_mhz, .. }
            | TickOutcome::AtTarget {
                freq_mhz: to_mhz, ..
            } => {
                self.current_freq_mhz = to_mhz;
                // PowerTarget doesn't propose voltage — keep current.
                TunerOutcome::Converged {
                    freq_mhz: to_mhz,
                    voltage_mv: self.current_voltage_mv,
                }
            }
        }
    }

    fn step_hashrate_target(
        &mut self,
        target_ths: f64,
        freq_min_mhz: u16,
        freq_max_mhz: u16,
        voltage_mv: u16,
        sample: TelemetrySample,
    ) -> TunerOutcome {
        if let Err(reason) = check_voltage_clamp(voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }
        if freq_min_mhz >= freq_max_mhz {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "frequency band min {} MHz >= max {} MHz",
                    freq_min_mhz, freq_max_mhz
                ),
            };
        }
        if target_ths <= 0.0 || !target_ths.is_finite() {
            return TunerOutcome::RefusedSafety {
                reason: "hashrate target must be > 0 TH/s".to_string(),
            };
        }
        // Tolerance gate.
        let error_ths = target_ths - sample.hashrate_ths;
        if error_ths.abs() / target_ths <= HASHRATE_TARGET_TOLERANCE {
            self.current_voltage_mv = voltage_mv;
            return TunerOutcome::Converged {
                freq_mhz: self.current_freq_mhz,
                voltage_mv,
            };
        }
        // Proportional adjust, clamped by slew + envelope.
        let raw_delta = HASHRATE_GAIN_MHZ_PER_THS * error_ths;
        let slew = SLEW_MHZ_PER_TICK as f64;
        let clamped_delta = raw_delta.clamp(-slew, slew);
        let proposed = (self.current_freq_mhz as i32 + clamped_delta.round() as i32)
            .clamp(freq_min_mhz as i32, freq_max_mhz as i32) as u16;
        self.current_freq_mhz = proposed;
        self.current_voltage_mv = voltage_mv;
        TunerOutcome::Converged {
            freq_mhz: proposed,
            voltage_mv,
        }
    }

    fn step_manual(
        &mut self,
        freq_mhz: u16,
        voltage_mv: u16,
        freq_min_mhz: u16,
        freq_max_mhz: u16,
    ) -> TunerOutcome {
        if let Err(reason) = check_voltage_clamp(voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }
        if freq_min_mhz >= freq_max_mhz {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "frequency band min {} MHz >= max {} MHz",
                    freq_min_mhz, freq_max_mhz
                ),
            };
        }
        // Envelope clamp on the operator-supplied freq.
        let clamped = freq_mhz.clamp(freq_min_mhz, freq_max_mhz);
        // Still honour the ±5 MHz/tick slew so a `Manual` swap from a
        // far-away current frequency ramps cleanly.
        let next = slew_toward(self.current_freq_mhz, clamped, freq_min_mhz, freq_max_mhz);
        self.current_freq_mhz = next;
        self.current_voltage_mv = voltage_mv;
        if next == clamped {
            TunerOutcome::Converged {
                freq_mhz: next,
                voltage_mv,
            }
        } else {
            // Still slewing toward the pinned target — that's a
            // converged outcome each tick, but the value isn't there
            // yet. Report converged (we did act) at the slewed value.
            TunerOutcome::Converged {
                freq_mhz: next,
                voltage_mv,
            }
        }
    }

    fn step_efficiency(
        &mut self,
        freq_min_mhz: u16,
        freq_max_mhz: u16,
        voltage_mv: u16,
        sample: TelemetrySample,
    ) -> TunerOutcome {
        if let Err(reason) = check_voltage_clamp(voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }
        if freq_min_mhz >= freq_max_mhz {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "frequency band min {} MHz >= max {} MHz",
                    freq_min_mhz, freq_max_mhz
                ),
            };
        }
        // If we already converged, hold and idle.
        if let Some(locked) = self.eff_converged_at {
            self.current_freq_mhz = locked;
            self.current_voltage_mv = voltage_mv;
            return TunerOutcome::Converged {
                freq_mhz: locked,
                voltage_mv,
            };
        }
        if sample.hashrate_ths <= 0.0 || sample.actual_watts <= 0.0 {
            // Need real telemetry to compute J/TH.
            return TunerOutcome::Probing;
        }
        let jth = sample.actual_watts / sample.hashrate_ths;
        // Compare against last J/TH. Improvement = lower J/TH (J/TH
        // and W/TH go the same direction at fixed power dimensions).
        let improving = match self.eff_last_jth {
            Some(prev) => jth + 1e-9 < prev,
            None => true,
        };
        self.eff_last_jth = Some(jth);
        if improving {
            self.eff_non_improving_ticks = 0;
            // Step frequency down, bounded by envelope.
            let target = self
                .current_freq_mhz
                .saturating_sub(EFFICIENCY_STEP_MHZ)
                .max(freq_min_mhz);
            let next = slew_toward(self.current_freq_mhz, target, freq_min_mhz, freq_max_mhz);
            self.current_freq_mhz = next;
            self.current_voltage_mv = voltage_mv;
            TunerOutcome::Converged {
                freq_mhz: next,
                voltage_mv,
            }
        } else {
            self.eff_non_improving_ticks = self.eff_non_improving_ticks.saturating_add(1);
            if self.eff_non_improving_ticks >= EFFICIENCY_CONVERGENCE_TICKS {
                // Lock in.
                self.eff_converged_at = Some(self.current_freq_mhz);
                self.current_voltage_mv = voltage_mv;
                TunerOutcome::Converged {
                    freq_mhz: self.current_freq_mhz,
                    voltage_mv,
                }
            } else {
                // Still inside the window — hold and observe.
                TunerOutcome::Probing
            }
        }
    }

    fn step_heater(
        &mut self,
        target_watts: u32,
        freq_min_mhz: u16,
        freq_max_mhz: u16,
        voltage_mv: u16,
        sample: TelemetrySample,
    ) -> TunerOutcome {
        if let Err(reason) = check_voltage_clamp(voltage_mv) {
            return TunerOutcome::RefusedSafety { reason };
        }
        // Heater is intrinsically home-mode. Fan PWM > 30 is refused
        // outright — fan blast is incompatible with space-heater use.
        if sample.fan_pwm > HOME_FAN_PWM_MAX {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "home_mode fan PWM {} exceeds home cap {} — refusing to engage",
                    sample.fan_pwm, HOME_FAN_PWM_MAX
                ),
            };
        }
        if freq_min_mhz >= freq_max_mhz {
            return TunerOutcome::RefusedSafety {
                reason: format!(
                    "frequency band min {} MHz >= max {} MHz",
                    freq_min_mhz, freq_max_mhz
                ),
            };
        }
        if target_watts == 0 {
            return TunerOutcome::RefusedSafety {
                reason: "heater target_watts must be > 0".to_string(),
            };
        }
        let target = target_watts as f64;
        let error_w = target - sample.actual_watts;
        self.heater_last_watts = Some(sample.actual_watts);
        // Within tolerance — hold.
        if error_w.abs() / target <= HEATER_TARGET_TOLERANCE {
            self.current_voltage_mv = voltage_mv;
            return TunerOutcome::Converged {
                freq_mhz: self.current_freq_mhz,
                voltage_mv,
            };
        }
        // Proportional adjust to drive wall watts (heat) toward
        // target. Heater shares the same slew + envelope clamps as
        // every other mode. Roughly 2 MHz/W matches the S4-03 default
        // Kp; we keep it bounded to ±5 MHz/tick.
        let raw_delta = 2.0 * error_w;
        let slew = SLEW_MHZ_PER_TICK as f64;
        let clamped_delta = raw_delta.clamp(-slew, slew);
        let proposed = (self.current_freq_mhz as i32 + clamped_delta.round() as i32)
            .clamp(freq_min_mhz as i32, freq_max_mhz as i32) as u16;
        self.current_freq_mhz = proposed;
        self.current_voltage_mv = voltage_mv;
        TunerOutcome::Converged {
            freq_mhz: proposed,
            voltage_mv,
        }
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// HARD voltage clamp shared across all variants. Returns the same
/// string the S4-03 `EngageError::VoltageClampViolation` Display
/// emits.
pub fn check_voltage_clamp(requested_mv: u16) -> Result<(), String> {
    if requested_mv > VOLTAGE_CLAMP_MV {
        return Err(format!(
            "voltage {} mV exceeds HARD clamp {} mV — refusing to engage",
            requested_mv, VOLTAGE_CLAMP_MV
        ));
    }
    Ok(())
}

/// Slew `current` toward `target` by at most [`SLEW_MHZ_PER_TICK`] per
/// step, then clamp into `[freq_min_mhz, freq_max_mhz]`.
fn slew_toward(current: u16, target: u16, freq_min_mhz: u16, freq_max_mhz: u16) -> u16 {
    let delta: i32 = target as i32 - current as i32;
    let slew = SLEW_MHZ_PER_TICK as i32;
    let bounded = delta.clamp(-slew, slew);
    let next = current as i32 + bounded;
    next.clamp(freq_min_mhz as i32, freq_max_mhz as i32) as u16
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_sample(watts: f64, ths: f64) -> TelemetrySample {
        TelemetrySample {
            actual_watts: watts,
            hashrate_ths: ths,
            voltage_mv: 13_700,
            fan_pwm: 10,
        }
    }

    // -----------------------------------------------------------------
    // Performance
    // -----------------------------------------------------------------

    #[test]
    fn performance_slews_toward_band_max() {
        let mut d = TunerDriver::new(
            TunerMode::Performance {
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::Converged {
                freq_mhz,
                voltage_mv,
            } => {
                assert!(freq_mhz > 600 && freq_mhz <= 700);
                assert_eq!(voltage_mv, 13_700);
            }
            other => panic!("expected Converged, got {:?}", other),
        }
    }

    #[test]
    fn performance_respects_slew_rate_per_tick() {
        let mut d = TunerDriver::new(
            TunerMode::Performance {
                freq_min_mhz: 500,
                freq_max_mhz: 800,
                voltage_mv: 13_700,
            },
            500,
            13_700,
        );
        let before = d.current_freq_mhz();
        d.step(ok_sample(900.0, 14.0));
        let after = d.current_freq_mhz();
        assert!((after as i32 - before as i32).abs() <= SLEW_MHZ_PER_TICK as i32);
    }

    #[test]
    fn performance_refuses_voltage_clamp_violation() {
        let mut d = TunerDriver::new(
            TunerMode::Performance {
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 14_600, // over clamp
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("14600 mV"));
                assert!(reason.contains("HARD clamp"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    #[test]
    fn performance_refuses_degenerate_band() {
        let mut d = TunerDriver::new(
            TunerMode::Performance {
                freq_min_mhz: 700,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("frequency band"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // PowerTarget — delegates to S4-03 controller
    // -----------------------------------------------------------------

    #[test]
    fn power_target_delegates_to_s4_03_controller() {
        let mut d = TunerDriver::new(
            TunerMode::PowerTarget {
                target_watts: 1000,
                home_mode: false,
            },
            600,
            13_700,
        );
        // First few ticks should be Probing (steady-state gate in the
        // S4-03 controller waits 3 stable ticks).
        let mut saw_probing = false;
        for _ in 0..3 {
            let outcome = d.step(ok_sample(900.0, 14.0));
            if matches!(outcome, TunerOutcome::Probing) {
                saw_probing = true;
            }
        }
        assert!(
            saw_probing,
            "PowerTarget should yield Probing while steady-state gate runs"
        );
        // Confirm a controller was actually constructed (delegation).
        assert!(
            d.power_controller.is_some(),
            "PowerTarget must own a PowerTargetController"
        );
    }

    #[test]
    fn power_target_refuses_home_fan_violation_via_controller() {
        let mut d = TunerDriver::new(
            TunerMode::PowerTarget {
                target_watts: 1000,
                home_mode: true,
            },
            600,
            13_700,
        );
        let mut hot = ok_sample(900.0, 14.0);
        hot.fan_pwm = 40; // over home cap
        match d.step(hot) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("home_mode fan PWM"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // HashrateTarget
    // -----------------------------------------------------------------

    #[test]
    fn hashrate_target_proportional_adjust_up_when_low() {
        let mut d = TunerDriver::new(
            TunerMode::HashrateTarget {
                target_ths: 14.0,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        let before = d.current_freq_mhz();
        // Hashrate well below target → should slew up.
        d.step(ok_sample(900.0, 10.0));
        let after = d.current_freq_mhz();
        assert!(
            after >= before,
            "low hashrate should slew freq up, got before={} after={}",
            before,
            after
        );
    }

    #[test]
    fn hashrate_target_converges_within_tolerance() {
        let mut d = TunerDriver::new(
            TunerMode::HashrateTarget {
                target_ths: 14.0,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        // Sample right at the target.
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::Converged { freq_mhz, .. } => {
                assert_eq!(freq_mhz, 600, "at-target hashrate should hold current freq");
            }
            other => panic!("expected Converged, got {:?}", other),
        }
    }

    #[test]
    fn hashrate_target_refuses_zero_or_negative_target() {
        let mut d = TunerDriver::new(
            TunerMode::HashrateTarget {
                target_ths: 0.0,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("hashrate target"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Manual
    // -----------------------------------------------------------------

    #[test]
    fn manual_pinned_still_envelope_clamped() {
        // Operator pins 9999 MHz; envelope is 500..700 — must clamp.
        let mut d = TunerDriver::new(
            TunerMode::Manual {
                freq_mhz: 9999,
                voltage_mv: 13_700,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
            },
            600,
            13_700,
        );
        // Slew per tick — drive until we get there.
        let mut last_freq = 0u16;
        for _ in 0..100 {
            match d.step(ok_sample(900.0, 14.0)) {
                TunerOutcome::Converged { freq_mhz, .. } => last_freq = freq_mhz,
                other => panic!("unexpected: {:?}", other),
            }
        }
        assert_eq!(
            last_freq, 700,
            "manual pin should converge to clamped envelope max"
        );
    }

    #[test]
    fn manual_refuses_voltage_clamp_violation() {
        let mut d = TunerDriver::new(
            TunerMode::Manual {
                freq_mhz: 600,
                voltage_mv: 14_600,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 14.0)) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("HARD clamp"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Efficiency
    // -----------------------------------------------------------------

    #[test]
    fn efficiency_walks_down_while_jth_improves_then_locks_in() {
        let mut d = TunerDriver::new(
            TunerMode::Efficiency {
                freq_min_mhz: 400,
                freq_max_mhz: 800,
                voltage_mv: 13_700,
            },
            700,
            13_700,
        );
        // Tick 1: J/TH = 900/14 = 64.3 (initial).
        // Tick 2: improve → J/TH = 800/14 = 57.1 → walk down.
        // Tick 3: improve → J/TH = 700/14 = 50.0 → walk down.
        // Ticks 4-6: hold-stable → J/TH steady → stop improving.
        let _ = d.step(ok_sample(900.0, 14.0));
        let _ = d.step(ok_sample(800.0, 14.0));
        let _ = d.step(ok_sample(700.0, 14.0));
        let after_descent = d.current_freq_mhz();
        // Now three non-improving ticks (same watts/hash) should
        // lock in.
        for _ in 0..EFFICIENCY_CONVERGENCE_TICKS {
            d.step(ok_sample(700.0, 14.0));
        }
        let locked = d.eff_converged_at.expect("efficiency must lock in");
        assert_eq!(locked, after_descent);
        // Subsequent ticks should hold the locked-in frequency.
        for _ in 0..5 {
            match d.step(ok_sample(700.0, 14.0)) {
                TunerOutcome::Converged { freq_mhz, .. } => assert_eq!(freq_mhz, locked),
                other => panic!("expected hold-Converged, got {:?}", other),
            }
        }
    }

    #[test]
    fn efficiency_terminates_within_bounded_tick_count() {
        // Pathological case: J/TH always improves slightly. The
        // walker is still bounded by envelope_min — must terminate at
        // the band floor.
        let mut d = TunerDriver::new(
            TunerMode::Efficiency {
                freq_min_mhz: 400,
                freq_max_mhz: 800,
                voltage_mv: 13_700,
            },
            500,
            13_700,
        );
        // Slowly-improving J/TH forever. We assert the walker either
        // locks in OR hits envelope_min within (max-min)/step + buffer
        // ticks — terminates either way.
        let max_ticks = ((500 - 400) / EFFICIENCY_STEP_MHZ as i32).abs() as usize
            + EFFICIENCY_CONVERGENCE_TICKS as usize
            + 10;
        let mut declining = 1000.0_f64;
        for _ in 0..max_ticks {
            declining -= 0.1;
            d.step(ok_sample(declining, 14.0));
        }
        assert!(
            d.eff_converged_at.is_some() || d.current_freq_mhz() == 400,
            "efficiency walker must terminate; current={}, converged_at={:?}",
            d.current_freq_mhz(),
            d.eff_converged_at,
        );
    }

    // -----------------------------------------------------------------
    // Heater
    // -----------------------------------------------------------------

    #[test]
    fn heater_refuses_when_fan_exceeds_home_cap() {
        let mut d = TunerDriver::new(
            TunerMode::Heater {
                target_watts: 800,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        let mut blast = ok_sample(700.0, 12.0);
        blast.fan_pwm = 60;
        match d.step(blast) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("home_mode fan PWM"));
                assert!(reason.contains("home cap"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    #[test]
    fn heater_proportional_adjust_toward_wattage_target() {
        let mut d = TunerDriver::new(
            TunerMode::Heater {
                target_watts: 1000,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        let before = d.current_freq_mhz();
        // Wall watts way below target → slew up.
        d.step(ok_sample(500.0, 8.0));
        let after = d.current_freq_mhz();
        assert!(
            after >= before,
            "low watts vs target should slew up; before={} after={}",
            before,
            after
        );
    }

    #[test]
    fn heater_holds_inside_tolerance_band() {
        let mut d = TunerDriver::new(
            TunerMode::Heater {
                target_watts: 1000,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 13_700,
            },
            600,
            13_700,
        );
        // Inside 5% tolerance → no change.
        match d.step(ok_sample(995.0, 12.0)) {
            TunerOutcome::Converged { freq_mhz, .. } => assert_eq!(freq_mhz, 600),
            other => panic!("expected hold-Converged, got {:?}", other),
        }
    }

    #[test]
    fn heater_refuses_voltage_clamp_violation() {
        let mut d = TunerDriver::new(
            TunerMode::Heater {
                target_watts: 1000,
                freq_min_mhz: 500,
                freq_max_mhz: 700,
                voltage_mv: 14_600,
            },
            600,
            13_700,
        );
        match d.step(ok_sample(900.0, 12.0)) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("HARD clamp"));
            }
            other => panic!("expected RefusedSafety, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Mode-transition steady-state gate
    // -----------------------------------------------------------------

    #[test]
    fn set_mode_resets_per_variant_state() {
        let mut d = TunerDriver::new(
            TunerMode::PowerTarget {
                target_watts: 1000,
                home_mode: false,
            },
            600,
            13_700,
        );
        // Run enough ticks to actually build the controller.
        d.step(ok_sample(900.0, 14.0));
        assert!(d.power_controller.is_some());
        // Swap to Manual — controller should drop, eff/heater state
        // should clear.
        d.set_mode(TunerMode::Manual {
            freq_mhz: 600,
            voltage_mv: 13_700,
            freq_min_mhz: 500,
            freq_max_mhz: 700,
        });
        assert!(
            d.power_controller.is_none(),
            "controller must clear on mode swap"
        );
        assert!(d.eff_last_jth.is_none());
        assert!(d.eff_converged_at.is_none());
        assert_eq!(d.eff_non_improving_ticks, 0);
        assert!(d.heater_last_watts.is_none());
    }

    #[test]
    fn set_mode_to_efficiency_then_jthm_window_starts_fresh() {
        let mut d = TunerDriver::new(
            TunerMode::Efficiency {
                freq_min_mhz: 400,
                freq_max_mhz: 800,
                voltage_mv: 13_700,
            },
            700,
            13_700,
        );
        // Half-progress through the convergence window.
        d.step(ok_sample(900.0, 14.0));
        d.step(ok_sample(900.0, 14.0));
        d.step(ok_sample(900.0, 14.0));
        // Swap modes and back — window should reset.
        d.set_mode(TunerMode::Performance {
            freq_min_mhz: 400,
            freq_max_mhz: 800,
            voltage_mv: 13_700,
        });
        d.set_mode(TunerMode::Efficiency {
            freq_min_mhz: 400,
            freq_max_mhz: 800,
            voltage_mv: 13_700,
        });
        assert_eq!(d.eff_non_improving_ticks, 0);
        assert!(d.eff_last_jth.is_none());
    }

    // -----------------------------------------------------------------
    // Serde — round-trip the TOML discriminant
    // -----------------------------------------------------------------

    #[test]
    fn tuner_mode_round_trips_through_toml_performance() {
        let mode = TunerMode::Performance {
            freq_min_mhz: 400,
            freq_max_mhz: 800,
            voltage_mv: 13_700,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"performance\""), "got: {}", s);
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn tuner_mode_round_trips_through_toml_power_target() {
        let mode = TunerMode::PowerTarget {
            target_watts: 1200,
            home_mode: true,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"power-target\""), "got: {}", s);
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn tuner_mode_round_trips_through_toml_heater() {
        let mode = TunerMode::Heater {
            target_watts: 800,
            freq_min_mhz: 400,
            freq_max_mhz: 700,
            voltage_mv: 13_700,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"heater\""));
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn tuner_mode_round_trips_through_toml_efficiency() {
        let mode = TunerMode::Efficiency {
            freq_min_mhz: 400,
            freq_max_mhz: 800,
            voltage_mv: 13_700,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"efficiency\""));
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn tuner_mode_round_trips_through_toml_manual() {
        let mode = TunerMode::Manual {
            freq_mhz: 525,
            voltage_mv: 13_500,
            freq_min_mhz: 400,
            freq_max_mhz: 700,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"manual\""));
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn tuner_mode_round_trips_through_toml_hashrate_target() {
        let mode = TunerMode::HashrateTarget {
            target_ths: 14.5,
            freq_min_mhz: 400,
            freq_max_mhz: 800,
            voltage_mv: 13_700,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"hashrate-target\""));
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);
    }

    // -----------------------------------------------------------------
    // HashrateQuota — ePIC UMC OS V1.18.2 analog. Delegates to the
    // SAME PowerTargetController path PowerTarget uses.
    // -----------------------------------------------------------------

    fn quota_fraction(frac: f32, rated_ths: f32, rated_w: u32) -> TunerMode {
        TunerMode::HashrateQuota {
            fraction: Some(frac),
            absolute_ths: None,
            rated_max_ths: rated_ths,
            rated_max_watts: rated_w,
            home_mode: false,
        }
    }

    #[test]
    fn hashrate_quota_fraction_maps_to_proportional_power_target() {
        // 50% of a 100 TH/s / 3000 W miner ⇒ a 1500 W power-target.
        // We can't read the derived watts directly (it's internal to
        // the delegated controller), but we CAN prove delegation
        // happened: a PowerTargetController gets constructed and the
        // first ticks are the S4-03 steady-state Probing gate — exactly
        // like the PowerTarget variant.
        let mut d = TunerDriver::new(quota_fraction(0.5, 100.0, 3000), 600, 13_700);
        let mut saw_probing = false;
        for _ in 0..3 {
            if matches!(d.step(ok_sample(900.0, 14.0)), TunerOutcome::Probing) {
                saw_probing = true;
            }
        }
        assert!(
            saw_probing,
            "HashrateQuota must delegate to PowerTargetController \
             (S4-03 steady-state gate yields Probing)"
        );
        assert!(
            d.power_controller.is_some(),
            "HashrateQuota MUST construct + own a PowerTargetController \
             — i.e. it delegates, it does not reimplement"
        );
    }

    #[test]
    fn hashrate_quota_delegates_through_gated_power_target_path() {
        // The load-bearing property: HashrateQuota goes through
        // step_power_target -> PowerTargetController, which is the
        // ONLY place the PI math + clamps live. Prove the controller
        // it builds carries the SAME target a direct PowerTarget would
        // for the equivalent wattage. 0.5 * 100 TH/s @ 2000 W = 1000 W.
        let mut d = TunerDriver::new(quota_fraction(0.5, 100.0, 2000), 600, 13_700);
        d.step(ok_sample(900.0, 14.0));
        let ctrl = d
            .power_controller
            .as_ref()
            .expect("delegated controller must exist");
        assert_eq!(
            ctrl.config().target_watts,
            1000,
            "quota 0.5 of 100 TH/s @ 2000 W rated must delegate a \
             1000 W power-target (linear ths→watts map)"
        );
        // And it must carry NO extra authority — same band/slew/tick
        // defaults as a plain PowerTarget construction (i.e. the
        // delegated config is the PowerTargetConfig default, NOT a
        // quota-specific widened envelope).
        assert_eq!(
            ctrl.config().slew_rate_mhz,
            PowerTargetConfig::default().slew_rate_mhz
        );
    }

    #[test]
    fn hashrate_quota_inherits_voltage_clamp_no_new_bypass() {
        // A voltage over the HARD clamp must be refused with the SAME
        // reason string the PowerTargetController emits — proving the
        // quota path did NOT introduce its own (weaker) voltage gate.
        let mut d = TunerDriver::new(quota_fraction(0.8, 100.0, 3000), 600, 13_700);
        let mut over = ok_sample(900.0, 14.0);
        over.voltage_mv = 14_600; // > VOLTAGE_CLAMP_MV (14_500)
        match d.step(over) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("14600 mV"));
                assert!(reason.contains("HARD clamp"));
            }
            other => panic!("expected voltage-clamp refuse, got {:?}", other),
        }
    }

    #[test]
    fn hashrate_quota_home_mode_inherits_fan_cap_via_controller() {
        // home_mode=true must forward into the delegated controller so
        // a fan PWM over the home cap is refused with the controller's
        // own phrasing — i.e. the cap is enforced by the SAME gated
        // path PowerTarget uses, not re-implemented here.
        let mut d = TunerDriver::new(
            TunerMode::HashrateQuota {
                fraction: Some(0.6),
                absolute_ths: None,
                rated_max_ths: 100.0,
                rated_max_watts: 3000,
                home_mode: true,
            },
            600,
            13_700,
        );
        let mut hot = ok_sample(900.0, 14.0);
        hot.fan_pwm = 45; // > HOME_FAN_PWM_MAX (30)
        match d.step(hot) {
            TunerOutcome::RefusedSafety { reason } => {
                assert!(reason.contains("home_mode fan PWM"));
                assert!(reason.contains("home cap"));
            }
            other => panic!("expected home-fan-cap refuse, got {:?}", other),
        }
    }

    #[test]
    fn hashrate_quota_absolute_ths_beats_fraction_and_is_capped() {
        // absolute_ths wins over fraction; a request above rated is
        // capped to rated (a quota is a CAP, never an overclock).
        // 150 TH/s req on a 100 TH/s / 1000 W miner ⇒ resolves 100 TH/s
        // ⇒ 1000 W delegated target (clamped to rated_max_watts).
        let mut d = TunerDriver::new(
            TunerMode::HashrateQuota {
                fraction: Some(0.9),
                absolute_ths: Some(150.0),
                rated_max_ths: 100.0,
                rated_max_watts: 1000,
                home_mode: false,
            },
            600,
            13_700,
        );
        d.step(ok_sample(900.0, 14.0));
        let ctrl = d.power_controller.as_ref().unwrap();
        assert_eq!(
            ctrl.config().target_watts,
            1000,
            "absolute_ths beats fraction AND a quota is a cap — \
             over-rated request must resolve to rated_max_watts"
        );
    }

    #[test]
    fn hashrate_quota_refuses_invalid_inputs() {
        // neither field
        let mut d = TunerDriver::new(
            TunerMode::HashrateQuota {
                fraction: None,
                absolute_ths: None,
                rated_max_ths: 100.0,
                rated_max_watts: 3000,
                home_mode: false,
            },
            600,
            13_700,
        );
        assert!(matches!(
            d.step(ok_sample(900.0, 14.0)),
            TunerOutcome::RefusedSafety { .. }
        ));
        // out-of-range fraction
        let mut d2 = TunerDriver::new(quota_fraction(0.0, 100.0, 3000), 600, 13_700);
        assert!(matches!(
            d2.step(ok_sample(900.0, 14.0)),
            TunerOutcome::RefusedSafety { .. }
        ));
        let mut d3 = TunerDriver::new(quota_fraction(1.5, 100.0, 3000), 600, 13_700);
        assert!(matches!(
            d3.step(ok_sample(900.0, 14.0)),
            TunerOutcome::RefusedSafety { .. }
        ));
        // degenerate rated envelope
        let mut d4 = TunerDriver::new(quota_fraction(0.5, 0.0, 3000), 600, 13_700);
        assert!(matches!(
            d4.step(ok_sample(900.0, 14.0)),
            TunerOutcome::RefusedSafety { .. }
        ));
        let mut d5 = TunerDriver::new(quota_fraction(0.5, 100.0, 0), 600, 13_700);
        assert!(matches!(
            d5.step(ok_sample(900.0, 14.0)),
            TunerOutcome::RefusedSafety { .. }
        ));
    }

    #[test]
    fn hashrate_quota_min_fraction_never_resolves_to_disabled() {
        // The minimum legal fraction must still build a controller
        // (target_watts >= 1), never trip PowerTargetController's
        // target_watts==0 == Disabled bail.
        let mut d = TunerDriver::new(
            quota_fraction(HASHRATE_QUOTA_MIN_FRACTION as f32, 14.0, 1400),
            600,
            13_700,
        );
        d.step(ok_sample(900.0, 14.0));
        assert!(
            d.power_controller.is_some(),
            "min-fraction quota must still delegate a live controller"
        );
        assert!(d.power_controller.as_ref().unwrap().config().target_watts >= 1);
    }

    #[test]
    fn hashrate_quota_round_trips_through_toml() {
        let mode = TunerMode::HashrateQuota {
            fraction: Some(0.65),
            absolute_ths: None,
            rated_max_ths: 100.0,
            rated_max_watts: 3000,
            home_mode: true,
        };
        let s = toml::to_string(&mode).unwrap();
        assert!(s.contains("mode = \"hashrate-quota\""), "got: {}", s);
        let back: TunerMode = toml::from_str(&s).unwrap();
        assert_eq!(mode, back);

        // absolute_ths variant too.
        let mode2 = TunerMode::HashrateQuota {
            fraction: None,
            absolute_ths: Some(42.5),
            rated_max_ths: 110.0,
            rated_max_watts: 3250,
            home_mode: false,
        };
        let s2 = toml::to_string(&mode2).unwrap();
        let back2: TunerMode = toml::from_str(&s2).unwrap();
        assert_eq!(mode2, back2);
    }

    #[test]
    fn hashrate_quota_minimal_toml_uses_defaults() {
        // Only `mode` + `fraction` supplied — rated_max_* + home_mode
        // fall back to the documented serde defaults.
        let toml_src = "mode = \"hashrate-quota\"\nfraction = 0.5\n";
        let m: TunerMode = toml::from_str(toml_src).unwrap();
        match m {
            TunerMode::HashrateQuota {
                fraction,
                absolute_ths,
                rated_max_ths,
                rated_max_watts,
                home_mode,
            } => {
                assert_eq!(fraction, Some(0.5));
                assert_eq!(absolute_ths, None);
                assert_eq!(rated_max_ths, DEFAULT_RATED_MAX_THS as f32);
                assert_eq!(rated_max_watts, DEFAULT_RATED_MAX_WATTS);
                assert!(home_mode, "home_mode defaults to true for a quota");
            }
            other => panic!("expected HashrateQuota, got {:?}", other),
        }
    }

    #[test]
    fn set_mode_to_hashrate_quota_resets_then_rebuilds_controller() {
        // Mode-swap must drop the prior controller; the new quota mode
        // rebuilds its own (proves no stale PI integrator carries over
        // — same contract as PowerTarget).
        let mut d = TunerDriver::new(
            TunerMode::PowerTarget {
                target_watts: 1000,
                home_mode: false,
            },
            600,
            13_700,
        );
        d.step(ok_sample(900.0, 14.0));
        assert!(d.power_controller.is_some());
        d.set_mode(quota_fraction(0.4, 100.0, 2500));
        assert!(
            d.power_controller.is_none(),
            "set_mode must clear the prior controller"
        );
        d.step(ok_sample(900.0, 14.0));
        assert!(
            d.power_controller.is_some(),
            "HashrateQuota rebuilds its own delegated controller"
        );
    }

    #[test]
    fn default_manual_at_seeds_with_live_values() {
        let mode = TunerMode::default_manual_at(525, 13_500);
        match mode {
            TunerMode::Manual {
                freq_mhz,
                voltage_mv,
                freq_min_mhz,
                freq_max_mhz,
            } => {
                assert_eq!(freq_mhz, 525);
                assert_eq!(voltage_mv, 13_500);
                assert_eq!(freq_min_mhz, DEFAULT_FREQ_MIN_MHZ);
                assert_eq!(freq_max_mhz, DEFAULT_FREQ_MAX_MHZ);
            }
            other => panic!("expected Manual, got {:?}", other),
        }
    }
}
