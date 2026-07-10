//! Per-model power / efficiency operating-point catalog (S9 → S21).
//!
//! ## Why this module exists (and is additive, never a regression)
//!
//! The existing [`crate::Profile`] / [`crate::SiliconTable`] catalog is a
//! discrete *integer-step* curve per **chip family** (BM1387, BM1398, …).
//! It is the canonical autotuner step ladder and stays the source of truth
//! for the freq/voltage/step machinery. But it has two structural limits a
//! per-MODEL power harvest needs to overcome:
//!
//!   1. One chip family maps to many MODELS with different geometry and
//!      different watt curves (e.g. BM1397 → S17 Pro / S17+ / T17 / T17+;
//!      BM1398 → S19 / S19 Pro / S19a Pro / S19 Pro Hydro / T19). A single
//!      `SiliconTable` cannot hold all of those curves.
//!   2. `Profile` has no per-board split, no MEASURED-vs-INFERRED-vs-GAP
//!      confidence field, and no per-datum source string. A wrong watt feeds
//!      a wrong autotuner power estimate → thermal / breaker safety risk, so
//!      provenance must be carried *in the data*, never inferred at the call
//!      site.
//!
//! This module is the **harvested, cited, per-model** companion catalog. It
//! does NOT touch `Profile`/`SiliconTable` or the autotuner `c_eff`
//! load-bearing path — it is a new read-only data surface the autotuner and
//! power-target code MAY consult for measured/vendor-extracted watt curves
//! when they exist.
//!
//! ## Provenance discipline (HARVEST + cite, never fabricate)
//!
//! Every [`OperatingPoint`] carries:
//!   - per-board AND per-unit watts + hashrate + J/TH
//!   - a [`PointConfidence`] of `Measured` / `Inferred` / `Gap`
//!   - a `&'static str` `source` citing the exact RE doc / jig Config.ini /
//!     crate row / live capture the number came from
//!
//! A `Gap` point carries `None` for the unknown columns — it is a documented
//! hole, never a guessed number. Consumers MUST treat `Gap` and any `None`
//! column as "unknown", never as fact.
//!
//! ## Sources (cited per row in the data below)
//!
//!
//!   (operator VNish RE — 820 watt~hashrate profiles, J/TH computed).
//!   §3.1-3.4 = LEGACY S17/S17+/T17/T17+ (explicit freq+watt+hashrate);
//!   §2.x / §2.17-2.22 = modern watt~hashrate-only profiles.
//!
//!   (Bitmain stock `levels.json` freq+voltage per hashboard SKU).
//!
//!   K-autotuner-profiles.md` (GOLD live LuxOS BM1362 S19j Pro capture).
//! -  +
//!   *-jig/Config.ini` (factory-test geometry).
//! - The in-crate `bm13xx.rs` `SiliconTable` rows (cross-referenced).
//! - `dcentrald-asic/src/drivers/mod.rs` MINER_PROFILES (the c_eff /
//!   nominal-point power-model table).
//!
//! Voltage convention notes baked into each model:
//!   - `voltage_mv` is the CHAIN-RAIL voltage (~8-15 V) UNLESS the row's
//!     source string says "chip-core" (downstream of the DC-DC, ~1.2-1.6 V).
//!     The two MUST NOT be conflated (a recurring harvest hazard).
//!   - VNish modern profiles carry NO freq/voltage (runtime-derived by the
//!     autotuner from a watt target) → those columns are `None` on those
//!     rows, by design, NOT a data loss.

#![allow(clippy::excessive_precision)]

use serde::{Deserialize, Serialize};

/// Per-datum confidence for an [`OperatingPoint`].
///
/// Distinct from both [`crate::ProfileSource`] (per-row provenance class of
/// the step ladder) and [`crate::ChipStatus`] (per-chip driver readiness).
/// This labels how trustworthy the watt/hashrate/freq/voltage numbers in a
/// single harvested operating point are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointConfidence {
    /// Numbers were measured on real hardware (live DCENT_OS / LuxOS /
    /// BraiinsOS capture, or an EEPROM-decoded live-probe value). Highest
    /// authority. A watt column may still be `Inferred` even when the
    /// hashrate is `Measured` — the row's `source` string says which.
    Measured,
    /// Numbers came from vendor RE (VNish profile catalog), a datasheet, a
    /// crate Reconstructed row, or a per-board split of a measured per-unit
    /// value. Real provenance, but not directly measured at this exact point
    /// on a DCENT unit. Treat as a usable estimate, not ground truth.
    Inferred,
    /// No data exists in any harvested source. The unknown columns are
    /// `None`. A documented hole — NEVER a guessed number. Consumers must
    /// surface this as "unknown", never treat it as a value.
    Gap,
}

impl PointConfidence {
    /// Authority rank (higher = more trustworthy). `Measured` (2) >
    /// `Inferred` (1) > `Gap` (0).
    pub const fn rank(self) -> u8 {
        match self {
            PointConfidence::Measured => 2,
            PointConfidence::Inferred => 1,
            PointConfidence::Gap => 0,
        }
    }

    /// `true` only for [`PointConfidence::Measured`].
    pub const fn is_measured(self) -> bool {
        matches!(self, PointConfidence::Measured)
    }

    /// `true` for [`PointConfidence::Gap`] — the row is a documented hole.
    pub const fn is_gap(self) -> bool {
        matches!(self, PointConfidence::Gap)
    }
}

/// A single harvested freq-vs-voltage power/efficiency operating point for a
/// specific Antminer MODEL.
///
/// All numeric columns are `Option` so a `Gap` (or a partially-known) point
/// can land without faking the missing values. `0`-valued `Some(_)` is NEVER
/// used to mean "unknown" — unknown is always `None`.
///
/// Per-board vs per-unit: where a source gives only a per-UNIT (all-board)
/// value, the per-board column is the per-unit value divided by the model's
/// `hashboards` (a uniform-board assumption, flagged `Inferred`). Real boards
/// differ by silicon bin / bad-core count; consumers needing exact per-board
/// telemetry must read it live, not trust this split.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OperatingPoint {
    /// Human-readable label for this point (e.g. `"stock nameplate"`,
    /// `"sweet spot"`, `"profile_835_80T"`, `"FACTORY-TEST (jig only)"`).
    pub label: &'static str,

    /// Target frequency in MHz. `None` for VNish modern watt~hashrate rows
    /// (freq is runtime-derived) — NOT a data loss.
    pub frequency_mhz: Option<u32>,

    /// Voltage in mV. CHAIN-RAIL unless the `source` string says
    /// "chip-core". `None` when the source carries no voltage.
    pub voltage_mv: Option<u32>,

    /// Hashrate per hashboard (TH/s). Usually per-unit / `hashboards`.
    pub hashrate_th_per_board: Option<f32>,

    /// Hashrate for the whole unit (TH/s, all boards).
    pub hashrate_th_per_unit: Option<f32>,

    /// Wall power per hashboard (W). Usually per-unit / `hashboards`.
    pub watts_per_board: Option<u32>,

    /// Wall power for the whole unit (W, all boards, AC-side).
    pub watts_per_unit: Option<u32>,

    /// Efficiency in J/TH (== watts_per_unit / hashrate_th_per_unit), as
    /// reported by the source. Pre-computed so a `Gap` row can omit it.
    pub j_per_th: Option<f32>,

    /// Confidence in these numbers — `Measured` / `Inferred` / `Gap`.
    pub confidence: PointConfidence,

    /// Exact source of this datum (RE doc + section / jig Config.ini /
    /// crate row / live capture). Never empty.
    pub source: &'static str,
}

impl OperatingPoint {
    /// Re-derive J/TH from the per-unit columns when both are known. Returns
    /// the stored `j_per_th` if present, else computes it, else `None`.
    pub fn computed_j_per_th(&self) -> Option<f32> {
        if let Some(j) = self.j_per_th {
            return Some(j);
        }
        let w = self.watts_per_unit? as f32;
        let h = self.hashrate_th_per_unit?;
        if h <= 0.0 {
            return None;
        }
        Some(w / h)
    }

    /// `true` if this point can drive a power estimate — it has both a
    /// per-unit watt figure and a per-unit hashrate, and is not a `Gap`.
    pub fn has_power_data(&self) -> bool {
        !self.confidence.is_gap()
            && self.watts_per_unit.is_some()
            && self.hashrate_th_per_unit.is_some()
    }
}

/// Cooling class of a model. Air-cooled curves MUST NOT be substituted for
/// hydro/immersion curves (and vice-versa) — they have different thermal
/// envelopes and board counts. The autotuner power-target picker uses this to
/// refuse cross-cooling substitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cooling {
    Air,
    Hydro,
    Immersion,
}

/// All harvested operating points + geometry for one Antminer MODEL.
///
/// `chip_id` matches `dcentrald-asic::drivers::MinerProfile::chip_id` where a
/// row exists; models with no MINER_PROFILES entry (S17+/T17+ BM1396, the
/// air/hydro S19/S21 variants) still get a `chip_id` of their chip family so
/// the autotuner can fall back to the family power model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPowerProfile {
    /// Marketing model name (e.g. `"Antminer S19j Pro"`).
    pub model: &'static str,
    /// Chip-family name (e.g. `"BM1362"`).
    pub chip_family: &'static str,
    /// Chip ID (0x1387 … 0x1370). For BM1396 (S17+/T17+) this is `0x1396`
    /// even though no MINER_PROFILES row exists yet.
    pub chip_id: u16,
    /// Number of hashboards (chains) in the unit.
    pub hashboards: u8,
    /// Chips per hashboard. `0` = unknown / not jig-confirmed (a GAP).
    pub chips_per_board: u16,
    /// Cores per chip. `0` = unknown for this chip family (e.g. BM1396).
    pub cores_per_chip: u32,
    /// Air / hydro / immersion. Drives cross-cooling substitution refusal.
    pub cooling: Cooling,
    /// The harvested operating points, low → high hashrate where ordered.
    pub points: &'static [OperatingPoint],
}

