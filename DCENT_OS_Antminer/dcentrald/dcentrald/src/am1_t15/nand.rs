//! Antminer T15 NAND write offsets (DATA-ONLY).
//!
//! Byte-exact NAND flash write offsets for the T15 firmware package layout,
//! pinned as constants for a future T15 bring-up. These are **not** wired into
//! any flash path — there are no callers and no flash operations in this module.
//!
//! # Source
//!
//! Knowledge-goldmine finding **S20 / F03** (HIGH confidence), extracted from the
//! DCENT_OS RE Dev Kit porting work order
//! `WORKSPACES/firmware_work/porting/build/firmware_packager.sh` lines 157-178
//! (delivered in `DCENT_OS_DEVELOPMENT_KITRE2` / `RE3`, byte-identical kits).
//!
//! (table row F03 + implementation candidate IC-3).
//!
//! The same packager (F02) describes the T15 NAND partition map as:
//! mtd0 = 32 MB (BOOT.bin + env + DTB + uImage), mtd1/mtd2 = 144 MB UBI rootfs,
//! mtd3 = 80 MB upgrade staging. The three write offsets below all live inside
//! the mtd0 boot region.

// These constants are intentionally not yet wired into any flash path — the T15
// is not in the live fleet, so there is no caller. `allow(dead_code)` keeps the
// build clean until a future T15 bring-up consumes them; the regression tests
// below still exercise every value so the offsets cannot silently drift.
#![allow(dead_code)]

/// DTB (device-tree blob) NAND write offset for the T15 firmware package.
///
/// Source: goldmine S20/F03, `firmware_packager.sh` lines 157-178.
pub const DTB_NAND_OFFSET: u32 = 0x0102_0000;

/// Upgrade-marker NAND write offset for the T15 firmware package.
///
/// The upgrade-marker itself is 16 bytes, bytes 0-3 = `\x00\x00\x00\x01`
/// (goldmine S20/F14). This constant is only the write OFFSET, not the marker
/// payload.
///
/// Source: goldmine S20/F03, `firmware_packager.sh` lines 157-178.
pub const UPGRADE_MARKER_NAND_OFFSET: u32 = 0x0104_0000;

/// Kernel (uImage) NAND write offset for the T15 firmware package.
///
/// Source: goldmine S20/F03, `firmware_packager.sh` lines 157-178.
pub const KERNEL_NAND_OFFSET: u32 = 0x0110_0000;

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the byte-exact offsets from goldmine S20/F03 so a future edit can't
    /// silently drift them away from the RE Dev Kit `firmware_packager.sh` values.
    #[test]
    fn t15_nand_offsets_match_devkit_re() {
        assert_eq!(DTB_NAND_OFFSET, 0x0102_0000);
        assert_eq!(UPGRADE_MARKER_NAND_OFFSET, 0x0104_0000);
        assert_eq!(KERNEL_NAND_OFFSET, 0x0110_0000);
    }

    /// The packager writes DTB, then the upgrade-marker, then the kernel — the
    /// offsets must be strictly increasing in that order.
    #[test]
    fn t15_nand_offsets_are_strictly_increasing() {
        assert!(DTB_NAND_OFFSET < UPGRADE_MARKER_NAND_OFFSET);
        assert!(UPGRADE_MARKER_NAND_OFFSET < KERNEL_NAND_OFFSET);
    }
}
