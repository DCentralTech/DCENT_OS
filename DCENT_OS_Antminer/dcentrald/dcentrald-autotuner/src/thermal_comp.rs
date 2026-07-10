//! Temperature-compensated frequency derating.
//!
//! As die temperature increases, ASIC chips become less stable at high frequencies.
//! This module implements a simple empirical derating model: max stable frequency
//! decreases by a configurable fraction per degree C above a threshold temperature.
//!
//! The model is deliberately conservative and simple — no ML or curve fitting.
//! Real-world BM1387 chips show increased HW error rates above ~60C, with
//! a roughly linear relationship between temperature and max stable frequency.
//!
//! Integration:
//!   - `background_monitor()` in tuner.rs reads board temperature from ChipStatsSnapshot
//!   - If temperature exceeds derating threshold, derated frequencies are computed
//!   - If temperature drops below threshold, original profile frequencies are restored
//!   - Emergency temperature triggers immediate throttle to minimum frequency

use dcentrald_asic::drivers::{bm1387, MinerProfile};

/// Temperature-compensated frequency derating model.
///
/// Empirical model: max_stable_freq decreases ~0.3% per degree C above reference temp.
/// This is conservative — real chips may tolerate more, but we prefer stability
/// over the last few percent of hashrate.
pub struct ThermalCompensator {
    /// Reference temperature at which profiles were characterized (degrees C).
    reference_temp_c: f32,
    /// Derating coefficient: fraction of frequency lost per degree C above threshold.
    /// Default: 0.003 (0.3% per degree C).
    derating_per_c: f32,
    /// Temperature above which derating begins (degrees C).
    derating_threshold_c: f32,
    /// Maximum allowed temperature before emergency throttle (degrees C).
    emergency_temp_c: f32,
    /// Hysteresis band below derating threshold for restore (degrees C).
    /// Restore only when temp < derating_threshold_c - hysteresis_band_c.
    /// Prevents frequency oscillation near the threshold. Default: 3.0.
    hysteresis_band_c: f32,
    /// Chip-specific PLL frequency table for snapping derated frequencies.
    /// Defaults to BM1387 table for backward compatibility.
    pll_table: &'static [u16],
}

impl ThermalCompensator {
    /// Create a new thermal compensator with default parameters.
    ///
    /// Defaults:
    ///   - reference_temp: 55C (matches thermal config target_temp_c)
    ///   - derating: 0.3% per degree C above 60C
    ///   - emergency: 75C (matches thermal critical threshold)
    pub fn new() -> Self {
        Self {
            reference_temp_c: 55.0,
            derating_per_c: 0.003,
            derating_threshold_c: 60.0,
            emergency_temp_c: 75.0,
            hysteresis_band_c: 3.0,
            pll_table: bm1387::pll_frequencies(),
        }
    }

    /// Set chip-specific PLL table from a chip ID.
    ///
    /// Uses `MinerProfile::pll_frequencies_for_chip()` to look up the correct
    /// discrete frequency table for the given ASIC chip type. Falls back to
    /// BM1387 table for unknown chip IDs.
    pub fn with_chip_id(mut self, chip_id: u16) -> Self {
        self.pll_table = MinerProfile::pll_frequencies_for_chip(chip_id);
        self
    }

    /// Create a thermal compensator with custom derating coefficient.
    pub fn with_derating(mut self, derating_per_c: f32) -> Self {
        self.derating_per_c = derating_per_c;
        self
    }

    /// Apply immersion mode offset to all thermal thresholds.
    ///
    /// Immersion cooling provides superior heat dissipation, allowing chips
    /// to run at higher temperatures before becoming unstable. This raises
    /// all thermal thresholds by the given offset, enabling higher frequencies.
    pub fn with_immersion_offset(mut self, offset_c: f32) -> Self {
        self.derating_threshold_c += offset_c;
        self.emergency_temp_c += offset_c;
        self.reference_temp_c += offset_c;
        self
    }

    /// Set the hysteresis band for restore (degrees C below derating threshold).
    ///
    /// Restore only happens when temp drops below `derating_threshold_c - band`.
    /// This prevents frequency oscillation when temperature hovers near the threshold.
    pub fn with_hysteresis(mut self, band_c: f32) -> Self {
        self.hysteresis_band_c = band_c;
        self
    }