impl ModelPowerProfile {
    /// Lowest-J/TH point that has real power data (skips `Gap` rows and rows
    /// missing watt/hashrate). `None` if no point has computable efficiency.
    pub fn best_efficiency_point(&self) -> Option<&'static OperatingPoint> {
        self.points
            .iter()
            .filter(|p| p.has_power_data() && p.computed_j_per_th().is_some())
            .min_by(|a, b| {
                a.computed_j_per_th()
                    .unwrap()
                    .partial_cmp(&b.computed_j_per_th().unwrap())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Find a point by exact label.
    pub fn point_by_label(&self, label: &str) -> Option<&'static OperatingPoint> {
        self.points.iter().find(|p| p.label == label)
    }

    /// Count of points at each confidence level: `(measured, inferred, gap)`.
    pub fn confidence_counts(&self) -> (usize, usize, usize) {
        let mut m = 0;
        let mut i = 0;
        let mut g = 0;
        for p in self.points {
            match p.confidence {
                PointConfidence::Measured => m += 1,
                PointConfidence::Inferred => i += 1,
                PointConfidence::Gap => g += 1,
            }
        }
        (m, i, g)
    }

    /// `true` if at least one point carries measured power data — i.e. this
    /// model's curve is anchored on a real measurement, not pure inference.
    pub fn has_measured_anchor(&self) -> bool {
        self.points
            .iter()
            .any(|p| p.confidence.is_measured() && p.has_power_data())
    }
}

// Convenience constructors keep the const tables below readable. Rust const
// fns can't default struct fields, so these wrap the common shapes.

/// A VNish modern watt~hashrate row (per-unit watts + per-unit hashrate +
/// J/TH; freq/voltage runtime-derived → omitted). `hashboards` divides for
/// the per-board split. INFERRED by default (vendor RE).
const fn vnish(
    label: &'static str,
    th_unit: f32,
    w_unit: u32,
    j_per_th: f32,
    hashboards: u32,
    source: &'static str,
) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: None,
        voltage_mv: None,
        hashrate_th_per_board: Some(th_unit / hashboards as f32),
        hashrate_th_per_unit: Some(th_unit),
        watts_per_board: Some(w_unit / hashboards),
        watts_per_unit: Some(w_unit),
        j_per_th: Some(j_per_th),
        confidence: PointConfidence::Inferred,
        source,
    }
}

/// A fully-specified row with freq + chain-rail mV + per-unit watts/hashrate.
/// Per-board = per-unit / `hashboards`.
#[allow(clippy::too_many_arguments)]
const fn full(
    label: &'static str,
    freq_mhz: u32,
    voltage_mv: u32,
    th_unit: f32,
    w_unit: u32,
    j_per_th: f32,
    hashboards: u32,
    confidence: PointConfidence,
    source: &'static str,
) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: Some(freq_mhz),
        voltage_mv: Some(voltage_mv),
        hashrate_th_per_board: Some(th_unit / hashboards as f32),
        hashrate_th_per_unit: Some(th_unit),
        watts_per_board: Some(w_unit / hashboards),
        watts_per_unit: Some(w_unit),
        j_per_th: Some(j_per_th),
        confidence,
        source,
    }
}

/// A documented GAP — geometry/test-only point with no production power data.
const fn gap(
    label: &'static str,
    freq_mhz: Option<u32>,
    voltage_mv: Option<u32>,
    source: &'static str,
) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: freq_mhz,
        voltage_mv,
        hashrate_th_per_board: None,
        hashrate_th_per_unit: None,
        watts_per_board: None,
        watts_per_unit: None,
        j_per_th: None,
        confidence: PointConfidence::Gap,
        source,
    }
}

// ===========================================================================
// S9 family (BM1387, 16 nm) — existing crate table is the anchor; the VNish
// catalog confirms there is NO denser vendor dataset to import (S9/T9+ VNish
// firmware has no watt/hashrate profile table). So the operating points here
// mirror the crate's well-sourced BM1387_PROFILES + the live DCENT_OS run +
// the live stock-probe SKU point, with explicit provenance.
// ===========================================================================

/// Antminer S9 (standard, 13.5 TH nameplate bin), BM1387, 3×63 chips.
pub const S9_POINTS: [OperatingPoint; 8] = [
    full("Step -3 (eco-low)", 250, 8400, 5.36, 620, 115.7, 3,
        PointConfidence::Inferred,
        "crate bm1387.rs BM1387_PROFILES step -3 (Reconstructed)"),
    full("Step -2", 350, 8600, 7.5, 750, 100.0, 3,
        PointConfidence::Inferred,
        "crate bm1387.rs BM1387_PROFILES step -2 (Reconstructed)"),
    full("Step -1 (sweet spot, lowest J/TH)", 450, 8800, 9.64, 880, 91.3, 3,
        PointConfidence::Measured,
        "crate bm1387.rs BM1387_PROFILES step -1 (LiveConfirmed); AMTC + S9 sustained cold-boot 2026-04-19"),
    full("Step 0 (nameplate / default)", 600, 9100, 13.5, 1320, 97.8, 3,
        PointConfidence::Measured,
        "crate bm1387.rs BM1387_PROFILES step 0 (OperatorConfirmed); Bitmain nameplate 13.5 TH @ 600 MHz / 1320 W"),
    full("Step +1 (== MINER_PROFILES 650 MHz)", 650, 9200, 14.43, 1500, 103.9, 3,
        PointConfidence::Inferred,
        "crate bm1387.rs step +1 (Reconstructed); table-B ghs_per_mhz=0.114*650=14.43 TH cross-check"),
    full("Step +2 (overclock)", 700, 9300, 15.4, 1720, 111.7, 3,
        PointConfidence::Inferred,
        "crate bm1387.rs BM1387_PROFILES step +2 (Reconstructed)"),
    full("Step +3 (extreme overclock)", 750, 9400, 16.3, 1980, 121.5, 3,
        PointConfidence::Inferred,
        "crate bm1387.rs BM1387_PROFILES step +3 (Reconstructed)"),
    // Live DCENT_OS run — hashrate measured, wall watts NOT metered (GAP on watts).
    OperatingPoint {
        label: "Live DCENT_OS cold-boot mining (2026-04-19, NAND .39)",
        frequency_mhz: Some(547),
        voltage_mv: Some(9100),
        hashrate_th_per_board: None,
        hashrate_th_per_unit: Some(10.5),
        watts_per_board: None,
        watts_per_unit: None,
        j_per_th: None,
        confidence: PointConfidence::Measured,
        source: "DCENT_OS_Antminer/ S9 sustained cold boot 2026-04-19 (10+ TH/s, 3/3 chains, 31+ shares); LIVE_RECON_S9_DEEP.md '9.10V, 547 MHz'. GAP: no wall-watt meter reading",
    },
];

/// Antminer S9 low-bin stock-probe variant (12.5 TH nameplate). The
/// `voltage_mv = 870` here is PER-CHIP CORE (code 0x0706), NOT chain rail.
pub const S9_LOWBIN_POINTS: [OperatingPoint; 1] = [OperatingPoint {
    label: "Live stock-firmware config (.82 probe, 12.5 TH SKU)",
    frequency_mhz: Some(500), // config value read from the live unit (real)
    voltage_mv: Some(870),    // PER-CHIP CORE, not chain rail — see source
    // hashrate/watts are GAP: the unit never reached mining (fan + chain7
    // PIC fault), so 12.5 TH is the SKU NAMEPLATE, not a measured-under-load
    // value, and no wall watts were captured. Null so consumers cannot treat
    // the nameplate as achieved.
    hashrate_th_per_board: None,
    hashrate_th_per_unit: None,
    watts_per_board: None,
    watts_per_unit: None,
    j_per_th: None,
    confidence: PointConfidence::Gap,
    source: " (.82, 2026-03-18): rated 12.5 TH SKU, bitmain-freq 500, PIC core 870 mV (CORE not rail). Unit never reached mining (fan + chain7 PIC fault) → 12.5 TH is NAMEPLATE not measured-under-load; no wall watts. freq/voltage are real live config reads",
}];

/// Antminer S9+ (84 chips/chain). Only factory-test geometry exists — the
/// entire production power curve is a GAP.
pub const S9_PLUS_POINTS: [OperatingPoint; 1] = [gap(
    "AMTC factory-test point (geometry only, NOT production)",
    Some(550),
    Some(755), // per-chip CORE test voltage midpoint, NOT chain rail
    "AMTC_TEST_JIG_RE.md: S9+ = BM1387 (maybe BM1387P 128-core), 84 chips/chain, 750-760 mV CORE test, 200-550 MHz. No production watt/hashrate/J-TH in any source",
)];

/// Antminer S9++ (54 chips/chain). Existence-only — entire profile is a GAP.
pub const S9_PLUSPLUS_POINTS: [OperatingPoint; 1] = [gap(
    "AMTC function-name evidence only (no parameters)",
    None,
    None,
    "AMTC_TEST_JIG_RE.md: fn set_Voltage_S9_plus_plus_BM1387_54 reveals 54 chips/chain. NO Config.ini, freq, voltage, watt, or hashrate anywhere",
)];

