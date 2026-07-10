//!  braiins-C — BraiinsOS+ Constraints DTO (HAL-free).
//!
//! Source RE evidence:
//!
//! §9.2.28 wire types (lines 1231-1240) + §5 Performance + §6 DPS +
//! §7 Cooling parameter listing.
//!
//! `ConfigurationService.GetConstraints()` returns min/max/default
//! for every configurable parameter on a BraiinsOS+ unit.
//! braiins-A pinned the gRPC method; this module ports the typed
//! Constraint shape + the parameter catalog so dcent-toolbox can
//! validate user input against the bounds without hitting Tonic.
//!
//! Wire types from RE doc §9.2.28:
//! - `Power` carries u64 watts.
//! - `TeraHashrate` carries f64 TH/s.
//! - `Temperature` carries f64 °C.
//! - `Hours` carries u32.
//! - Percentage fields use u8 (0-100).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Generic Constraint<T>
// ---------------------------------------------------------------------------

/// A min/max/default triple. `min ≤ default ≤ max` is enforced by
/// `documented_constraint()`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Constraint<T> {
    pub min: T,
    pub max: T,
    pub default: T,
}

impl<T: PartialOrd + Copy> Constraint<T> {
    /// True iff `min ≤ default ≤ max`.
    pub fn is_well_ordered(&self) -> bool {
        self.min <= self.default && self.default <= self.max
    }

    /// True iff `value` is inside `[min, max]` inclusive.
    pub fn contains(&self, value: T) -> bool {
        self.min <= value && value <= self.max
    }
}

// ---------------------------------------------------------------------------
// ConstraintParam catalog
// ---------------------------------------------------------------------------

/// Named parameter. Each variant maps to a documented constraint
/// row in `BRAIINSOS_REVERSE_ENGINEERING.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintParam {
    /// `Power` watts target — autotuner power-target mode.
    PowerTargetWatts,
    /// `TeraHashrate` target — autotuner hashrate-target mode.
    HashrateTargetThs,
    /// Fan speed percent (0-100).
    FanSpeedPct,
    /// Minimum fan speed percent.
    MinFanSpeedPct,
    /// Maximum fan speed percent.
    MaxFanSpeedPct,
    /// Target chip-die temperature (°C). Mining envelope center.
    TargetTemperatureC,
    /// Hot-threshold (°C). Triggers DPS scaling-down.
    HotTemperatureC,
    /// Dangerous-threshold (°C). Triggers emergency shutdown.
    DangerousTemperatureC,
    /// DPS power step (watts) — increment/decrement granularity.
    PowerStepWatts,
    /// DPS hashrate step (TH/s).
    HashrateStepThs,
    /// DPS minimum power target floor.
    MinPowerTargetWatts,
    /// DPS minimum hashrate target floor.
    MinHashrateTargetThs,
    /// DPS shutdown duration (hours) before retry.
    ShutdownDurationHours,
    /// Autotuner startup-delay window (minutes).
    AutotunerStartupMinutes,
    /// ATM temperature window (°C) for profile-step decisions.
    AtmTempWindowC,
}

/// Semantic grouping per RE doc §5/§6/§7 organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintCategory {
    Performance,
    Thermal,
    Fan,
    Autotuner,
}

impl ConstraintParam {
    /// Documented constraint anchor for this parameter (live `a lab unit`/
    /// `a lab unit` evidence-derived bounds + nameplate defaults).
    pub fn documented_u32(&self) -> Option<Constraint<u32>> {
        match self {
            // Power-side: u32 watts at the API surface; full Power
            // wire type uses u64 but constraint bounds fit in u32 for
            // every documented model.
            Self::PowerTargetWatts => Some(Constraint {
                min: 500,
                max: 5500,
                default: 3500,
            }),
            Self::PowerStepWatts => Some(Constraint {
                min: 10,
                max: 500,
                default: 100,
            }),
            Self::MinPowerTargetWatts => Some(Constraint {
                min: 100,
                max: 2000,
                default: 500,
            }),
            // Time-side
            Self::ShutdownDurationHours => Some(Constraint {
                min: 1,
                max: 168, // 1 week
                default: 4,
            }),
            Self::AutotunerStartupMinutes => Some(Constraint {
                min: 0,
                max: 240,
                default: 30,
            }),
            // Anything else is f64 or u8.
            _ => None,
        }
    }

