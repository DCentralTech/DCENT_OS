//! BM1397 silicon characterization table (Antminer S17 / S17 Pro / S19
//! / S19j â€” first-generation BM139x chip family on Zynq am1).
//!
//! 5 discrete steps from `-2` (eco-low) to `+2` (overclock). Source
//! provenance:
//! - **`mining-bible-v1/_canonical/chip-init-sequences.md`** â€”
//!   BM1397 register-init sequence with `cores_per_chip = 672` and the
//!   PLL3-derived 6.25 Mbaud upgrade.
//! - **`mining-bible-v1/_canonical/baud-switching-analysis.md` line 33**
//!   â€” BM1397 op baud 6.25 Mbaud, MiscCtrl `0x18 = 0x00006031`.
//! - **AMTC test jig + S17 maintenance docs** â€” S17 Pro nameplate
//!   ~76 TH/s @ ~3,182 W (â‰ˆ 41.9 W/TH) with 48 chips/chain Ã— 3
//!   chains = 144 chips total.
//! - **Reconstructed**: linear extrapolation around the operator-known
//!   nameplate point, identical pattern to the existing `bm1387.rs`
//!   and `bm1485.rs` modules. Live cgminer-API capture from an S17
//!   running BraiinsOS+ is queued for re-verification.
//!
//! Sweet spot at Step -2 (~2,520 W / ~62 TH/s â‰ˆ 40.6 J/TH) â€” slightly
//! better than nameplate efficiency, mirroring the underclock-efficient
//! pattern seen on every Bitmain SHA-256 chip we've characterized.
//!
//! `voltage_v` is the chain-rail voltage. BM1397 cold-boots to ~14.8V
//! open-core overshoot, then trims to 13.8V autotune-target.
//!
//! ## Harvest cross-reference (2026-06-14) — legacy VNish curve supersedes
//! the reconstructed watt rows here for S17 Pro power estimation.
//!
//! The 5 rows below are `Reconstructed` (and step 0 is labeled
//! `OperatorConfirmed` for the *nameplate*, but there is **no live S17** on
//! the fleet — the table is `RegisterMappedFromRE`). The step-0 anchor
//! 675 MHz / 76 TH / 3182 W (≈41.9 J/TH) **overstates efficiency** vs the
//! operator's live VNish RE curve, which puts 675 MHz at **65 TH / 2680 W**
//! (≈41.2 J/TH) and stock S17 Pro at ~53 TH @ 1750 W.
//!
//! These step rows are intentionally **left unchanged** (they remain the
//! step-ladder the autotuner walks, and the existing tests pin them). The
//! authoritative per-MODEL S17 Pro / S17+ / T17 / T17+ watt curves now live
//! in [`crate::operating_points`] (`S17_PRO`, `S17_PLUS`, `T17`, `T17_PLUS`)
//! as `VendorExtracted` (POWER_PROFILES_CATALOG §3.1-3.4). Power-estimate
//! consumers should prefer those `VendorExtracted` rows over these
//! reconstructed watts. See `operating_points::S17_PRO` row
//! `"profile_675_65T (supersedes crate 675/76TH)"`.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 5 BM1397 silicon profile rows, ordered by `step`.
///
/// Voltage column is the chain-rail voltage in volts. Hashrate column
/// is in TH/s summed across 3 boards Ã— 48 chips.
pub const BM1397_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 575,
        voltage_v: 13.4,
        wall_watts: Some(2520),
        hashrate_ths: Some(62.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 625,
        voltage_v: 13.6,
        wall_watts: Some(2840),
        hashrate_ths: Some(69.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 675,
        voltage_v: 13.8,
        wall_watts: Some(3182),
        hashrate_ths: Some(76.0), // S17 Pro nameplate
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 720,
        voltage_v: 14.0,
        wall_watts: Some(3520),
        hashrate_ths: Some(81.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 765,
        voltage_v: 14.2,
        wall_watts: Some(3870),
        hashrate_ths: Some(86.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1397 silicon table. Default = nameplate (Step 0).
/// Sweet spot at Step -2 (~2,520 W / 62 TH/s â‰ˆ 40.6 J/TH).
pub const BM1397_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1397",
    profiles: &BM1397_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    // HONESTY FIX 2026-06-10 (HashSource S17 jig RE): BM1397 has NEVER been
    // hashed on a live DCENT_OS unit (no S17 on the fleet), so LiveConfirmed
    // (rank 5 = "hashed on real hardware") was an over-claim. The register set
    // is now byte-exact RE-confirmed from the S17 single-board-test jig (see
    // the RE consts below +  §2) → the honest
    // status is RegisterMappedFromRE (rank 3), matching bm1370/bm1373/bm1360/
    // bm1485/bm1491. ChipFamily::Bm1397 production membership is a separate
    // hardcoded registry list (registry.rs), unaffected by this label. Promote
    // back to LiveConfirmed only after a real S17 hashes under DCENT_OS.
    live_status: crate::ChipStatus::RegisterMappedFromRE,
};

/// Per-chip `cores_per_chip` per
/// `mining-bible-v1/_canonical/chip-init-sequences.md` BM1397 entry.
pub const BM1397_CORES_PER_CHIP: u32 = 672;

/// Standard Antminer S17 Pro chips per chain (48).
pub const BM1397_CHIPS_PER_CHAIN_S17_PRO: u32 = 48;

/// Standard S17 Pro chain count (3).
pub const BM1397_CHAIN_COUNT_S17_PRO: u32 = 3;

/// Operational baud after baud-upgrade per `baud-switching-analysis.md`.
pub const BM1397_OPERATIONAL_BAUD: u32 = 6_250_000;

/// Canonical MiscCtrl value to write at register 0x18 to upgrade to the
/// operational baud.
pub const BM1397_MISCCTRL_BAUD_VALUE: u32 = 0x0000_6031;

// ── HashSource S17 BHB07601 test-jig RE (2026-06-10) ──────────────────────
// Byte-exact from the Bitmain S17 single-board-test binary, decompiled corpus
// .
// CATALOG/REFERENCE constants documenting the BHB07601 (S17 BM1397) factory-jig
// chain path — NOT wired into a live path (no live S17 unit; the AM2 path uses
// BM1397_MISCCTRL_BAUD_VALUE above). The fast-UART mechanism here is a DISTINCT
// two-register write, NOT the single MiscCtrl 0x18 write — do not merge; the
// divisor still needs an on-wire scope confirm. See
//

/// BM1397 chip-ID reply bytes (reg-0 read): `[0x13, 0x97]` confirm BM1397
/// present. Source `check_BM1397_asic_reg@266C8.c` L96-101.
pub const BM1397_CHIP_ID_REPLY: [u8; 2] = [0x13, 0x97];

/// BM1397 MISC_CONTROL register default, RE-confirmed from the S17 jig
/// (`reset_single_BM1397_global_arg@26AAC.c` L29). Distinct from the AM2
/// baud-upgrade value `BM1397_MISCCTRL_BAUD_VALUE` (0x6031) above.
pub const BM1397_MISC_CONTROL_DEFAULT: u32 = 0x0000_3A01;

/// BM1397 fast-UART (BHB07601 jig path) = a TWO-register write: reg 0x68 then
/// reg 0x28. Source `set_baud_ext@269A4.c`. UNVERIFIED on-wire — do NOT wire
/// into a live path without a scope capture (RE-ASK-CHIP-BM1397).
pub const BM1397_FAST_UART_REG68: u32 = 0xC066_0011;
pub const BM1397_FAST_UART_REG28: u32 = 0x0600_000F;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_steps_in_correct_range() {
        assert_eq!(BM1397_TABLE.profiles.len(), 5);
        assert_eq!(BM1397_TABLE.min_step(), -2);
        assert_eq!(BM1397_TABLE.max_step(), 2);
    }

    #[test]
    fn nameplate_default_step_anchors_s17_pro() {
        // S17 Pro nameplate: 76 TH/s @ 3,182 W â†’ â‰ˆ 41.9 W/TH.
        let default = BM1397_TABLE.default_profile().unwrap();
        assert_eq!(default.wall_watts, Some(3182));
        assert!((default.hashrate_ths.unwrap() - 76.0).abs() < 1e-3);
        let eff = default.watts_per_ths().unwrap();
        assert!(
            (40.0..=43.0).contains(&eff),
            "S17 Pro nameplate efficiency {} W/TH outside [40, 43]",
            eff
        );
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre = BM1397_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1397_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(pre.step, computed.step);
    }

    #[test]
    fn underclocked_steps_beat_default_efficiency() {
        let default = BM1397_TABLE.default_profile().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        for step in [-2, -1] {
            let s = BM1397_TABLE.by_step(step).unwrap();
            let eff = s.watts_per_ths().unwrap();
            assert!(
                eff < eff_def,
                "step {} efficiency ({}) should beat default ({})",
                step,
                eff,
                eff_def
            );
        }
    }

    #[test]
    fn s17_pro_hardware_constants_match_re_doc() {
        assert_eq!(BM1397_CORES_PER_CHIP, 672);
        assert_eq!(BM1397_CHIPS_PER_CHAIN_S17_PRO, 48);
        assert_eq!(BM1397_CHAIN_COUNT_S17_PRO, 3);
        // 48 Ã— 3 = 144 chips total per S17 Pro.
        assert_eq!(
            BM1397_CHIPS_PER_CHAIN_S17_PRO * BM1397_CHAIN_COUNT_S17_PRO,
            144
        );
    }

    #[test]
    fn operational_baud_matches_re_doc() {
        // baud-switching-analysis.md line 33: 6.25 Mbaud.
        assert_eq!(BM1397_OPERATIONAL_BAUD, 6_250_000);
        assert_eq!(BM1397_MISCCTRL_BAUD_VALUE, 0x0000_6031);
    }

    #[test]
    fn step_voltage_increases_with_frequency() {
        for window in BM1397_PROFILES.windows(2) {
            assert!(
                window[1].voltage_v >= window[0].voltage_v,
                "voltage non-monotonic at step {}",
                window[1].step
            );
            assert!(
                window[1].freq_mhz > window[0].freq_mhz,
                "frequency non-monotonic at step {}",
                window[1].step
            );
        }
    }

    #[test]
    fn json_round_trip_preserves_profile_fields() {
        let original = BM1397_TABLE.by_step(0).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    #[test]
    fn nameplate_voltage_sits_at_138v_autotune_target() {
        // Per baud-switching-analysis.md + chip-init-sequences.md, the
        // BM1397 autotune target is 13.8V (post-open-core trim from
        // 14.8V open-core overshoot). Pin the nameplate row at the
        // documented autotune target.
        let default = BM1397_TABLE.default_profile().unwrap();
        assert!((default.voltage_v - 13.8).abs() < 1e-3);
    }
}
