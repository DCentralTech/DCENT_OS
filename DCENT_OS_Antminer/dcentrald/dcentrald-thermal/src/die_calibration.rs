//! Per-chip die-temperature calibration (R-13, BM1362 / am2-s19jpro-zynq).
//!
//! # Why this exists
//!
//! LuxOS calibrates each chip's on-die temperature ADC against an ABSOLUTE PCB
//! temperature sensor at a cold/known-baseline startup sample, then applies a
//! per-chip offset for the rest of the run. DCENT_OS historically reported the
//! RAW die-ADC reading (`soc_die_fallback` on `a lab unit`), which can be biased by a
//! per-chip ADC offset. This module closes that gap — as an EXPLICIT,
//! default-OFF opt-in with a hard fail-safe.
//!
//! # SAFETY-CRITICAL contract (do not regress)
//!
//! A WRONG calibration is worse than NO calibration: it can hide a real
//! over-temperature. So the safety posture is asymmetric — **over-reporting is
//! safe, under-reporting is dangerous** — and is baked into every path here:
//!
//! - **Default-OFF.** With `enabled == false` this module is a pure raw
//!   passthrough: `apply_*` returns the raw reading byte-for-byte. There is
//!   zero behavioral delta on every existing unit unless the operator opts in.
//! - **Fail-safe to RAW.** If the baseline sample is missing, the reference is
//!   implausible, the unit was not actually cold, an offset is out of band, or
//!   the spread across chips is implausible, the baseline is REJECTED and no
//!   calibration is ever applied (`apply_*` stays a raw passthrough).
//! - **Never report BELOW raw.** Even with a captured baseline, the
//!   safety-facing calibrated temperature is guaranteed `>= raw`. A calibration
//!   that would push the reading below raw beyond a tiny epsilon is rejected for
//!   that reading (falls back to raw); a slightly-negative correction is floored
//!   at raw. This guarantees the thermal supervisor can only ever trip EARLIER
//!   than it would on the raw reading — never later. Calibration can therefore
//!   never suppress an over-temp trip.
//! - **The XADC die-temp fallback is untouched.** This module only transforms a
//!   die reading the supervisor already has; it never removes the die source,
//!   never triggers a shutdown, and never runs when the raw value is non-finite
//!   (it passes non-finite through so the caller's existing empty/NaN
//!   fail-closed logic still fires).
//!
//! Mirrors the [`crate::immersion::ImmersionConfig`] /
//! `ThermalSupervisorConfig` opt-in pattern (`enabled: bool`,
//! `#[serde(default)]`, `Default` = off) so the daemon-side `ThermalConfig` can
//! embed `[thermal.die_temp_calibration]` the same way. The daemon-side wiring
//! (reading this config, an env override, capturing the baseline at the cold
//! pre-stratum poll, and applying it to the XADC die read) is a separate-crate
//! change; this crate owns the pure policy + the fail-safe.

use serde::{Deserialize, Serialize};

/// Die-temperature calibration configuration.
///
/// **Default-OFF.** When `enabled == false` the calibration is a pure raw
/// passthrough on every path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DieCalibrationConfig {
    /// **Default false.** Explicit operator opt-in. Conceptually the
    /// `die_temp_calibration_enabled` gate. With this false, [`DieCalibration`]
    /// never captures a baseline and `apply_*` returns the raw reading.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum trusted per-chip |offset| (celsius). A captured offset whose
    /// magnitude exceeds this is physically implausible (bad reference, bad
    /// die-ADC, or the unit was not actually cold) → the whole baseline is
    /// REJECTED. Default 15 °C.
    #[serde(default = "default_max_abs_offset_c")]
    pub max_abs_offset_c: f32,

    /// Maximum trusted spread (max − min) of per-chip offsets (celsius). If the
    /// chips disagree about the ADC-vs-PCB offset by more than this, the sample
    /// is not a coherent cold baseline → the whole baseline is REJECTED.
    /// Default 10 °C.
    #[serde(default = "default_max_chip_spread_c")]
    pub max_chip_spread_c: f32,

    /// Reject baseline capture unless the reference PCB temperature is at or
    /// below this (celsius) — i.e. the unit really was cold/idle. Prevents
    /// seeding calibration from a hot, already-mining unit. Default 60 °C.
    #[serde(default = "default_baseline_max_reference_c")]
    pub baseline_max_reference_c: f32,

    /// Tiny epsilon (celsius) for the "never report below raw" guard. A
    /// calibrated reading below `raw - apply_epsilon_c` is rejected (uses raw);
    /// a reading within the epsilon band is floored at raw. Default 0.5 °C.
    #[serde(default = "default_apply_epsilon_c")]
    pub apply_epsilon_c: f32,
}