    /// Documented constraint anchor for f64-typed parameters.
    pub fn documented_f64(&self) -> Option<Constraint<f64>> {
        match self {
            Self::HashrateTargetThs => Some(Constraint {
                min: 1.0,
                max: 500.0,
                default: 110.0,
            }),
            Self::HashrateStepThs => Some(Constraint {
                min: 0.5,
                max: 50.0,
                default: 5.0,
            }),
            Self::MinHashrateTargetThs => Some(Constraint {
                min: 0.5,
                max: 100.0,
                default: 10.0,
            }),
            // Temperature ranges (°C) per H-thermal-fan.md §2.1
            Self::TargetTemperatureC => Some(Constraint {
                min: 60.0,
                max: 90.0,
                default: 75.0,
            }),
            Self::HotTemperatureC => Some(Constraint {
                min: 70.0,
                max: 95.0,
                default: 85.0,
            }),
            Self::DangerousTemperatureC => Some(Constraint {
                min: 80.0,
                max: 100.0,
                default: 90.0,
            }),
            Self::AtmTempWindowC => Some(Constraint {
                min: 1.0,
                max: 20.0,
                default: 5.0,
            }),
            _ => None,
        }
    }

    /// Documented constraint anchor for u8 percentage parameters.
    pub fn documented_pct(&self) -> Option<Constraint<u8>> {
        match self {
            Self::FanSpeedPct => Some(Constraint {
                min: 0,
                max: 100,
                default: 70,
            }),
            Self::MinFanSpeedPct => Some(Constraint {
                min: 0,
                max: 100,
                default: 20,
            }),
            Self::MaxFanSpeedPct => Some(Constraint {
                min: 0,
                max: 100,
                default: 100,
            }),
            _ => None,
        }
    }

    /// Semantic grouping per RE doc.
    pub fn category(&self) -> ConstraintCategory {
        match self {
            Self::PowerTargetWatts
            | Self::HashrateTargetThs
            | Self::PowerStepWatts
            | Self::HashrateStepThs
            | Self::MinPowerTargetWatts
            | Self::MinHashrateTargetThs => ConstraintCategory::Performance,
            Self::TargetTemperatureC
            | Self::HotTemperatureC
            | Self::DangerousTemperatureC
            | Self::AtmTempWindowC => ConstraintCategory::Thermal,
            Self::FanSpeedPct | Self::MinFanSpeedPct | Self::MaxFanSpeedPct => {
                ConstraintCategory::Fan
            }
            Self::ShutdownDurationHours | Self::AutotunerStartupMinutes => {
                ConstraintCategory::Autotuner
            }
        }
    }
}