/// Antminer T9 (57 chips/chain). Only AMTC test geometry; production GAP.
pub const T9_POINTS: [OperatingPoint; 1] = [gap(
    "AMTC factory-test point (geometry only, NOT production)",
    Some(550),
    Some(860), // per-chip CORE test voltage, NOT chain rail
    "AMTC_TEST_JIG_RE.md: T9 = BM1387, 57 chips/chain, 860 mV CORE test, 200-550 MHz, TMP421. No production watt/hashrate. NOTE: do NOT reuse the S9 (63 ch) curve for T9 (57 ch) — mis-scales watts/hashrate",
)];

/// Antminer T9+ (18 chips/chain). Only AMTC test geometry; production GAP.
pub const T9_PLUS_POINTS: [OperatingPoint; 1] = [gap(
    "AMTC factory-test point (geometry only, NOT production)",
    Some(550),
    Some(860), // per-chip CORE test voltage, NOT chain rail
    "AMTC_TEST_JIG_RE.md: T9+ = BM1387, 18 chips/chain (fewest), 860 mV CORE test. HashSource T9+/bmminer.dec present but unmined for power. No production curve",
)];

// ===========================================================================
// S17 / T17 family (BM1397 7nm + BM1396 7nm). The 56 LEGACY VNish profiles
// (POWER_PROFILES_CATALOG §3.1-3.4) carry EXPLICIT freq+watt+hashrate — the
// richest S17-family dataset. voltage_mv on the legacy rows is the fixed
// cgminer.conf bitmain-voltage 1680 mV (CHIP-CORE, NOT chain rail). No live
// S17/T17 on the fleet → every row is INFERRED.
// ===========================================================================

/// S17 Pro legacy curve helper: fixed 1680 mV chip-core, 3 boards.
const fn s17pro(label: &'static str, freq: u32, th: f32, w: u32, j: f32) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: Some(freq),
        voltage_mv: Some(1680), // CHIP-CORE (cgminer.conf bitmain-voltage), NOT chain rail
        hashrate_th_per_board: Some(th / 3.0),
        hashrate_th_per_unit: Some(th),
        watts_per_board: Some(w / 3),
        watts_per_unit: Some(w),
        j_per_th: Some(j),
        confidence: PointConfidence::Inferred,
        source:
            "POWER_PROFILES_CATALOG.md §3.1 (VNish legacy S17 Pro; voltage 1680 mV is chip-CORE)",
    }
}

/// Antminer S17 Pro (BM1397, 3×48). 16 VendorExtracted legacy points.
pub const S17_PRO_POINTS: [OperatingPoint; 16] = [
    s17pro("profile_400_38T (eco / sweet spot)", 400, 38.0, 960, 25.3),
    s17pro("profile_450_43T", 450, 43.0, 1210, 28.1),
    s17pro("profile_500_48T", 500, 48.0, 1485, 30.9),
    s17pro("profile_550_53T (stock nameplate)", 550, 53.0, 1750, 33.0),
    s17pro("profile_575_55T", 575, 55.0, 1860, 33.8),
    s17pro("profile_600_58T", 600, 58.0, 2100, 36.2),
    s17pro("profile_625_60T", 625, 60.0, 2240, 37.3),
    s17pro("profile_650_62T", 650, 62.0, 2480, 40.0),
    s17pro("profile_660_63T", 660, 63.0, 2550, 40.5),
    s17pro(
        "profile_675_65T (supersedes crate 675/76TH)",
        675,
        65.0,
        2680,
        41.2,
    ),
    s17pro("profile_700_67T", 700, 67.0, 2850, 42.5),
    s17pro("profile_725_70T", 725, 70.0, 3050, 43.6),
    s17pro("profile_750_72T", 750, 72.0, 3240, 45.0),
    s17pro("profile_775_74T", 775, 74.0, 3480, 47.0),
    s17pro("profile_800_77T", 800, 77.0, 3630, 47.1),
    s17pro("profile_835_80T (max OC)", 835, 80.0, 3800, 47.5),
];

/// S17+ (BM1396) legacy curve helper.
///
/// PROVENANCE NOTE (review must-fix 2026-06-14): the cited source
/// `POWER_PROFILES_CATALOG.md §3.2/§3.4` HEADERS label S17+/T17+ as "BM1397", but
/// we deliberately attribute them to **BM1396** (chip_id 0x1396) per the
/// corroborating corpus (root   / the
/// `am2-s17pro-zynq` variant: S17+/T17+ carry BM1396, distinct from the S17/T17
/// BM1397). The watt/hashrate/freq ROWS are byte-exact to the catalog; only the
/// chip-family label intentionally overrides the source header.
const fn s17plus(label: &'static str, freq: u32, th: f32, w: u32, j: f32) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: Some(freq),
        voltage_mv: Some(1680), // CHIP-CORE assumption
        hashrate_th_per_board: Some(th / 3.0),
        hashrate_th_per_unit: Some(th),
        watts_per_board: Some(w / 3),
        watts_per_unit: Some(w),
        j_per_th: Some(j),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §3.2 (VNish legacy S17+; BM1396 data-only table; voltage 1680 mV chip-CORE)",
    }
}

/// Antminer S17+ (BM1396, 3×65 support-matrix scaffold). 16 VendorExtracted legacy points.
pub const S17_PLUS_POINTS: [OperatingPoint; 16] = [
    s17plus(
        "profile_400_52T (eco / safe-default)",
        400,
        52.0,
        1700,
        32.7,
    ),
    s17plus("profile_420_55T", 420, 55.0, 1800, 32.7),
    s17plus("profile_450_58T", 450, 58.0, 2050, 35.3),
    s17plus("profile_460_60T", 460, 60.0, 2100, 35.0),
    s17plus("profile_475_62T", 475, 62.0, 2200, 35.5),
    s17plus("profile_500_65T", 500, 65.0, 2370, 36.5),
    s17plus("profile_520_68T", 520, 68.0, 2500, 36.8),
    s17plus("profile_550_71T (≈stock)", 550, 71.0, 2700, 38.0),
    s17plus("profile_565_74T", 565, 74.0, 2800, 37.8),
    s17plus("profile_600_78T", 600, 78.0, 3100, 39.7),
    s17plus("profile_610_80T", 610, 80.0, 3150, 39.4),
    s17plus("profile_650_84T", 650, 84.0, 3500, 41.7),
    s17plus("profile_675_88T", 675, 88.0, 3650, 41.5),
    s17plus("profile_700_91T", 700, 91.0, 3900, 42.9),
    s17plus("profile_715_93T", 715, 93.0, 4050, 43.5),
    s17plus("profile_730_95T (max OC)", 730, 95.0, 4150, 43.7),
];

/// T17 (BM1397) legacy curve helper.
const fn t17(label: &'static str, freq: u32, th: f32, w: u32, j: f32) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: Some(freq),
        voltage_mv: Some(1680), // CHIP-CORE assumption
        hashrate_th_per_board: Some(th / 3.0),
        hashrate_th_per_unit: Some(th),
        watts_per_board: Some(w / 3),
        watts_per_unit: Some(w),
        j_per_th: Some(j),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §3.3 (VNish legacy T17; chips/board pinned 30; voltage 1680 mV chip-CORE)",
    }
}

/// Antminer T17 (BM1397, 3 hashboards x 30 chips). 10 VendorExtracted legacy points.
pub const T17_POINTS: [OperatingPoint; 10] = [
    t17(
        "profile_600_36T (min / safe-default)",
        600,
        36.0,
        1570,
        43.6,
    ),
    t17("profile_650_38T", 650, 38.0, 1760, 46.3),
    t17("profile_675_40T", 675, 40.0, 1850, 46.3),
    t17("profile_700_42T", 700, 42.0, 2050, 48.8),
    t17("profile_750_45T", 750, 45.0, 2350, 52.2),
    t17("profile_800_48T", 800, 48.0, 2530, 52.7),
    t17("profile_825_50T", 825, 50.0, 2615, 52.3),
    t17("profile_850_51T", 850, 51.0, 2720, 53.3),
    t17("profile_875_52T", 875, 52.0, 2830, 54.4),
    t17("profile_900_54T (max OC)", 900, 54.0, 2910, 53.9),
];

/// T17+ (BM1396) legacy curve helper.
const fn t17plus(label: &'static str, freq: u32, th: f32, w: u32, j: f32) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: Some(freq),
        voltage_mv: Some(1680), // CHIP-CORE assumption
        hashrate_th_per_board: Some(th / 3.0),
        hashrate_th_per_unit: Some(th),
        watts_per_board: Some(w / 3),
        watts_per_unit: Some(w),
        j_per_th: Some(j),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §3.4 (VNish legacy T17+; BM1396 data-only table; chips/board INFERRED ~44; voltage 1680 mV chip-CORE)",
    }
}

/// Antminer T17+ (BM1396, 3×44 inferred). 14 VendorExtracted legacy points.
pub const T17_PLUS_POINTS: [OperatingPoint; 14] = [
    t17plus("profile_400_35T (min / sweet spot)", 400, 35.0, 1150, 32.9),
    t17plus("profile_450_40T", 450, 40.0, 1350, 33.8),
    t17plus("profile_500_44T", 500, 44.0, 1615, 36.7),
    t17plus("profile_540_48T", 540, 48.0, 1800, 37.5),
    t17plus("profile_600_53T (≈stock)", 600, 53.0, 2180, 41.1),
    t17plus("profile_620_55T", 620, 55.0, 2310, 42.0),
    t17plus("profile_650_57T", 650, 57.0, 2500, 43.9),
    t17plus("profile_700_61T", 700, 61.0, 2850, 46.7),
    t17plus("profile_720_63T", 720, 63.0, 2960, 47.0),
    t17plus("profile_750_66T", 750, 66.0, 3200, 48.5),
    t17plus("profile_770_68T", 770, 68.0, 3350, 49.3),
    t17plus("profile_800_70T", 800, 70.0, 3600, 51.4),
    t17plus("profile_825_73T", 825, 73.0, 3720, 50.9),
    t17plus("profile_850_75T (max OC)", 850, 75.0, 3850, 51.3),
];

