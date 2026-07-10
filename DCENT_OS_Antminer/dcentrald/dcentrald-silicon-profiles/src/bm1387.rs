//! BM1387 silicon characterization table (Antminer S9 / S9i / T9).
//!
//! 7 discrete steps from `-3` (eco-low) to `+3` (extreme overclock).
//! Source provenance:
//! - **`AMTC_TEST_JIG_RE.md` Test Jig Config.ini** (live-confirmed): S9 profile
//!   with BM1387 = 63 chips/chain Ã— 114 cores @ 890 mV per-chip core voltage,
//!   300-550 MHz functional test range, sensor TMP421, CommandMode=VIL.
//! - **S9 Maintenance Guide / Bitmain datasheet** (operator-confirmed): nameplate
//!   13.5 TH/s @ 600 MHz / 1320 W wall (default Step 0).
//! - **Reconstructed**: extrapolated linearly from the 600 MHz / 13.5 TH/s
//!   anchor using the documented 22.5 mW/(MHzÂ·chip) silicon constant.
//!
//! Sweet spot: Step -1 (450 MHz / 9.0 V chain / ~1020 W / ~9.6 TH/s) at
//! ~106 J/TH â€” substantially better than the nameplate ~98 J/TH but trades
//! peak hashrate for efficiency, matching home-mining heater optimization.
//!
//! Hardware constants (per AMTC):
//! - `BM1387 = 114 cores per chip` (S9 standard) / `BM1387P = 128 cores`
//!   (die revision used in S9+; not in this table).
//! - `BM1390P = 128 cores` (S11; separate chip family â€” not BM1387).
//! - Bug-core tolerance: up to 7 defective cores per chip (PIC flash 0xF80).
//!
//! Voltage convention: `voltage_v` = chain rail voltage in volts (set by PIC
//! DAC formula `pic_val = round(1608.42 - 170.42 * V)`; range 7.94 V .. 9.44 V).
//! NOT to be confused with the 890 mV per-chip core voltage from AMTC, which
//! lives downstream of the on-board DC-DC step-down.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 7 BM1387 silicon profile rows, ordered by `step`.
///
/// Wall watts and hashrate values reflect the well-known S9 efficiency
/// curve: underclocking improves J/TH up to a knee around 450 MHz, after
/// which leakage/IO overhead dominates. Above the 600 MHz nameplate the
/// voltage required to maintain stability rises super-linearly, eroding
/// J/TH efficiency.
pub const BM1387_PROFILES: [Profile; 7] = [
    Profile {
        step: -3,
        freq_mhz: 250,
        voltage_v: 8.4,
        wall_watts: Some(620),
        hashrate_ths: Some(5.36),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -2,
        freq_mhz: 350,
        voltage_v: 8.6,
        wall_watts: Some(750),
        hashrate_ths: Some(7.5),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 450,
        voltage_v: 8.8,
        wall_watts: Some(880),
        hashrate_ths: Some(9.64),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: 0,
        freq_mhz: 600,
        voltage_v: 9.1,
        wall_watts: Some(1320),
        hashrate_ths: Some(13.50),
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 650,
        voltage_v: 9.2,
        wall_watts: Some(1500),
        hashrate_ths: Some(14.43),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 700,
        voltage_v: 9.3,
        wall_watts: Some(1720),
        hashrate_ths: Some(15.40),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 3,
        freq_mhz: 750,
        voltage_v: 9.4,
        wall_watts: Some(1980),
        hashrate_ths: Some(16.30),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1387 silicon table. Default = nameplate (Step 0).
/// Sweet spot pre-baked at Step -1 (450 MHz / 9.0 V); J/TH efficiency
/// minimum at this step within the documented range.
pub const BM1387_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1387",
    profiles: &BM1387_PROFILES,
    default_step: 0,
    sweet_spot_step: -1,
    // S9 sustained cold-boot mining 2026-04-19 (10+ TH/s, 31+ shares).
    live_status: crate::ChipStatus::LiveConfirmed,
};

/// Cores per BM1387 chip, per AMTC Config.ini.
pub const BM1387_CORES_PER_CHIP: u32 = 114;

/// Standard S9 chip count per chain (3 chains Ã— 63 = 189 chips total).
pub const BM1387_CHIPS_PER_CHAIN_S9: u32 = 63;

/// Standard S9 chain count.
pub const BM1387_CHAIN_COUNT_S9: u32 = 3;

/// Bug-core tolerance per AMTC: up to 7 defective cores per chip stored
/// in PIC flash at 0xF80.
pub const BM1387_BUG_CORE_TOLERANCE_PER_CHIP: u32 = 7;

/// A01 (goldmine 2026-06-10): BM1391 (Antminer S11) chips-per-chain = 84.
///
/// Distinct from the S9's `BM1387_CHIPS_PER_CHAIN_S9 = 63`. From the HashSource
/// S11 single-board-test jig `board_init@1338C`, whose enumeration loop retries
/// until the count equals 84 (findings/s1-bm1391-s11.md F10). Additive catalog
/// constant — no runtime change unless/until S11 hardware is auto-detected
/// (no live S11 in the fleet today). The registry-side mirror is
/// `crate::registry::S11_BM1391_DEFAULT_CHIPS_PER_CHAIN`.
pub const BM1391_CHIPS_PER_CHAIN: u32 = 84;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_seven_steps_in_correct_range() {
        assert_eq!(BM1387_TABLE.profiles.len(), 7);
        assert_eq!(BM1387_TABLE.min_step(), -3);
        assert_eq!(BM1387_TABLE.max_step(), 3);
    }

    #[test]
    fn by_step_lookup_returns_canonical_rows() {
        let p = BM1387_TABLE.by_step(0).unwrap();
        assert_eq!(p.freq_mhz, 600);
        assert_eq!(p.wall_watts, Some(1320));
        let p = BM1387_TABLE.by_step(-1).unwrap();
        assert_eq!(p.freq_mhz, 450);
        assert_eq!(p.source, ProfileSource::LiveConfirmed);
    }

    #[test]
    fn by_name_lookup_handles_default_and_freq_naming() {
        // Step 0 is named "default" per `Profile::profile_name()`.
        let p = BM1387_TABLE.by_name("default").unwrap();
        assert_eq!(p.step, 0);
        // Other steps are named "<freq>MHz".
        let p = BM1387_TABLE.by_name("450MHz").unwrap();
        assert_eq!(p.step, -1);
        // Unknown name â†’ None.
        assert!(BM1387_TABLE.by_name("999MHz").is_none());
    }

    #[test]
    fn default_profile_matches_s9_nameplate() {
        let default = BM1387_TABLE.default_profile().unwrap();
        // S9 nameplate: 13.5 TH/s @ 600 MHz / 1320 W.
        assert_eq!(default.freq_mhz, 600);
        assert_eq!(default.wall_watts, Some(1320));
        assert!((default.hashrate_ths.unwrap() - 13.50).abs() < 0.01);
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre_baked = BM1387_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1387_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(
            pre_baked.step, computed.step,
            "sweet_spot_step={} but computed minimum J/TH is at step {}",
            pre_baked.step, computed.step
        );
    }

    #[test]
    fn step_minus_one_efficiency_is_better_than_default() {
        let s_minus_1 = BM1387_TABLE.by_step(-1).unwrap();
        let default = BM1387_TABLE.default_profile().unwrap();
        let eff_low = s_minus_1.watts_per_ths().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        assert!(
            eff_low < eff_def,
            "step -1 J/TH ({}) should be lower than default J/TH ({})",
            eff_low,
            eff_def
        );
    }

    #[test]
    fn hardware_constants_match_amtc() {
        assert_eq!(BM1387_CORES_PER_CHIP, 114);
        assert_eq!(BM1387_CHIPS_PER_CHAIN_S9, 63);
        assert_eq!(BM1387_CHAIN_COUNT_S9, 3);
        assert_eq!(BM1387_BUG_CORE_TOLERANCE_PER_CHIP, 7);
    }

    #[test]
    fn bm1391_s11_chips_per_chain_is_84() {
        // A01 (goldmine 2026-06-10): S11/BM1391 = 84 chips/chain (jig
        // board_init@1338C enumeration loop `!= 84`), distinct from S9's 63.
        assert_eq!(BM1391_CHIPS_PER_CHAIN, 84);
        assert_ne!(BM1391_CHIPS_PER_CHAIN, BM1387_CHIPS_PER_CHAIN_S9);
    }

    #[test]
    fn json_round_trip_preserves_every_field() {
        let original = BM1387_TABLE.by_step(-1).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }
}