fn default_max_abs_offset_c() -> f32 {
    15.0
}
fn default_max_chip_spread_c() -> f32 {
    10.0
}
fn default_baseline_max_reference_c() -> f32 {
    60.0
}
fn default_apply_epsilon_c() -> f32 {
    0.5
}

impl Default for DieCalibrationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_abs_offset_c: default_max_abs_offset_c(),
            max_chip_spread_c: default_max_chip_spread_c(),
            baseline_max_reference_c: default_baseline_max_reference_c(),
            apply_epsilon_c: default_apply_epsilon_c(),
        }
    }
}

/// Why a baseline capture was accepted or rejected. Returned by
/// [`DieCalibration::capture_baseline`] so callers can log a precise reason and
/// tests can assert the exact branch. On any `Rejected*` (or `Disabled`) the
/// calibration remains uncaptured and `apply_*` stays a raw passthrough.
#[derive(Debug, Clone, PartialEq)]
pub enum BaselineOutcome {
    /// The feature is off (`enabled == false`). Nothing captured.
    Disabled,
    /// Captured `n` per-chip offsets. Calibration is now active.
    Captured { chips: usize },
    /// No raw die readings were supplied (empty sample).
    RejectedNoSample,
    /// The reference PCB temperature was non-finite.
    RejectedReferenceNonFinite,
    /// One or more raw die readings were non-finite — a garbage sensor must
    /// never seed calibration.
    RejectedRawNonFinite,
    /// The reference PCB temperature was above `baseline_max_reference_c` — the
    /// unit was not cold, so this is not a trustworthy baseline.
    RejectedNotCold { reference_c: f32, max_c: f32 },
    /// A per-chip |offset| exceeded `max_abs_offset_c`.
    RejectedOffsetTooLarge { worst_abs_c: f32, max_c: f32 },
    /// The spread of per-chip offsets exceeded `max_chip_spread_c`.
    RejectedImplausibleSpread { spread_c: f32, max_c: f32 },
}

impl BaselineOutcome {
    /// True iff a baseline was actually captured (calibration is now active).
    pub fn is_captured(&self) -> bool {
        matches!(self, BaselineOutcome::Captured { .. })
    }
}

/// Pure safety kernel: the SAFETY-facing calibrated die temperature for ONE
/// chip.
///
/// Guarantees the returned value is never below `raw` (over-reporting is safe;
/// under-reporting is dangerous). Returns `raw` unchanged when:
/// - `raw` is non-finite (the caller's empty/NaN fail-closed path still fires),
/// - there is no offset,
/// - the offset is non-finite,
/// - the offset would push the reading below `raw - |epsilon|` (an
///   under-reporting calibration is rejected).
///
/// A slightly-negative offset (within epsilon) is floored at `raw`, so the
/// result is always `>= raw` for finite inputs.
pub fn calibrated_safe_die_c(raw: f32, offset: Option<f32>, epsilon_c: f32) -> f32 {
    if !raw.is_finite() {
        return raw;
    }
    let offset = match offset {
        Some(o) if o.is_finite() => o,
        _ => return raw,
    };
    let calibrated = raw + offset;
    if !calibrated.is_finite() {
        return raw;
    }
    // Under-reporting beyond the epsilon band is dangerous → reject to raw.
    if calibrated < raw - epsilon_c.abs() {
        return raw;
    }
    // Never report below raw: floor a slight downward correction at raw.
    calibrated.max(raw)
}

/// Stateful per-chip die-temperature calibration.
///
/// Construct with [`DieCalibration::new`], capture a cold baseline once with
/// [`DieCalibration::capture_baseline`], then transform runtime die readings
/// with [`DieCalibration::apply_one`] (single XADC die, the am2 path) or
/// [`DieCalibration::apply_max`] (per-chip die array).
#[derive(Debug, Clone, PartialEq)]
pub struct DieCalibration {
    cfg: DieCalibrationConfig,
    /// Per-chip offsets captured at the cold baseline. `None` ⇒ no valid
    /// baseline (or feature off) ⇒ `apply_*` is a pure raw passthrough.
    offsets: Option<Vec<f32>>,
}

impl DieCalibration {
    /// Create an uncaptured calibrator from config.
    pub fn new(cfg: DieCalibrationConfig) -> Self {
        Self { cfg, offsets: None }
    }