// ===========================================================================
// S19 / S19 Pro / S19a Pro / T19 (BM1398, 7nm/5nm). S19 Pro is the only
// live-confirmed unit (.129). VNish §2.1-2.3/2.9 watt~hashrate rows densify
// the air S19 / S19a Pro / Hydro curves; T19 AIR is a FULL GAP (only Hydro
// T19 exists in the corpus).
// ===========================================================================

/// Antminer S19 Pro (BM1398, 3×114). Crate steps (chain-rail V) + factory
/// jig test points (chip-core mV, GAP for power).
pub const S19_PRO_POINTS: [OperatingPoint; 7] = [
    full("Step -2 (eco-low, sweet spot)", 580, 13400, 100.0, 2830, 28.3, 3,
        PointConfidence::Inferred,
        "crate bm1398.rs step -2 (Reconstructed; declared sweet_spot)"),
    full("Step -1", 615, 13600, 105.0, 3050, 29.0, 3,
        PointConfidence::Inferred,
        "crate bm1398.rs step -1 (Reconstructed)"),
    full("Step 0 (nameplate / default)", 650, 13800, 110.0, 3250, 29.5, 3,
        PointConfidence::Measured,
        "crate bm1398.rs step 0 (OperatorConfirmed); S19 Pro nameplate ~110 TH @ ~3250 W; .129 cold-boot mining 2026-04-10"),
    full("Step +1 (overclock)", 690, 14000, 117.0, 3550, 30.3, 3,
        PointConfidence::Inferred,
        "crate bm1398.rs step +1 (Reconstructed)"),
    full("Step +2 (max overclock)", 730, 14200, 124.0, 3870, 31.2, 3,
        PointConfidence::Inferred,
        "crate bm1398.rs step +2 (Reconstructed)"),
    gap("Factory jig test L1 (chip-core, NOT production)", Some(525), Some(1360),
        "amtc-s19pro-jig/Config.ini Test_Loop L1 (Voltage=1360 mV CORE, Frequence=525, Pre_Open_Core 1500). Geometry: Asic_Num=114, 38 domains, NCT218+LM75A, 12 Mbaud"),
    gap("Factory jig test L3 (chip-core, NOT production)", Some(525), Some(1320),
        "amtc-s19pro-jig/Config.ini Test_Loop L3 (Voltage=1320 mV CORE). Test-only"),
];

/// Antminer S19 Pro Hydro (BM1398, water-cooled). VNish curve — distinct
/// cooling envelope; freq/voltage runtime-derived.
pub const S19_PRO_HYDRO_POINTS: [OperatingPoint; 3] = [
    vnish(
        "Hydro low end (best eff)",
        88.0,
        2552,
        29.0,
        3,
        "POWER_PROFILES_CATALOG.md §2.9 (S19 Pro Hydro 120TH, lowest row)",
    ),
    vnish(
        "Hydro nameplate ~120 TH",
        118.0,
        3422,
        29.0,
        3,
        "POWER_PROFILES_CATALOG.md §2.9 (flat 29.0 J/TH)",
    ),
    vnish(
        "Hydro max (push, 201 TH — liquid only)",
        201.0,
        7405,
        36.8,
        3,
        "POWER_PROFILES_CATALOG.md §2.9 top row (liquid-cooled envelope only)",
    ),
];

/// Antminer S19 (126 TH air, BM1398, 3×42). VNish §2.1 curve.
pub const S19_126_POINTS: [OperatingPoint; 4] = [
    vnish(
        "eco-low (most efficient)",
        67.0,
        1630,
        24.3,
        3,
        "POWER_PROFILES_CATALOG.md §2.1 lowest row; also baked S19_126TH_PROFILES step 0",
    ),
    vnish(
        "~stock equivalent (105 TH)",
        105.0,
        3095,
        29.5,
        3,
        "POWER_PROFILES_CATALOG.md §2.1",
    ),
    vnish(
        "nameplate-class (110 TH)",
        110.0,
        3310,
        30.1,
        3,
        "POWER_PROFILES_CATALOG.md §2.1",
    ),
    vnish(
        "max overclock (130 TH)",
        130.0,
        4700,
        36.2,
        3,
        "POWER_PROFILES_CATALOG.md §2.1 top row",
    ),
];

/// Antminer S19 (88 TH air, BM1398, chips/board UNKNOWN). VNish §2.2 curve.
pub const S19_88_POINTS: [OperatingPoint; 3] = [
    OperatingPoint {
        label: "eco-low (best eff)",
        frequency_mhz: None,
        voltage_mv: None,
        hashrate_th_per_board: None,
        hashrate_th_per_unit: Some(64.0),
        watts_per_board: None,
        watts_per_unit: Some(1500),
        j_per_th: Some(23.4),
        confidence: PointConfidence::Inferred,
        source:
            "POWER_PROFILES_CATALOG.md §2.2 lowest row (chips/board UNKNOWN → no per-board split)",
    },
    OperatingPoint {
        label: "stock-class (96 TH)",
        frequency_mhz: None,
        voltage_mv: None,
        hashrate_th_per_board: None,
        hashrate_th_per_unit: Some(96.0),
        watts_per_board: None,
        watts_per_unit: Some(3300),
        j_per_th: Some(34.4),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.2",
    },
    OperatingPoint {
        label: "max (110 TH)",
        frequency_mhz: None,
        voltage_mv: None,
        hashrate_th_per_board: None,
        hashrate_th_per_unit: Some(110.0),
        watts_per_board: None,
        watts_per_unit: Some(4000),
        j_per_th: Some(36.4),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.2 top row",
    },
];

/// Antminer S19a Pro (BM1398 air, chips/board UNKNOWN). VNish §2.3 curve.
pub const S19A_PRO_POINTS: [OperatingPoint; 3] = [
    OperatingPoint {
        label: "low end (100 TH)",
        frequency_mhz: None, voltage_mv: None,
        hashrate_th_per_board: None, hashrate_th_per_unit: Some(100.0),
        watts_per_board: None, watts_per_unit: Some(2800),
        j_per_th: Some(28.0), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.3 lowest row (S19a Pro; chips/board UNKNOWN; NOT in either crate table)",
    },
    OperatingPoint {
        label: "nameplate-class (110 TH)",
        frequency_mhz: None, voltage_mv: None,
        hashrate_th_per_board: None, hashrate_th_per_unit: Some(110.0),
        watts_per_board: None, watts_per_unit: Some(3200),
        j_per_th: Some(29.1), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.3",
    },
    OperatingPoint {
        label: "max (162 TH)",
        frequency_mhz: None, voltage_mv: None,
        hashrate_th_per_board: None, hashrate_th_per_unit: Some(162.0),
        watts_per_board: None, watts_per_unit: Some(5770),
        j_per_th: Some(35.6), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.3 top row",
    },
];

/// Antminer T19 (air-cooled, BM1398). FULL GAP — no air-T19 power data in
/// any held asset (only Hydro T19 exists in the corpus).
pub const T19_POINTS: [OperatingPoint; 1] = [gap(
    "T19 air-cooled — ALL OPERATING POINTS GAP",
    None,
    None,
    "No held asset carries air-cooled T19 watt/hashrate/freq/voltage. T19 Hydro (§2.14) is liquid-cooled, NOT applicable. cgminer.t19.1.2.7 JSON has IOC metadata but no extracted preset rows. Datasheet ~88 TH @ ~3250 W deliberately NOT entered",
)];

// ===========================================================================
// S19j Pro family (BM1362, 5nm). The 21-step LuxOS-live BM1362 table is the
// RICHEST in the crate — these points mirror representative live-confirmed
// rows + the high-bin VNish curves (chips/board differs per SKU). Voltage is
// chain-rail on the live rows; chip-core on the levels.json rows.
// ===========================================================================

/// Antminer S19j Pro (BHB42601, BM1362, 3×126). Representative live-confirmed
/// LuxOS rows + the bring-up nominal + levels.json (chip-core) GAP rows.
pub const S19J_PRO_POINTS: [OperatingPoint; 11] = [
    full("Step -16 (deep-undervolt floor)", 145, 11880, 28.1, 997, 35.5, 3,
        PointConfidence::Measured,
        "LuxOS .79 cgminer-API profiles (K-doc §1.1) = bm1362.rs Step -16; chain-rail V"),
    full("Step -13 (Whisper mode)", 220, 11880, 42.7, 1244, 29.1, 3,
        PointConfidence::Measured,
        "LuxOS .79 profiles (K-doc §1.1) = bm1362.rs Step -13"),
    full("Step -11 (MaxEfficiency / ATM floor)", 270, 12150, 52.4, 1466, 27.98, 3,
        PointConfidence::Measured,
        "LuxOS .79 profiles (K-doc §1.1) = bm1362.rs Step -11"),
    full("Step -9 (EFFICIENCY SWEET SPOT)", 320, 12450, 62.1, 1714, 27.6, 3,
        PointConfidence::Measured,
        "LuxOS .79 profiles (K-doc §1.4) = bm1362.rs sweet_spot_step=-9 (6.6% better J/TH than nameplate)"),
    full("Step -7 (recommended Heater mode)", 370, 12750, 71.8, 1994, 27.77, 3,
        PointConfidence::Measured,
        "LuxOS .79 profiles (K-doc §1.1/§11.4) = bm1362.rs Step -7 (~12700 BTU/h Heater default)"),
    full("Step -5 (last live-confirmed row)", 420, 13050, 81.6, 2297, 28.15, 3,
        PointConfidence::Measured,
        "LuxOS .79 profiles (K-doc §1.1, last verbatim row before truncation) = bm1362.rs Step -5"),
    full("Step -2 (reconstructed)", 495, 13500, 96.2, 2802, 29.13, 3,
        PointConfidence::Inferred,
        "bm1362.rs Step -2 (Reconstructed; K-doc §1.3)"),
    full("~500 MHz / 13700 mV — bring-up nominal (.25/.109 cold-boot)", 500, 13700, 104.0, 3068, 29.5, 3,
        PointConfidence::Inferred,
        "MINER_PROFILES default 500MHz/13700mV + spec nameplate 104TH/3068W (c_eff 0.0000817 back-solve). CONSERVATIVE bring-up point, NOT a LuxOS row"),
    full("Step 0 (default / nameplate, c_eff wall anchor)", 545, 13800, 105.8, 3126, 29.55, 3,
        PointConfidence::Measured,
        "LuxOS .79 EEPROM 'Default 545MHz' + profiles Step 0 (K-doc §1.2) = bm1362.rs Step 0. AC-side wall anchor (~12% APW loss)"),
    full("Step +4 (max OC, reconstructed)", 645, 14400, 125.0, 3830, 30.64, 3,
        PointConfidence::Inferred,
        "bm1362.rs Step +4 (Reconstructed; K-doc §1.3). Voltage may saturate at APW 14.5V ceiling — re-verify"),
    gap("BHB42601 levels.json band (chip-core, freq/voltage only)", Some(465), Some(1380),
        " §1.1 BHB42601 levels.json:51-54. 1380 mV is per-CHIP CORE (downstream of buck), NOT ~13.8V chain rail. 17-row 545@1320..465@1380; no watts/TH"),
];