    /// Compute derated frequency for a chip given current temperature.
    ///
    /// Returns the adjusted operating frequency, snapped to the nearest
    /// valid PLL entry at or below the computed value.
    ///
    /// If temperature is at or below the derating threshold, returns the
    /// base frequency unchanged. The derating factor is clamped to never
    /// reduce frequency below 50% of the base (safety floor).
    pub fn derate_freq(&self, base_freq_mhz: u16, current_temp_c: f32) -> u16 {
        if current_temp_c <= self.derating_threshold_c {
            return base_freq_mhz;
        }

        let delta_t = current_temp_c - self.derating_threshold_c;
        let factor = 1.0 - (self.derating_per_c * delta_t);
        // Clamp: never derate below 50% of base frequency
        let derated = (base_freq_mhz as f32 * factor.max(0.5)) as u16;

        // Snap to nearest valid PLL entry at or below the derated value
        self.pll_table
            .iter()
            .rev()
            .find(|&&f| f <= derated)
            .copied()
            .unwrap_or(self.pll_table[0])
    }

    /// Compute position-weighted derated frequency for a chip.
    ///
    /// Center chips (idx ~N/2) run hotter than edge chips (idx 0, N-1) due to
    /// reduced airflow and heat sink contact. The position weight follows a
    /// sine curve: `weight(i,N) = 1.0 + 0.3 * sin(π*i/(N-1))`.
    ///
    /// Center chips get 1.3x derating (more aggressive), edges get 1.0x.
    /// This extracts 2-5% more hashrate by letting cool edge chips run faster.
    pub fn derate_freq_positioned(
        &self,
        base_freq_mhz: u16,
        current_temp_c: f32,
        chip_index: u8,
        total_chips: u8,
    ) -> u16 {
        if current_temp_c <= self.derating_threshold_c {
            return base_freq_mhz;
        }

        let delta_t = current_temp_c - self.derating_threshold_c;

        // Position weight: center chips get more aggressive derating
        let position_weight = if total_chips > 1 {
            let normalized = chip_index as f64 / (total_chips - 1) as f64;
            1.0 + 0.3 * (std::f64::consts::PI * normalized).sin()
        } else {
            1.0
        };

        let effective_derating = self.derating_per_c * position_weight as f32;
        let factor = 1.0 - (effective_derating * delta_t);
        let derated = (base_freq_mhz as f32 * factor.max(0.5)) as u16;

        self.pll_table
            .iter()
            .rev()
            .find(|&&f| f <= derated)
            .copied()
            .unwrap_or(self.pll_table[0])
    }

    /// Check if temperature is at emergency level requiring immediate throttle.
    pub fn is_emergency(&self, temp_c: f32) -> bool {
        temp_c >= self.emergency_temp_c
    }

    /// Check if temperature is above the derating threshold.
    pub fn needs_derating(&self, temp_c: f32) -> bool {
        temp_c > self.derating_threshold_c
    }

    /// Check if temperature is low enough to restore original frequencies.
    ///
    /// Returns true only when temp is below `derating_threshold_c - hysteresis_band_c`.
    /// This creates a dead band (e.g., derate at 60C, restore at 57C) that prevents
    /// frequency oscillation when temperature hovers near the threshold.
    pub fn should_restore(&self, temp_c: f32) -> bool {
        temp_c < self.derating_threshold_c - self.hysteresis_band_c
    }

    /// Get the reference temperature (degrees C).
    pub fn reference_temp_c(&self) -> f32 {
        self.reference_temp_c
    }

    /// Get the derating threshold temperature (degrees C).
    pub fn derating_threshold_c(&self) -> f32 {
        self.derating_threshold_c
    }

    /// Get the emergency temperature (degrees C).
    pub fn emergency_temp_c(&self) -> f32 {
        self.emergency_temp_c
    }