    /// True iff the feature is enabled in config.
    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// True iff a valid baseline has been captured (calibration is active).
    pub fn is_calibrated(&self) -> bool {
        self.offsets.is_some()
    }

    /// The number of captured per-chip offsets, or 0 if none.
    pub fn chip_count(&self) -> usize {
        self.offsets.as_ref().map(Vec::len).unwrap_or(0)
    }

    /// Capture the per-chip baseline offset = `reference_pcb_c - raw_die[i]`.
    ///
    /// Validates the sample and either stores the offsets (calibration becomes
    /// active) or rejects it (calibration stays a raw passthrough). Idempotent
    /// only in the sense that a later successful capture replaces an earlier
    /// one; a rejection leaves any prior good baseline untouched EXCEPT that a
    /// fresh capture attempt clears nothing until it succeeds.
    pub fn capture_baseline(&mut self, reference_pcb_c: f32, raw_die: &[f32]) -> BaselineOutcome {
        if !self.cfg.enabled {
            return BaselineOutcome::Disabled;
        }
        if raw_die.is_empty() {
            return BaselineOutcome::RejectedNoSample;
        }
        if !reference_pcb_c.is_finite() {
            return BaselineOutcome::RejectedReferenceNonFinite;
        }
        if raw_die.iter().any(|v| !v.is_finite()) {
            return BaselineOutcome::RejectedRawNonFinite;
        }
        if reference_pcb_c > self.cfg.baseline_max_reference_c {
            return BaselineOutcome::RejectedNotCold {
                reference_c: reference_pcb_c,
                max_c: self.cfg.baseline_max_reference_c,
            };
        }

        let offsets: Vec<f32> = raw_die.iter().map(|&raw| reference_pcb_c - raw).collect();

        let worst_abs = offsets.iter().fold(0.0_f32, |acc, o| acc.max(o.abs()));
        if worst_abs > self.cfg.max_abs_offset_c {
            return BaselineOutcome::RejectedOffsetTooLarge {
                worst_abs_c: worst_abs,
                max_c: self.cfg.max_abs_offset_c,
            };
        }

        let min = offsets.iter().copied().fold(f32::INFINITY, f32::min);
        let max = offsets.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let spread = max - min;
        if spread > self.cfg.max_chip_spread_c {
            return BaselineOutcome::RejectedImplausibleSpread {
                spread_c: spread,
                max_c: self.cfg.max_chip_spread_c,
            };
        }

        let chips = offsets.len();
        self.offsets = Some(offsets);
        BaselineOutcome::Captured { chips }
    }

    /// Apply calibration to a SINGLE raw die reading (the am2 XADC SoC-die
    /// path). Uses the first captured offset. Returns the raw reading unchanged
    /// when the feature is off, no baseline is captured, or the safety guard
    /// rejects the correction. The result is always `>= raw` for finite input.
    pub fn apply_one(&self, raw_die_c: f32) -> f32 {
        if !self.cfg.enabled {
            return raw_die_c;
        }
        let offset = self.offsets.as_ref().and_then(|o| o.first().copied());
        calibrated_safe_die_c(raw_die_c, offset, self.cfg.apply_epsilon_c)
    }

