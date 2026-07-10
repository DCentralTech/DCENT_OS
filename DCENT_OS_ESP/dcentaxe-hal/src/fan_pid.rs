// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — Fan PID + EMA filter
//
// Ports the fan-controller tuning from ESP-Miner PR #1640 (commit 6718e6e):
// EMA-smoothed input, PID output with I-term windup guard, clamped output,
// PID only enabled after the first valid temperature reading.
//
// The upstream ESP-Miner task runs at 100 ms. Our main loop currently calls
// this at ~5 s. The `dt_ms` argument makes the integrator time-correct across
// cadences; the default constants below are tuned for 100 ms but remain
// stable at multi-second intervals thanks to `INTEGRAL_CLAMP`.

/// Default PID gains — match ESP-Miner PR #1640 / commit 6718e6e.
pub const DEFAULT_KP: f32 = 5.0;
/// Per-second integral gain. Multiplied by `dt_ms / 1000` on each tick.
pub const DEFAULT_KI: f32 = 0.1;
pub const DEFAULT_KD: f32 = 2.0;

/// EMA smoothing factor for temperature input (α). 0.2 = new sample = 20 %,
/// previous filtered = 80 %. Matches ESP-Miner `#define EMA_ALPHA 0.2f`.
pub const EMA_ALPHA: f32 = 0.2;

/// Integrator clamp — prevents windup when the fan is already saturated
/// (e.g. running at 100 % during a long overheat).
pub const INTEGRAL_CLAMP: f32 = 40.0;

/// HALT-9: low-pass smoothing factor (α) for the *derivative* term. The PID
/// gains above are ported verbatim from ESP-Miner's 100 ms loop, but our loop
/// runs at ~5 s, so a single noisy temperature sample (e.g. a sensor glitch
/// that moves `filt_temp` several degrees in one tick) injects a large
/// derivative kick (`Δerror/dt × Kd`) that audibly surges the fan on a quiet
/// home BitAxe. We filter the derivative with its own EMA before applying Kd:
/// `0.3` = new sample 30 %, previous 70 %. This is purely additive smoothing —
/// it cannot lower the output below `min_pct` or above `max_pct` (the clamp at
/// `update()` is unchanged) and converges to the same steady-state derivative
/// (which is ~0 in thermal equilibrium), so it never weakens the cooling
/// response, it only de-noises the transient. A value of `1.0` would restore
/// the exact pre-HALT-9 unfiltered behavior.
pub const DERIVATIVE_EMA_ALPHA: f32 = 0.3;

/// HALT-9 (pure, host-testable): apply the derivative-term low-pass.
///
/// Returns the new filtered derivative given the raw instantaneous derivative
/// and the previous filtered derivative. On the first sample (`prev` is NaN)
/// the raw value passes through unfiltered so the controller is not artificially
/// damped at startup. Non-finite `raw` (e.g. division by a zero `dt`) is treated
/// as "no new derivative information" and the previous filtered value is held.
///
/// This is the only place the derivative smoothing math lives, so the
/// esp-idf-gated `FanPid::update()` and the host tests can never disagree.
#[inline]
pub fn filter_derivative(prev_filt_deriv: f32, raw_deriv: f32, alpha: f32) -> f32 {
    if !raw_deriv.is_finite() {
        // No usable derivative this tick — hold the previous filtered value
        // (or 0.0 if we never had one), never propagate NaN/Inf into the PID.
        return if prev_filt_deriv.is_finite() {
            prev_filt_deriv
        } else {
            0.0
        };
    }
    if !prev_filt_deriv.is_finite() {
        // First valid derivative: seed the filter with it (no startup damping).
        return raw_deriv;
    }
    alpha * raw_deriv + (1.0 - alpha) * prev_filt_deriv
}

/// Single-channel PID controller with EMA-filtered input.
///
/// Units are in "percent points" (output) and "degrees Celsius" (input).
/// The error sign convention matches ESP-Miner: `error = measured - setpoint`,
/// so a *positive* error (temperature above target) drives the output *up*.
#[derive(Debug, Clone)]
pub struct FanPid {
    pub kp: f32,
    pub ki: f32,
    pub kd: f32,
    /// EMA-filtered measured temperature. NaN until the first sample arrives.
    filt_temp: f32,
    integral: f32,
    prev_error: f32,
    /// HALT-9: EMA-filtered derivative term. NaN until the first derivative is
    /// computed, after which it is smoothed by `DERIVATIVE_EMA_ALPHA` so a
    /// single noisy temperature tick cannot spike the fan command.
    filt_deriv: f32,
    /// Last output, in percent (0–100). Used by `current_output()` for telemetry.
    last_output: f32,
    /// Whether the controller has seen its first valid temperature (upstream
    /// gates PID activation on this to avoid a huge derivative spike at boot).
    primed: bool,
}

