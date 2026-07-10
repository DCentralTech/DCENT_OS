//! DVFS (Dynamic Voltage-Frequency Scaling) joint optimization.
//!
//! No competitor does joint V/F optimization. BraiinsOS+ does power-budget at
//! fixed voltage. VNish does sequential V+F independently. We discover the
//! *actual Pareto-optimal* operating point per chip by characterizing at
//! multiple voltage points.
//!
//! Algorithm — Voltage-Stepped Characterization:
//!   1. Start at operating voltage. Run TABS binary search → per-chip max_stable at V₁.
//!   2. Drop voltage by step_mv. Run TABS again → per-chip max_stable at V₂.
//!   3. Repeat for N voltage points.
//!   4. For each chip, compute efficiency (J/TH) at each V/F point.
//!   5. Select operating point per chip based on target mode.
//!
//! Total time: N voltage points × 15s binary search = 75s at 5 points.
//! Still vastly faster than any competitor (hours).

use serde::{Deserialize, Serialize};

use crate::config::TuneTarget;
use crate::power_budget::PowerModel;

/// A single voltage-frequency measurement point for one chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfPoint {
    /// Voltage at which this measurement was taken (millivolts).
    pub voltage_mv: u16,
    /// Maximum stable frequency discovered at this voltage (MHz).
    pub max_stable_mhz: u16,
    /// Estimated dynamic power at this operating point (watts).
    pub estimated_power_w: f64,
    /// Estimated hashrate at this operating point (GH/s).
    pub estimated_hashrate_ghs: f64,
    /// Efficiency at this operating point (J/TH). Lower is better.
    pub efficiency_jth: f64,
}

/// Complete V/F characterization curve for one chip.
#[derive(Debug, Clone)]
pub struct ChipVfCurve {
    /// Chip index on the chain.
    pub chip_index: u8,
    /// Measured V/F points, sorted by voltage descending.
    pub points: Vec<VfPoint>,
}

/// DVFS optimizer that selects optimal operating points from V/F curves.
pub struct DvfsOptimizer {
    power_model: PowerModel,
}

impl DvfsOptimizer {
    /// Create a new DVFS optimizer with the given power model.
    pub fn new(power_model: PowerModel) -> Self {
        Self { power_model }
    }

    /// Compute V/F points for a chip at multiple voltages.
    ///
    /// `voltage_freq_pairs`: Vec of (voltage_mv, max_stable_mhz) from binary search
    /// at each voltage level.
    pub fn build_vf_curve(&self, chip_index: u8, voltage_freq_pairs: &[(u16, u16)]) -> ChipVfCurve {
        let mut points: Vec<VfPoint> = voltage_freq_pairs
            .iter()
            .map(|&(voltage_mv, max_stable_mhz)| {
                let voltage_v = voltage_mv as f64 / 1000.0;
                let power = self.power_model.chip_power_w(voltage_v, max_stable_mhz);
                let hashrate = crate::chip_geometry::chip_hashrate_ghs_for_chip(
                    self.power_model.chip_id(),
                    max_stable_mhz,
                );
                let hashrate_ths = hashrate / 1000.0;
                let efficiency = if hashrate_ths > 0.0 {
                    power / hashrate_ths
                } else {
                    f64::INFINITY
                };

                VfPoint {
                    voltage_mv,
                    max_stable_mhz,
                    estimated_power_w: power,
                    estimated_hashrate_ghs: hashrate,
                    efficiency_jth: efficiency,
                }
            })
            .collect();

        // Sort by voltage descending (highest voltage first)
        points.sort_by(|a, b| b.voltage_mv.cmp(&a.voltage_mv));

        ChipVfCurve { chip_index, points }
    }