    /// The safety-facing MAX calibrated die temperature across a per-chip raw
    /// die array. Each chip is transformed by [`calibrated_safe_die_c`] using
    /// its own captured offset (raw passthrough for chips beyond the captured
    /// count, or when off/uncaptured). Returns `None` only when no finite raw
    /// reading exists (so the caller's existing "no valid temperature"
    /// fail-closed path still fires). The returned value is always `>=` the max
    /// of the finite raw readings.
    pub fn apply_max(&self, raw_die: &[f32]) -> Option<f32> {
        let mut max_c = f32::NEG_INFINITY;
        for (idx, &raw) in raw_die.iter().enumerate() {
            if !raw.is_finite() {
                continue;
            }
            let effective = if self.cfg.enabled {
                let offset = self.offsets.as_ref().and_then(|o| o.get(idx).copied());
                calibrated_safe_die_c(raw, offset, self.cfg.apply_epsilon_c)
            } else {
                raw
            };
            max_c = max_c.max(effective);
        }
        if max_c.is_finite() {
            Some(max_c)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- config defaults ----

    #[test]
    fn default_config_is_off_and_sane() {
        let cfg = DieCalibrationConfig::default();
        assert!(!cfg.enabled, "die-temp calibration must default OFF");
        assert_eq!(cfg.max_abs_offset_c, 15.0);
        assert_eq!(cfg.max_chip_spread_c, 10.0);
        assert_eq!(cfg.baseline_max_reference_c, 60.0);
        assert_eq!(cfg.apply_epsilon_c, 0.5);
    }

    // ---- feature-off → EXACT raw passthrough ----

    #[test]
    fn feature_off_is_exact_raw_passthrough() {
        let mut cal = DieCalibration::new(DieCalibrationConfig::default());
        // Even a "capture" while disabled must not activate calibration.
        assert_eq!(
            cal.capture_baseline(45.0, &[40.0]),
            BaselineOutcome::Disabled
        );
        assert!(!cal.is_calibrated());
        // Runtime apply returns the raw value byte-for-byte across the range.
        for raw in [0.0_f32, 25.3, 49.7, 72.1, 95.0, 124.9] {
            assert_eq!(cal.apply_one(raw), raw);
            assert_eq!(cal.apply_max(&[raw]), Some(raw));
        }
    }

    // ---- baseline-missing → raw passthrough ----

    #[test]
    fn baseline_missing_is_raw_passthrough() {
        let cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        // Enabled but NO baseline captured yet → apply must be raw passthrough.
        assert!(cal.enabled());
        assert!(!cal.is_calibrated());
        assert_eq!(cal.apply_one(50.0), 50.0);
        assert_eq!(cal.apply_max(&[50.0, 60.0]), Some(60.0));
    }

    #[test]
    fn empty_sample_is_rejected() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(
            cal.capture_baseline(40.0, &[]),
            BaselineOutcome::RejectedNoSample
        );
        assert!(!cal.is_calibrated());
        assert_eq!(cal.apply_one(50.0), 50.0);
    }

    #[test]
    fn non_finite_reference_or_raw_rejected() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(
            cal.capture_baseline(f32::NAN, &[40.0]),
            BaselineOutcome::RejectedReferenceNonFinite
        );
        assert_eq!(
            cal.capture_baseline(40.0, &[40.0, f32::NAN]),
            BaselineOutcome::RejectedRawNonFinite
        );
        assert!(!cal.is_calibrated());
        assert_eq!(cal.apply_one(50.0), 50.0);
    }

