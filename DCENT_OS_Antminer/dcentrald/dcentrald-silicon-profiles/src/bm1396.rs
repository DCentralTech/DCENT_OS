//! BM1396 data-only silicon profile for S17+ / T17+.
//!
//! BM1396 is intentionally not registered as a runtime chip driver. The rows
//! below are vendor-extracted operating-point anchors for UI/autotuner catalog
//! display and host-side planning only. A live `0x1396` chip enumeration must
//! still fail closed in `dcentrald-asic` until BP-ZYNQ-NAND-BRINGUP captures
//! the chip ID and validates dispatch on owned bench hardware.

use crate::{Profile, ProfileSource, SiliconTable};

/// BM1396 numeric chip ID observed in the family catalog.
pub const BM1396_CHIP_ID: u32 = 0x0000_1396;

/// S17+ chips per chain from the support-matrix scaffold row.
pub const BM1396_CHIPS_PER_CHAIN_S17_PLUS: u32 = 65;

/// T17+ chips per chain from the support-matrix scaffold row.
pub const BM1396_CHIPS_PER_CHAIN_T17_PLUS: u32 = 44;

/// BM1396 plus-family chain count.
pub const BM1396_CHAIN_COUNT: u32 = 3;

/// Sparse BM1396 vendor anchors. `voltage_v` is chip-core voltage because the
/// retained VNish plus-family rows expose a chip-core target, not a chain-rail
/// APW target. Do not use these rows as hardware envelopes.
pub const BM1396_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 400,
        voltage_v: 1.68,
        wall_watts: Some(1700),
        hashrate_ths: Some(52.0),
        source: ProfileSource::VendorExtracted,
    },
    Profile {
        step: -1,
        freq_mhz: 500,
        voltage_v: 1.68,
        wall_watts: Some(2370),
        hashrate_ths: Some(65.0),
        source: ProfileSource::VendorExtracted,
    },
    Profile {
        step: 0,
        freq_mhz: 600,
        voltage_v: 1.68,
        wall_watts: Some(3100),
        hashrate_ths: Some(78.0),
        source: ProfileSource::VendorExtracted,
    },
    Profile {
        step: 1,
        freq_mhz: 700,
        voltage_v: 1.68,
        wall_watts: Some(3900),
        hashrate_ths: Some(91.0),
        source: ProfileSource::VendorExtracted,
    },
    Profile {
        step: 2,
        freq_mhz: 730,
        voltage_v: 1.68,
        wall_watts: Some(4150),
        hashrate_ths: Some(95.0),
        source: ProfileSource::VendorExtracted,
    },
];

pub const BM1396_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1396",
    profiles: &BM1396_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    live_status: crate::ChipStatus::NamedOnly,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1396_identity_and_geometry_are_plus_family_only() {
        assert_eq!(BM1396_CHIP_ID, 0x1396);
        assert_eq!(BM1396_CHAIN_COUNT, 3);
        assert_eq!(BM1396_CHIPS_PER_CHAIN_S17_PLUS, 65);
        assert_eq!(BM1396_CHIPS_PER_CHAIN_T17_PLUS, 44);
    }

    #[test]
    fn bm1396_profile_is_data_only_not_live_ready() {
        assert_eq!(BM1396_TABLE.chip_family, "BM1396");
        assert_eq!(BM1396_TABLE.live_status, crate::ChipStatus::NamedOnly);
        assert_eq!(BM1396_TABLE.profiles.len(), 5);
        assert_eq!(BM1396_TABLE.default_profile().unwrap().freq_mhz, 600);
        assert!(BM1396_TABLE.computed_sweet_spot().is_some());
    }
}
