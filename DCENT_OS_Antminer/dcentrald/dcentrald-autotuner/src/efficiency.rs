//! Efficiency mode optimizer for minimizing J/TH (joules per terahash).
//!
//! Key insight: For CMOS dynamic power, J/TH = P/H = (C_eff * V^2 * f) / (k * f) = C_eff * V^2 / k.
//! Efficiency is independent of frequency — it only depends on voltage!
//!
//! So for efficiency mode:
//!   1. Minimize voltage (lowest stable voltage the hardware supports)
//!   2. At that voltage, run each chip at its max stable frequency
//!
//! This gives the theoretical best J/TH while still maximizing hashrate at the
//! chosen voltage. Hashrate is lower than max-hashrate mode (because voltage is
//! lower), but every watt produces the most hashes possible.

use crate::power_budget::PowerModel;
use crate::profile::ChipProfile;

/// Efficiency mode optimizer.
///
/// Finds the voltage/frequency combination that minimizes J/TH.
pub struct EfficiencyOptimizer;

impl EfficiencyOptimizer {
    /// Find the optimal voltage and per-chip frequencies for minimum J/TH.
    ///
    /// Since J/TH = C_eff * V^2 / k (independent of frequency), the optimal
    /// strategy is:
    ///   1. Use the minimum safe voltage
    ///   2. Run each chip at its max stable frequency (which may be lower at lower voltage,
    ///      but our profiles are characterized at a fixed voltage, so we use max_stable_mhz)
    ///
    /// In practice, chips may need re-characterization at lower voltages because
    /// max stable frequency typically drops with voltage. For now, we apply a
    /// voltage-derating factor to account for this:
    ///   freq_at_lower_v ≈ max_stable × (V_new / V_original)^0.5
    ///
    /// Returns (optimal_voltage_mv, per_chip_target_frequencies).
    ///
    /// `power_model`: the power model for estimating power consumption.
    /// `chip_profiles`: per-chip tuning profiles from characterization.
    /// `min_voltage_mv`: minimum allowed voltage in millivolts.
    /// `min_freq_mhz`: minimum allowed frequency in MHz.
    /// `chip_id`: ASIC chip ID for PLL frequency table lookup (e.g., 0x1387).
    /// : voltage at which chip profiles were characterized (e.g., 9100 for S9).
    /// `preferred_voltage_mv`: measured chain operating voltage to honor when a
    /// runtime voltage search already discovered a lower stable point.
    pub fn optimize(
        _power_model: &PowerModel,
        chip_profiles: &[ChipProfile],
        min_voltage_mv: u16,
        min_freq_mhz: u16,
        chip_id: u16,
        : u16,
        preferred_voltage_mv: Option<u16>,
    ) -> (u16, Vec<u16>) {
        if chip_profiles.is_empty() {
            return (min_voltage_mv, Vec::new());
        }

        // Prefer an actually measured low-voltage operating point when the
        // runtime voltage search already validated one for this chain.
        let optimal_voltage_mv = preferred_voltage_mv
            .map(|mv| mv.max(min_voltage_mv))
            .filter(|&target_mv| {
                chip_profiles.iter().any(|profile| {
                    profile
                        .measured_max_stable_at_or_below_voltage(target_mv)
                        .is_some()
                })
            })
            .unwrap_or(min_voltage_mv);

        // At lower voltage, chips may not reach their characterized max_stable_mhz.
        // Apply a conservative derating based on the voltage ratio.
        let voltage_ratio = optimal_voltage_mv as f64 /  as f64;

        // Derating factor: approximate that max frequency scales with sqrt(V)
        // This is conservative — real silicon may derate more or less.
        let derating = voltage_ratio.sqrt();

        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chip_id);