/// All ConstraintParam variants in stable iteration order.
pub const ALL_CONSTRAINT_PARAMS: &[ConstraintParam] = &[
    ConstraintParam::PowerTargetWatts,
    ConstraintParam::HashrateTargetThs,
    ConstraintParam::FanSpeedPct,
    ConstraintParam::MinFanSpeedPct,
    ConstraintParam::MaxFanSpeedPct,
    ConstraintParam::TargetTemperatureC,
    ConstraintParam::HotTemperatureC,
    ConstraintParam::DangerousTemperatureC,
    ConstraintParam::PowerStepWatts,
    ConstraintParam::HashrateStepThs,
    ConstraintParam::MinPowerTargetWatts,
    ConstraintParam::MinHashrateTargetThs,
    ConstraintParam::ShutdownDurationHours,
    ConstraintParam::AutotunerStartupMinutes,
    ConstraintParam::AtmTempWindowC,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_15_documented_params() {
        // Pin the catalog size so a refactor cannot silently remove
        // a param.
        assert_eq!(ALL_CONSTRAINT_PARAMS.len(), 15);
    }

    #[test]
    fn every_param_has_a_documented_constraint() {
        // Exactly one of (u32, f64, u8) is populated per param.
        for p in ALL_CONSTRAINT_PARAMS.iter().copied() {
            let u = p.documented_u32().is_some();
            let f = p.documented_f64().is_some();
            let pct = p.documented_pct().is_some();
            let count = (u as u8) + (f as u8) + (pct as u8);
            assert_eq!(
                count, 1,
                "{:?} must have exactly one documented constraint type, found {}",
                p, count
            );
        }
    }

    #[test]
    fn every_documented_constraint_is_well_ordered() {
        // min ≤ default ≤ max for every documented param.
        for p in ALL_CONSTRAINT_PARAMS.iter().copied() {
            if let Some(c) = p.documented_u32() {
                assert!(
                    c.is_well_ordered(),
                    "{:?} u32 constraint not well-ordered: {:?}",
                    p,
                    c
                );
            }
            if let Some(c) = p.documented_f64() {
                assert!(
                    c.is_well_ordered(),
                    "{:?} f64 constraint not well-ordered: {:?}",
                    p,
                    c
                );
            }
            if let Some(c) = p.documented_pct() {
                assert!(
                    c.is_well_ordered(),
                    "{:?} pct constraint not well-ordered: {:?}",
                    p,
                    c
                );
            }
        }
    }

    #[test]
    fn fan_speed_pct_constrained_to_0_to_100() {
        // Fan speed wire type is u8 0-100. Pin the bounds.
        for p in [
            ConstraintParam::FanSpeedPct,
            ConstraintParam::MinFanSpeedPct,
            ConstraintParam::MaxFanSpeedPct,
        ] {
            let c = p.documented_pct().unwrap();
            assert_eq!(c.min, 0);
            assert_eq!(c.max, 100);
        }
    }

    #[test]
    fn target_temperature_in_60_to_90_window() {
        // H-thermal-fan.md §2.1: target_temp default 55-75 °C; bounds
        // [60, 90] are conservative.
        let c = ConstraintParam::TargetTemperatureC
            .documented_f64()
            .unwrap();
        assert!(c.min >= 60.0);
        assert!(c.max <= 90.0);
    }

    #[test]
    fn dangerous_above_hot_above_target() {
        // Three-tier hierarchy must be strictly increasing per RE doc
        // §2.1.
        let target = ConstraintParam::TargetTemperatureC
            .documented_f64()
            .unwrap();
        let hot = ConstraintParam::HotTemperatureC.documented_f64().unwrap();
        let dangerous = ConstraintParam::DangerousTemperatureC
            .documented_f64()
            .unwrap();
        assert!(
            target.default < hot.default && hot.default < dangerous.default,
            "tier defaults not strictly increasing: target={} hot={} dangerous={}",
            target.default,
            hot.default,
            dangerous.default
        );
    }

    #[test]
    fn power_target_default_matches_s21_nameplate() {
        // S21 nameplate ~3,500 W (per  bm1368 silicon profile).
        // The default power target should land near that nameplate.
        let c = ConstraintParam::PowerTargetWatts.documented_u32().unwrap();
        assert_eq!(c.default, 3500);
        assert!(c.min < c.default && c.default < c.max);
    }

    #[test]
    fn hashrate_target_uses_f64_not_u32() {
        // RE doc §9.2.28: TeraHashrate carries f64 (terahash_per_second).
        // Pin that hashrate is f64 (preserves fractional TH/s).
        let f = ConstraintParam::HashrateTargetThs.documented_f64();
        let u = ConstraintParam::HashrateTargetThs.documented_u32();
        assert!(f.is_some());
        assert!(u.is_none());
    }

    #[test]
    fn power_target_uses_u32_not_f64() {
        // RE doc §9.2.28: Power.watt is u64 — but the constraint
        // bounds fit in u32 for every model. Pin the typed surface.
        let u = ConstraintParam::PowerTargetWatts.documented_u32();
        let f = ConstraintParam::PowerTargetWatts.documented_f64();
        assert!(u.is_some());
        assert!(f.is_none());
    }

    #[test]
    fn category_grouping_matches_re_doc() {
        // Performance bucket
        for p in [
            ConstraintParam::PowerTargetWatts,
            ConstraintParam::HashrateTargetThs,
            ConstraintParam::PowerStepWatts,
            ConstraintParam::HashrateStepThs,
            ConstraintParam::MinPowerTargetWatts,
            ConstraintParam::MinHashrateTargetThs,
        ] {
            assert_eq!(p.category(), ConstraintCategory::Performance);
        }
        // Thermal bucket
        for p in [
            ConstraintParam::TargetTemperatureC,
            ConstraintParam::HotTemperatureC,
            ConstraintParam::DangerousTemperatureC,
            ConstraintParam::AtmTempWindowC,
        ] {
            assert_eq!(p.category(), ConstraintCategory::Thermal);
        }
        // Fan bucket
        for p in [
            ConstraintParam::FanSpeedPct,
            ConstraintParam::MinFanSpeedPct,
            ConstraintParam::MaxFanSpeedPct,
        ] {
            assert_eq!(p.category(), ConstraintCategory::Fan);
        }
        // Autotuner bucket
        for p in [
            ConstraintParam::ShutdownDurationHours,
            ConstraintParam::AutotunerStartupMinutes,
        ] {
            assert_eq!(p.category(), ConstraintCategory::Autotuner);
        }
    }

    #[test]
    fn constraint_contains_predicate_works() {
        let c = ConstraintParam::FanSpeedPct.documented_pct().unwrap();
        assert!(c.contains(0));
        assert!(c.contains(50));
        assert!(c.contains(100));
        // u8 max is 255; out-of-range below isn't representable, but
        // contains() should still return false for max+1 patterns.
        // Verify with a custom Constraint.
        let custom = Constraint {
            min: 10u32,
            max: 20,
            default: 15,
        };
        assert!(!custom.contains(5));
        assert!(custom.contains(10));
        assert!(custom.contains(15));
        assert!(custom.contains(20));
        assert!(!custom.contains(21));
    }

    #[test]
    fn constraint_round_trips_through_serde_for_u32_and_f64() {
        let u = Constraint {
            min: 100u32,
            max: 5500,
            default: 3500,
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: Constraint<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);

        let f = Constraint {
            min: 60.0_f64,
            max: 90.0,
            default: 75.0,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: Constraint<f64> = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn constraint_param_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ConstraintParam::PowerTargetWatts).unwrap(),
            "\"power_target_watts\""
        );
        assert_eq!(
            serde_json::to_string(&ConstraintParam::HashrateTargetThs).unwrap(),
            "\"hashrate_target_ths\""
        );
        assert_eq!(
            serde_json::to_string(&ConstraintParam::DangerousTemperatureC).unwrap(),
            "\"dangerous_temperature_c\""
        );
    }

    #[test]
    fn category_round_trips_through_serde() {
        for c in [
            ConstraintCategory::Performance,
            ConstraintCategory::Thermal,
            ConstraintCategory::Fan,
            ConstraintCategory::Autotuner,
        ] {
            let json = serde_json::to_string(&c).unwrap();
            let back: ConstraintCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn shutdown_duration_max_is_one_week() {
        // ShutdownDurationHours bound — operator-meaningful max.
        let c = ConstraintParam::ShutdownDurationHours
            .documented_u32()
            .unwrap();
        assert_eq!(c.max, 168); // 7 days
        assert_eq!(c.min, 1);
    }

    #[test]
    fn min_max_fan_speed_defaults_match_typical_curve() {
        // Min default 20% (quiet idle), max default 100% (emergency
        // ceiling). Pin so a refactor doesn't accidentally invert.
        let min_c = ConstraintParam::MinFanSpeedPct.documented_pct().unwrap();
        let max_c = ConstraintParam::MaxFanSpeedPct.documented_pct().unwrap();
        assert!(min_c.default < max_c.default);
        assert_eq!(min_c.default, 20);
        assert_eq!(max_c.default, 100);
    }
}
