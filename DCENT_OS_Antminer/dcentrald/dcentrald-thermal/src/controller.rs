//! PID-based thermal control loop.
//!
//! The thermal controller runs at a 5-second interval. It reads chain
//! temperatures via I2C, applies PID control to compute fan PWM output,
//! and issues frequency throttle commands when temperatures exceed thresholds.
//!
//! Control loop architecture:
//!   Temperature Sensors (I2C TMP75 or PIC readback)
//!     |
//!     v
//!   PID Controller (5s interval)
//!     |
//!     +-> Fan PWM Adjust (0-100)
//!     +-> Frequency Throttle (reduce MHz or disable boards)

use crate::immersion::{ImmersionConfig, ImmersionDecision};
use crate::profiles::ThermalProfile;
use crate::ThermalState;
use dcentrald_api_types::thermal_model::{safe_fan_pwm, FanMode, FanSafetyTrigger};

const TEMP_STALE_TIMEOUT_S: u64 = 30;
const STARTUP_TEMP_GRACE_S: u64 = 60;
/// Braiins fan-control PWM ceiling. Mirrors dcentrald_hal::fan::PWM_MAX.
const FAN_PWM_MAX: u8 = 100;
const PID_GAIN_MIN: f32 = 0.0;
const PID_GAIN_MAX: f32 = 10.0;

/// Map a profile's `fan_max_pwm` setting back to the canonical
/// `FanMode` enum from `dcentrald-api-types`. The caller passes the
/// configured profile cap and gets the matching mode; profiles whose
/// `fan_max_pwm` doesn't line up exactly with a canonical cap fall back
/// to the safest mode whose cap is ≥ the configured value.
fn fan_mode_from_profile_cap(profile_max_pwm: u8) -> FanMode {
    match profile_max_pwm {
        0..=10 => FanMode::QuietHome,
        11..=30 => FanMode::Home,
        31..=64 => FanMode::Balanced,
        65..=100 => FanMode::Advanced,
        _ => FanMode::HashrateMax,
    }
}

/// Chokepoint for every fan PWM write inside the thermal controller.
/// Routes through `dcentrald_api_types::thermal_model::safe_fan_pwm`,
/// which is the canonical helper that defends the home-mining PWM cap.
/// Wired in  so a corrupted `profile.fan_max_pwm` (>127) or a
/// stray raw 127 write cannot leak past this layer. Pass
/// `Some(trigger)` to force the mode-cap PWM (safety override) or
/// `None` to clamp `requested` to the cap.
fn safety_capped_pwm(profile_max_pwm: u8, trigger: Option<FanSafetyTrigger>, requested: u8) -> u8 {
    let mode = fan_mode_from_profile_cap(profile_max_pwm);
    // Never round a configured cap UP to the coarse FanMode bucket ceiling on a
    // safety trigger: `fan_max_pwm` is an ABSOLUTE quiet ceiling, so a sub-boundary
    // value (e.g. 25 in the 11..=30 Home bucket whose cap is 30) must not yield a
    // safety PWM of 30. Clamp to the EXACT configured cap too, so `current_pwm`
    // honors the operator's ceiling rather than relying on the daemon's downstream
    // re-clamp as the only guard. This only ever LOWERS the result (no-op on the
    // normal None-path callers, which already pass requested == profile_max_pwm).
    safe_fan_pwm(mode, trigger, requested).min(profile_max_pwm)
}

/// Clamp a PWM request to a profile min/max pair without trusting the pair's
/// ordering. `ThermalController::new()` normalizes the profile, but this keeps
/// the point-of-use safe if a future constructor bypass or test mutation
/// creates an inverted pair.
fn safe_pwm_clamp(requested: u8, min: u8, max: u8) -> u8 {
    let low = min.min(max);
    let high = min.max(max);
    debug_assert!(low <= high);
    requested.clamp(low, high)
}

fn clamp_pid_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(PID_GAIN_MIN, PID_GAIN_MAX)
    } else {
        PID_GAIN_MIN
    }
}

fn bounded_pid_output(raw_output: f32) -> f32 {
    if raw_output.is_finite() {
        raw_output.clamp(0.0, FAN_PWM_MAX as f32)
    } else if raw_output.is_nan() {
        FAN_PWM_MAX as f32
    } else if raw_output.is_sign_negative() {
        0.0
    } else {
        FAN_PWM_MAX as f32
    }
}

/// PID controller for thermal management.
pub struct PidController {
    /// Proportional gain.
    pub kp: f32,
    /// Integral gain.
    pub ki: f32,
    /// Derivative gain.
    pub kd: f32,
    /// Temperature setpoint (target_temp_c).
    pub setpoint: f32,
    /// Accumulated integral error.
    integral: f32,
    /// Previous error (for derivative term).
    prev_error: f32,
    /// PID output (fan PWM value, 0.0 to 100.0).
    output: f32,
    /// Integral windup limit.
    integral_limit: f32,
}

impl PidController {
    /// Create a new PID controller with default gains.
    pub fn new(setpoint: f32) -> Self {
        Self {
            kp: 2.0,
            ki: 0.5,
            kd: 0.5,
            setpoint,
            integral: 0.0,
            prev_error: 0.0,
            output: 0.0,
            integral_limit: 250.0,
        }
    }

    /// Compute the next PID output given the current temperature.
    ///
    /// Returns a fan PWM value in the range [0.0, 100.0].
    pub fn update(&mut self, current_temp: f32) -> f32 {
        // Fail-safe backstop: a non-finite temperature (NaN / ±inf from a failed
        // or glitching sensor) is a LOSS of thermal proof, not a "cold" reading.
        // Never let it flow into the PID math and produce a NaN output — that
        // saturating-casts to 0 PWM downstream (`pid_output as u8`), i.e. fans OFF
        // on a dead sensor, a fail-OPEN. Force the max PWM (which the downstream
        // `safety_capped_pwm` still clamps to the home cap) and do NOT poison the
        // integrator with a non-finite error. The thermal supervisor already
        // filters non-finite temps before calling this; this is the defense-in-depth
        // guard at the control-math boundary so the invariant holds unconditionally.
        if !current_temp.is_finite() {
            self.output = FAN_PWM_MAX as f32;
            return self.output;
        }
        let error = current_temp - self.setpoint;

        // Proportional term
        let p_term = self.kp * error;

        // Integral term (with anti-windup)
        self.integral += error;
        self.integral = self
            .integral
            .clamp(-self.integral_limit, self.integral_limit);
        let i_term = self.ki * self.integral;

        // Derivative term
        let d_term = self.kd * (error - self.prev_error);
        self.prev_error = error;

        // Sum and clamp output. Extreme finite inputs can overflow or cancel to
        // NaN if an operator changes the PID gains; keep the command bounded.
        self.output = bounded_pid_output(p_term + i_term + d_term);
        self.output
    }

    /// Reset the PID controller state.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.output = 0.0;
    }

    /// Get the current PID output.
    pub fn output(&self) -> f32 {
        self.output
    }

    /// Get the current PID state for diagnostics.
    pub fn state(&self) -> PidState {
        PidState {
            setpoint: self.setpoint,
            integral: self.integral,
            prev_error: self.prev_error,
            output: self.output,
            kp: self.kp,
            ki: self.ki,
            kd: self.kd,
        }
    }
}

/// PID controller state snapshot (for /api/debug/pid-state).
#[derive(Debug, Clone)]
pub struct PidState {
    pub setpoint: f32,
    pub integral: f32,
    pub prev_error: f32,
    pub output: f32,
    pub kp: f32,
    pub ki: f32,
    pub kd: f32,
}

/// The main thermal controller that manages the thermal state machine.
pub struct ThermalController {
    /// PID controller for fan speed.
    pid: PidController,
    /// Current thermal state.
    state: ThermalState,
    /// Active thermal profile.
    profile: ThermalProfile,
    /// Per-chain temperatures (celsius).
    chain_temps: Vec<f32>,
    /// Current fan PWM output.
    current_pwm: u8,
    /// Fan failure counter (consecutive zero-RPM reads).
    fan_zero_count: u32,
    /// Thermal control loop interval in seconds.
    #[allow(dead_code)]
    interval_s: u32,
    /// Timestamp of last valid temperature reading.
    last_temp_update: std::time::Instant,
    /// Whether hardware fan tachometer is available.
    /// When false, fan_rpm is synthesized and fan-failure detection is disabled.
    /// Temperature-based protection still operates (stalled fan → temp rise → throttle → shutdown).
    tach_available: bool,
    /// Immersion / hydro cooling mode. **Default-OFF** (`ImmersionConfig::default()`
    /// → `immersion_active() == false`), so the controller is byte-identical to
    /// the pre-immersion path on every air-cooled unit. When active, the fan-RAMP
    /// behavior is bypassed (immersion rigs have no chassis fans) while the thermal
    /// SAFETY net stays intact: die/chip temp is still monitored, and stale /
    /// sensor-failure / dangerous temp still fails closed by CUTTING HASH
    /// (`EmergencyShutdown`) — never by blasting nonexistent fans. Set via
    /// `enable_immersion()`. See `crate::immersion`.
    immersion_active: bool,
}