impl Default for FanPid {
    fn default() -> Self {
        Self::new(DEFAULT_KP, DEFAULT_KI, DEFAULT_KD)
    }
}

impl FanPid {
    pub fn new(kp: f32, ki: f32, kd: f32) -> Self {
        Self {
            kp,
            ki,
            kd,
            filt_temp: f32::NAN,
            integral: 0.0,
            prev_error: 0.0,
            filt_deriv: f32::NAN,
            last_output: 30.0,
            primed: false,
        }
    }

    /// Reset integrator + derivative state without clearing the EMA filter.
    /// Call when the operating point changes (e.g., user edits target) to
    /// avoid carrying a stale integral across the discontinuity.
    pub fn reset_dynamic(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.filt_deriv = f32::NAN;
    }

    /// Hard reset — clears everything. Call on mode changes (auto ↔ manual).
    pub fn reset(&mut self) {
        self.filt_temp = f32::NAN;
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.filt_deriv = f32::NAN;
        self.primed = false;
    }

    /// Feed a new temperature sample through the EMA filter. Public because
    /// callers may want to read `filtered_temp()` independently of the PID
    /// update cadence.
    pub fn ingest(&mut self, temp_c: f32) {
        if !temp_c.is_finite() {
            return;
        }
        if self.filt_temp.is_nan() {
            self.filt_temp = temp_c;
        } else {
            self.filt_temp = EMA_ALPHA * temp_c + (1.0 - EMA_ALPHA) * self.filt_temp;
        }
        self.primed = true;
    }

    /// Current EMA-filtered temperature (NaN if no sample has been ingested).
    pub fn filtered_temp(&self) -> f32 {
        self.filt_temp
    }

    /// Whether the filter has seen at least one valid sample.
    pub fn primed(&self) -> bool {
        self.primed
    }

    /// Compute the next fan-speed command, clamped to `[min_pct, max_pct]`.
    ///
    /// Returns `None` if no valid temperature has been ingested — callers
    /// should keep the current fan speed until the sensor comes online,
    /// matching ESP-Miner's "PID enabled only after first valid temp" guard.
    pub fn update(
        &mut self,
        setpoint_c: f32,
        dt_ms: u32,
        min_pct: f32,
        max_pct: f32,
    ) -> Option<f32> {
        if !self.primed || !self.filt_temp.is_finite() {
            return None;
        }
        let dt_s = (dt_ms as f32) / 1000.0;
        let error = self.filt_temp - setpoint_c;

        // Accumulate integral with anti-windup clamp.
        self.integral = (self.integral + error * dt_s).clamp(-INTEGRAL_CLAMP, INTEGRAL_CLAMP);

        // Derivative on error. HALT-9: at the ~5 s production cadence (vs the
        // 100 ms loop these gains were tuned for) a single noisy temperature
        // sample injects a large instantaneous derivative, so we low-pass the
        // derivative with its own EMA before applying Kd. Pure, host-tested.
        let raw_derivative = if dt_s > 0.0 {
            (error - self.prev_error) / dt_s
        } else {
            0.0
        };
        self.prev_error = error;
        self.filt_deriv = filter_derivative(self.filt_deriv, raw_derivative, DERIVATIVE_EMA_ALPHA);
        let derivative = self.filt_deriv;

        // Baseline output at the low end of the range, let the PID push it up.
        let raw = min_pct + self.kp * error + self.ki * self.integral + self.kd * derivative;
        let clamped = raw.clamp(min_pct, max_pct);
        self.last_output = clamped;
        Some(clamped)
    }