        let per_chip_freqs: Vec<u16> = chip_profiles
            .iter()
            .map(|profile| {
                if profile.max_stable_mhz == 0 {
                    return min_freq_mhz;
                }

                if let Some(measured_freq) =
                    profile.measured_max_stable_at_or_below_voltage(optimal_voltage_mv)
                {
                    let target = measured_freq.max(min_freq_mhz);
                    return pll
                        .iter()
                        .rev()
                        .find(|&&f| f <= target)
                        .copied()
                        .unwrap_or(min_freq_mhz);
                }

                // Derate the max stable frequency for the lower voltage
                let derated_freq = (profile.max_stable_mhz as f64 * derating) as u16;
                let target = derated_freq.max(min_freq_mhz);

                // Snap to nearest PLL entry <= target
                pll.iter()
                    .rev()
                    .find(|&&f| f <= target)
                    .copied()
                    .unwrap_or(min_freq_mhz)
            })
            .collect();

        (optimal_voltage_mv, per_chip_freqs)
    }

    /// W9.4 — Estimate the efficiency (J/TH) using the operator-confirmed
    /// wattmeter reading as the source of truth, falling back to the modeled
    /// estimate when no wattmeter calibration is on file.
    ///
    /// This is the canonical cost function for `TuneTarget::EfficiencyJTH`.
    /// When the operator has confirmed `(measured_wall_watts, hashrate_ths)`
    /// via `POST /api/perf/calibrate`, the autotuner uses that ratio as the
    /// source-of-truth J/TH and biases per-chip optimization away from
    /// frequency/voltage steps that the model says improve efficiency but
    /// the operator's wattmeter would reject.
    ///
    /// `operator_jth`: the live `PowerCalibration::operator_confirmed_jth()`
    /// snapshot (watts/TH), if any.
    ///
    /// Returns operator-confirmed J/TH when present; otherwise the modeled
    /// estimate.
    pub fn estimate_efficiency_jth_with_operator_anchor(
        power_model: &PowerModel,
        voltage_v: f64,
        chip_profiles: &[ChipProfile],
        operator_jth: Option<f64>,
    ) -> f64 {
        if let Some(anchor) = operator_jth {
            if anchor.is_finite() && anchor > 0.0 {
                return anchor;
            }
        }
        Self::estimate_efficiency_jth(power_model, voltage_v, chip_profiles)
    }

    /// Estimate the efficiency (J/TH) at a given voltage.
    ///
    /// J/TH = P / H where:
    ///   P = C_eff * V^2 * f (watts)
    ///   H = k * f (hashes/sec, where k depends on chip architecture)
    ///   → J/TH = C_eff * V^2 / k
    ///
    /// For BM1387: each chip does ~89 GH/s at 650 MHz → k ≈ 0.137 GH/s per MHz.
    /// But since J/TH ∝ V^2, the actual hashrate constant cancels out in comparisons.
    ///
    /// **Basis / truthfulness note:** this is a purely MODELED estimate derived
    /// from the `C_eff` power model and the nominal `chip_geometry` hashrate
    /// constant at the *configured* per-chip frequencies — it is NOT a measured
    /// value and does not read the live wall wattmeter or live hashrate. It is
    /// intended as a relative cost function for comparing candidate
    /// voltage/frequency points during tuning, where the absolute calibration
    /// cancels out. It can therefore DIVERGE from any J/TH figure displayed to
    /// the operator: the displayed/operator-facing efficiency should come from
    /// [`Self::estimate_efficiency_jth_with_operator_anchor`], which prefers the
    /// operator's confirmed wattmeter ratio when one is on file. Do not present
    /// this raw modeled output as a measured efficiency.
    ///
    /// Returns estimated J/TH (lower is better).
    pub fn estimate_efficiency_jth(
        power_model: &PowerModel,
        voltage_v: f64,
        chip_profiles: &[ChipProfile],
    ) -> f64 {
        if chip_profiles.is_empty() {
            return 0.0;
        }

        // BM1387 hashrate per chip: ~0.137 GH/s per MHz
        // (S9: 189 chips × 650 MHz ≈ 13.5 TH/s → 13500 / 189 / 650 ≈ 0.11 GH/s/MHz)
        // More precisely: 14 TH/s / 189 chips / 650 MHz = 0.114 GH/s/MHz

        let total_power: f64 = chip_profiles
            .iter()
            .map(|p| power_model.chip_power_w(voltage_v, p.operating_mhz))
            .sum();

        let total_hashrate_ghs: f64 = chip_profiles
            .iter()
            .map(|p| {
                crate::chip_geometry::chip_hashrate_ghs_for_chip(
                    power_model.chip_id(),
                    p.operating_mhz,
                )
            })
            .sum();

        let total_hashrate_ths = total_hashrate_ghs / 1000.0;

        if total_hashrate_ths > 0.0 {
            total_power / total_hashrate_ths
        } else {
            f64::INFINITY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ChipGrade;

    fn make_test_profiles(count: usize, max_stable: u16) -> Vec<ChipProfile> {
        (0..count)
            .map(|i| ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: max_stable,
                operating_mhz: max_stable,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect()
    }

    #[test]
    fn estimate_efficiency_jth_zero_hashrate_is_the_inf_sentinel_never_nan() {
        // Division-by-zero-in-verdict pin (part of the div-by-zero sweep). Efficiency
        // J/TH = total_power / total_hashrate. When operating chips report ZERO
        // hashrate (0 MHz), this must return the DELIBERATE f64::INFINITY sentinel
        // ("no efficiency"), never a div-by-zero NaN (0.0/0.0) or a panic — a NaN
        // J/TH would corrupt every downstream efficiency comparison (min-J/TH tuning).
        let model = PowerModel::new_bm1387();

        // Non-empty profiles but 0 MHz -> zero hashrate -> the +inf sentinel, not NaN.
        let zero_hz = make_test_profiles(10, 0);
        let jth_zero = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &zero_hz);
        assert!(!jth_zero.is_nan(), "efficiency J/TH must never be NaN");
        assert!(
            jth_zero.is_infinite() && jth_zero > 0.0,
            "zero-hashrate efficiency must be the +inf sentinel, got {jth_zero}"
        );

        // Documented empty-profile early return (0.0).
        assert_eq!(
            EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &[]),
            0.0
        );

        // Real operating chips -> a finite, positive J/TH.
        let profiles = make_test_profiles(10, 650);
        let jth = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);
        assert!(
            jth.is_finite() && jth > 0.0,
            "real chips must give a finite positive J/TH, got {jth}"
        );
    }

    #[test]
    fn test_optimize_prefers_lower_voltage() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(10, 650);

        let (voltage_mv, _freqs) =
            EfficiencyOptimizer::optimize(&model, &profiles, 8500, 200, 0x1387, 9100, None);

        // Should choose the minimum voltage
        assert_eq!(voltage_mv, 8500);
    }

    #[test]
    fn test_optimize_returns_valid_frequencies() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(10, 650);

        let (_voltage_mv, freqs) =
            EfficiencyOptimizer::optimize(&model, &profiles, 8500, 200, 0x1387, 9100, None);

        let pll = dcentrald_asic::drivers::bm1387::pll_frequencies();

        assert_eq!(freqs.len(), 10);
        for &f in &freqs {
            assert!(f >= 200, "Frequency {} below minimum 200", f);
            assert!(pll.contains(&f), "Frequency {} not in PLL table", f);
        }
    }

    #[test]
    fn test_optimize_derates_for_lower_voltage() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(1, 700);

        // At reference voltage (9100mV), chip should run at max_stable
        let (_, freqs_high) =
            EfficiencyOptimizer::optimize(&model, &profiles, 9100, 200, 0x1387, 9100, None);

        // At lower voltage (8000mV), chip should run at lower frequency
        let (_, freqs_low) =
            EfficiencyOptimizer::optimize(&model, &profiles, 8000, 200, 0x1387, 9100, None);

        assert!(
            freqs_low[0] <= freqs_high[0],
            "Lower voltage ({}) should give lower or equal frequency: {} vs {}",
            8000,
            freqs_low[0],
            freqs_high[0]
        );
    }

    #[test]
    fn test_optimize_empty_profiles() {
        let model = PowerModel::new_bm1387();
        let (voltage, freqs) =
            EfficiencyOptimizer::optimize(&model, &[], 8500, 200, 0x1387, 9100, None);
        assert_eq!(voltage, 8500);
        assert!(freqs.is_empty());
    }

    #[test]
    fn test_optimize_prefers_measured_curve_over_sqrt_guess() {
        let model = PowerModel::new_bm1387();
        let mut profiles = make_test_profiles(1, 700);
        profiles[0].vf_curve = Some(vec![
            crate::dvfs::VfPoint {
                voltage_mv: 9100,
                max_stable_mhz: 700,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 8900,
                max_stable_mhz: 625,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
        ]);

        let (voltage_mv, freqs) =
            EfficiencyOptimizer::optimize(&model, &profiles, 8500, 200, 0x1387, 9100, Some(8920));

        assert_eq!(voltage_mv, 8920);
        assert_eq!(freqs, vec![625]);
    }

    #[test]
    fn test_efficiency_lower_voltage_better() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(63, 650);

        let eff_low_v = EfficiencyOptimizer::estimate_efficiency_jth(&model, 8.5, &profiles);
        let eff_high_v = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);

        // Lower voltage should give better (lower) J/TH
        assert!(
            eff_low_v < eff_high_v,
            "Lower voltage should be more efficient: {:.1} J/TH at 8.5V vs {:.1} J/TH at 9.1V",
            eff_low_v,
            eff_high_v
        );
    }

    #[test]
    fn test_efficiency_empty_profiles() {
        let model = PowerModel::new_bm1387();
        let eff = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &[]);
        assert_eq!(eff, 0.0);
    }

    #[test]
    fn test_operator_anchor_overrides_modeled() {
        // W9.4: the operator's wattmeter reading must beat the model's
        // estimate when EfficiencyJTH is the active target.
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(63, 650);
        let modeled = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);
        let with_anchor = EfficiencyOptimizer::estimate_efficiency_jth_with_operator_anchor(
            &model,
            9.1,
            &profiles,
            Some(95.0),
        );
        assert_eq!(with_anchor, 95.0);
        assert!(
            (modeled - 95.0).abs() > 0.001,
            "test would be vacuous if the model also returned 95.0"
        );
    }

    #[test]
    fn test_operator_anchor_falls_back_to_model_when_none() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(63, 650);
        let modeled = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);
        let fallback = EfficiencyOptimizer::estimate_efficiency_jth_with_operator_anchor(
            &model, 9.1, &profiles, None,
        );
        assert_eq!(fallback, modeled);
    }

    #[test]
    fn test_operator_anchor_rejects_zero_and_nan() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(63, 650);
        let modeled = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);
        // Zero anchor → falls through to modeled (defends against
        // pathological calibration entries that survived a deserialization
        // edge case).
        let zero = EfficiencyOptimizer::estimate_efficiency_jth_with_operator_anchor(
            &model,
            9.1,
            &profiles,
            Some(0.0),
        );
        assert_eq!(zero, modeled);
        // NaN → falls through to modeled.
        let nan = EfficiencyOptimizer::estimate_efficiency_jth_with_operator_anchor(
            &model,
            9.1,
            &profiles,
            Some(f64::NAN),
        );
        assert_eq!(nan, modeled);
    }

    #[test]
    fn test_efficiency_estimate_is_chip_aware() {
        let bm1387 = PowerModel::new_for_chip(0x1387);
        let bm1398 = PowerModel::new_for_chip(0x1398);
        let profiles = make_test_profiles(10, 650);

        let s9_eff = EfficiencyOptimizer::estimate_efficiency_jth(&bm1387, 9.1, &profiles);
        let s19_eff = EfficiencyOptimizer::estimate_efficiency_jth(&bm1398, 13.8, &profiles);

        assert_ne!(s9_eff, s19_eff);
    }

    #[test]
    fn test_s9_() {
        let model = PowerModel::new_bm1387();
        let profiles = make_test_profiles(189, 650);

        let eff = EfficiencyOptimizer::estimate_efficiency_jth(&model, 9.1, &profiles);

        // S9 at 650 MHz, 9.1V: ~1350W total, ~14 TH/s → ~96 J/TH
        // Our model only considers dynamic power per chip, not static,
        // so it will be somewhat lower. Just sanity check it's reasonable.
        assert!(
            eff > 30.0 && eff < 200.0,
            "S9 efficiency should be in reasonable range, got {:.1} J/TH",
            eff
        );
    }
}