/// Antminer S19j Pro+ (BHB42611, BM1362, 3×120 high-bin). VNish §2.5 curve.
/// freq/voltage shown are APPROXIMATE BHB42611 levels.json band pairings only
/// (VNish does not store per-profile f/V) — flagged INFERRED.
pub const S19J_PRO_PLUS_POINTS: [OperatingPoint; 4] = [
    OperatingPoint {
        label: "VNish low-end 1450W / 65TH (family eff floor)",
        frequency_mhz: Some(610), voltage_mv: Some(1380), // chip-core APPROX
        hashrate_th_per_board: Some(65.0 / 3.0), hashrate_th_per_unit: Some(65.0),
        watts_per_board: Some(1450 / 3), watts_per_unit: Some(1450),
        j_per_th: Some(22.3), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.5 (VNish S19j Pro+; freq/voltage=BHB42611 levels.json band APPROX, chip-CORE; 120 ASIC/chain)",
    },
    OperatingPoint {
        label: "VNish mid 2650W / 103TH",
        frequency_mhz: Some(650), voltage_mv: Some(1340),
        hashrate_th_per_board: Some(103.0 / 3.0), hashrate_th_per_unit: Some(103.0),
        watts_per_board: Some(2650 / 3), watts_per_unit: Some(2650),
        j_per_th: Some(25.7), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.5 (freq/voltage APPROX)",
    },
    OperatingPoint {
        label: "VNish nameplate-class 3200W / 116TH",
        frequency_mhz: Some(670), voltage_mv: Some(1320),
        hashrate_th_per_board: Some(116.0 / 3.0), hashrate_th_per_unit: Some(116.0),
        watts_per_board: Some(3200 / 3), watts_per_unit: Some(3200),
        j_per_th: Some(27.6), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.5 (freq/voltage APPROX)",
    },
    OperatingPoint {
        label: "VNish max OC 5900W / 159TH (NOT home-safe)",
        frequency_mhz: Some(670), voltage_mv: Some(1380),
        hashrate_th_per_board: Some(159.0 / 3.0), hashrate_th_per_unit: Some(159.0),
        watts_per_board: Some(5900 / 3), watts_per_unit: Some(5900),
        j_per_th: Some(37.1), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.5 top OC (extreme; freq/voltage out-of-table OC, APPROX)",
    },
];

/// Antminer S19j Pro-A / S19j XP (BHB42801 high-bin, BM1362, 3×88). VNish
/// §2.4 curve. Requires APW12+. freq/voltage APPROX (chip-core). The >140TH
/// rows are immersion/OC, NOT air-cooled home territory.
pub const S19J_PRO_A_POINTS: [OperatingPoint; 5] = [
    OperatingPoint {
        label: "VNish low-end 1740W / 65TH (most efficient air point)",
        frequency_mhz: Some(585), voltage_mv: Some(1600), // chip-core APPROX
        hashrate_th_per_board: Some(65.0 / 3.0), hashrate_th_per_unit: Some(65.0),
        watts_per_board: Some(1740 / 3), watts_per_unit: Some(1740),
        j_per_th: Some(26.8), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.4 (VNish S19j Pro-A; BHB42801 levels.json band APPROX, chip-CORE; 88 ASIC/chain; APW12+)",
    },
    OperatingPoint {
        label: "VNish best J/TH 1850W / 76TH",
        frequency_mhz: Some(615), voltage_mv: Some(1565),
        hashrate_th_per_board: Some(76.0 / 3.0), hashrate_th_per_unit: Some(76.0),
        watts_per_board: Some(1850 / 3), watts_per_unit: Some(1850),
        j_per_th: Some(24.3), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.4 (24.3 J/TH best in §2.4; freq/voltage APPROX)",
    },
    OperatingPoint {
        label: "VNish nameplate-class 3080W / 104TH",
        frequency_mhz: Some(645), voltage_mv: Some(1545),
        hashrate_th_per_board: Some(104.0 / 3.0), hashrate_th_per_unit: Some(104.0),
        watts_per_board: Some(3080 / 3), watts_per_unit: Some(3080),
        j_per_th: Some(29.6), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.4 (freq/voltage APPROX)",
    },
    OperatingPoint {
        label: "VNish high OC 4110W / 130TH (APW12+ required)",
        frequency_mhz: Some(675), voltage_mv: Some(1530),
        hashrate_th_per_board: Some(130.0 / 3.0), hashrate_th_per_unit: Some(130.0),
        watts_per_board: Some(4110 / 3), watts_per_unit: Some(4110),
        j_per_th: Some(31.6), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.4 (>130TH needs APW12+; freq/voltage APPROX)",
    },
    OperatingPoint {
        label: "VNish extreme/immersion 7164W / 199TH (NOT air-cooled)",
        frequency_mhz: Some(900), voltage_mv: Some(1600),
        hashrate_th_per_board: Some(199.0 / 3.0), hashrate_th_per_unit: Some(199.0),
        watts_per_board: Some(7164 / 3), watts_per_unit: Some(7164),
        j_per_th: Some(36.0), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.4 top (immersion/hydro OC ONLY; far above air + APW12; freq APPROX)",
    },
];

// ===========================================================================
// S19k Pro / S19 XP (BM1366, 5nm). S19k Pro has a live .78 EEPROM-decoded
// freq/voltage/hashrate point (watts inferred from the flat VNish curve) +
// the rich VNish §2.6 Normal/Performance curves. S19 XP air is spec-only.
// ===========================================================================

/// S19k Pro VNish Normal-mode curve helper (flat ~25.8 J/TH, 3 boards).
const fn s19kpro_normal(label: &'static str, th: f32, w: u32, j: f32) -> OperatingPoint {
    OperatingPoint {
        label,
        frequency_mhz: None, // VNish modern — runtime-derived
        voltage_mv: None,
        hashrate_th_per_board: Some(th / 3.0),
        hashrate_th_per_unit: Some(th),
        watts_per_board: Some(w / 3),
        watts_per_unit: Some(w),
        j_per_th: Some(j),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.6 Normal (VNish S19k Pro; watt+hashrate real; freq/voltage runtime-derived)",
    }
}

/// Antminer S19k Pro (BM1366, 3×76). Live .78 measured point + VNish curve +
/// levels.json (chip-core) GAP rows.
pub const S19K_PRO_POINTS: [OperatingPoint; 11] = [
    // Live .78 EEPROM-decoded — freq/voltage/hashrate MEASURED, watts inferred.
    OperatingPoint {
        label: "Live .78 BHB56902 stock (BraiinsOS+ 25.07)",
        frequency_mhz: Some(670),
        voltage_mv: Some(13900), // chain-rail, bosminer-decoded
        hashrate_th_per_board: Some(46.12),
        hashrate_th_per_unit: Some(138.36),
        watts_per_board: Some(1190),
        watts_per_unit: Some(3570),
        j_per_th: Some(25.8),
        confidence: PointConfidence::Measured,
        source: " (3× BHB56902; Voltage Avg 13.90V, Freq Avg 670MHz, Hashrate 46121 GH/s/board MEASURED). Watts INFERRED at flat 25.8 J/TH (no wall meter)",
    },
    OperatingPoint {
        label: "VNish stock nameplate (catalog-stated)",
        frequency_mhz: Some(605),
        voltage_mv: Some(13800), // chain-rail (crate default_step)
        hashrate_th_per_board: Some(40.0),
        hashrate_th_per_unit: Some(120.0),
        watts_per_board: Some(920),
        watts_per_unit: Some(2760),
        j_per_th: Some(23.0),
        confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.6 ('Stock is 120 TH @ ~2760W'). CONTRADICTS crate bm1366.rs default (120TH/3420W/28.5 J/TH) — VNish curve is the measured-curve authority; see GAP note",
    },
    s19kpro_normal("VNish Normal eco-low (safe-default) 80 TH", 80.0, 2050, 25.6),
    s19kpro_normal("VNish Normal 100 TH", 100.0, 2600, 26.0),
    s19kpro_normal("VNish Normal 121 TH (≈nameplate)", 121.0, 3120, 25.8),
    s19kpro_normal("VNish Normal 152 TH", 152.0, 3920, 25.8),
    s19kpro_normal("VNish Normal max 188 TH (top of Normal, OC)", 188.0, 4860, 25.9),
    OperatingPoint {
        label: "VNish Performance min 78 TH (~36 J/TH)",
        frequency_mhz: None, voltage_mv: None,
        hashrate_th_per_board: Some(26.0), hashrate_th_per_unit: Some(78.0),
        watts_per_board: Some(950), watts_per_unit: Some(2850),
        j_per_th: Some(36.5), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.6 Performance (trades eff for V/board)",
    },
    OperatingPoint {
        label: "VNish Performance max 135 TH",
        frequency_mhz: None, voltage_mv: None,
        hashrate_th_per_board: Some(45.0), hashrate_th_per_unit: Some(135.0),
        watts_per_board: Some(1667), watts_per_unit: Some(5000),
        j_per_th: Some(37.0), confidence: PointConfidence::Inferred,
        source: "POWER_PROFILES_CATALOG.md §2.6 Performance top",
    },
    gap("BHB42701 levels.json Step 0 (chip-core, legacy SKU)", Some(575), Some(1240),
        " §1.9 BHB42701 levels.json:584. 1240 mV per-CHIP CORE, NOT ~13.9V chain rail. Legacy ID; production S19k Pro is BHB56902 (levels.json GAP). No watts/TH"),
    gap("BHB42701 levels.json Step 6 (chip-core, lowest)", Some(500), Some(1220),
        " §1.9 BHB42701 levels.json:602. Per-chip-CORE mV. No watts/TH"),
];

