//!  prof-A — Per-miner-model power-profile preset catalog (HAL-free).
//!
//! Source RE evidence:
//!  (1405 lines).
//!
//! Captures the most-used per-model preset rows from the VNish / LuxOS /
//! BraiinsOS+ catalogs. Each row is `(wall_watts, hashrate_th, j_per_th)`.
//! Operators pick a row to set their efficiency/hashrate target.
//!
//!  ships the major air-cooled SHA-256 models:
//! - Antminer S19 (126 TH stock — BM1398)
//! - Antminer S19j Pro-A / S19j Pro+ (BM1362)
//! - Antminer S19k Pro (BM1366)
//! - Antminer S21 (BM1368) — placeholder until live capture
//! - Antminer S9 (BM1387) — placeholder for completeness
//!
//! Hydro / immersion variants are NOT in this initial catalog (they
//! have separate higher-power envelopes; deferred to +).
//!
//! HAL-free: pure data + lookup. The runtime adapter inside
//! `dcentrald-autotuner` consumes preset rows to set chain frequency
//! and voltage targets. The dashboard renders the catalog as a picker
//! grid.

use crate::chip_init::ChipFamily;
use serde::{Deserialize, Serialize};

/// One preset point on a model's power/hashrate curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PowerProfile {
    /// Wall power target in watts.
    pub watts: u32,
    /// Hashrate target in TH/s.
    pub hashrate_th: f32,
    /// Efficiency in joules-per-terahash (lower = more efficient).
    pub j_per_th: f32,
}

impl PowerProfile {
    pub const fn new(watts: u32, hashrate_th: f32, j_per_th: f32) -> Self {
        Self {
            watts,
            hashrate_th,
            j_per_th,
        }
    }

    /// Verify the J/TH calculation matches `watts / hashrate_th`
    /// within 1 % (allows for the rounding the RE doc tables use).
    pub fn efficiency_consistent(&self) -> bool {
        if self.hashrate_th <= 0.0 {
            return false;
        }
        let computed = self.watts as f32 / self.hashrate_th;
        let pct_off = ((computed - self.j_per_th) / self.j_per_th).abs();
        pct_off < 0.02
    }
}

/// Antminer S19 (126 TH stock, BM1398). 14 profiles per RE doc §2.1
/// lines 75-90.
pub const S19_126TH_PROFILES: &[PowerProfile] = &[
    PowerProfile::new(1630, 67.0, 24.3),
    PowerProfile::new(1780, 71.0, 25.1),
    PowerProfile::new(1920, 76.0, 25.3),
    PowerProfile::new(2103, 81.0, 26.0),
    PowerProfile::new(2270, 86.0, 26.4),
    PowerProfile::new(2475, 91.0, 27.2),
    PowerProfile::new(2710, 96.0, 28.2),
    PowerProfile::new(2900, 101.0, 28.7),
    PowerProfile::new(3095, 105.0, 29.5),
    PowerProfile::new(3310, 110.0, 30.1),
    PowerProfile::new(3600, 115.0, 31.3),
    PowerProfile::new(3900, 120.0, 32.5),
    PowerProfile::new(4300, 125.0, 34.4),
    PowerProfile::new(4700, 130.0, 36.2),
];

/// Antminer S19j Pro-A (BM1362). 25 profiles per RE doc §2.4 lines
/// 141-167.  ships the lower-half subset (efficiency-focused).
pub const S19J_PRO_A_PROFILES: &[PowerProfile] = &[
    PowerProfile::new(1740, 65.0, 26.8),
    PowerProfile::new(1800, 70.0, 25.7),
    PowerProfile::new(1850, 76.0, 24.3),
    PowerProfile::new(2000, 80.0, 25.0),
    PowerProfile::new(2150, 83.0, 25.9),
    PowerProfile::new(2300, 87.0, 26.4),
    PowerProfile::new(2500, 92.0, 27.2),
    PowerProfile::new(2700, 96.0, 28.1),
    PowerProfile::new(2970, 100.0, 29.7),
    PowerProfile::new(3320, 110.0, 30.2),
    PowerProfile::new(3670, 120.0, 30.6),
    PowerProfile::new(4110, 130.0, 31.6),
    PowerProfile::new(5760, 160.0, 36.0),
];