impl ThermalController {
    /// Create a new thermal controller with the given profile.
    pub fn new(profile: ThermalProfile) -> Self {
        // Clamp dangerous_temp_c to absolute maximum of 90C for residential safety.
        let mut profile = profile;
        if profile.dangerous_temp_c > 90 {
            tracing::warn!(
                configured = profile.dangerous_temp_c,
                clamped = 90,
                "dangerous_temp_c clamped to 90C (residential safety limit)"
            );
            profile.dangerous_temp_c = 90;
        }
        if profile.fan_max_pwm > FAN_PWM_MAX {
            tracing::warn!(
                configured = profile.fan_max_pwm,
                clamped = FAN_PWM_MAX,
                "fan_max_pwm clamped to Braiins fan-control 0-100 PWM ceiling"
            );
            profile.fan_max_pwm = FAN_PWM_MAX;
        }
        if profile.fan_min_pwm > FAN_PWM_MAX {
            tracing::warn!(
                configured = profile.fan_min_pwm,
                clamped = FAN_PWM_MAX,
                "fan_min_pwm clamped to Braiins fan-control 0-100 PWM ceiling"
            );
            profile.fan_min_pwm = FAN_PWM_MAX;
        }
        if profile.fan_min_pwm > profile.fan_max_pwm {
            tracing::warn!(
                fan_min = profile.fan_min_pwm,
                fan_max = profile.fan_max_pwm,
                "fan_min_pwm exceeded fan_max_pwm after PWM ceiling clamp; lowering min to max"
            );
            profile.fan_min_pwm = profile.fan_max_pwm;
        }
        debug_assert!(
            profile.fan_min_pwm <= profile.fan_max_pwm,
            "ThermalController::new() must normalize fan_min_pwm <= fan_max_pwm"
        );

        // Enforce a monotonic threshold ladder: target < hot < dangerous. The
        // state machine evaluates `>= dangerous` before `>= hot` before `>= target`,
        // so an inverted ladder from a bad config makes the miner respond at the
        // wrong point: hot >= dangerous skips the throttle step entirely (straight
        // to EmergencyShutdown), and target >= hot puts the PID setpoint at or above
        // the throttle point (throttle-oscillation). Pull the LOWER thresholds down
        // under the (already residential-clamped) dangerous one; every correction
        // moves the thermal response earlier, never later (fail-safe direction).
        if profile.hot_temp_c >= profile.dangerous_temp_c {
            let fixed = profile.dangerous_temp_c.saturating_sub(1);
            tracing::warn!(
                configured = profile.hot_temp_c,
                clamped = fixed,
                dangerous = profile.dangerous_temp_c,
                "hot_temp_c >= dangerous_temp_c; lowering hot below dangerous to preserve the throttle step"
            );
            profile.hot_temp_c = fixed;
        }
        if profile.target_temp_c >= profile.hot_temp_c {
            let fixed = profile.hot_temp_c.saturating_sub(1);
            tracing::warn!(
                configured = profile.target_temp_c,
                clamped = fixed,
                hot = profile.hot_temp_c,
                "target_temp_c >= hot_temp_c; lowering target below hot to keep the PID setpoint under the throttle point"
            );
            profile.target_temp_c = fixed;
        }

        let pid = PidController::new(profile.target_temp_c as f32);

        Self {
            pid,
            state: ThermalState::ColdStart,
            profile,
            chain_temps: Vec::new(),
            current_pwm: 0,
            fan_zero_count: 0,
            interval_s: 5,
            last_temp_update: std::time::Instant::now(),
            tach_available: true, // default: assume tach works, caller overrides for Amlogic
            immersion_active: false, // default-OFF: byte-identical fan behavior on air-cooled units
        }
    }

    /// Run one iteration of the thermal control loop.
    ///
    /// Reads temperatures, updates PID, adjusts fans, and returns
    /// the recommended action (fan PWM, frequency throttle, or shutdown).
    ///
    /// When immersion mode is active (`immersion_active() == true`) the fan-RAMP
    /// behavior is bypassed: any fan-PWM-carrying action is rewritten to command
    /// NO fan (PWM 0), the tach-based fan-failure path is skipped (immersion rigs
    /// have no fans / tach), and `current_pwm` stays 0. The fail-closed
    /// HASH-CUT paths (stale temp, sensor failure, dangerous temp →
    /// `EmergencyShutdown`) and the frequency throttle are preserved
    /// byte-for-byte — immersion only removes the fan ramp, never the over-temp
    /// hash-cut. With immersion OFF (default) the output is byte-identical to
    /// the pre-immersion path.
    pub fn tick(&mut self, temps: &[f32], fan_rpm: u32) -> ThermalAction {
        let action = self.tick_inner(temps, fan_rpm);
        if self.immersion_active {
            self.apply_immersion_fan_suppression(action)
        } else {
            action
        }
    }

    /// Rewrite a controller action for immersion mode: bypass the fan RAMP
    /// while preserving the hash-cut safety net.
    ///
    /// - `EmergencyShutdown` / `FanFailure` / `RestartInit`: pass through
    ///   UNCHANGED. The over-temp / stale-temp / sensor-failure hash-cut is the
    ///   immersion safety net and must never be weakened. (`FanFailure` does not
    ///   arise in immersion — the tach path is skipped — but if some future
    ///   caller produced it, escalating is still strictly safe.)
    /// - `SetFanPwm(_)`: rewritten to `SetFanPwm(0)` and `current_pwm` cleared
    ///   to 0 — the controller commands NO fan on an immersion rig.
    /// - `ThrottleAndFan { freq_reduction_pct, .. }`: the frequency throttle is
    ///   PRESERVED (it cuts heat at the source — exactly what an immersion rig
    ///   wants on a hot board), but the fan portion is zeroed (`pwm: 0`).
    fn apply_immersion_fan_suppression(&mut self, action: ThermalAction) -> ThermalAction {
        match action {
            // Hash-cut safety net — never weakened by immersion. The hash-cut
            // DECISION is preserved exactly; we only correct `current_pwm` back
            // to 0 so telemetry honestly reports "no fan commanded" (the inner
            // safety path may have set it to fan_max for an air-cooled unit's
            // blast, which is meaningless on an immersion rig — and the daemon
            // skips the fan write entirely while immersion is active anyway).
            ThermalAction::EmergencyShutdown
            | ThermalAction::FanFailure
            | ThermalAction::RestartInit => {
                self.current_pwm = 0;
                action
            }
            // Normal fan ramp → command NO fan.
            ThermalAction::SetFanPwm(_) => {
                self.current_pwm = 0;
                ThermalAction::SetFanPwm(0)
            }
            // Keep the frequency throttle (cut heat at the source); drop the fan ramp.
            ThermalAction::ThrottleAndFan {
                freq_reduction_pct, ..
            } => {
                self.current_pwm = 0;
                ThermalAction::ThrottleAndFan {
                    pwm: 0,
                    freq_reduction_pct,
                }
            }
        }
    }

