//! Efficiency helpers and home-mining profile recommendations.
//!
//! Builds on the canonical silicon characterization tables in `bm1362.rs`
//! (and future BM1366/BM1368/BM1387/BM1398 modules) to expose:
//!
//! - Wall-efficiency curves (J/TH) computed from `(watts, hashrate)` rows.
//! - Sweet-spot detection (lowest J/TH).
//! - Home-mining profile recommendations (Quiet, MaxEfficiency, Whisper,
//!   Heater) per the source RE document §11.
//! - ATM (Advanced Thermal Management) band suggestions.
//!
//! All helpers are pure functions over `Profile` slices — no HAL, no I/O.
//!
//! Source:

use crate::{Profile, SiliconTable};

/// Home-mining use-case identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeMiningMode {
    /// Whisper mode — minimal watts, ambient room with no dedicated cooling.
    Whisper,
    /// Quiet & efficient — recommended default for home use.
    Quiet,
    /// Maximum hashrate per watt (efficient at slightly higher hashrate
    /// than Quiet, suitable for cheap-power regions).
    MaxEfficiency,
    /// Standard space heater — DCENT_OS "Heater" mode default.
    Heater,
}

/// A home-mining recommendation: which profile to pick for a given mode.
#[derive(Debug, Clone, Copy)]
pub struct HomeMiningRecommendation {
    pub mode: HomeMiningMode,
    pub profile: &'static Profile,
    /// Short rationale string, suitable for showing in dashboard tooltips.
    pub rationale: &'static str,
}

/// Recommend a profile for the given home-mining mode on a BM1362 silicon
/// table. Selections are anchored to specific steps from the source RE
/// document §11 — these are NOT computed automatically because the
/// recommendations carry operator-facing rationale (noise, BTU/h, room
/// size) that cannot be inferred from the watts/hashrate columns alone.
///
/// Returns `None` if the table doesn't have a row at the recommended step.
pub fn home_mining_recommendation_bm1362(
    table: &SiliconTable,
    mode: HomeMiningMode,
) -> Option<HomeMiningRecommendation> {
    let step = match mode {
        // Per RE doc §11.3 — barely above nameplate efficiency, ~40%
        // nameplate hashrate, stays cool enough for small rooms.
        HomeMiningMode::Whisper => -13,
        // Per RE doc §11.1 — 6.6% better than nameplate, 45% less wall
        // power, comparable to a small space heater for one bedroom.
        HomeMiningMode::Quiet => -9,
        // Per RE doc §11.2 — nearly identical efficiency to Quiet,
        // 53% less wall power than nameplate.
        HomeMiningMode::MaxEfficiency => -11,
        // Per RE doc §11.4 — recommended default for the "Heater" mode.
        HomeMiningMode::Heater => -7,
    };

    let profile = table.by_step(step)?;
    let rationale = match mode {
        HomeMiningMode::Whisper => {
            "barely above nameplate efficiency, ~40% nameplate hashrate, fits a small room"
        }
        HomeMiningMode::Quiet => {
            "6.6% better efficiency than nameplate, 45% less wall power, single-bedroom space heater"
        }
        HomeMiningMode::MaxEfficiency => {
            "nearly identical efficiency to Quiet, 53% less wall power, optimal hashrate-per-watt"
        }
        HomeMiningMode::Heater => {
            "~6% better efficiency than nameplate, sized for a 250 ft\u{00B2} room at low ambient"
        }
    };

    Some(HomeMiningRecommendation {
        mode,
        profile,
        rationale,
    })
}

/// Recommended ATM (Advanced Thermal Management) band for autonomous
/// home mining.
///
/// Per the source RE doc §11.6: keeps the autotuner inside the J/TH
/// efficiency basin and never enters either inefficient extreme.
#[derive(Debug, Clone, Copy)]
pub struct AtmBand {
    /// Lower bound profile (most-efficient floor at low temperature).
    pub min_profile: &'static Profile,
    /// Upper bound profile (most-efficient ceiling at high temperature).
    pub max_profile: &'static Profile,
}