    pub fn current_output(&self) -> f32 {
        self.last_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_until_primed() {
        let mut pid = FanPid::default();
        assert!(pid.update(60.0, 100, 30.0, 100.0).is_none());
        pid.ingest(55.0);
        assert!(pid.update(60.0, 100, 30.0, 100.0).is_some());
    }

    #[test]
    fn below_target_drives_minimum() {
        let mut pid = FanPid::default();
        // Warm the EMA up at a cool temperature.
        for _ in 0..20 {
            pid.ingest(40.0);
        }
        let out = pid.update(60.0, 100, 30.0, 100.0).unwrap();
        assert_eq!(out, 30.0, "cool temp should clamp at the floor");
    }

    #[test]
    fn above_target_ramps_up() {
        let mut pid = FanPid::default();
        for _ in 0..20 {
            pid.ingest(75.0);
        }
        let out = pid.update(60.0, 100, 30.0, 100.0).unwrap();
        // 15 °C over target × Kp=5 + integrator offset ≫ min — should saturate high.
        assert!(out > 80.0, "hot temp should saturate fan high, got {}", out);
    }

    #[test]
    fn integrator_clamp_prevents_windup() {
        let mut pid = FanPid::default();
        pid.ingest(100.0);
        for _ in 0..1000 {
            pid.update(60.0, 1000, 30.0, 100.0);
        }
        assert!(pid.integral.abs() <= INTEGRAL_CLAMP);
    }

    // ── HALT-9: pure derivative low-pass ─────────────────────────────────────
    #[test]
    fn filter_derivative_seeds_then_smooths() {
        // First valid sample passes through (no startup damping).
        let d0 = filter_derivative(f32::NAN, 4.0, DERIVATIVE_EMA_ALPHA);
        assert_eq!(d0, 4.0);
        // Second sample is blended: 0.3*0 + 0.7*4 = 2.8 (attenuated, not full).
        let d1 = filter_derivative(d0, 0.0, DERIVATIVE_EMA_ALPHA);
        assert!((d1 - 2.8).abs() < 1e-5, "expected 2.8, got {d1}");
    }

    #[test]
    fn filter_derivative_holds_on_nonfinite_raw() {
        // A non-finite raw derivative (e.g. dt==0 division) holds the prior value.
        assert_eq!(filter_derivative(1.5, f32::NAN, DERIVATIVE_EMA_ALPHA), 1.5);
        assert_eq!(
            filter_derivative(1.5, f32::INFINITY, DERIVATIVE_EMA_ALPHA),
            1.5
        );
        // With no prior value, a non-finite raw collapses to 0.0 (fail-benign).
        assert_eq!(
            filter_derivative(f32::NAN, f32::NAN, DERIVATIVE_EMA_ALPHA),
            0.0
        );
    }

    #[test]
    fn alpha_one_restores_unfiltered_behavior() {
        // α = 1.0 must reproduce the exact raw derivative every tick.
        let mut prev = f32::NAN;
        for raw in [3.0_f32, -2.0, 5.5, 0.0] {
            let out = filter_derivative(prev, raw, 1.0);
            assert_eq!(out, raw);
            prev = out;
        }
    }

    #[test]
    fn derivative_filter_attenuates_single_tick_glitch() {
        // A controller warmed NEAR the setpoint (so the P/I terms are small and
        // the derivative actually influences the output) sees a one-tick hot
        // glitch on its SECOND derivative sample. The filtered derivative must
        // blunt the fan kick vs the unfiltered path, while never dropping below
        // min_pct or exceeding max_pct.
        //
        // Warm at 58 C with setpoint 60 C: steady error ≈ -2, so the baseline
        // output hovers just above the 30% floor and a derivative kick is
        // observable rather than swamped by a large negative P-term.
        let mut pid = FanPid::default();
        for _ in 0..40 {
            pid.ingest(58.0);
        }
        // Establish a NON-NaN filtered derivative first (steady state ≈ 0), so
        // the glitch lands on the *smoothed* path, not the first-sample seed.
        let _ = pid.update(60.0, 5000, 30.0, 100.0).unwrap();
        let baseline = pid.update(60.0, 5000, 30.0, 100.0).unwrap();

        // One glitchy hot sample moves filt_temp up by 0.2*(72-58)=2.8 C.
        pid.ingest(72.0);
        let after_glitch = pid.update(60.0, 5000, 30.0, 100.0).unwrap();

        // Clamps always hold; cooling is never weakened (output does not drop).
        assert!(after_glitch >= 30.0, "must never fall below min_pct");
        assert!(after_glitch <= 100.0, "must never exceed max_pct");
        assert!(
            after_glitch >= baseline,
            "a hot glitch must not LOWER the fan command (cooling never weakened)"
        );

        // Direct proof the smoothing attenuates: on a second derivative sample,
        // the EMA-filtered derivative is strictly smaller in magnitude than the
        // raw (α=1.0) derivative for the same kick. Steady-state prev ≈ 0.
        let raw_kick = filter_derivative(0.0, 10.0, 1.0); // unfiltered == 10.0
        let filtered_kick = filter_derivative(0.0, 10.0, DERIVATIVE_EMA_ALPHA);
        assert!(
            filtered_kick < raw_kick,
            "filtered derivative ({filtered_kick}) must be gentler than raw ({raw_kick})"
        );
        // And it converges to the same steady value (no permanent attenuation of
        // a sustained gradient), so the cooling response is preserved long-term.
        let mut d = 0.0_f32;
        for _ in 0..200 {
            d = filter_derivative(d, 10.0, DERIVATIVE_EMA_ALPHA);
        }
        assert!(
            (d - 10.0).abs() < 1e-3,
            "must converge to the true derivative"
        );
    }
}
