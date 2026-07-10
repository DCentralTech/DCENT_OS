//! BM1391 data-only silicon profile for S11/S15/T15 scaffolds.
//!
//! The BM1391 ASIC driver remains scaffold-gated and production-unregistered.
//! This module records the host-testable geometry split so S15/T15 metadata can
//! stop borrowing S9 geometry while still refusing live mining by default.

use crate::{Profile, ProfileSource, SiliconTable};

/// BM1391 numeric chip ID from the S11/S15/T15 family catalog.
pub const BM1391_CHIP_ID: u32 = 0x0000_1391;

/// HashSource S11 jig geometry.
pub const BM1391_CHIPS_PER_CHAIN_S11_JIG: u32 = 84;

/// S15 scaffold geometry follows the S11 jig until a live S15 capture replaces it.
pub const BM1391_CHIPS_PER_CHAIN_S15_SCAFFOLD: u32 = 84;

/// T15 scaffold geometry pinned by the hashboard catalog.
pub const BM1391_CHIPS_PER_CHAIN_T15_SCAFFOLD: u32 = 63;

pub const BM1391_CHAIN_COUNT: u32 = 3;

/// Single conservative host-planning row. The voltage and performance fields
/// are deliberately unknown because retained sources disagree on BM1391 core
/// geometry and voltage.
pub const BM1391_PROFILES: [Profile; 1] = [Profile {
    step: 0,
    freq_mhz: 500,
    voltage_v: 0.0,
    wall_watts: None,
    hashrate_ths: None,
    source: ProfileSource::VendorExtracted,
}];

pub const BM1391_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1391",
    profiles: &BM1391_PROFILES,
    default_step: 0,
    sweet_spot_step: 0,
    live_status: crate::ChipStatus::NamedOnly,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1391_scaffold_geometry_is_explicit() {
        assert_eq!(BM1391_CHIP_ID, 0x1391);
        assert_eq!(BM1391_CHAIN_COUNT, 3);
        assert_eq!(BM1391_CHIPS_PER_CHAIN_S11_JIG, 84);
        assert_eq!(BM1391_CHIPS_PER_CHAIN_S15_SCAFFOLD, 84);
        assert_eq!(BM1391_CHIPS_PER_CHAIN_T15_SCAFFOLD, 63);
    }

    #[test]
    fn bm1391_profile_is_named_only_and_power_unknown() {
        let row = BM1391_TABLE.default_profile().unwrap();
        assert_eq!(BM1391_TABLE.live_status, crate::ChipStatus::NamedOnly);
        assert_eq!(row.wall_watts, None);
        assert_eq!(row.hashrate_ths, None);
        assert!(row.watts_per_ths().is_none());
    }
}