/// Antminer S19 XP (BM1366 air, 3×110). Spec nameplate + levels.json
/// (chip-core) GAP rows. No air-XP VNish curve exists (only Hydro).
pub const S19_XP_POINTS: [OperatingPoint; 3] = [
    OperatingPoint {
        label: "Air-cooled nameplate (spec)",
        frequency_mhz: Some(500),
        voltage_mv: Some(12800), // MINER_PROFILES nominal (autotuner target, NOT ~13.8V rail)
        hashrate_th_per_board: Some(46.7),
        hashrate_th_per_unit: Some(140.0),
        watts_per_board: Some(1003),
        watts_per_unit: Some(3010),
        j_per_th: Some(21.5),
        confidence: PointConfidence::Inferred,
        source: "MINER_PROFILES 0x1366 'S19 XP' (500MHz/12800mV, 140TH spec, ~3010W, ~21.5 J/TH) + HASHBOARD_DIAGNOSTICS §1/§12.1. SPEC not live-measured; 12800 is autotuner nominal not chain rail",
    },
    gap("BHB42801 levels.json Step 0 (chip-core, shared w/ S19j XP bin)", Some(675), Some(1580),
        " §1.5 BHB42801 levels.json:61. 1580 mV per-chip-CORE; table shared with higher-clock S19j XP. Treat as factory freq/voltage envelope, NOT the air S19 XP eff point. No watts/TH"),
    gap("BHB42801 levels.json eco Step 5 (chip-core)", Some(615), Some(1530),
        " §1.5/§1.8 (615MHz @ 1530mV). Per-chip-CORE. No watts/TH"),
];

// ===========================================================================
// S21 family (BM1368 5nm air + BM1370 3nm). Air S21 / S21 Pro nameplates are
// OperatorConfirmed anchors w/ Reconstructed curves. The VNish S21 corpus is
// ALL hydro/immersion — those densify the hydro SKUs but MUST NOT replace the
// air curves (Cooling::Hydro guards the substitution).
// ===========================================================================

/// Antminer S21 (air, BM1368, 3×108). Crate steps + factory jig GAP.
pub const S21_POINTS: [OperatingPoint; 6] = [
    full("eco-low (step -2 = sweet spot)", 480, 13400, 175.0, 3000, 17.14, 3,
        PointConfidence::Inferred,
        "crate bm1368.rs step -2 (Reconstructed); chain-rail V"),
    full("underclock (step -1)", 500, 13600, 187.0, 3220, 17.22, 3,
        PointConfidence::Inferred,
        "crate bm1368.rs step -1 (Reconstructed)"),
    full("stock / nameplate (step 0 = default)", 525, 13800, 200.0, 3500, 17.5, 3,
        PointConfidence::Measured,
        "crate bm1368.rs step 0 (OperatorConfirmed, S21 nameplate); HASHBOARD_DIAGNOSTICS 17.5 J/TH; .135 first-hash 2026-04-11"),
    full("overclock (step +1)", 555, 14000, 213.0, 3820, 17.93, 3,
        PointConfidence::Inferred,
        "crate bm1368.rs step +1 (Reconstructed)"),
    full("max overclock (step +2)", 590, 14200, 226.0, 4180, 18.5, 3,
        PointConfidence::Inferred,
        "crate bm1368.rs step +2 (Reconstructed)"),
    gap("FACTORY-TEST (jig only, NOT production)", Some(450), Some(1320),
        "amtc-s21-jig/Config.ini Level 1: Voltage=1320 CORE, Frequence=450, Pre_Open_Core 1500, sweep 420-540. Chip-CORE mV NOT ~13.8V rail"),
];

/// Antminer S21 Hydro (BM1368, water). VNish §2.17 curve.
pub const S21_HYDRO_POINTS: [OperatingPoint; 4] = [
    vnish(
        "Normal low (safe-default candidate)",
        92.0,
        1630,
        17.7,
        3,
        "POWER_PROFILES_CATALOG.md §2.17 S21 Hydro normal row 1",
    ),
    vnish(
        "Normal mid",
        235.0,
        4210,
        17.9,
        3,
        "POWER_PROFILES_CATALOG.md §2.17 S21 Hydro normal",
    ),
    vnish(
        "Performance sweet spot (best Hydro J/TH)",
        259.0,
        4160,
        16.1,
        3,
        "POWER_PROFILES_CATALOG.md §2.17 + §5.1 (S21 Hydro Perf 16.1 J/TH)",
    ),
    vnish(
        "Performance max",
        383.0,
        6300,
        16.4,
        3,
        "POWER_PROFILES_CATALOG.md §2.17 perf top",
    ),
];

/// Antminer S21e Hydro (BM1368, best-eff BM1368 SKU). VNish §2.18 curve.
pub const S21E_HYDRO_POINTS: [OperatingPoint; 3] = [
    vnish(
        "Low-power best (best BM1368 eff)",
        165.0,
        2450,
        14.8,
        3,
        "POWER_PROFILES_CATALOG.md §2.18 + §5.1 (S21e Hydro LP 14.8 J/TH)",
    ),
    vnish(
        "Normal (flat 16.5 J/TH)",
        200.0,
        3300,
        16.5,
        3,
        "POWER_PROFILES_CATALOG.md §2.18 normal (flat 16.5)",
    ),
    vnish(
        "Performance max",
        482.0,
        14428,
        29.9,
        3,
        "POWER_PROFILES_CATALOG.md §2.18 perf top (80 profiles total)",
    ),
];

/// Antminer S21 Pro (air, BM1370, 3×65). Crate steps (RegisterMappedFromRE —
/// no live unit) + the nameplate anchor.
pub const S21_PRO_POINTS: [OperatingPoint; 5] = [
    full("eco-low (step -2 = sweet spot)", 480, 13400, 215.0, 3000, 13.95, 3,
        PointConfidence::Inferred,
        "crate bm1370.rs step -2 (Reconstructed); chain-rail V; 195 chips total (65×3)"),
    full("underclock (step -1)", 500, 13600, 224.0, 3220, 14.38, 3,
        PointConfidence::Inferred,
        "crate bm1370.rs step -1 (Reconstructed)"),
    full("stock / nameplate (step 0 = default)", 525, 13800, 234.0, 3510, 15.0, 3,
        PointConfidence::Inferred,
        "crate bm1370.rs step 0 (OperatorConfirmed label, but NO live S21 Pro — RegisterMappedFromRE); 234TH/195chips=1.2TH/chip @525MHz"),
    full("overclock (step +1)", 555, 14000, 245.0, 3820, 15.59, 3,
        PointConfidence::Inferred,
        "crate bm1370.rs step +1 (Reconstructed)"),
    full("max overclock (step +2)", 585, 14200, 256.0, 4150, 16.21, 3,
        PointConfidence::Inferred,
        "crate bm1370.rs step +2 (Reconstructed)"),
];

/// Antminer S21+ Hydro (BM1370). VNish §2.19 — richest BM1370 dataset.
pub const S21_PLUS_HYDRO_POINTS: [OperatingPoint; 4] = [
    vnish(
        "Ultra-low-power best (absolute best in catalog)",
        171.0,
        1985,
        11.6,
        3,
        "POWER_PROFILES_CATALOG.md §2.19 + §5.1 (S21+ Hydro ULP 11.6 J/TH)",
    ),
    vnish(
        "Immersion/extended (flat ~13.4 J/TH)",
        246.0,
        3300,
        13.4,
        3,
        "POWER_PROFILES_CATALOG.md §2.19 immersion (28 rows flat 13.4-13.5)",
    ),
    vnish(
        "Low-power (~14.1 J/TH)",
        206.0,
        2910,
        14.1,
        3,
        "POWER_PROFILES_CATALOG.md §2.19 + §5.1 (LP 14.1 J/TH)",
    ),
    vnish(
        "Performance max",
        448.0,
        7347,
        16.4,
        3,
        "POWER_PROFILES_CATALOG.md §2.19 perf top (102 profiles total)",
    ),
];