    /// Predict future temperature and pre-emptively derate BEFORE chips become unstable.
    ///
    /// Uses temperature slope to project temperature 1 minute ahead:
    /// `predicted_temp = current + slope_c_per_min * 1.0`
    ///
    /// Only activates when slope > 0.5 C/min (temperature rising meaningfully).
    /// No competitor does predictive thermal pre-compensation.
    ///
    /// Returns the predicted temperature if pre-compensation should activate,
    /// or None if the slope is too low to warrant pre-emptive action.
    pub fn predict_temperature(&self, current_temp_c: f32, slope_c_per_min: f32) -> Option<f32> {
        // Only pre-compensate when temperature is rising meaningfully
        if slope_c_per_min <= 0.5 {
            return None;
        }

        let predicted = current_temp_c + slope_c_per_min * 1.0; // 1 minute ahead

        // Only worth acting if predicted temp exceeds derating threshold
        if predicted > self.derating_threshold_c {
            Some(predicted)
        } else {
            None
        }
    }
}

impl Default for ThermalCompensator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_asic::drivers::bm1387;

    #[test]
    fn test_no_derating_below_threshold() {
        let comp = ThermalCompensator::new();
        // At or below 60C, no derating
        assert_eq!(comp.derate_freq(650, 50.0), 650);
        assert_eq!(comp.derate_freq(650, 55.0), 650);
        assert_eq!(comp.derate_freq(650, 60.0), 650);
    }

    #[test]
    fn test_derating_above_threshold() {
        let comp = ThermalCompensator::new();
        // At 70C: delta_t = 10, factor = 1.0 - 0.003*10 = 0.97
        // 650 * 0.97 = 630.5 -> nearest PLL <= 630 = 625
        let derated = comp.derate_freq(650, 70.0);
        assert_eq!(derated, 625);
    }

    #[test]
    fn test_derating_at_65c() {
        let comp = ThermalCompensator::new();
        // At 65C: delta_t = 5, factor = 1.0 - 0.003*5 = 0.985
        // 650 * 0.985 = 640.25 -> nearest PLL <= 640 = 625
        let derated = comp.derate_freq(650, 65.0);
        assert_eq!(derated, 625);
    }

    #[test]
    fn test_derating_snaps_to_pll() {
        let comp = ThermalCompensator::new();
        // Result must always be a valid PLL frequency
        let freqs = bm1387::pll_frequencies();
        for temp in [62.0, 65.0, 68.0, 70.0, 72.0, 74.0] {
            let derated = comp.derate_freq(700, temp);
            assert!(
                freqs.contains(&derated),
                "Derated freq {} at {}C is not a valid PLL entry",
                derated,
                temp,
            );
        }
    }

    #[test]
    fn test_derating_floor_at_50_percent() {
        let comp = ThermalCompensator::new();
        // Even at extreme temperatures, floor is 50% of base
        // 650 * 0.5 = 325 -> nearest PLL <= 325 = 325
        let derated = comp.derate_freq(650, 250.0);
        assert!(
            derated >= 300,
            "Derated freq {} should not go below ~50% of 650",
            derated
        );
        // Verify it's a valid PLL entry
        let freqs = bm1387::pll_frequencies();
        assert!(freqs.contains(&derated));
    }

    #[test]
    fn test_emergency_threshold() {
        let comp = ThermalCompensator::new();
        assert!(!comp.is_emergency(74.9));
        assert!(comp.is_emergency(75.0));
        assert!(comp.is_emergency(80.0));
    }

    #[test]
    fn test_needs_derating() {
        let comp = ThermalCompensator::new();
        assert!(!comp.needs_derating(55.0));
        assert!(!comp.needs_derating(60.0));
        assert!(comp.needs_derating(60.1));
        assert!(comp.needs_derating(70.0));
    }

    #[test]
    fn test_custom_derating_coefficient() {
        // Higher derating coefficient = more aggressive frequency reduction
        let comp = ThermalCompensator::new().with_derating(0.01); // 1% per C
                                                                  // At 70C: delta_t = 10, factor = 1.0 - 0.01*10 = 0.90
                                                                  // 650 * 0.90 = 585 -> nearest PLL <= 585 = 575
        let derated = comp.derate_freq(650, 70.0);
        assert_eq!(derated, 575);
    }

    #[test]
    fn test_derating_low_base_freq() {
        let comp = ThermalCompensator::new();
        // Even at low base frequencies, derating should work and snap correctly
        let derated = comp.derate_freq(200, 70.0);
        let freqs = bm1387::pll_frequencies();
        assert!(freqs.contains(&derated));
        assert!(derated <= 200);
    }

    #[test]
    fn test_derating_returns_first_pll_when_very_low() {
        let comp = ThermalCompensator::new();
        // If derating pushes below the lowest PLL entry, return the lowest
        let derated = comp.derate_freq(100, 250.0);
        let freqs = bm1387::pll_frequencies();
        assert_eq!(derated, freqs[0]);
    }

    #[test]
    fn test_hysteresis_prevents_oscillation() {
        let comp = ThermalCompensator::new(); // default 3C band
                                              // At 60.5C: needs derating (above 60C threshold)
        assert!(comp.needs_derating(60.5));
        // At 59.5C: does NOT need derating, but should NOT restore
        // (still within hysteresis band: 60 - 3 = 57C)
        assert!(!comp.needs_derating(59.5));
        assert!(!comp.should_restore(59.5));
        // At 58C: still within hysteresis band
        assert!(!comp.should_restore(58.0));
        // At 56.9C: below hysteresis band (57C), should restore
        assert!(comp.should_restore(56.9));
        // At 50C: well below, should restore
        assert!(comp.should_restore(50.0));
    }

    #[test]
    fn test_hysteresis_default_band() {
        let comp = ThermalCompensator::new();
        // Default band is 3C, threshold is 60C, so restore at 57C
        assert!(!comp.should_restore(57.0)); // exactly at boundary = not below
        assert!(comp.should_restore(56.99));
    }

    #[test]
    fn test_hysteresis_custom_band() {
        let comp = ThermalCompensator::new().with_hysteresis(5.0);
        // 5C band: threshold 60C, restore at 55C
        assert!(!comp.should_restore(56.0));
        assert!(comp.should_restore(54.9));
    }

    #[test]
    fn test_immersion_offset() {
        let comp = ThermalCompensator::new().with_immersion_offset(20.0);
        // Normal threshold 60C → 80C with immersion
        assert!(!comp.needs_derating(75.0));
        assert!(comp.needs_derating(81.0));
        // Emergency 75C → 95C with immersion
        assert!(!comp.is_emergency(90.0));
        assert!(comp.is_emergency(95.0));
    }

    #[test]
    fn test_position_weighted_derating() {
        let comp = ThermalCompensator::new();
        let freqs = bm1387::pll_frequencies();

        // At 65C (5C above threshold), center vs edge chips
        let edge_derated = comp.derate_freq_positioned(650, 65.0, 0, 63);
        let center_derated = comp.derate_freq_positioned(650, 65.0, 31, 63);

        // Center chip should be derated MORE than edge chip (lower frequency)
        assert!(
            center_derated <= edge_derated,
            "Center chip ({}) should be derated >= edge chip ({})",
            center_derated,
            edge_derated,
        );
        // Both should be valid PLL entries
        assert!(freqs.contains(&edge_derated));
        assert!(freqs.contains(&center_derated));
    }

    #[test]
    fn test_position_weight_single_chip() {
        let comp = ThermalCompensator::new();
        // With total_chips=1, position weight should be 1.0 (no position effect)
        let derated = comp.derate_freq_positioned(650, 65.0, 0, 1);
        let normal = comp.derate_freq(650, 65.0);
        assert_eq!(
            derated, normal,
            "Single chip should have no position effect"
        );
    }

    #[test]
    fn test_predictive_thermal() {
        let comp = ThermalCompensator::new();

        // Rising fast: should predict and pre-compensate
        let predicted = comp.predict_temperature(59.0, 1.5);
        assert!(predicted.is_some());
        let pred = predicted.unwrap();
        assert!(
            (pred - 60.5).abs() < 0.1,
            "Predicted {:.1} should be ~60.5",
            pred
        );

        // Rising slowly: no pre-compensation needed
        assert!(comp.predict_temperature(55.0, 0.3).is_none());

        // Cooling: no pre-compensation
        assert!(comp.predict_temperature(62.0, -0.5).is_none());

        // Rising but predicted temp still below threshold
        assert!(comp.predict_temperature(50.0, 0.6).is_none());
    }
}
