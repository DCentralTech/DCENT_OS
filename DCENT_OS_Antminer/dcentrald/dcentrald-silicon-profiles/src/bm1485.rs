//! BM1485 silicon characterization table (Antminer L3 / L3+ / L3++,
//! LiteAxe scrypt mining).
//!
//! 5 discrete steps from `-2` (eco-low) to `+2` (overclock). Source
//! provenance:
//! - **`SCRYPT_ASIC_CHIPS.md` Â§3** (live-confirmed): BM1485 = 12 cores
//!   per chip (`BM1485_CORE_NUM = 12` from cgminer-ltc source), 72
//!   chips/board on L3+, 504 MH/s @ 800 W stock.
//! - **L3+ Repair Guide** (operator-confirmed): nameplate 504 MH/s @
//!   800 W (default Step 0).
//! - **Reconstructed**: linear extrapolation from the 800 W nameplate.
//!
//! Sweet spot: Step -1 (~600 W / ~430 MH/s) at ~1.39 J/MH â€” modest
//! efficiency improvement over nameplate ~1.59 J/MH.
//!
//! **Note**: scrypt mining is measured in MH/s (not TH/s). The
//! `Profile::hashrate_ths` field is repurposed to carry MH/s Ã— 1e-6
//! so the same `Profile` struct can hold scrypt data without breaking
//! existing SHA-256 consumers â€” but consumers MUST treat BM1485 rows
//! as MH/s when displayed. The label / chip_family discriminator on
//! `SiliconTable` makes this explicit.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 5 BM1485 silicon profile rows, ordered by `step`.
///
/// `hashrate_ths` field carries MH/s Ã— 1e-6 (scaled for shared struct).
/// E.g. 504 MH/s â†’ 0.000504. Display layer multiplies by 1e6 for MH/s.
pub const BM1485_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 270,
        voltage_v: 9.6,
        wall_watts: Some(480),
        hashrate_ths: Some(0.000_360), // 360 MH/s
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 320,
        voltage_v: 9.8,
        wall_watts: Some(600),
        hashrate_ths: Some(0.000_432), // 432 MH/s â€” sweet spot
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 384,
        voltage_v: 10.0,
        wall_watts: Some(800),
        hashrate_ths: Some(0.000_504), // 504 MH/s â€” L3+ nameplate
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 432,
        voltage_v: 10.2,
        wall_watts: Some(950),
        hashrate_ths: Some(0.000_565),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 480,
        voltage_v: 10.4,
        wall_watts: Some(1100),
        hashrate_ths: Some(0.000_625),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1485 silicon table. Default = nameplate (Step 0).
/// Sweet spot at Step -2 (~480 W / 360 MH/s â‰ˆ 1.333 W/MH) â€” scrypt
/// silicon, like SHA-256, gets more efficient as you underclock.
pub const BM1485_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1485",
    profiles: &BM1485_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    // L3+ nameplate 384 MHz / 10.0 V is operator-confirmed; register
    // set is `[GAP]` §8 (no
    // dcentrald-asic driver yet). Treat as register-mapped only.
    live_status: crate::ChipStatus::RegisterMappedFromRE,
};

/// Cores per BM1485 chip per cgminer-ltc source `BM1485_CORE_NUM = 12`.
pub const BM1485_CORES_PER_CHIP: u32 = 12;

/// Standard L3+ chip count per chain-board: 72 chips/board (the chip-comm
/// chains run Chip 1 -> Chip 72), x4 chain-boards/miner = 288 total chips, per
///  §77-81
/// ("Chips per Board: 72 (L3+)", "Total Chips/Miner: 288 (L3+)"). Telemetry/
/// geometry only — no PLL/voltage/safety consumer (bug-hunt HIGH 2026-05-28
/// corrected a stale `18` that contradicted the RE doc + this file's own
/// 504/288≈1.75 MH/s per-chip figure).
pub const BM1485_CHIPS_PER_CHAIN_L3PLUS: u32 = 72;

/// Standard L3+ chain count per miner.
pub const BM1485_CHAIN_COUNT_L3PLUS: u32 = 4;

/// Per-chip hashrate at L3+ nameplate (504 MH/s / 288 chips â‰ˆ 1.75 MH/s).
pub const BM1485_PER_CHIP_HASHRATE_MHS: f32 = 1.75;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_steps_in_correct_range() {
        assert_eq!(BM1485_TABLE.profiles.len(), 5);
        assert_eq!(BM1485_TABLE.min_step(), -2);
        assert_eq!(BM1485_TABLE.max_step(), 2);
    }

    #[test]
    fn nameplate_default_step_anchors_l3_plus() {
        // L3+ nameplate: 504 MH/s @ 800 W (per Repair Guide).
        let default = BM1485_TABLE.default_profile().unwrap();
        assert_eq!(default.wall_watts, Some(800));
        // hashrate_ths = MH/s Ã— 1e-6 â†’ 504 MH/s = 0.000504.
        assert!((default.hashrate_ths.unwrap() - 0.000_504).abs() < 1e-9);
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre_baked = BM1485_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1485_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(pre_baked.step, computed.step);
    }

    #[test]
    fn underclocked_steps_beat_default_efficiency() {
        // Scrypt silicon (like SHA-256) gets more efficient as you
        // underclock. Verify both -1 and -2 beat the nameplate.
        let default = BM1485_TABLE.default_profile().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        for step in [-2, -1] {
            let s = BM1485_TABLE.by_step(step).unwrap();
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
    fn hardware_constants_match_re_doc() {
        assert_eq!(BM1485_CORES_PER_CHIP, 12);
        assert_eq!(BM1485_CHIPS_PER_CHAIN_L3PLUS, 72);
        assert_eq!(BM1485_CHAIN_COUNT_L3PLUS, 4);
        // RE doc SCRYPT_ASIC_CHIPS.md §77-81: 72 chips/board x 4 boards = 288 total.
        let total = BM1485_CHIPS_PER_CHAIN_L3PLUS * BM1485_CHAIN_COUNT_L3PLUS;
        assert_eq!(total, 288, "L3+ total chips must be 288 per the RE doc");
        // Per-chip nameplate: 504 MH/s / 288 chips ≈ 1.75 MH/s — confirms the
        // chip count is internally consistent with BM1485_PER_CHIP_HASHRATE_MHS.
        assert!((504.0_f32 / total as f32 - BM1485_PER_CHIP_HASHRATE_MHS).abs() < 0.01);
    }

    #[test]
    fn json_round_trip_preserves_profile_fields() {
        let original = BM1485_TABLE.by_step(-1).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    #[test]
    fn step_voltage_increases_with_frequency() {
        // Sanity: as freq increases through the steps, voltage also
        // increases monotonically (VÂ²f scales).
        for window in BM1485_PROFILES.windows(2) {
            assert!(
                window[1].voltage_v >= window[0].voltage_v,
                "voltage non-monotonic at step {}",
                window[1].step
            );
        }
    }

    #[test]
    fn nameplate_hashrate_matches_repair_guide_504mhs() {
        // RE doc Â§2 line 80: "504 MH/s (L3+)".
        let default = BM1485_TABLE.default_profile().unwrap();
        // Convert hashrate_ths back to MH/s.
        let mhs = default.hashrate_ths.unwrap() * 1e6;
        assert!((mhs - 504.0).abs() < 0.5);
    }
}
