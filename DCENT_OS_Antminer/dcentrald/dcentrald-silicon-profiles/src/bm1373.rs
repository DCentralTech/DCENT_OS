//! BM1373 silicon characterization table - Antminer S23 scaffold.
//!
//! BM1373 is gated behind the `experimental_chips` feature from `lib.rs`.
//! This module intentionally carries only reconstructed placeholder rows:
//! the ASIC driver scaffold exists, but there is no live S23 first-hash
//! capture, vendor table, or confirmed per-chip geometry in the current RE
//! corpus. Keeping the table explicit prevents a detected BM1373 from
//! silently falling through to BM1370 defaults.
//!
//! Source discipline:
//! -  records the
//!   S23 hypothesis and projected 318 TH/s / ~3498 W class.
//! - `dcentrald-asic/src/drivers/bm1373.rs` is a pre-hardware scaffold
//!   with projected BM1370-derived register values.
//! - No row below is live-confirmed or vendor-extracted.

use crate::{Profile, ProfileSource, SiliconTable};

/// Placeholder BM1373 silicon profile rows.
///
/// Every numeric value is reconstructed. Consumers must treat
/// `BM1373_HAS_LIVE_DATA == false` as a refuse-to-mine signal unless an
/// explicit lab gate enables scaffold chips.
pub const BM1373_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 480,
        voltage_v: 13.0,
        wall_watts: Some(2800),
        hashrate_ths: Some(255.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 505,
        voltage_v: 13.2,
        wall_watts: Some(3150),
        hashrate_ths: Some(285.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 525,
        voltage_v: 13.4,
        wall_watts: Some(3498),
        hashrate_ths: Some(318.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 1,
        freq_mhz: 555,
        voltage_v: 13.7,
        wall_watts: Some(3850),
        hashrate_ths: Some(342.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 585,
        voltage_v: 14.0,
        wall_watts: Some(4250),
        hashrate_ths: Some(365.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1373 silicon table.
pub const BM1373_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1373",
    profiles: &BM1373_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    live_status: crate::ChipStatus::RegisterMappedFromRE,
};

/// Big SHA-256 cores per BM1373 chip.
///
/// NerdQAxePlus early bring-up RE (128 big cores/chip, commit 67dc677a),
/// aligned with `dcentrald-asic/src/drivers/bm1373.rs::CORES_PER_CHIP`. Still
/// needs live S23 verification (NerdQAxePlus marks several values TODO).
pub const BM1373_CORES_PER_CHIP: u32 = 128;

/// Small (nonce-attribution) cores per BM1373 chip.
///
/// NerdQAxePlus `BM1373_SMALL_CORE_COUNT` (6860, corrected down from 7000,
/// commit 36124e1e). Mirrors the driver scaffold's `SMALL_CORE_COUNT`.
pub const BM1373_SMALL_CORE_COUNT: u32 = 6860;

/// Projected default chips per chain, copied from the ASIC driver scaffold.
pub const BM1373_CHIPS_PER_CHAIN_PROJECTED: u32 = 90;

/// Standard projected S23 chain count.
pub const BM1373_CHAIN_COUNT_PROJECTED: u32 = 3;

/// Whether any BM1373 row in the table is live-confirmed.
pub const BM1373_HAS_LIVE_DATA: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_projected_steps() {
        assert_eq!(BM1373_TABLE.profiles.len(), 5);
        assert_eq!(BM1373_TABLE.min_step(), -2);
        assert_eq!(BM1373_TABLE.max_step(), 2);
        assert_eq!(BM1373_TABLE.default_step, 0);
    }

    #[test]
    fn every_row_is_reconstructed_placeholder() {
        for p in BM1373_PROFILES.iter() {
            assert_eq!(
                p.source,
                ProfileSource::Reconstructed,
                "BM1373 step {} must remain Reconstructed until live S23 data lands",
                p.step
            );
        }
        assert!(!BM1373_HAS_LIVE_DATA);
    }

    #[test]
    fn default_row_matches_projected_s23_class() {
        let default = BM1373_TABLE.default_profile().unwrap();
        assert_eq!(default.freq_mhz, 525);
        assert_eq!(default.wall_watts, Some(3498));
        assert_eq!(default.hashrate_ths, Some(318.0));
    }

    #[test]
    fn projected_geometry_matches_driver_scaffold() {
        // Core counts are NerdQAxePlus RE (128 big / 6860 small); chips-per-chain
        // + chain-count stay projected. All still need live S23 verification.
        assert_eq!(BM1373_CORES_PER_CHIP, 128);
        assert_eq!(BM1373_SMALL_CORE_COUNT, 6860);
        assert_eq!(BM1373_CHIPS_PER_CHAIN_PROJECTED, 90);
        assert_eq!(BM1373_CHAIN_COUNT_PROJECTED, 3);
    }

    #[test]
    fn chip_family_label_matches_scaffold_id() {
        assert_eq!(BM1373_TABLE.chip_family, "BM1373");
    }
}