    #[test]
    fn hot_unit_is_not_a_valid_cold_baseline() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            baseline_max_reference_c: 60.0,
            ..Default::default()
        });
        // Reference 75 °C > 60 °C → the unit was not cold → reject.
        match cal.capture_baseline(75.0, &[74.0]) {
            BaselineOutcome::RejectedNotCold { reference_c, max_c } => {
                assert_eq!(reference_c, 75.0);
                assert_eq!(max_c, 60.0);
            }
            other => panic!("expected RejectedNotCold, got {other:?}"),
        }
        assert!(!cal.is_calibrated());
    }

    // ---- implausible-offset → rejected ----

    #[test]
    fn implausible_offset_magnitude_rejected() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            max_abs_offset_c: 15.0,
            ..Default::default()
        });
        // reference 45, raw 20 → offset 25 (> 15) → reject.
        match cal.capture_baseline(45.0, &[20.0]) {
            BaselineOutcome::RejectedOffsetTooLarge { worst_abs_c, max_c } => {
                assert!((worst_abs_c - 25.0).abs() < 1e-4);
                assert_eq!(max_c, 15.0);
            }
            other => panic!("expected RejectedOffsetTooLarge, got {other:?}"),
        }
        assert!(!cal.is_calibrated());
        // And with no valid baseline, apply stays raw — no under-report risk.
        assert_eq!(cal.apply_one(70.0), 70.0);
    }

    #[test]
    fn implausible_chip_spread_rejected() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            max_abs_offset_c: 15.0,
            max_chip_spread_c: 10.0,
            ..Default::default()
        });
        // reference 45: raw [43, 27] → offsets [2, 18]. 18 > 15 → offset-too-large
        // fires first; use raw [43, 30] → offsets [2, 15] → spread 13 > 10.
        match cal.capture_baseline(45.0, &[43.0, 30.0]) {
            BaselineOutcome::RejectedImplausibleSpread { spread_c, max_c } => {
                assert!((spread_c - 13.0).abs() < 1e-4);
                assert_eq!(max_c, 10.0);
            }
            other => panic!("expected RejectedImplausibleSpread, got {other:?}"),
        }
        assert!(!cal.is_calibrated());
    }

    // ---- normal offset → applied ----

    #[test]
    fn normal_positive_offset_is_applied() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        // Cold baseline: PCB 45 °C, raw die reads 40 °C (ADC biased 5 °C low).
        assert_eq!(
            cal.capture_baseline(45.0, &[40.0]),
            BaselineOutcome::Captured { chips: 1 }
        );
        assert!(cal.is_calibrated());
        // Runtime: raw die 60 → calibrated 65 (offset +5). Over-reporting is safe.
        assert!((cal.apply_one(60.0) - 65.0).abs() < 1e-4);
        assert!((cal.apply_max(&[60.0]).unwrap() - 65.0).abs() < 1e-4);
    }

    #[test]
    fn multi_chip_offsets_applied_per_index() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        // PCB 44: raw [40, 42] → offsets [+4, +2], spread 2 (OK).
        assert_eq!(
            cal.capture_baseline(44.0, &[40.0, 42.0]),
            BaselineOutcome::Captured { chips: 2 }
        );
        // Runtime raw [50, 55] → calibrated [54, 57] → max 57.
        let got = cal.apply_max(&[50.0, 55.0]).unwrap();
        assert!((got - 57.0).abs() < 1e-4, "expected 57, got {got}");
    }

    // ---- calibration must NEVER lower an over-temp reading below the trip ----

    #[test]
    fn negative_offset_never_lowers_below_raw() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            apply_epsilon_c: 0.5,
            ..Default::default()
        });
        // Cold baseline: PCB 40 °C, raw die reads 50 °C (ADC biased 10 °C HIGH)
        // → offset -10. This is within the |15| band so it captures, but the
        // safety floor must ensure the reading is NEVER pulled below raw.
        assert_eq!(
            cal.capture_baseline(40.0, &[50.0]),
            BaselineOutcome::Captured { chips: 1 }
        );
        // A hot raw of 80 °C would "calibrate" to 70 °C — under-reporting.
        // The guard rejects it: the safety reading stays at the raw 80 °C.
        assert!((cal.apply_one(80.0) - 80.0).abs() < 1e-4);
        assert!((cal.apply_max(&[80.0]).unwrap() - 80.0).abs() < 1e-4);
    }

    #[test]
    fn slight_negative_offset_is_floored_at_raw() {
        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            apply_epsilon_c: 0.5,
            ..Default::default()
        });
        // offset -0.3 (within the 0.5 epsilon band).
        assert_eq!(
            cal.capture_baseline(41.7, &[42.0]),
            BaselineOutcome::Captured { chips: 1 }
        );
        // raw 70 → calibrated 69.7 (within epsilon) → floored to raw 70.
        let got = cal.apply_one(70.0);
        assert!(got >= 70.0, "safety floor: must be >= raw, got {got}");
        assert!((got - 70.0).abs() < 1e-4);
    }

    #[test]
    fn calibrated_never_below_raw_property_sweep() {
        // Property: for ANY finite raw and ANY offset in the trusted band, the
        // safety kernel never returns below raw. (Over-report allowed; under
        // never.)
        for offset in [-15.0_f32, -10.0, -5.0, -0.4, 0.0, 3.3, 10.0, 15.0] {
            for raw in [0.0_f32, 25.0, 49.3, 65.0, 75.0, 95.0, 124.0] {
                let got = calibrated_safe_die_c(raw, Some(offset), 0.5);
                assert!(
                    got >= raw - 1e-4,
                    "under-report! raw={raw} offset={offset} got={got}"
                );
            }
        }
    }

    #[test]
    fn non_finite_raw_passes_through_for_failclosed_path() {
        // A non-finite raw must pass through unchanged so the caller's existing
        // empty/NaN fail-closed logic still triggers — calibration must not
        // launder a garbage reading into a finite number.
        assert!(calibrated_safe_die_c(f32::NAN, Some(5.0), 0.5).is_nan());
        assert!(calibrated_safe_die_c(f32::INFINITY, Some(-5.0), 0.5).is_infinite());

        let mut cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        cal.capture_baseline(45.0, &[40.0]);
        assert!(cal.apply_one(f32::NAN).is_nan());
        // A per-chip array of all-NaN yields None (no finite temperature).
        assert_eq!(cal.apply_max(&[f32::NAN, f32::NAN]), None);
    }

    #[test]
    fn non_finite_offset_falls_back_to_raw() {
        assert_eq!(calibrated_safe_die_c(60.0, Some(f32::NAN), 0.5), 60.0);
        assert_eq!(calibrated_safe_die_c(60.0, None, 0.5), 60.0);
    }

    #[test]
    fn apply_max_empty_is_none() {
        let cal = DieCalibration::new(DieCalibrationConfig {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(cal.apply_max(&[]), None);
    }
}