/// Recommended ATM band for BM1362 home mining. Floor = Step -11
/// (270 MHz / 1466 W / 27.98 J/TH), ceiling = Step -6
/// (395 MHz / 2142 W / 27.93 J/TH). Both ends are inside the J/TH basin.
pub fn home_mining_atm_band_bm1362(table: &SiliconTable) -> Option<AtmBand> {
    Some(AtmBand {
        min_profile: table.by_step(-11)?,
        max_profile: table.by_step(-6)?,
    })
}

/// Build a J/TH efficiency curve over the entire silicon table.
///
/// Returns `(step, watts_per_ths)` pairs in step order. Skips rows whose
/// hashrate is zero (would divide by zero). Useful for dashboards that
/// want to render the curve as a chart.
pub fn efficiency_curve(table: &SiliconTable) -> Vec<(i32, f32)> {
    table
        .profiles
        .iter()
        .filter_map(|p| p.watts_per_ths().map(|eff| (p.step, eff)))
        .collect()
}

/// Find the band of profiles whose J/TH is within `tolerance` of the
/// minimum (most-efficient) row.
///
/// Returns the (min_step, max_step) of that band. With `tolerance = 0.5`
/// J/TH this returns the "efficiency basin" — the range of profiles where
/// efficiency is essentially as good as the sweet spot. Per the BM1362
/// RE doc §1.4 the basin is Step -11..=-7 within ~0.4 J/TH of optimum.
///
/// Returns `None` if the table is empty or has no rows with finite
/// efficiency.
pub fn efficiency_basin(table: &SiliconTable, tolerance_jth: f32) -> Option<(i32, i32)> {
    let min = table
        .profiles
        .iter()
        .filter_map(|p| p.watts_per_ths().map(|e| (p.step, e)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;

    let basin: Vec<i32> = table
        .profiles
        .iter()
        .filter_map(|p| {
            p.watts_per_ths().and_then(|eff| {
                if (eff - min.1).abs() <= tolerance_jth {
                    Some(p.step)
                } else {
                    None
                }
            })
        })
        .collect();

    if basin.is_empty() {
        return None;
    }
    Some((*basin.iter().min().unwrap(), *basin.iter().max().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm1362::BM1362_TABLE;

    #[test]
    fn home_mining_quiet_picks_step_minus_9() {
        let rec = home_mining_recommendation_bm1362(&BM1362_TABLE, HomeMiningMode::Quiet).unwrap();
        assert_eq!(rec.profile.step, -9);
        assert_eq!(rec.profile.freq_mhz, 320);
    }

    #[test]
    fn home_mining_heater_picks_step_minus_7() {
        let rec = home_mining_recommendation_bm1362(&BM1362_TABLE, HomeMiningMode::Heater).unwrap();
        assert_eq!(rec.profile.step, -7);
        assert_eq!(rec.profile.freq_mhz, 370);
    }

    #[test]
    fn home_mining_whisper_picks_step_minus_13() {
        let rec =
            home_mining_recommendation_bm1362(&BM1362_TABLE, HomeMiningMode::Whisper).unwrap();
        assert_eq!(rec.profile.step, -13);
        assert_eq!(rec.profile.freq_mhz, 220);
    }

    #[test]
    fn home_mining_max_efficiency_picks_step_minus_11() {
        let rec = home_mining_recommendation_bm1362(&BM1362_TABLE, HomeMiningMode::MaxEfficiency)
            .unwrap();
        assert_eq!(rec.profile.step, -11);
        assert_eq!(rec.profile.freq_mhz, 270);
    }

    #[test]
    fn home_mining_recommendations_carry_operator_rationale() {
        // Every recommendation must come with a non-empty rationale
        // string for the dashboard tooltip.
        for mode in [
            HomeMiningMode::Whisper,
            HomeMiningMode::Quiet,
            HomeMiningMode::MaxEfficiency,
            HomeMiningMode::Heater,
        ] {
            let rec = home_mining_recommendation_bm1362(&BM1362_TABLE, mode).unwrap();
            assert!(
                !rec.rationale.is_empty(),
                "mode {:?} has no rationale",
                mode
            );
        }
    }

    #[test]
    fn atm_band_sits_inside_efficiency_basin() {
        // Per RE doc §11.6 the recommended ATM band is Step -11..=-6
        // — both endpoints are inside the J/TH basin.
        let band = home_mining_atm_band_bm1362(&BM1362_TABLE).unwrap();
        assert_eq!(band.min_profile.step, -11);
        assert_eq!(band.max_profile.step, -6);

        // Both endpoints should be within ~0.4 J/TH of the sweet spot.
        let sweet_eff = BM1362_TABLE
            .sweet_spot_profile()
            .unwrap()
            .watts_per_ths()
            .unwrap();
        let min_eff = band.min_profile.watts_per_ths().unwrap();
        let max_eff = band.max_profile.watts_per_ths().unwrap();
        assert!((min_eff - sweet_eff).abs() < 0.5);
        assert!((max_eff - sweet_eff).abs() < 0.5);
    }

    #[test]
    fn efficiency_curve_has_one_point_per_profile_row() {
        let curve = efficiency_curve(&BM1362_TABLE);
        assert_eq!(curve.len(), BM1362_TABLE.profiles.len());
    }

    #[test]
    fn efficiency_curve_is_step_ordered() {
        let curve = efficiency_curve(&BM1362_TABLE);
        for window in curve.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "curve not step-ordered: {} not < {}",
                window[0].0,
                window[1].0
            );
        }
    }

    #[test]
    fn efficiency_basin_widens_with_increasing_tolerance() {
        // The RE doc §1.4 prose says "Step -11..=-7 within ~0.4 J/TH of
        // optimum" but the actual numbers in the table show -6 also sits
        // 0.33 J/TH from the sweet spot. Pin the actual computed bands
        // at several tolerances rather than the doc's loose "~0.4":
        //
        //   tol = 0.05: only -9, -8 are within (deltas 0.000, 0.041)
        //   tol = 0.20: -10..=-7 (deltas 0.11, 0.00, 0.04, 0.17)
        //   tol = 0.40: -11..=-6 (deltas 0.38, ..., 0.33)

        let (low, high) = efficiency_basin(&BM1362_TABLE, 0.05).unwrap();
        assert_eq!((low, high), (-9, -8));

        let (low, high) = efficiency_basin(&BM1362_TABLE, 0.20).unwrap();
        assert_eq!((low, high), (-10, -7));

        let (low, high) = efficiency_basin(&BM1362_TABLE, 0.40).unwrap();
        assert_eq!((low, high), (-11, -6));
    }

    #[test]
    fn efficiency_basin_zero_tolerance_returns_only_sweet_spot() {
        let (low, high) = efficiency_basin(&BM1362_TABLE, 0.0).unwrap();
        assert_eq!(low, -9);
        assert_eq!(high, -9);
    }

    #[test]
    fn home_mining_modes_are_strictly_lower_watts_than_default() {
        // Every home-mining mode must use less wall power than the
        // nameplate default — that's the whole point of having them.
        let default_watts = BM1362_TABLE
            .default_profile()
            .unwrap()
            .wall_watts
            .expect("BM1362 default profile is baked-confirmed; wall_watts is Some(...)");
        for mode in [
            HomeMiningMode::Whisper,
            HomeMiningMode::Quiet,
            HomeMiningMode::MaxEfficiency,
            HomeMiningMode::Heater,
        ] {
            let rec = home_mining_recommendation_bm1362(&BM1362_TABLE, mode).unwrap();
            let rec_watts = rec
                .profile
                .wall_watts
                .expect("BM1362 home-mining-recommended row is baked-confirmed");
            assert!(
                rec_watts < default_watts,
                "mode {:?}: {} W is not less than default {} W",
                mode,
                rec_watts,
                default_watts
            );
        }
    }
}