/// Antminer S21 XP Hydro / Immersion (BM1370 flagship). VNish §2.20/§2.21.
pub const S21_XP_POINTS: [OperatingPoint; 3] = [
    vnish(
        "XP Hydro flat 12.0 J/TH (flagship eff)",
        385.0,
        4620,
        12.0,
        3,
        "POWER_PROFILES_CATALOG.md §2.20 + §5.1 (S21 XP Hydro 12.0 J/TH)",
    ),
    vnish(
        "XP Hydro top",
        562.0,
        7588,
        13.5,
        3,
        "POWER_PROFILES_CATALOG.md §2.20 top",
    ),
    vnish(
        "XP Immersion perf best (13.5 J/TH)",
        218.0,
        2943,
        13.5,
        3,
        "POWER_PROFILES_CATALOG.md §2.21 + §5.1 (S21 XP Immersion 13.5 J/TH)",
    ),
];

// ===========================================================================
// Per-model registry + lookup.
// ===========================================================================

/// Antminer S9 (13.5 TH bin).
pub const S9: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S9",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 63,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &S9_POINTS,
};

/// Antminer S9 low-bin (12.5 TH stock-probe).
pub const S9_LOWBIN: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S9 (12.5 TH bin)",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 63,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &S9_LOWBIN_POINTS,
};

/// Antminer S9+ (84 chips/chain) — production curve is a GAP.
pub const S9_PLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S9+",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 84,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &S9_PLUS_POINTS,
};

/// Antminer S9++ (54 chips/chain) — entire profile is a GAP.
pub const S9_PLUSPLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S9++",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 54,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &S9_PLUSPLUS_POINTS,
};

/// Antminer T9 (57 chips/chain) — production curve is a GAP.
pub const T9: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer T9",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 57,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &T9_POINTS,
};

/// Antminer T9+ (18 chips/chain) — production curve is a GAP.
pub const T9_PLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer T9+",
    chip_family: "BM1387",
    chip_id: 0x1387,
    hashboards: 3,
    chips_per_board: 18,
    cores_per_chip: 114,
    cooling: Cooling::Air,
    points: &T9_PLUS_POINTS,
};

/// Antminer S17 Pro (BM1397).
pub const S17_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S17 Pro",
    chip_family: "BM1397",
    chip_id: 0x1397,
    hashboards: 3,
    chips_per_board: 48,
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &S17_PRO_POINTS,
};

/// Antminer S17+ (BM1396) — data-only silicon table; power data is VNish-only.
pub const S17_PLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S17+",
    chip_family: "BM1396",
    chip_id: 0x1396,
    hashboards: 3,
    chips_per_board: 65, // support-matrix scaffold; live enumerate still pending
    cores_per_chip: 0,   // BM1396 cores unknown
    cooling: Cooling::Air,
    points: &S17_PLUS_POINTS,
};

/// Antminer T17 (BM1397).
pub const T17: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer T17",
    chip_family: "BM1397",
    chip_id: 0x1397,
    hashboards: 3,
    chips_per_board: 30, // Pinned by BM1397 address-interval test + model catalog.
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &T17_POINTS,
};

/// Antminer T17+ (BM1396) — data-only silicon table; power data is VNish-only.
pub const T17_PLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer T17+",
    chip_family: "BM1396",
    chip_id: 0x1396,
    hashboards: 3,
    chips_per_board: 44, // INFERRED
    cores_per_chip: 0,
    cooling: Cooling::Air,
    points: &T17_PLUS_POINTS,
};

/// Antminer S19 Pro (BM1398) — live-confirmed on .129.
pub const S19_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19 Pro",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 114,
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &S19_PRO_POINTS,
};

/// Antminer S19 Pro Hydro (BM1398, water-cooled).
pub const S19_PRO_HYDRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19 Pro Hydro",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 114,
    cores_per_chip: 672,
    cooling: Cooling::Hydro,
    points: &S19_PRO_HYDRO_POINTS,
};

/// Antminer S19 (126 TH air, BM1398).
pub const S19_126: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19 (126 TH)",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 42,
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &S19_126_POINTS,
};

/// Antminer S19 (88 TH air, BM1398) — chips/board unknown.
pub const S19_88: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19 (88 TH)",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 0, // UNKNOWN (88 ambiguous: total vs per-board)
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &S19_88_POINTS,
};

/// Antminer S19a Pro (BM1398 air) — chips/board unknown.
pub const S19A_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19a Pro",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 0, // UNKNOWN; no BM1398 S19a Pro jig/levels.json
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &S19A_PRO_POINTS,
};

/// Antminer T19 (air, BM1398) — FULL GAP.
pub const T19: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer T19",
    chip_family: "BM1398",
    chip_id: 0x1398,
    hashboards: 3,
    chips_per_board: 0, // UNKNOWN — not in held RE
    cores_per_chip: 672,
    cooling: Cooling::Air,
    points: &T19_POINTS,
};

/// Antminer S19j Pro (BHB42601, BM1362) — fleet units .79/.25/.109.
pub const S19J_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19j Pro",
    chip_family: "BM1362",
    chip_id: 0x1362,
    hashboards: 3,
    chips_per_board: 126,
    cores_per_chip: 65, // die core count (silicon layer); autotuner uses nonce_attribution_cores=894
    cooling: Cooling::Air,
    points: &S19J_PRO_POINTS,
};

/// Antminer S19j Pro+ (BHB42611, BM1362, high-bin).
pub const S19J_PRO_PLUS: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19j Pro+",
    chip_family: "BM1362",
    chip_id: 0x1362,
    hashboards: 3,
    chips_per_board: 120, // BHB42611 = 120 (not 126)
    cores_per_chip: 65,
    cooling: Cooling::Air,
    points: &S19J_PRO_PLUS_POINTS,
};

/// Antminer S19j Pro-A / S19j XP (BHB42801 high-bin, BM1362). APW12+.
pub const S19J_PRO_A: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19j Pro-A / S19j XP",
    chip_family: "BM1362",
    chip_id: 0x1362,
    hashboards: 3,
    chips_per_board: 88,
    cores_per_chip: 65,
    cooling: Cooling::Air,
    points: &S19J_PRO_A_POINTS,
};

/// Antminer S19k Pro (BM1366) — live .78 anchor.
pub const S19K_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19k Pro",
    chip_family: "BM1366",
    chip_id: 0x1366,
    hashboards: 3,
    chips_per_board: 76,
    cores_per_chip: 894,
    cooling: Cooling::Air,
    points: &S19K_PRO_POINTS,
};

/// Antminer S19 XP (BM1366 air) — spec-only.
pub const S19_XP: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S19 XP",
    chip_family: "BM1366",
    chip_id: 0x1366,
    hashboards: 3,
    chips_per_board: 110,
    cores_per_chip: 894,
    cooling: Cooling::Air,
    points: &S19_XP_POINTS,
};

/// Antminer S21 (air, BM1368) — live-confirmed on .135.
pub const S21: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21",
    chip_family: "BM1368",
    chip_id: 0x1368,
    hashboards: 3,
    chips_per_board: 108,
    cores_per_chip: 1280,
    cooling: Cooling::Air,
    points: &S21_POINTS,
};

/// Antminer S21 Hydro (BM1368, water).
pub const S21_HYDRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21 Hydro",
    chip_family: "BM1368",
    chip_id: 0x1368,
    hashboards: 3,
    chips_per_board: 108,
    cores_per_chip: 1280,
    cooling: Cooling::Hydro,
    points: &S21_HYDRO_POINTS,
};

/// Antminer S21e Hydro (BM1368) — best-eff BM1368 SKU.
pub const S21E_HYDRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21e Hydro",
    chip_family: "BM1368",
    chip_id: 0x1368,
    hashboards: 3,
    chips_per_board: 108,
    cores_per_chip: 1280,
    cooling: Cooling::Hydro,
    points: &S21E_HYDRO_POINTS,
};

/// Antminer S21 Pro (air, BM1370) — RegisterMappedFromRE, no live unit.
pub const S21_PRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21 Pro",
    chip_family: "BM1370",
    chip_id: 0x1370,
    hashboards: 3,
    chips_per_board: 65,
    cores_per_chip: 1280,
    cooling: Cooling::Air,
    points: &S21_PRO_POINTS,
};

/// Antminer S21+ Hydro (BM1370) — richest BM1370 dataset.
pub const S21_PLUS_HYDRO: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21+ Hydro",
    chip_family: "BM1370",
    chip_id: 0x1370,
    hashboards: 3,
    chips_per_board: 0, // unknown — VNish carries no chip geometry
    cores_per_chip: 1280,
    cooling: Cooling::Hydro,
    points: &S21_PLUS_HYDRO_POINTS,
};

/// Antminer S21 XP Hydro / Immersion (BM1370 flagship).
pub const S21_XP: ModelPowerProfile = ModelPowerProfile {
    model: "Antminer S21 XP Hydro / Immersion",
    chip_family: "BM1370",
    chip_id: 0x1370,
    hashboards: 1,
    chips_per_board: 230, // support-matrix scaffold; first-light still pending
    cores_per_chip: 1280,
    cooling: Cooling::Immersion,
    points: &S21_XP_POINTS,
};

/// Every harvested model, S9 → S21. The single registry consumers iterate.
pub const ALL_MODELS: &[&ModelPowerProfile] = &[
    &S9,
    &S9_LOWBIN,
    &S9_PLUS,
    &S9_PLUSPLUS,
    &T9,
    &T9_PLUS,
    &S17_PRO,
    &S17_PLUS,
    &T17,
    &T17_PLUS,
    &S19_PRO,
    &S19_PRO_HYDRO,
    &S19_126,
    &S19_88,
    &S19A_PRO,
    &T19,
    &S19J_PRO,
    &S19J_PRO_PLUS,
    &S19J_PRO_A,
    &S19K_PRO,
    &S19_XP,
    &S21,
    &S21_HYDRO,
    &S21E_HYDRO,
    &S21_PRO,
    &S21_PLUS_HYDRO,
    &S21_XP,
];