    /// Inner thermal control loop — the pre-immersion behavior. The public
    /// `tick` calls this and (only when immersion is active) post-processes the
    /// result. Keeping this private + unchanged guarantees the immersion-OFF
    /// path is byte-identical.
    fn tick_inner(&mut self, temps: &[f32], fan_rpm: u32) -> ThermalAction {
        // SAFETY: Stale temperature detection.
        // If no temperature update has arrived in >30 seconds, force fans to maximum.
        // This catches: dead sensor, stuck I2C bus, crashed temp-reading thread.
        if self.last_temp_update.elapsed() > std::time::Duration::from_secs(TEMP_STALE_TIMEOUT_S) {
            // Home mining: stale temps → fans to profile max, then cut voltage.
            // NEVER blast above fan_max_pwm. Remove heat source, don't scream.
            //
            // THERM-4 (intentional + backstopped): this immediate
            // `EmergencyShutdown` only fires on the TRANSITION tick where fans
            // are not yet at the profile cap — it exists to ramp fans to max
            // *and* signal shutdown in one step. When `current_pwm` is already
            // at `fan_max_pwm` we fall through instead of re-emitting it every
            // tick. That suppression is safe because the stale condition that
            // triggers it (dead sensor / stuck I2C / crashed temp thread) means
            // no fresh temps arrive, so the `temps.is_empty()` block below fails
            // closed with its own unconditional `EmergencyShutdown`. If fresh
            // temps DO arrive, the stale-recovery tick further down handles it.
            if self.current_pwm != self.profile.fan_max_pwm {
                tracing::warn!(
                    elapsed_s = self.last_temp_update.elapsed().as_secs(),
                    fan_max = self.profile.fan_max_pwm,
                    "Temperature reading stale (>30s) — fans to profile max, requesting shutdown"
                );
                self.current_pwm = safety_capped_pwm(
                    self.profile.fan_max_pwm,
                    Some(FanSafetyTrigger::StaleTemp),
                    self.profile.fan_max_pwm,
                );
                return ThermalAction::EmergencyShutdown;
            }
        }

        // The first valid board-temperature sample can lag startup because the
        // dispatcher only reads one chain every 5 seconds. Give the runtime a
        // bounded grace window to populate the shared sensor state, but keep
        // fans at safety max while waiting. After the first real sample, or once
        // the grace window expires, missing temperatures remain a hard stop.
        if temps.is_empty() {
            let elapsed = self.last_temp_update.elapsed();
            if self.chain_temps.is_empty()
                && elapsed <= std::time::Duration::from_secs(STARTUP_TEMP_GRACE_S)
            {
                let startup_pwm = safety_capped_pwm(
                    self.profile.fan_max_pwm,
                    Some(FanSafetyTrigger::SensorError),
                    self.profile.fan_max_pwm,
                );
                if self.current_pwm != startup_pwm {
                    tracing::warn!(
                        elapsed_s = elapsed.as_secs(),
                        fan_pwm = startup_pwm,
                        "No board temperatures yet during startup grace — forcing fans to safety max while waiting for the first valid sample"
                    );
                    self.current_pwm = startup_pwm;
                }
                return ThermalAction::SetFanPwm(self.current_pwm);
            }
            tracing::warn!(
                "SAFETY: no temperature readings available — triggering emergency shutdown"
            );
            self.state = ThermalState::DangerousShutdown;
            return ThermalAction::EmergencyShutdown;
        }

        // SAFETY (fail-closed on a garbage sensor decode): drop any non-finite
        // (NaN / ±Inf) temperatures before computing the hottest chain. Rust's
        // `f32::max` returns the *non-NaN* argument, so an all-NaN / all-garbage
        // slice fed straight to the fold below would collapse to
        // `f32::NEG_INFINITY` — which reads as colder than every threshold and
        // would idle the FSM into ColdStart + fan_min_pwm. That treats a fully
        // dead sensor set as "safe/cold", contradicting the never-treat-a-bad-
        // sensor-as-safe invariant (and is inconsistent with the NaN-fails-closed
        // share-validation path in dcentrald-api-types). A non-empty sample that
        // is ENTIRELY non-finite is a dead/garbage decode — identical risk to no
        // sample at all — so it takes the SAME hard EmergencyShutdown as the
        // `temps.is_empty()` path above. A partially-finite sample (e.g.
        // `[NaN, 95.0]`) keeps only the real readings, so a hot chain is still
        // seen and still trips; the NaN can never mask a real over-temp.
        let finite_temps: Vec<f32> = temps.iter().copied().filter(|t| t.is_finite()).collect();
        let dropped_non_finite = temps.len() - finite_temps.len();
        if finite_temps.is_empty() {
            tracing::warn!(
                dropped_non_finite,
                "SAFETY: all {} temperature reading(s) were non-finite (NaN/±Inf) — triggering emergency shutdown",
                dropped_non_finite
            );
            self.state = ThermalState::DangerousShutdown;
            return ThermalAction::EmergencyShutdown;
        }
        if dropped_non_finite > 0 {
            tracing::warn!(
                dropped_non_finite,
                finite_count = finite_temps.len(),
                "Dropped {} non-finite temperature reading(s); folding hottest chain over {} finite sample(s) only",
                dropped_non_finite,
                finite_temps.len()
            );
        }

        let stale_elapsed = self.last_temp_update.elapsed();
        self.chain_temps = temps.to_vec();

        // Update stale temp tracking
        self.last_temp_update = std::time::Instant::now();

        if stale_elapsed > std::time::Duration::from_secs(TEMP_STALE_TIMEOUT_S) {
            tracing::warn!(
                elapsed_s = stale_elapsed.as_secs(),
                "Temperature readings were stale (>30s) - forcing one max-fan recovery tick"
            );
            self.pid.reset();
            self.current_pwm = safety_capped_pwm(
                self.profile.fan_max_pwm,
                Some(FanSafetyTrigger::StaleTemp),
                self.profile.fan_max_pwm,
            );
            return ThermalAction::SetFanPwm(self.current_pwm);
        }

        // Get the hottest chain temperature (over the FINITE samples only — any
        // NaN/±Inf was already dropped above, so this can never collapse to
        // NEG_INFINITY for a non-empty sample).
        let max_temp = finite_temps
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);

        // Fan failure detection: 3 consecutive zero-RPM reads (~15s debounce at 5s interval).
        // Previous threshold of 1 read was too aggressive — fans can briefly report 0 RPM
        // during PWM transitions.
        // SAFETY (2026-04-11): Skip when tach_available=false (Amlogic — RPM is synthesized).
        // Temperature-based protection still operates: stalled fan → temp rise → throttle → shutdown.
        // IMMERSION: skip when immersion is active — an immersion/hydro rig has no chassis fans
        // (no tach), so "0 RPM" is the expected state, not a failure. The temperature-based
        // hash-cut paths above/below remain the safety net (over-temp / stale-temp → shutdown).
        if !self.immersion_active && self.tach_available && self.current_pwm > 0 && fan_rpm == 0 {
            self.fan_zero_count += 1;
            if self.fan_zero_count >= 3 {
                tracing::warn!(
                    pwm = self.current_pwm,
                    consecutive_zero = self.fan_zero_count,
                    "SAFETY: fan failure detected — 0 RPM for {} consecutive reads at PWM {}. \
                     Setting fans to MAX and triggering emergency shutdown.",
                    self.fan_zero_count,
                    self.current_pwm,
                );
                // SAFETY: Fan failure must set fans to MAX, not zero.
                // Zero fans with boards powered = thermal runaway in <30 seconds.
                // Max PWM keeps fans spinning in case it was a tach sensor glitch.
                // If the fan motor is truly stalled, MAX PWM won't make it worse —
                // the emergency shutdown will cut hash board power immediately.
                self.current_pwm = safety_capped_pwm(
                    self.profile.fan_max_pwm,
                    Some(FanSafetyTrigger::FanFailure),
                    self.profile.fan_max_pwm,
                );
                self.state = ThermalState::DangerousShutdown;
                return ThermalAction::FanFailure;
            }
        } else {
            self.fan_zero_count = 0;
        }

        // Degraded fan detection: fan spinning but much slower than expected.
        // At PWM > 30 (>24% duty), a healthy fan should spin at >300 RPM.
        // Sub-300 RPM at high PWM indicates bearing failure or obstruction.
        // Skip when tach unavailable (Amlogic — RPM is synthesized from PWM).
        // IMMERSION: skip — no chassis fans on an immersion/hydro rig.
        if !self.immersion_active
            && self.tach_available
            && fan_rpm > 0
            && fan_rpm < 300
            && self.current_pwm > 30
        {
            tracing::warn!(
                fan_rpm,
                pwm = self.current_pwm,
                "Fan degraded: only {} RPM at PWM {} — possible bearing failure or obstruction",
                fan_rpm,
                self.current_pwm,
            );
        }

        // Thermal state machine
        let dangerous = self.profile.dangerous_temp_c as f32;
        let hot = self.profile.hot_temp_c as f32;
        let target = self.profile.target_temp_c as f32;
        let hysteresis = self.profile.hysteresis_c as f32;