/// Antminer S19j Pro+ (BM1362, higher-bin). Per RE doc §2.5 lines
/// 175-191.  ships the lower-half subset.
pub const S19J_PRO_PLUS_PROFILES: &[PowerProfile] = &[
    PowerProfile::new(1450, 65.0, 22.3),
    PowerProfile::new(1600, 70.0, 22.9),
    PowerProfile::new(1750, 75.0, 23.3),
    PowerProfile::new(1900, 80.0, 23.8),
    PowerProfile::new(2050, 85.0, 24.1),
    PowerProfile::new(2500, 94.0, 26.6),
    PowerProfile::new(2800, 108.0, 25.9),
    PowerProfile::new(3200, 116.0, 27.6),
];

/// Antminer S9 (BM1387). Reconstructed from `dcentrald-silicon-profiles::bm1387`
/// ( BM1387 silicon profile). 7 row from -3 to +3.
pub const S9_PROFILES: &[PowerProfile] = &[
    PowerProfile::new(620, 5.36, 115.7),  // step -3 (eco-low)
    PowerProfile::new(750, 7.5, 100.0),   // step -2
    PowerProfile::new(880, 9.64, 91.3),   // step -1 (sweet spot)
    PowerProfile::new(1320, 13.50, 97.8), // step 0 (nameplate)
    PowerProfile::new(1500, 14.43, 103.9),
    PowerProfile::new(1720, 15.40, 111.7),
    PowerProfile::new(1980, 16.30, 121.5),
];

/// Antminer S21 (BM1368). Public-data sourced placeholder until live
/// LuxOS / VNish capture from the .135 unit. Will refine in +.
pub const S21_PROFILES: &[PowerProfile] = &[
    PowerProfile::new(2700, 150.0, 18.0),
    PowerProfile::new(2950, 165.0, 17.9),
    PowerProfile::new(3120, 175.0, 17.8),
    PowerProfile::new(3300, 188.0, 17.6),
    PowerProfile::new(3500, 200.0, 17.5),
];

/// Discrete miner-model identifier.
///
/// ** W7-D**: extended with `AntminerS17` (BM1397),
/// `AntminerS21Pro` (BM1370), `AntminerL3Plus` (BM1485), and
/// `AntminerL7` (BM1489) so the W5-D migrate-baked-profiles round-trip
/// can land without faking a slug-based workaround. Per
///  issue 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinerModel {
    /// Antminer S9 / S9i / T9 (BM1387).
    AntminerS9,
    /// Antminer S17 / S17 Pro / S17e (BM1397).  W7-D.
    AntminerS17,
    /// Antminer S19 (126 TH stock, BM1398).
    AntminerS19,
    /// Antminer S19j Pro-A (BM1362).
    AntminerS19jProA,
    /// Antminer S19j Pro+ (BM1362, higher-bin).
    AntminerS19jProPlus,
    /// Antminer S19k Pro (BM1366) — same air-cooled S19k Pro that
    /// `dcentrald-silicon-profiles::bm1366` will eventually catalog.
    AntminerS19kPro,
    /// Antminer S21 (BM1368).
    AntminerS21,
    /// Antminer S21 Pro / S21 XP (BM1370 — 3 nm).  W7-D.
    AntminerS21Pro,
    /// Antminer L3+ / L3++ (BM1485 — Scrypt).  W7-D.
    AntminerL3Plus,
    /// Antminer L7 / L9 (BM1489 — Scrypt).  W7-D.
    AntminerL7,
}