    /// Select the optimal operating point for a chip based on target mode.
    ///
    /// Returns (voltage_mv, freq_mhz) for the selected operating point.
    pub fn select_operating_point(
        &self,
        curve: &ChipVfCurve,
        target: TuneTarget,
        power_budget_per_chip_w: Option<f64>,
    ) -> Option<(u16, u16)> {
        if curve.points.is_empty() {
            return None;
        }

        // Remove dominated points (Pareto filter)
        let pareto = self.pareto_frontier(&curve.points);

        match target {
            TuneTarget::Hashrate => {
                // Pick highest frequency (highest voltage point)
                pareto
                    .iter()
                    .max_by_key(|p| p.max_stable_mhz)
                    .map(|p| (p.voltage_mv, p.max_stable_mhz))
            }
            TuneTarget::Efficiency | TuneTarget::EfficiencyJTH => {
                // Pick lowest J/TH (usually lowest voltage). EfficiencyJTH uses
                // the same Pareto-front selection here; the operator-confirmed
                // wattmeter anchor enters higher up in the tuner — see
                // `tuner.rs::evaluate_target_*` and `efficiency.rs::optimize`
                // — to bias the model J/TH onto the measured baseline.
                pareto
                    .iter()
                    .filter(|p| p.efficiency_jth.is_finite())
                    .min_by(|a, b| {
                        a.efficiency_jth
                            .partial_cmp(&b.efficiency_jth)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|p| (p.voltage_mv, p.max_stable_mhz))
            }
            TuneTarget::Power | TuneTarget::HashrateTarget => {
                // Pick best hashrate within power budget.
                // HashrateTarget uses a synthetic power budget computed from the
                // target hashrate, so the same DVFS logic applies.
                let budget = power_budget_per_chip_w.unwrap_or(f64::INFINITY);
                pareto
                    .iter()
                    .filter(|p| p.estimated_power_w <= budget)
                    .max_by_key(|p| p.max_stable_mhz)
                    .or_else(|| {
                        // If nothing fits budget, pick lowest power point
                        pareto.iter().min_by(|a, b| {
                            a.estimated_power_w
                                .partial_cmp(&b.estimated_power_w)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                    })
                    .map(|p| (p.voltage_mv, p.max_stable_mhz))
            }
        }
    }

    /// Compute the Pareto frontier: remove dominated points.
    ///
    /// A point is dominated if another point has BOTH higher hashrate AND lower power.
    fn pareto_frontier<'a>(&self, points: &'a [VfPoint]) -> Vec<&'a VfPoint> {
        let mut frontier: Vec<&VfPoint> = Vec::new();

        for p in points {
            let is_dominated = points.iter().any(|other| {
                other.estimated_hashrate_ghs > p.estimated_hashrate_ghs
                    && other.estimated_power_w < p.estimated_power_w
            });

            if !is_dominated {
                frontier.push(p);
            }
        }

        frontier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_power_model() -> PowerModel {
        PowerModel::new_bm1387()
    }

    #[test]
    fn test_dvfs_discovers_pareto_frontier() {
        let optimizer = DvfsOptimizer::new(make_power_model());

        // Simulate: higher voltage = higher freq but more power
        let curve = optimizer.build_vf_curve(
            0,
            &[
                (9100, 650),
                (9000, 640),
                (8900, 625),
                (8800, 600),
                (8700, 575),
            ],
        );

        assert_eq!(curve.points.len(), 5);
        // Points should be sorted by voltage descending
        assert_eq!(curve.points[0].voltage_mv, 9100);
        assert_eq!(curve.points[4].voltage_mv, 8700);

        // Verify efficiency improves at lower voltage (J/TH goes down)
        // since J/TH = C_eff * V^2 / k, and V decreases faster than freq
        let eff_high_v = curve.points[0].efficiency_jth;
        let eff_low_v = curve.points[4].efficiency_jth;
        assert!(
            eff_low_v < eff_high_v,
            "Lower voltage should be more efficient: {:.1} vs {:.1}",
            eff_low_v,
            eff_high_v
        );
    }

    #[test]
    fn test_dvfs_efficiency_mode_picks_lowest_voltage() {
        let optimizer = DvfsOptimizer::new(make_power_model());

        let curve =
            optimizer.build_vf_curve(0, &[(9100, 650), (9000, 640), (8900, 625), (8800, 600)]);

        let (voltage, _freq) = optimizer
            .select_operating_point(&curve, TuneTarget::Efficiency, None)
            .unwrap();

        // Efficiency mode should pick the lowest voltage (best J/TH)
        assert_eq!(voltage, 8800, "Efficiency mode should pick lowest voltage");
    }

    #[test]
    fn test_dvfs_hashrate_mode_picks_highest_freq() {
        let optimizer = DvfsOptimizer::new(make_power_model());

        let curve = optimizer.build_vf_curve(0, &[(9100, 650), (9000, 640), (8900, 625)]);

        let (voltage, freq) = optimizer
            .select_operating_point(&curve, TuneTarget::Hashrate, None)
            .unwrap();

        assert_eq!(voltage, 9100, "Hashrate mode should pick highest voltage");
        assert_eq!(freq, 650, "Hashrate mode should pick highest frequency");
    }

    #[test]
    fn test_dvfs_power_mode_respects_budget() {
        let optimizer = DvfsOptimizer::new(make_power_model());

        let curve =
            optimizer.build_vf_curve(0, &[(9100, 650), (9000, 640), (8900, 625), (8800, 600)]);

        // Set a tight power budget that excludes the highest voltage point
        let high_v_power = curve.points[0].estimated_power_w;
        let budget = high_v_power * 0.85; // 85% of highest power

        let (voltage, _freq) = optimizer
            .select_operating_point(&curve, TuneTarget::Power, Some(budget))
            .unwrap();

        assert!(
            voltage < 9100,
            "Power mode with budget should pick lower voltage, got {}",
            voltage
        );
    }

    #[test]
    fn test_vf_curve_serialization() {
        let point = VfPoint {
            voltage_mv: 9100,
            max_stable_mhz: 650,
            estimated_power_w: 6.24,
            estimated_hashrate_ghs: 74.1,
            efficiency_jth: 84.2,
        };

        let json = serde_json::to_string(&point).unwrap();
        let deserialized: VfPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.voltage_mv, 9100);
        assert_eq!(deserialized.max_stable_mhz, 650);
        assert!((deserialized.estimated_power_w - 6.24).abs() < 0.01);
    }
}