        match self.state {
            ThermalState::ColdStart => {
                if max_temp >= dangerous {
                    self.state = ThermalState::DangerousShutdown;
                    return ThermalAction::EmergencyShutdown;
                } else if max_temp >= hot {
                    self.state = ThermalState::HotThrottle;
                    let capped =
                        safety_capped_pwm(self.profile.fan_max_pwm, None, self.profile.fan_max_pwm);
                    self.current_pwm = capped;
                    return ThermalAction::ThrottleAndFan {
                        pwm: capped,
                        freq_reduction_pct: 10,
                    };
                } else if max_temp >= target {
                    self.state = ThermalState::NormalMining;
                }

                let pwm = self.profile.fan_min_pwm;
                self.current_pwm = pwm;
                ThermalAction::SetFanPwm(pwm)
            }

            ThermalState::NormalMining => {
                if max_temp >= dangerous {
                    self.state = ThermalState::DangerousShutdown;
                    return ThermalAction::EmergencyShutdown;
                } else if max_temp >= hot {
                    self.state = ThermalState::HotThrottle;
                    let capped =
                        safety_capped_pwm(self.profile.fan_max_pwm, None, self.profile.fan_max_pwm);
                    self.current_pwm = capped;
                    return ThermalAction::ThrottleAndFan {
                        pwm: capped,
                        freq_reduction_pct: 10,
                    };
                }

                let pid_output = self.pid.update(max_temp);
                let pwm = safe_pwm_clamp(
                    pid_output as u8,
                    self.profile.fan_min_pwm,
                    self.profile.fan_max_pwm,
                );
                self.current_pwm = pwm;
                ThermalAction::SetFanPwm(pwm)
            }

            ThermalState::HotThrottle => {
                if max_temp >= dangerous {
                    self.state = ThermalState::DangerousShutdown;
                    return ThermalAction::EmergencyShutdown;
                } else if max_temp < hot - hysteresis {
                    self.state = ThermalState::NormalMining;
                }

                let capped =
                    safety_capped_pwm(self.profile.fan_max_pwm, None, self.profile.fan_max_pwm);
                self.current_pwm = capped;
                ThermalAction::ThrottleAndFan {
                    pwm: capped,
                    freq_reduction_pct: 10,
                }
            }

            ThermalState::DangerousShutdown => {
                if max_temp < dangerous - hysteresis {
                    self.state = ThermalState::ColdStart;
                    self.pid.reset();
                    return ThermalAction::RestartInit;
                }
                ThermalAction::EmergencyShutdown
            }

            ThermalState::Sleep => {
                if max_temp >= dangerous {
                    self.state = ThermalState::DangerousShutdown;
                    return ThermalAction::EmergencyShutdown;
                }

                // THERMAL-3: route the sleep-state PWM through `safety_capped_pwm`
                // so the home-mining cap owns it like every other fan write. The
                // requested ~25 (≈20% for PSU cooling) is clamped down to the
                // profile's `fan_max_pwm` when the operator's quiet cap is lower
                // (e.g. the PWM-30 / quieter home profiles), instead of silently
                // exceeding it. This only ever LOWERS the value — a profile with a
                // cap ≥ 25 is unaffected.
                let pwm = safety_capped_pwm(self.profile.fan_max_pwm, None, 25);
                self.current_pwm = pwm;
                ThermalAction::SetFanPwm(pwm)
            }
        }
    }

    /// Enable (or refuse) immersion / hydro cooling mode from an
    /// [`ImmersionConfig`] (W8 parity gap: DCENT ❌/⚠️ vs LuxOS/VNish ✅).
    ///
    /// Immersion mode bypasses the air-fan RAMP behavior — immersion / hydro
    /// rigs have no chassis fans, cooling is external. It does NOT remove the
    /// thermal SAFETY net: die/chip temp is still monitored every tick and
    /// stale / sensor-failure / dangerous temp still fail closed by CUTTING
    /// HASH (`EmergencyShutdown`), never by blasting nonexistent fans.
    ///
    /// **SAFETY-CRITICAL: default-OFF, explicit opt-in, refuses on air-cooled.**
    /// The caller passes `platform_looks_air_cooled = true` when the running
    /// platform appears to be a normal air-cooled chassis. On such a platform
    /// the controller REFUSES to activate immersion (keeps fan management)
    /// unless `config.acknowledge_air_cooled_override` is explicitly set —
    /// silently bypassing fans on an air-cooled unit would cook the boards.
    ///
    /// Returns the [`ImmersionDecision`] so the daemon can surface it; the
    /// controller has already applied the matching state (and emitted the
    /// matching `tracing` warning) before returning. Idempotent — calling with
    /// a disabled config turns immersion back off.
    pub fn enable_immersion(
        &mut self,
        config: &ImmersionConfig,
        platform_looks_air_cooled: bool,
    ) -> ImmersionDecision {
        let decision = config.decide(platform_looks_air_cooled);
        self.immersion_active = decision.fans_bypassed();
        match decision {
            ImmersionDecision::Disabled => {
                // No log — this is the default air-cooled path on every unit.
            }
            ImmersionDecision::Activated => {
                tracing::warn!(
                    "IMMERSION MODE ACTIVE — air-fan management is BYPASSED (no chassis fans). \
                     The thermal SAFETY net stays intact: die/chip temp is still monitored and \
                     over-temp / stale-temp still CUT HASH. Ensure the external cooling loop \
                     (pump + dry-cooler / radiator) is running before mining."
                );
            }
            ImmersionDecision::ActivatedAirCooledOverride => {
                tracing::warn!(
                    "IMMERSION MODE ACTIVE on an AIR-COOLED-LOOKING platform via explicit \
                     acknowledge_air_cooled_override. Fan management is BYPASSED. This is \
                     CATASTROPHIC if this unit does NOT actually have an external cooling loop — \
                     the boards will cook with no airflow. The over-temp HASH-CUT safety net is \
                     still active, but it is the LAST line of defense, not the cooling strategy."
                );
            }
            ImmersionDecision::RefusedAirCooled => {
                tracing::warn!(
                    "IMMERSION MODE REQUESTED but REFUSED: this platform looks air-cooled and \
                     `acknowledge_air_cooled_override` was not set. Keeping normal fan management \
                     (fail-closed). Bypassing fans on an air-cooled unit would cook the boards. \
                     Set acknowledge_air_cooled_override=true only if you have an external cooling \
                     loop on this chassis."
                );
            }
        }
        decision
    }

    /// True iff immersion mode is active (fan ramp bypassed). The daemon uses
    /// this to SKIP the HAL fan write entirely on an immersion rig — the
    /// controller never commands fans while this is true.
    pub fn immersion_active(&self) -> bool {
        self.immersion_active
    }

    /// Set whether hardware fan tach is available.
    /// Call this after platform detection. When false, fan-failure detection
    /// is disabled and the controller relies on temperature thresholds only.
    pub fn set_tach_available(&mut self, available: bool) {
        self.tach_available = available;
        if !available {
            tracing::info!(
                "Thermal: fan tach unavailable — fan-failure detection disabled, \
                            relying on temperature thresholds for safety"
            );
        }
    }

    /// Get the current thermal state.
    pub fn state(&self) -> ThermalState {
        self.state
    }

    /// Enter sleep mode.
    pub fn enter_sleep(&mut self) {
        self.state = ThermalState::Sleep;
        self.pid.reset();
    }

    /// Exit sleep mode.
    pub fn exit_sleep(&mut self) {
        self.state = ThermalState::ColdStart;
        self.pid.reset();
    }

    /// Get the PID controller state for diagnostics.
    pub fn pid_state(&self) -> PidState {
        self.pid.state()
    }

    /// Check if temperature readings are stale (>30s old).
    ///
    /// If readings are stale, fans should be forced to maximum as a safety measure.
    /// Returns true if the last valid temperature reading is older than 30 seconds.
    pub fn is_temp_stale(&self) -> bool {
        self.last_temp_update.elapsed().as_secs() > 30
    }

    /// Get current fan PWM output.
    pub fn current_pwm(&self) -> u8 {
        self.current_pwm
    }

    /// Update PID parameters (hacker mode override).
    pub fn set_pid_params(&mut self, kp: f32, ki: f32, kd: f32) {
        self.pid.kp = clamp_pid_gain(kp);
        self.pid.ki = clamp_pid_gain(ki);
        self.pid.kd = clamp_pid_gain(kd);
    }
}

/// Actions the thermal controller recommends to the daemon.
#[derive(Debug, Clone)]
pub enum ThermalAction {
    /// Set fan PWM to this value.
    SetFanPwm(u8),

    /// Throttle frequency and set fan PWM.
    ThrottleAndFan { pwm: u8, freq_reduction_pct: u8 },

    /// Emergency shutdown: disable all hash boards, fans to max.
    EmergencyShutdown,

    /// Fan failure detected: disable boards, fans to max.
    FanFailure,

    /// Temperature has recovered, restart init sequence.
    RestartInit,
}