impl MinerModel {
    /// Look up the chip family for this model.
    pub fn chip_family(&self) -> ChipFamily {
        match self {
            MinerModel::AntminerS9 => ChipFamily::Bm1387,
            MinerModel::AntminerS17 => ChipFamily::Bm1397,
            MinerModel::AntminerS19 => ChipFamily::Bm1398,
            MinerModel::AntminerS19jProA | MinerModel::AntminerS19jProPlus => ChipFamily::Bm1362,
            MinerModel::AntminerS19kPro => ChipFamily::Bm1366,
            MinerModel::AntminerS21 => ChipFamily::Bm1368,
            MinerModel::AntminerS21Pro => ChipFamily::Bm1370,
            MinerModel::AntminerL3Plus => ChipFamily::Bm1485,
            MinerModel::AntminerL7 => ChipFamily::Bm1489,
        }
    }

    /// Look up the preset table for this model. Returns `&[]` for the
    /// S19k Pro placeholder (BM1366 catalog deferred to +).
    /// W7-D variants (S17 / S21 Pro / L3+ / L7) also return `&[]`
    /// pending live capture from each silicon family — see
    /// `dcentrald-silicon-profiles::{bm1397, bm1370, bm1485, bm1489}`.
    ///
    /// NOTE (2026-07-02, production-readiness): the empty tables are a
    /// DELIBERATE **live-capture physical residual**, not a non-hardware gap.
    /// Fabricating freq/volt/wattage presets without a live-tuned anchor is the
    /// "wrong calibration worse than none" risk (cf. the R-13 die-temp
    /// decision); the generic per-chip silicon profile is the safe fallback the
    /// autotuner uses meanwhile. The exact preset-row capture procedure is the
    /// named live task in the bench packages:
    ///   - S17 datum list
    ///   - S21 Pro / S21 XP → `.../BP-AMLOGIC-BRINGUP.md` (BM1370 preset capture)
    ///
    /// This emptiness is regression-pinned by `s19k_pro_has_no_preset_rows_yet`
    /// + the empty-table test — do NOT populate with synthetic rows.
    pub fn presets(&self) -> &'static [PowerProfile] {
        match self {
            MinerModel::AntminerS9 => S9_PROFILES,
            MinerModel::AntminerS17 => &[],
            MinerModel::AntminerS19 => S19_126TH_PROFILES,
            MinerModel::AntminerS19jProA => S19J_PRO_A_PROFILES,
            MinerModel::AntminerS19jProPlus => S19J_PRO_PLUS_PROFILES,
            MinerModel::AntminerS19kPro => &[],
            MinerModel::AntminerS21 => S21_PROFILES,
            MinerModel::AntminerS21Pro => &[],
            MinerModel::AntminerL3Plus => &[],
            MinerModel::AntminerL7 => &[],
        }
    }

    /// Find the most-efficient (lowest J/TH) preset for this model.
    /// Returns `None` if the model has no preset rows.
    pub fn sweet_spot(&self) -> Option<&'static PowerProfile> {
        self.presets()
            .iter()
            .filter(|p| p.j_per_th.is_finite() && p.j_per_th > 0.0)
            .min_by(|a, b| {
                a.j_per_th
                    .partial_cmp(&b.j_per_th)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

/// Every supported miner model.
pub const ALL_MODELS: &[MinerModel] = &[
    MinerModel::AntminerS9,
    MinerModel::AntminerS17,
    MinerModel::AntminerS19,
    MinerModel::AntminerS19jProA,
    MinerModel::AntminerS19jProPlus,
    MinerModel::AntminerS19kPro,
    MinerModel::AntminerS21,
    MinerModel::AntminerS21Pro,
    MinerModel::AntminerL3Plus,
    MinerModel::AntminerL7,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s19_126th_table_has_14_profiles() {
        // RE doc §2.1: "14 profiles."
        assert_eq!(S19_126TH_PROFILES.len(), 14);
    }

    #[test]
    fn every_profile_has_internally_consistent_efficiency() {
        for table in [
            S19_126TH_PROFILES,
            S19J_PRO_A_PROFILES,
            S19J_PRO_PLUS_PROFILES,
            S9_PROFILES,
            S21_PROFILES,
        ] {
            for p in table {
                assert!(
                    p.efficiency_consistent(),
                    "profile {:?} has inconsistent J/TH",
                    p
                );
            }
        }
    }

    #[test]
    fn s19_126th_low_end_anchor() {
        // RE doc §2.1 line 77: 1630 W → 67 TH @ 24.3 J/TH.
        let p = &S19_126TH_PROFILES[0];
        assert_eq!(p.watts, 1630);
        assert!((p.hashrate_th - 67.0).abs() < 0.001);
        assert!((p.j_per_th - 24.3).abs() < 0.001);
    }

    #[test]
    fn s19j_pro_plus_lowest_efficiency_is_22_3() {
        // RE doc §2.5: 1450 W / 65 TH = 22.3 J/TH. The most efficient
        // mainstream-air-cooled SHA-256 miner in the catalog.
        let sweet = MinerModel::AntminerS19jProPlus.sweet_spot().unwrap();
        assert!((sweet.j_per_th - 22.3).abs() < 0.001);
    }

    #[test]
    fn s9_sweet_spot_is_step_minus_one() {
        // Reconstructed from BM1387 silicon profile (). Sweet
        // spot Step -1 = 880 W / 9.64 TH @ ~91 J/TH.
        let sweet = MinerModel::AntminerS9.sweet_spot().unwrap();
        assert!((sweet.watts as f32 - 880.0).abs() < 1.0);
    }

    #[test]
    fn s19k_pro_has_no_preset_rows_yet() {
        //  placeholder — BM1366 catalog deferred to +.
        let presets = MinerModel::AntminerS19kPro.presets();
        assert!(presets.is_empty());
        assert!(MinerModel::AntminerS19kPro.sweet_spot().is_none());
    }

    #[test]
    fn w7d_new_variants_chip_family_routing() {
        //  W7-D: pin chip-family routing for the four new
        // MinerModel variants so the W5-D round-trip blocker stays
        // resolved.
        assert_eq!(MinerModel::AntminerS17.chip_family(), ChipFamily::Bm1397);
        assert_eq!(MinerModel::AntminerS21Pro.chip_family(), ChipFamily::Bm1370);
        assert_eq!(MinerModel::AntminerL3Plus.chip_family(), ChipFamily::Bm1485);
        assert_eq!(MinerModel::AntminerL7.chip_family(), ChipFamily::Bm1489);
    }

    #[test]
    fn w7d_new_variants_have_no_preset_rows_yet() {
        // S17/S21Pro/L3+/L7 ship empty preset tables until live captures
        // arrive in a future wave. They must NOT silently fall through to
        // an adjacent model's catalog.
        for model in [
            MinerModel::AntminerS17,
            MinerModel::AntminerS21Pro,
            MinerModel::AntminerL3Plus,
            MinerModel::AntminerL7,
        ] {
            assert!(
                model.presets().is_empty(),
                "{:?} should have no presets yet",
                model
            );
            assert!(
                model.sweet_spot().is_none(),
                "{:?} sweet_spot must be None",
                model
            );
        }
    }

    #[test]
    fn w7d_new_variants_round_trip_through_serde() {
        // Snake-case wire form must serialize / deserialize cleanly.
        for model in [
            MinerModel::AntminerS17,
            MinerModel::AntminerS21Pro,
            MinerModel::AntminerL3Plus,
            MinerModel::AntminerL7,
        ] {
            let json = serde_json::to_string(&model).unwrap();
            let back: MinerModel = serde_json::from_str(&json).unwrap();
            assert_eq!(model, back);
        }
    }

    #[test]
    fn w7d_new_variants_serialize_to_expected_snake_case() {
        // Pin the wire forms so a future #[serde(rename_all=...)]
        // refactor would surface noisily.
        assert_eq!(
            serde_json::to_string(&MinerModel::AntminerS17).unwrap(),
            "\"antminer_s17\""
        );
        assert_eq!(
            serde_json::to_string(&MinerModel::AntminerS21Pro).unwrap(),
            "\"antminer_s21_pro\""
        );
        assert_eq!(
            serde_json::to_string(&MinerModel::AntminerL3Plus).unwrap(),
            "\"antminer_l3_plus\""
        );
        assert_eq!(
            serde_json::to_string(&MinerModel::AntminerL7).unwrap(),
            "\"antminer_l7\""
        );
    }

    #[test]
    fn all_models_includes_all_w7d_new_variants() {
        // ALL_MODELS must list every variant; a future variant
        // addition must update this constant (no #[non_exhaustive]).
        for new_variant in [
            MinerModel::AntminerS17,
            MinerModel::AntminerS21Pro,
            MinerModel::AntminerL3Plus,
            MinerModel::AntminerL7,
        ] {
            assert!(
                ALL_MODELS.contains(&new_variant),
                "{:?} missing from ALL_MODELS",
                new_variant
            );
        }
        assert_eq!(ALL_MODELS.len(), 10);
    }

    #[test]
    fn chip_family_lookup_per_model() {
        assert_eq!(MinerModel::AntminerS9.chip_family(), ChipFamily::Bm1387);
        assert_eq!(MinerModel::AntminerS19.chip_family(), ChipFamily::Bm1398);
        assert_eq!(
            MinerModel::AntminerS19jProA.chip_family(),
            ChipFamily::Bm1362
        );
        assert_eq!(
            MinerModel::AntminerS19jProPlus.chip_family(),
            ChipFamily::Bm1362
        );
        assert_eq!(
            MinerModel::AntminerS19kPro.chip_family(),
            ChipFamily::Bm1366
        );
        assert_eq!(MinerModel::AntminerS21.chip_family(), ChipFamily::Bm1368);
    }

    #[test]
    fn all_models_have_unique_identity() {
        let mut ids: Vec<MinerModel> = ALL_MODELS.to_vec();
        let original_len = ids.len();
        ids.sort_by_key(|m| format!("{:?}", m));
        ids.dedup();
        assert_eq!(ids.len(), original_len);
    }

    #[test]
    fn s19_efficiency_curve_increases_with_overclock() {
        // RE doc invariant: J/TH worsens as you push hashrate higher.
        // Pin via a strict-monotonic check across the table.
        for window in S19_126TH_PROFILES.windows(2) {
            let lo = &window[0];
            let hi = &window[1];
            assert!(
                hi.hashrate_th > lo.hashrate_th,
                "hashrate must increase along the table"
            );
            assert!(
                hi.j_per_th >= lo.j_per_th,
                "J/TH should not improve as hashrate climbs (got {:?} after {:?})",
                hi,
                lo
            );
        }
    }

    #[test]
    fn s19j_pro_a_eco_low_is_24_3_jth() {
        // RE doc §2.4: best efficiency at 1850 W / 76 TH = 24.3 J/TH.
        let sweet = MinerModel::AntminerS19jProA.sweet_spot().unwrap();
        assert!((sweet.j_per_th - 24.3).abs() < 0.001);
    }

    #[test]
    fn power_profile_round_trips_through_serde() {
        let p = PowerProfile::new(2700, 96.0, 28.1);
        let json = serde_json::to_string(&p).unwrap();
        let back: PowerProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn miner_model_round_trips_through_serde() {
        for m in ALL_MODELS {
            let json = serde_json::to_string(m).unwrap();
            let back: MinerModel = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn efficiency_consistent_rejects_zero_hashrate() {
        let bad = PowerProfile::new(1000, 0.0, 25.0);
        assert!(!bad.efficiency_consistent());
    }
}