/// Look up a model power profile by exact model name (case-sensitive).
pub fn by_model(model: &str) -> Option<&'static ModelPowerProfile> {
    ALL_MODELS.iter().copied().find(|m| m.model == model)
}

/// All harvested models for a chip ID. Multiple models can share a chip
/// (e.g. 0x1398 → S19 Pro / S19 / S19a Pro / T19). Returns them in
/// `ALL_MODELS` order.
pub fn by_chip_id(chip_id: u16) -> impl Iterator<Item = &'static ModelPowerProfile> {
    ALL_MODELS
        .iter()
        .copied()
        .filter(move |m| m.chip_id == chip_id)
}

/// Pick the best air-cooled measured power anchor for a chip ID, refusing to
/// substitute a hydro/immersion curve for an air-cooled unit. Used by the
/// autotuner power-target picker to find a measured watt anchor without
/// cross-cooling contamination. `cooling` is the unit's actual cooling.
///
/// Returns the first model (in `ALL_MODELS` order) matching both `chip_id`
/// and `cooling` that has a measured power anchor, else the first matching
/// model with any power data, else `None`.
pub fn measured_anchor_for(chip_id: u16, cooling: Cooling) -> Option<&'static ModelPowerProfile> {
    let matching: Vec<&'static ModelPowerProfile> = ALL_MODELS
        .iter()
        .copied()
        .filter(|m| m.chip_id == chip_id && m.cooling == cooling)
        .collect();
    matching
        .iter()
        .copied()
        .find(|m| m.has_measured_anchor())
        .or_else(|| {
            matching
                .iter()
                .copied()
                .find(|m| m.best_efficiency_point().is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_point_has_a_nonempty_source() {
        for m in ALL_MODELS {
            for p in m.points {
                assert!(
                    !p.source.is_empty(),
                    "{} / {} has empty source",
                    m.model,
                    p.label
                );
            }
        }
    }

    #[test]
    fn gap_points_carry_no_power_numbers() {
        // A Gap must never smuggle in a watt/hashrate value — that would let
        // a consumer treat a documented hole as fact.
        for m in ALL_MODELS {
            for p in m.points {
                if p.confidence.is_gap() {
                    assert!(
                        p.watts_per_unit.is_none()
                            && p.watts_per_board.is_none()
                            && p.hashrate_th_per_unit.is_none()
                            && p.hashrate_th_per_board.is_none()
                            && p.j_per_th.is_none(),
                        "{} / {} is Gap but carries power numbers",
                        m.model,
                        p.label
                    );
                    assert!(
                        !p.has_power_data(),
                        "{} / {} is Gap but has_power_data() is true",
                        m.model,
                        p.label
                    );
                }
            }
        }
    }

    #[test]
    fn non_gap_points_with_both_columns_have_consistent_j_per_th() {
        // Where a row carries per-unit watts + hashrate + a stated J/TH, the
        // stated J/TH must match the computed one within rounding (the
        // sources round J/TH to 1 decimal). Catches transcription errors.
        for m in ALL_MODELS {
            for p in m.points {
                if let (Some(w), Some(h), Some(j)) =
                    (p.watts_per_unit, p.hashrate_th_per_unit, p.j_per_th)
                {
                    if h > 0.0 {
                        let computed = w as f32 / h;
                        // Allow 5% slack: sources round both watts and
                        // hashrate independently before computing J/TH.
                        let rel = (computed - j).abs() / j;
                        assert!(
                            rel < 0.05,
                            "{} / {}: stated J/TH {} vs computed {} (>5% off)",
                            m.model,
                            p.label,
                            j,
                            computed
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn best_efficiency_point_skips_gaps() {
        let p = S19J_PRO.best_efficiency_point().unwrap();
        assert!(!p.confidence.is_gap());
        // BM1362 sweet spot is Step -9 @ 27.6 J/TH per the LuxOS live curve.
        let j = p.computed_j_per_th().unwrap();
        assert!(
            (27.0..=28.5).contains(&j),
            "S19j Pro best efficiency {} J/TH outside expected sweet-spot band",
            j
        );
    }

    #[test]
    fn s9_has_measured_anchor() {
        assert!(S9.has_measured_anchor());
    }

    #[test]
    fn live_confirmed_models_have_measured_anchors() {
        // The fleet-proven models must each carry at least one measured
        // power anchor so the autotuner can calibrate against real data.
        for m in [&S9, &S19_PRO, &S19J_PRO, &S19K_PRO, &S21] {
            assert!(
                m.has_measured_anchor(),
                "{} should have a measured power anchor",
                m.model
            );
        }
    }

    #[test]
    fn t19_air_is_full_gap() {
        // T19 air-cooled is a documented full gap — no model should claim
        // it has power data.
        assert!(!T19.has_measured_anchor());
        assert!(T19.best_efficiency_point().is_none());
        let (_m, _i, g) = T19.confidence_counts();
        assert!(g >= 1, "T19 must carry at least one Gap row");
    }

    #[test]
    fn t17_geometry_matches_driver_and_model_catalog() {
        assert_eq!(T17.chips_per_board, 30);
        assert_eq!(T17.hashboards, 3);
        assert_eq!(
            u32::from(T17.chips_per_board) * u32::from(T17.hashboards),
            90
        );
        assert_eq!(T17.cores_per_chip, 672);
        for point in T17.points {
            assert!(
                point.source.contains("chips/board pinned 30"),
                "plain T17 point must cite the reconciled 3 x 30 topology: {}",
                point.source
            );
            assert!(
                !point.source.contains("~44"),
                "plain T17 must not carry the old 3 x 44 inference: {}",
                point.source
            );
        }
    }

    #[test]
    fn by_chip_id_groups_bm1398_models() {
        let models: Vec<&str> = by_chip_id(0x1398).map(|m| m.model).collect();
        assert!(models.contains(&"Antminer S19 Pro"));
        assert!(models.contains(&"Antminer S19 (126 TH)"));
        assert!(models.contains(&"Antminer T19"));
    }

    #[test]
    fn by_model_finds_and_misses() {
        assert!(by_model("Antminer S9").is_some());
        assert!(by_model("Antminer NonExistent").is_none());
    }

    #[test]
    fn measured_anchor_refuses_cross_cooling_substitution() {
        // BM1370 S21 Pro is air but has no measured anchor (no live unit);
        // the BM1370 hydro/immersion curves must NOT be returned as an air
        // anchor. Air request returns the air S21 Pro (inferred, no measured)
        // — never a hydro model.
        let air = measured_anchor_for(0x1370, Cooling::Air);
        if let Some(m) = air {
            assert_eq!(
                m.cooling,
                Cooling::Air,
                "must not return a hydro model for an air request"
            );
        }
        // Hydro request for BM1370 returns a hydro model.
        let hydro = measured_anchor_for(0x1370, Cooling::Hydro);
        assert!(hydro.is_some());
        assert_eq!(hydro.unwrap().cooling, Cooling::Hydro);
    }

    #[test]
    fn s17_pro_legacy_curve_supersedes_crate_675_overstatement() {
        // The harvest's #3 caveat: crate bm1397.rs step0 = 675MHz/76TH/3182W
        // (41.9 J/TH) overstates efficiency vs the live VNish curve. The
        // legacy 675 MHz row here is 65 TH / 2680 W (41.2 J/TH) and is the
        // VendorExtracted authority.
        let p = S17_PRO
            .point_by_label("profile_675_65T (supersedes crate 675/76TH)")
            .unwrap();
        assert_eq!(p.hashrate_th_per_unit, Some(65.0));
        assert_eq!(p.watts_per_unit, Some(2680));
    }

    #[test]
    fn s19kpro_vnish_curve_is_leaner_than_crate_default() {
        // The harvest's highest-priority S19k Pro correction: crate
        // bm1366.rs default = 120TH/3420W (28.5 J/TH) but the VNish
        // measured curve is ~25.8 J/TH flat (120 TH ≈ 3120 W). The VNish
        // nameplate row here must be leaner than the crate's 3420 W.
        let p = S19K_PRO
            .point_by_label("VNish stock nameplate (catalog-stated)")
            .unwrap();
        assert!(p.watts_per_unit.unwrap() < 3420);
    }

    #[test]
    fn confidence_counts_sum_to_point_count() {
        for m in ALL_MODELS {
            let (mm, i, g) = m.confidence_counts();
            assert_eq!(mm + i + g, m.points.len(), "{} count mismatch", m.model);
        }
    }

    #[test]
    fn json_round_trip_preserves_operating_point() {
        // `OperatingPoint` carries `&'static str` fields, so a normal
        // `from_str(&local_string)` can't borrow-deserialize. Leak the JSON
        // to `'static` for the round-trip (test-only — bounded leak).
        let p = S9_POINTS[3]; // nameplate
        let json: &'static str = Box::leak(serde_json::to_string(&p).unwrap().into_boxed_str());
        let back: OperatingPoint = serde_json::from_str(json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn all_models_covers_s9_through_s21_chip_families() {
        // Sanity: every mapped chip family is represented at least once.
        let families: Vec<&str> = ALL_MODELS.iter().map(|m| m.chip_family).collect();
        for fam in [
            "BM1387", "BM1397", "BM1396", "BM1398", "BM1362", "BM1366", "BM1368", "BM1370",
        ] {
            assert!(families.contains(&fam), "missing chip family {}", fam);
        }
    }
}