/// Reconcile the controller's `ThermalAction` with the Wave-E
/// `ThermalSupervisor`'s per-tick `SupervisorAction`s (RE-005 / Wave-G G1
/// E3b closure).
///
/// **Strongest-safety-wins.** The supervisor can only make the thermal
/// response *more* conservative (cut hash / power off a board / drive fans
/// up to the home cap); it can NEVER weaken the controller's existing
/// dangerous-temp / stale-temp / sensor-failure fail-closed floor. When the
/// supervisor is disabled (the caller passes an empty slice), this returns
/// the controller's action **byte-identical** — the disabled path has zero
/// behavioral delta.
///
/// Load-bearing:
/// - `RequestFansMax` maps to `SetFanPwm(fan_max_pwm)` — **never 255**
///   (quiet-home cap; the `a lab unit` contract). A board crossing panic
///   (`RequestBoardPowerOff`) or an all-board emergency
///   (`RequestEmergencyShutdown`) escalates to `EmergencyShutdown` (cut hash
///   before noise — the controller has no per-board granularity, so the
///   conservative whole-unit shutdown is the correct fail-closed mapping).
/// - `EmitFanFailure` is telemetry-only (a single stalled fan while
///   `min_fans` is still satisfied) — it does NOT escalate; the supervisor
///   raises `RequestEmergencyShutdown { FanPanic }` separately when working
///   fans drop below `min_fans`.
/// - `RequestFansCurve` / `RequestFansMin` / `RequestProfileStep*` /
///   `EmitSensorDropped` / `AttemptBoardRecovery` /
///   `EmitRecoveryBudgetExhausted` / `NoOp` are advisory or telemetry — they
///   never override the controller's decision.
///
/// VNish thresholds are NOT used here; the supervisor is configured from
/// DCENT_OS `[thermal.supervisor]` thresholds (Wave-F
/// `vnish_thresholds_not_used_as_dcentos_thermal_limits` contract).
pub fn reconcile_with_supervisor(
    controller_action: ThermalAction,
    supervisor_actions: &[crate::supervisor::SupervisorAction],
    fan_max_pwm: u8,
) -> ThermalAction {
    use crate::supervisor::SupervisorAction as SA;

    // Disabled supervisor (empty slice) → controller path is byte-identical.
    if supervisor_actions.is_empty() {
        return controller_action;
    }

    // The controller already commands the strongest response.
    if matches!(controller_action, ThermalAction::EmergencyShutdown) {
        return ThermalAction::EmergencyShutdown;
    }

    // Supervisor escalations that demand the strongest controller response.
    // The controller has no per-board power-off granularity, so a board
    // panic conservatively escalates to a whole-unit EmergencyShutdown
    // (cut hash before noise — fail-closed).
    let supervisor_demands_shutdown = supervisor_actions.iter().any(|a| {
        matches!(
            a,
            SA::RequestEmergencyShutdown { .. } | SA::RequestBoardPowerOff { .. }
        )
    });
    if supervisor_demands_shutdown {
        return ThermalAction::EmergencyShutdown;
    }

    // Controller fan-failure stays unless we escalated to shutdown above.
    if matches!(controller_action, ThermalAction::FanFailure) {
        return ThermalAction::FanFailure;
    }

    // Supervisor requests max cooling → cap at fan_max_pwm (NEVER 255). Only
    // override when it is stronger (more cooling) than the controller's
    // current fan command; preserve any frequency throttle the controller
    // already applied.
    let supervisor_wants_max_fans = supervisor_actions
        .iter()
        .any(|a| matches!(a, SA::RequestFansMax { .. }));
    if supervisor_wants_max_fans {
        let target = fan_max_pwm; // load-bearing: home cap, never 255
        return match controller_action {
            ThermalAction::SetFanPwm(pwm) if pwm >= target => ThermalAction::SetFanPwm(pwm),
            ThermalAction::SetFanPwm(_) => ThermalAction::SetFanPwm(target),
            ThermalAction::ThrottleAndFan {
                pwm,
                freq_reduction_pct,
            } if pwm >= target => ThermalAction::ThrottleAndFan {
                pwm,
                freq_reduction_pct,
            },
            ThermalAction::ThrottleAndFan {
                freq_reduction_pct, ..
            } => ThermalAction::ThrottleAndFan {
                pwm: target,
                freq_reduction_pct,
            },
            // RestartInit / (already-handled Emergency/FanFailure) — defer to
            // the supervisor's cooling request as a bare fan command.
            _ => ThermalAction::SetFanPwm(target),
        };
    }

    // All remaining supervisor actions are advisory / telemetry; they never
    // weaken the controller's decision.
    controller_action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile() -> ThermalProfile {
        ThermalProfile {
            target_temp_c: 60,
            hot_temp_c: 70,
            dangerous_temp_c: 80,
            fan_min_pwm: 10,
            fan_max_pwm: 30,
            ramp_delay_s: 300,
            hysteresis_c: 3,
        }
    }

    #[test]
    fn startup_missing_temps_force_max_fan_instead_of_shutdown() {
        let mut controller = ThermalController::new(test_profile());

        let action = controller.tick(&[], 1200);

        assert!(matches!(action, ThermalAction::SetFanPwm(30)));
        assert_eq!(controller.current_pwm(), 30);
        assert!(matches!(controller.state(), ThermalState::ColdStart));
    }

    // -- THERMAL-3: sleep-state PWM is owned by the safety cap, not a raw 25 --
    #[test]
    fn sleep_state_pwm_is_clamped_to_profile_cap() {
        // A quiet home profile caps fans below the historical hardcoded 25.
        // Sleep must respect that cap, not silently exceed it.
        let mut profile = test_profile();
        profile.fan_max_pwm = 20; // quieter-than-25 home cap
        let mut controller = ThermalController::new(profile);
        controller.enter_sleep();
        // Valid temp + healthy RPM so we reach the Sleep match arm (empty temps
        // / 0 RPM would trip the earlier startup-grace / fan-failure guards).
        let action = controller.tick(&[40.0], 1000);
        match action {
            ThermalAction::SetFanPwm(pwm) => {
                assert!(
                    pwm <= 20,
                    "sleep PWM {pwm} must not exceed the profile cap (20)"
                );
                assert_eq!(
                    controller.current_pwm(),
                    pwm,
                    "current_pwm must track the clamped sleep PWM"
                );
            }
            other => panic!("sleep tick should SetFanPwm, got {other:?}"),
        }
    }

    #[test]
    fn sleep_state_dangerous_temp_still_cuts_hash() {
        let profile = test_profile();
        let mut controller = ThermalController::new(profile);
        controller.enter_sleep();

        let action = controller.tick(&[85.0], 1000);

        assert!(matches!(action, ThermalAction::EmergencyShutdown));
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    #[test]
    fn sleep_state_cool_temp_keeps_pwm_no_regression() {
        let mut profile = test_profile();
        profile.fan_max_pwm = 64;
        let mut controller = ThermalController::new(profile);
        controller.enter_sleep();

        let action = controller.tick(&[40.0], 1000);

        assert!(matches!(action, ThermalAction::SetFanPwm(25)));
        assert_eq!(controller.current_pwm(), 25);
        assert!(matches!(controller.state(), ThermalState::Sleep));
    }

    #[test]
    fn safety_capped_pwm_never_exceeds_cap_or_the_home_ceiling() {
        use dcentrald_api_types::thermal_model::FanSafetyTrigger::{
            DaemonCrash, EmergencyShutdown, FanFailure, SensorError, StaleTemp,
        };
        // Load-bearing home-mining rule (rust-firmware.md): NO safety path (sensor
        // error, fan failure, emergency shutdown, daemon crash, stale temp) and NO
        // requested value may ever drive fan PWM above the configured profile cap —
        // and for ANY Home profile (cap <= 30) it may never exceed PWM 30. Property
        // test: exhaustive over every profile cap 0..=127, every trigger plus the
        // None (normal-request) path, and a dense set of requested values including
        // out-of-range 200/255. Pins `safety_capped_pwm` against a future edit that
        // drops the final `.min(profile_max_pwm)` or rounds up to a FanMode ceiling.
        let triggers = [
            None,
            Some(SensorError),
            Some(FanFailure),
            Some(EmergencyShutdown),
            Some(DaemonCrash),
            Some(StaleTemp),
        ];
        for cap in 0u8..=127 {
            for trig in triggers {
                for &requested in &[0u8, 1, 10, 25, 30, 31, 64, 100, 127, 200, 255] {
                    let out = safety_capped_pwm(cap, trig, requested);
                    assert!(
                        out <= cap,
                        "cap={cap} trig={trig:?} req={requested} -> {out} exceeds the configured cap"
                    );
                    if cap <= 30 {
                        assert!(
                            out <= 30,
                            "HOME cap={cap} trig={trig:?} req={requested} -> {out} exceeds the PWM 30 home ceiling"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn safe_pwm_clamp_orders_inverted_bounds_without_panic() {
        for min in 0u8..=u8::MAX {
            for max in 0u8..=u8::MAX {
                for requested in [0, 1, 10, 20, 30, 40, 64, 100, 127, 200, 255] {
                    let out = safe_pwm_clamp(requested, min, max);
                    assert!(
                        out >= min.min(max) && out <= min.max(max),
                        "req={requested} min={min} max={max} -> {out} outside ordered bounds"
                    );
                }
            }
        }
    }

    #[test]
    fn normal_mining_pwm_clamp_survives_corrupted_inverted_profile_bounds() {
        let mut controller = ThermalController::new(test_profile());

        let first = controller.tick(&[65.0], 1200);
        assert!(
            matches!(first, ThermalAction::SetFanPwm(_)),
            "first tick should enter NormalMining below hot threshold, got {first:?}"
        );
        assert!(matches!(controller.state(), ThermalState::NormalMining));

        // Simulate a future constructor bypass or in-memory corruption after
        // `new()` normalization. The point-of-use clamp must not panic.
        controller.profile.fan_min_pwm = 40;
        controller.profile.fan_max_pwm = 20;

        let action = controller.tick(&[65.0], 1205);
        let ThermalAction::SetFanPwm(pwm) = action else {
            panic!("corrupted NormalMining profile should still set a bounded PWM, got {action:?}");
        };
        assert!(
            (20..=40).contains(&pwm),
            "inverted profile bounds must be ordered before clamping, got {pwm}"
        );
    }

    #[test]
    fn thermal_profile_ladder_is_forced_monotonic_and_fail_safe() {
        // A valid ladder passes through unchanged.
        let good = ThermalProfile {
            target_temp_c: 55,
            hot_temp_c: 65,
            dangerous_temp_c: 75,
            ..Default::default()
        };
        let c = ThermalController::new(good);
        assert_eq!(
            (
                c.profile.target_temp_c,
                c.profile.hot_temp_c,
                c.profile.dangerous_temp_c
            ),
            (55, 65, 75),
            "a valid monotonic profile must not be altered"
        );

        // dangerous set absurdly high -> clamped to the 90C residential ceiling, so
        // the emergency cutoff can NEVER be configured away.
        let sky_high = ThermalProfile {
            target_temp_c: 55,
            hot_temp_c: 65,
            dangerous_temp_c: 200,
            ..Default::default()
        };
        let c = ThermalController::new(sky_high);
        assert!(
            c.profile.dangerous_temp_c <= 90,
            "dangerous_temp_c must be clamped to the residential ceiling, got {}",
            c.profile.dangerous_temp_c
        );

        // Fully inverted ladder (target >= hot >= dangerous) -> forced strictly
        // monotonic with every threshold under the (clamped) dangerous one, and the
        // PID setpoint tracking the CORRECTED target (not the bad 95).
        let inverted = ThermalProfile {
            target_temp_c: 95,
            hot_temp_c: 95,
            dangerous_temp_c: 60,
            ..Default::default()
        };
        let c = ThermalController::new(inverted);
        assert!(c.profile.dangerous_temp_c <= 90);
        assert!(
            c.profile.hot_temp_c < c.profile.dangerous_temp_c,
            "hot ({}) must be < dangerous ({})",
            c.profile.hot_temp_c,
            c.profile.dangerous_temp_c
        );
        assert!(
            c.profile.target_temp_c < c.profile.hot_temp_c,
            "target ({}) must be < hot ({})",
            c.profile.target_temp_c,
            c.profile.hot_temp_c
        );
        assert!(
            (c.pid.setpoint - c.profile.target_temp_c as f32).abs() < 0.5,
            "PID setpoint {} must track the corrected target {}",
            c.pid.setpoint,
            c.profile.target_temp_c
        );
    }

    #[test]
    fn pid_update_output_is_always_bounded_finite_and_fails_safe_on_bad_temps() {
        // Property: PidController::update must ALWAYS return a finite PWM in
        // [0, FAN_PWM_MAX] for any (temperature, setpoint), and a non-finite temp
        // (dead-sensor NaN / ±inf) must fail SAFE to FAN_PWM_MAX — never NaN, which
        // saturating-casts to 0 PWM downstream (fans OFF on a dead sensor). Sweeps a
        // wide finite temp grid + finite extremes, repeated to build the integrator
        // (anti-windup must keep it bounded), across several setpoints, plus the
        // three non-finite inputs.
        for &sp in &[0.0f32, 45.0, 60.0, 75.0, 1e6, -1e6] {
            let mut pid = PidController::new(sp);
            for &bad in &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let out = pid.update(bad);
                assert!(
                    out.is_finite(),
                    "sp {sp}: non-finite temp produced non-finite PWM"
                );
                assert_eq!(
                    out, FAN_PWM_MAX as f32,
                    "sp {sp}: non-finite temp must fail safe to max PWM"
                );
            }
            pid.reset();
            for _pass in 0..3 {
                let mut t = -200.0f32;
                while t <= 200.0 {
                    let out = pid.update(t);
                    assert!(out.is_finite(), "sp {sp} temp {t}: non-finite PWM");
                    assert!(
                        (0.0..=FAN_PWM_MAX as f32).contains(&out),
                        "sp {sp} temp {t} -> PWM {out} outside [0,{FAN_PWM_MAX}]"
                    );
                    t += 0.5;
                }
                for &t in &[f32::MIN, f32::MAX, 1e30f32, -1e30] {
                    let out = pid.update(t);
                    assert!(
                        out.is_finite() && (0.0..=FAN_PWM_MAX as f32).contains(&out),
                        "sp {sp} extreme temp {t} -> PWM {out}"
                    );
                }
            }
        }
    }

    #[test]
    fn set_pid_params_clamps_non_finite_and_extreme_gains() {
        let mut controller = ThermalController::new(test_profile());

        controller.set_pid_params(f32::NAN, f32::INFINITY, -1.0);
        let state = controller.pid_state();
        assert_eq!(state.kp, PID_GAIN_MIN);
        assert_eq!(state.ki, PID_GAIN_MIN);
        assert_eq!(state.kd, PID_GAIN_MIN);

        controller.set_pid_params(PID_GAIN_MAX * 2.0, 1.25, PID_GAIN_MAX);
        let state = controller.pid_state();
        assert_eq!(state.kp, PID_GAIN_MAX);
        assert_eq!(state.ki, 1.25);
        assert_eq!(state.kd, PID_GAIN_MAX);
    }

    #[test]
    fn pid_output_stays_bounded_with_maximum_custom_gains() {
        let mut controller = ThermalController::new(test_profile());
        controller.set_pid_params(PID_GAIN_MAX, PID_GAIN_MAX, PID_GAIN_MAX);

        for &temp in &[
            f32::MAX,
            f32::MAX / 2.0,
            -f32::MAX,
            -f32::MAX / 2.0,
            1.0e30,
            -1.0e30,
        ] {
            let out = controller.pid.update(temp);
            assert!(
                out.is_finite() && (0.0..=FAN_PWM_MAX as f32).contains(&out),
                "temp {temp} produced unbounded PID output {out}"
            );
        }
    }

    #[test]
    fn sleep_state_pwm_is_25_when_cap_allows() {
        // When the profile cap is >= 25 the sleep PWM stays at the ~20%-for-PSU
        // value (25), unchanged from the historical behavior.
        let mut profile = test_profile();
        profile.fan_max_pwm = 64; // Balanced — comfortably above 25
        let mut controller = ThermalController::new(profile);
        controller.enter_sleep();
        let action = controller.tick(&[40.0], 1000);
        assert!(matches!(action, ThermalAction::SetFanPwm(25)));
        assert_eq!(controller.current_pwm(), 25);
    }

    // -- Wave G G1: reconcile_with_supervisor (E3b closure) --

    use crate::supervisor::{SupervisorAction, ThermalReason};

    // 1. Disabled supervisor (empty actions) → controller action byte-identical.
    #[test]
    fn disabled_supervisor_returns_controller_action_byte_identical() {
        let a = reconcile_with_supervisor(ThermalAction::SetFanPwm(15), &[], 30);
        assert!(matches!(a, ThermalAction::SetFanPwm(15)));
        let b = reconcile_with_supervisor(
            ThermalAction::ThrottleAndFan {
                pwm: 22,
                freq_reduction_pct: 10,
            },
            &[],
            30,
        );
        assert!(matches!(
            b,
            ThermalAction::ThrottleAndFan {
                pwm: 22,
                freq_reduction_pct: 10
            }
        ));
    }

    // 2. Supervisor emergency overrides a benign controller fan command.
    #[test]
    fn supervisor_emergency_overrides_controller_fan_request() {
        let a = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(12),
            &[SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::ChipPanic,
            }],
            30,
        );
        assert!(matches!(a, ThermalAction::EmergencyShutdown));
    }

    // 3. Supervisor board power-off escalates to whole-unit shutdown (fail-closed).
    #[test]
    fn supervisor_board_power_off_escalates_to_shutdown() {
        let a = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(20),
            &[SupervisorAction::RequestBoardPowerOff {
                chain_id: 1,
                reason: ThermalReason::BoardPanic,
                recoverable: true,
            }],
            30,
        );
        assert!(matches!(a, ThermalAction::EmergencyShutdown));
    }

    // 4. LOAD-BEARING: RequestFansMax maps to fan_max_pwm, NEVER 255.
    #[test]
    fn request_fans_max_maps_to_fan_max_pwm_not_255() {
        let a = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(12),
            &[SupervisorAction::RequestFansMax {
                reason: ThermalReason::BoardHot,
            }],
            30,
        );
        assert!(matches!(a, ThermalAction::SetFanPwm(30)));
        // Even if a hypothetical caller passed a higher cap, the helper uses
        // exactly the supplied fan_max_pwm — it never invents 255.
        let b = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(8),
            &[SupervisorAction::RequestFansMax {
                reason: ThermalReason::ChipHot,
            }],
            45,
        );
        assert!(matches!(b, ThermalAction::SetFanPwm(45)));
    }

    // 5. RequestFansMax preserves a stronger controller fan command (no weakening).
    #[test]
    fn request_fans_max_does_not_weaken_stronger_controller_fan() {
        // Controller already commands a higher PWM than the cap (e.g. a
        // hacker-mode override) — the supervisor must not lower it.
        let a = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(60),
            &[SupervisorAction::RequestFansMax {
                reason: ThermalReason::BoardHot,
            }],
            30,
        );
        assert!(matches!(a, ThermalAction::SetFanPwm(60)));
    }

    // 6. RequestFansMax preserves an existing controller frequency throttle.
    #[test]
    fn request_fans_max_preserves_controller_throttle() {
        let a = reconcile_with_supervisor(
            ThermalAction::ThrottleAndFan {
                pwm: 10,
                freq_reduction_pct: 25,
            },
            &[SupervisorAction::RequestFansMax {
                reason: ThermalReason::ChipHot,
            }],
            30,
        );
        match a {
            ThermalAction::ThrottleAndFan {
                pwm,
                freq_reduction_pct,
            } => {
                assert_eq!(pwm, 30, "fan raised to cap");
                assert_eq!(freq_reduction_pct, 25, "throttle preserved");
            }
            other => panic!("expected ThrottleAndFan, got {:?}", other),
        }
    }

    // 7. Advisory/telemetry actions never weaken the controller's decision.
    #[test]
    fn advisory_actions_do_not_override_controller() {
        for action in [
            SupervisorAction::RequestFansCurve,
            SupervisorAction::RequestFansMin,
            SupervisorAction::RequestProfileStepUp,
            SupervisorAction::RequestProfileStepDown {
                reason: ThermalReason::BoardHot,
            },
            SupervisorAction::EmitFanFailure { fan_index: 1 },
            SupervisorAction::EmitSensorDropped {
                chain_id: 0,
                sensor_index: 2,
            },
            SupervisorAction::AttemptBoardRecovery {
                chain_id: 0,
                attempt: 1,
            },
            SupervisorAction::EmitRecoveryBudgetExhausted { chain_id: 0 },
            SupervisorAction::NoOp,
        ] {
            let a = reconcile_with_supervisor(ThermalAction::SetFanPwm(14), &[action.clone()], 30);
            assert!(
                matches!(a, ThermalAction::SetFanPwm(14)),
                "advisory action {:?} must not override controller",
                action
            );
        }
    }

    // 8. EmitFanFailure (single stalled fan, min_fans still met) does NOT
    //    escalate to ThermalAction::FanFailure — only RequestEmergencyShutdown
    //    { FanPanic } does.
    #[test]
    fn emit_fan_failure_is_telemetry_only() {
        let a = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(18),
            &[SupervisorAction::EmitFanFailure { fan_index: 0 }],
            30,
        );
        assert!(matches!(a, ThermalAction::SetFanPwm(18)));
        // But a true fan panic escalates.
        let b = reconcile_with_supervisor(
            ThermalAction::SetFanPwm(18),
            &[SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::FanPanic,
            }],
            30,
        );
        assert!(matches!(b, ThermalAction::EmergencyShutdown));
    }

    // 9. Controller emergency-shutdown is never weakened by the supervisor.
    #[test]
    fn controller_emergency_is_never_weakened() {
        let a = reconcile_with_supervisor(
            ThermalAction::EmergencyShutdown,
            &[SupervisorAction::RequestFansMin],
            30,
        );
        assert!(matches!(a, ThermalAction::EmergencyShutdown));
    }

    #[test]
    fn missing_temps_after_first_valid_sample_trigger_shutdown() {
        let mut controller = ThermalController::new(test_profile());

        let _ = controller.tick(&[55.0], 1200);
        let action = controller.tick(&[], 1200);

        assert!(matches!(action, ThermalAction::EmergencyShutdown));
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    #[test]
    fn hot_throttle_respects_profile_fan_cap() {
        let mut controller = ThermalController::new(test_profile());

        let action = controller.tick(&[72.0], 1200);

        assert!(matches!(
            action,
            ThermalAction::ThrottleAndFan {
                pwm: 30,
                freq_reduction_pct: 10
            }
        ));
        assert_eq!(controller.current_pwm(), 30);
        assert!(matches!(controller.state(), ThermalState::HotThrottle));
    }

    // -- Immersion / hydro cooling mode (W8 parity gap closure) --
    //
    // Three load-bearing guarantees:
    //   1. immersion-OFF  → byte-identical fan behavior (default; no delta).
    //   2. immersion-ON   → still cuts hash on dangerous/stale temp (never hot).
    //   3. immersion-ON   → never commands fans (PWM stays 0).

    use crate::immersion::{ImmersionConfig, ImmersionDecision};

    /// A genuine immersion / hydro rig: not air-cooled.
    const NOT_AIR_COOLED: bool = false;
    /// An air-cooled-looking chassis (the dangerous case to refuse).
    const AIR_COOLED: bool = true;

    fn immersion_on() -> ImmersionConfig {
        ImmersionConfig {
            enabled: true,
            acknowledge_air_cooled_override: false,
        }
    }

    // 1. immersion-OFF (default) → tick output is byte-identical across a
    //    representative sequence (cold/normal/hot). We mirror the exact
    //    sequence on two controllers — one with immersion never enabled, one
    //    with immersion enabled-then-the-config-is-disabled — and assert the
    //    actions match. The default controller never calls enable_immersion, so
    //    immersion_active() must be false and the post-processor is never run.
    #[test]
    fn immersion_off_is_byte_identical() {
        let mut baseline = ThermalController::new(test_profile());
        let mut never_enabled = ThermalController::new(test_profile());
        // never_enabled stays default — assert the field is off out of the box.
        assert!(!never_enabled.immersion_active());

        // A disabled ImmersionConfig must NOT activate, on either platform.
        let mut disabled_cfg = ThermalController::new(test_profile());
        let dec_air = disabled_cfg.enable_immersion(&ImmersionConfig::default(), AIR_COOLED);
        let dec_liquid = disabled_cfg.enable_immersion(&ImmersionConfig::default(), NOT_AIR_COOLED);
        assert_eq!(dec_air, ImmersionDecision::Disabled);
        assert_eq!(dec_liquid, ImmersionDecision::Disabled);
        assert!(!disabled_cfg.immersion_active());

        // Same input sequence → identical actions + identical current_pwm on
        // all three (immersion-off) controllers.
        let seq = [50.0_f32, 56.0, 72.0, 58.0, 50.0];
        for &t in &seq {
            let a = baseline.tick(&[t], 1200);
            let b = never_enabled.tick(&[t], 1200);
            let c = disabled_cfg.tick(&[t], 1200);
            assert_eq!(
                format!("{a:?}"),
                format!("{b:?}"),
                "immersion-off (never enabled) must be byte-identical to baseline at temp {t}"
            );
            assert_eq!(
                format!("{a:?}"),
                format!("{c:?}"),
                "disabled ImmersionConfig must be byte-identical to baseline at temp {t}"
            );
            assert_eq!(baseline.current_pwm(), never_enabled.current_pwm());
            assert_eq!(baseline.current_pwm(), disabled_cfg.current_pwm());
        }
    }

    // 2. immersion-ON → dangerous temp still triggers EmergencyShutdown
    //    (cut hash, never run hot). The hash-cut decision is NOT weakened.
    #[test]
    fn immersion_on_still_cuts_hash_on_dangerous_temp() {
        let mut controller = ThermalController::new(test_profile());
        assert_eq!(
            controller.enable_immersion(&immersion_on(), NOT_AIR_COOLED),
            ImmersionDecision::Activated
        );
        assert!(controller.immersion_active());

        // dangerous_temp_c = 80 in test_profile. 85 °C must cut hash.
        let action = controller.tick(&[85.0], 0);
        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "immersion must still cut hash on dangerous temp, got {action:?}"
        );
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
        // And it must NOT have commanded a fan to do it.
        assert_eq!(
            controller.current_pwm(),
            0,
            "immersion must never command a fan, even on the hash-cut path"
        );
    }

    // 2b. immersion-ON → stale temperature still fails closed (EmergencyShutdown).
    #[test]
    fn immersion_on_still_fails_closed_on_stale_temp() {
        let mut controller = ThermalController::new(test_profile());
        controller.enable_immersion(&immersion_on(), NOT_AIR_COOLED);

        // Seed a valid sample so we are past the startup grace.
        let _ = controller.tick(&[50.0], 1200);
        // After a real sample, an empty temps reading is a hard stop.
        let action = controller.tick(&[], 1200);
        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "immersion must fail closed (cut hash) on lost temperature, got {action:?}"
        );
        assert_eq!(
            controller.current_pwm(),
            0,
            "no fan commanded on fail-closed"
        );
    }

    // 2c. immersion-ON → a hot board still gets a FREQUENCY THROTTLE (cut heat
    //     at the source) but the fan portion is zeroed — never a fan ramp.
    #[test]
    fn immersion_on_keeps_freq_throttle_but_zeroes_fan() {
        let mut controller = ThermalController::new(test_profile());
        controller.enable_immersion(&immersion_on(), NOT_AIR_COOLED);

        // hot_temp_c = 70, dangerous = 80. 72 °C → HotThrottle.
        let action = controller.tick(&[72.0], 0);
        match action {
            ThermalAction::ThrottleAndFan {
                pwm,
                freq_reduction_pct,
            } => {
                assert_eq!(pwm, 0, "immersion must NOT ramp fans on a hot board");
                assert_eq!(
                    freq_reduction_pct, 10,
                    "the frequency throttle (heat-at-source cut) must be preserved"
                );
            }
            other => panic!("expected ThrottleAndFan with pwm=0, got {other:?}"),
        }
        assert_eq!(controller.current_pwm(), 0);
        assert!(matches!(controller.state(), ThermalState::HotThrottle));
    }

    // 3. immersion-ON → across a full cold→normal→hot→cool sweep, the
    //    controller NEVER commands a non-zero fan PWM. Any fan-PWM-carrying
    //    action carries 0, and current_pwm stays 0 the whole time.
    #[test]
    fn immersion_on_never_commands_fans() {
        let mut controller = ThermalController::new(test_profile());
        controller.enable_immersion(&immersion_on(), NOT_AIR_COOLED);

        // Includes cold (50), normal (58), hot (72 → throttle), and a benign
        // tach=0 reading (which on an air-cooled unit would latch a fan-failure
        // blast — here it must NOT, because there are no fans).
        let seq: &[(f32, u32)] = &[
            (50.0, 0),
            (58.0, 0),
            (62.0, 0),
            (72.0, 0),
            (58.0, 0),
            (50.0, 0),
        ];
        for &(t, rpm) in seq {
            let action = controller.tick(&[t], rpm);
            let commanded_pwm = match action {
                ThermalAction::SetFanPwm(p) => Some(p),
                ThermalAction::ThrottleAndFan { pwm, .. } => Some(pwm),
                ThermalAction::EmergencyShutdown
                | ThermalAction::FanFailure
                | ThermalAction::RestartInit => None,
            };
            if let Some(p) = commanded_pwm {
                assert_eq!(
                    p, 0,
                    "immersion commanded fan PWM {p} at temp {t} (rpm {rpm}) — must be 0"
                );
            }
            assert_eq!(
                controller.current_pwm(),
                0,
                "immersion current_pwm must stay 0 at temp {t}"
            );
            // tach=0 must NOT escalate to a fan-failure blast in immersion.
            assert!(
                !matches!(action, ThermalAction::FanFailure),
                "tach=0 must not trip FanFailure in immersion (no fans), temp {t}"
            );
        }
    }

    // 3b. SAFETY: immersion REFUSES on an air-cooled platform without the
    //     explicit acknowledgement — fan management is KEPT (fail-closed).
    #[test]
    fn immersion_refuses_on_air_cooled_without_ack() {
        let mut controller = ThermalController::new(test_profile());
        let decision = controller.enable_immersion(&immersion_on(), AIR_COOLED);
        assert_eq!(decision, ImmersionDecision::RefusedAirCooled);
        assert!(
            !controller.immersion_active(),
            "immersion must NOT activate on an air-cooled unit without acknowledgement"
        );

        // Because it refused, normal fan management is intact: a hot board
        // ramps fans to the profile cap (30), exactly like the non-immersion path.
        let action = controller.tick(&[72.0], 1200);
        assert!(matches!(
            action,
            ThermalAction::ThrottleAndFan {
                pwm: 30,
                freq_reduction_pct: 10
            }
        ));
        assert_eq!(controller.current_pwm(), 30);
    }

    // 3c. SAFETY: immersion DOES activate on an air-cooled platform WITH the
    //     explicit acknowledgement (operator owns an external loop on an air
    //     chassis) — and then never commands fans.
    #[test]
    fn immersion_activates_on_air_cooled_with_explicit_ack() {
        let mut controller = ThermalController::new(test_profile());
        let cfg = ImmersionConfig {
            enabled: true,
            acknowledge_air_cooled_override: true,
        };
        let decision = controller.enable_immersion(&cfg, AIR_COOLED);
        assert_eq!(decision, ImmersionDecision::ActivatedAirCooledOverride);
        assert!(controller.immersion_active());

        // Still bypasses fans...
        let action = controller.tick(&[72.0], 0);
        match action {
            ThermalAction::ThrottleAndFan { pwm, .. } => assert_eq!(pwm, 0),
            other => panic!("expected ThrottleAndFan pwm=0, got {other:?}"),
        }
        assert_eq!(controller.current_pwm(), 0);

        // ...and still cuts hash on dangerous temp (safety net intact even on
        // the air-cooled override path).
        let action = controller.tick(&[85.0], 0);
        assert!(matches!(action, ThermalAction::EmergencyShutdown));
        assert_eq!(controller.current_pwm(), 0);
    }

    // 3d. enable_immersion is idempotent / reversible: re-calling with a
    //     disabled config turns immersion back off and restores fan ramp.
    #[test]
    fn immersion_can_be_turned_back_off() {
        let mut controller = ThermalController::new(test_profile());
        controller.enable_immersion(&immersion_on(), NOT_AIR_COOLED);
        assert!(controller.immersion_active());

        controller.enable_immersion(&ImmersionConfig::default(), NOT_AIR_COOLED);
        assert!(!controller.immersion_active());

        // Fan ramp restored: a hot board ramps to the profile cap again.
        let action = controller.tick(&[72.0], 1200);
        assert!(matches!(
            action,
            ThermalAction::ThrottleAndFan {
                pwm: 30,
                freq_reduction_pct: 10
            }
        ));
        assert_eq!(controller.current_pwm(), 30);
    }

    // -- SAFETY: non-finite (NaN/±Inf) temperatures must fail CLOSED --
    //
    // Regression guard for the fail-open hole where an all-NaN/garbage `temps`
    // slice collapsed (via `f32::max`) to NEG_INFINITY → read as COLD → idled
    // the FSM into ColdStart + fan_min instead of shutting down. A dead/garbage
    // sensor decode must be identical risk to NO sample: a hard EmergencyShutdown.

    // A fully non-finite single-sensor sample fails closed exactly like empty
    // temps (same state set + same EmergencyShutdown return).
    #[test]
    fn all_nan_temps_fail_closed_like_empty() {
        let mut controller = ThermalController::new(test_profile());

        let action = controller.tick(&[f32::NAN], 1200);

        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "an all-NaN sample must EmergencyShutdown (same as empty), got {action:?}"
        );
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    // A partially-finite sample drops the NaN but STILL sees the real hot
    // reading — the NaN must never mask a genuine over-temp.
    #[test]
    fn nan_mixed_with_hot_temp_still_trips_at_real_value() {
        let mut controller = ThermalController::new(test_profile());

        // dangerous_temp_c = 80 in test_profile; 95 °C is a hard over-temp.
        let action = controller.tick(&[f32::NAN, 95.0], 1200);

        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "NaN must be dropped and the real 95 °C must still trip shutdown, got {action:?}"
        );
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    // +Inf is non-finite → fails closed.
    #[test]
    fn positive_infinity_temp_fails_closed() {
        let mut controller = ThermalController::new(test_profile());

        let action = controller.tick(&[f32::INFINITY], 1200);

        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "an all-+Inf sample must EmergencyShutdown, got {action:?}"
        );
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    // -Inf is the EXACT old fail-open vector (NEG_INFINITY read as cold). It
    // must now fail closed, not idle into ColdStart + fan_min.
    #[test]
    fn negative_infinity_temp_fails_closed_not_cold() {
        let mut controller = ThermalController::new(test_profile());

        let action = controller.tick(&[f32::NEG_INFINITY], 1200);

        assert!(
            matches!(action, ThermalAction::EmergencyShutdown),
            "an all--Inf sample must EmergencyShutdown (was the fail-open hole), got {action:?}"
        );
        assert!(matches!(
            controller.state(),
            ThermalState::DangerousShutdown
        ));
    }

    // NO REGRESSION: a normal finite COLD slice still idles into ColdStart at
    // fan_min — the non-finite filter must not perturb the healthy path.
    #[test]
    fn finite_cold_temps_still_coldstart_min_pwm_no_regression() {
        let mut controller = ThermalController::new(test_profile());

        // 50 °C < target(60) < hot(70) < dangerous(80) → ColdStart, fan_min(10).
        let action = controller.tick(&[50.0], 1200);

        assert!(
            matches!(action, ThermalAction::SetFanPwm(10)),
            "finite cold slice must SetFanPwm(fan_min=10), got {action:?}"
        );
        assert_eq!(controller.current_pwm(), 10);
        assert!(matches!(controller.state(), ThermalState::ColdStart));
    }

    // NO REGRESSION: a normal finite HOT slice still trips throttle/shutdown.
    #[test]
    fn finite_hot_temps_still_trip_no_regression() {
        let mut controller = ThermalController::new(test_profile());

        // 72 °C ≥ hot(70), < dangerous(80) → HotThrottle (fan cap 30, 10% cut).
        let action = controller.tick(&[72.0], 1200);
        assert!(matches!(
            action,
            ThermalAction::ThrottleAndFan {
                pwm: 30,
                freq_reduction_pct: 10
            }
        ));
        assert!(matches!(controller.state(), ThermalState::HotThrottle));
    }
}
