//!  vnish-C — VNish firmware archive layout DTOs (HAL-free).
//!
//! Source RE evidence:
//!  §1.3
//! (Boot Sequence) — three boot modes with different recovery paths.
//!
//! VNish ships ONE firmware archive that supports two install modes
//! plus a CVitek-specific quirk:
//!
//! 1. **SD Card / USB** — non-persistent live boot. Original NAND
//!    firmware is UNTOUCHED. Removing the SD reverts to stock. Accepts
//!    USB sticks or SD cards up to 32 GB, FAT32-formatted.
//! 2. **NAND** — persistent flash via Hashcore Toolkit. Stock backup
//!    preserved in a separate partition for rollback.
//! 3. **CVitek special case** — locked bootloader always boots from a
//!    verified partition first; the VNish Toolkit must run on the
//!    network and re-inject the firmware after certain events.
//!
//! This module pins the boot-mode catalog, the canonical SD-card
//! layout, and the recovery strategy per mode so dcent-toolbox's
//! source-firmware install adapter can warn operators about the
//! correct recovery path.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Boot modes
// ---------------------------------------------------------------------------

/// VNish operating mode at boot time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishBootMode {
    /// Live boot from SD card or USB stick. NAND untouched. Operator
    /// reverts to stock by removing the media.
    SdCardLive,
    /// NAND-flashed via Hashcore Toolkit. Stock backup preserved in a
    /// separate partition.
    NandPersistent,
    /// CVitek board. Locked bootloader; persistent install requires
    /// the Toolkit to re-inject the firmware over the network.
    CVitekToolkit,
}

impl VnishBootMode {
    /// True iff removing the install media reverts the unit to stock
    /// (SD card mode only).
    pub fn is_removable(&self) -> bool {
        matches!(self, Self::SdCardLive)
    }

    /// True iff this mode writes to NAND flash on install.
    pub fn writes_nand(&self) -> bool {
        matches!(self, Self::NandPersistent | Self::CVitekToolkit)
    }

    /// True iff this mode requires the network-side Toolkit to be
    /// reachable AFTER install (CVitek's locked bootloader will revert
    /// otherwise).
    pub fn requires_toolkit_on_network(&self) -> bool {
        matches!(self, Self::CVitekToolkit)
    }

    /// Canonical recovery strategy for a unit installed in this mode.
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            Self::SdCardLive => RecoveryStrategy::RemoveSdCard,
            Self::NandPersistent => RecoveryStrategy::RestoreFromBackupPartition,
            Self::CVitekToolkit => RecoveryStrategy::RestoreViaToolkit,
        }
    }
}

/// Recovery path for a VNish-installed unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStrategy {
    /// Pull the SD card / USB stick. NAND is untouched.
    RemoveSdCard,
    /// Restore stock from the backup partition created at install time.
    RestoreFromBackupPartition,
    /// Use the network Toolkit to re-flash with stock firmware.
    RestoreViaToolkit,
}

// ---------------------------------------------------------------------------
// SD-card archive layout
// ---------------------------------------------------------------------------

/// Maximum supported SD/USB media size for the live-boot path.
pub const SD_CARD_MAX_SIZE_GB: u32 = 32;

/// Required filesystem on the SD/USB media.
pub const SD_CARD_FILESYSTEM: &str = "FAT32";

/// LED indicator pattern at the end of a successful live boot.
/// "Wait until green LED on front panel lights up and stays on."
pub const LED_READY_INDICATOR: &str = "green_solid";

/// Files contained in the SD-card / USB archive (all live at the root
/// of the FAT32 media).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdCardArchiveFile {
    /// First-stage bootloader (BL2 / SPL / FSBL — board-dependent).
    Boot1,
    /// U-Boot image.
    Uboot,
    /// Linux kernel + DTB (uImage or fitImage).
    KernelImage,
    /// Compressed root filesystem.
    Rootfs,
    /// VNish firmware-version manifest.
    Manifest,
}

impl SdCardArchiveFile {
    /// Canonical filename at the FAT32 root.
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Boot1 => "MLO",
            Self::Uboot => "u-boot.img",
            Self::KernelImage => "uImage",
            Self::Rootfs => "rootfs.tar.gz",
            Self::Manifest => "manifest.txt",
        }
    }
}

/// All files in the live-boot SD card archive in the canonical
/// boot-load order (first-stage → U-Boot → kernel → rootfs).
pub const SD_CARD_ARCHIVE_FILES: &[SdCardArchiveFile] = &[
    SdCardArchiveFile::Boot1,
    SdCardArchiveFile::Uboot,
    SdCardArchiveFile::KernelImage,
    SdCardArchiveFile::Rootfs,
    SdCardArchiveFile::Manifest,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_mode_recovery_strategy_pinned() {
        assert_eq!(
            VnishBootMode::SdCardLive.recovery_strategy(),
            RecoveryStrategy::RemoveSdCard
        );
        assert_eq!(
            VnishBootMode::NandPersistent.recovery_strategy(),
            RecoveryStrategy::RestoreFromBackupPartition
        );
        assert_eq!(
            VnishBootMode::CVitekToolkit.recovery_strategy(),
            RecoveryStrategy::RestoreViaToolkit
        );
    }

    #[test]
    fn only_sd_card_mode_is_removable() {
        // Per RE doc §1.3: SD/USB removable, persistent modes touch
        // flash. Pin so a refactor doesn't accidentally classify NAND
        // as "removable" (it's not — that would be a brick risk).
        assert!(VnishBootMode::SdCardLive.is_removable());
        assert!(!VnishBootMode::NandPersistent.is_removable());
        assert!(!VnishBootMode::CVitekToolkit.is_removable());
    }

    #[test]
    fn nand_modes_write_flash() {
        // NAND-persistent and CVitek-toolkit both write to flash;
        // SD-card mode does NOT.
        assert!(!VnishBootMode::SdCardLive.writes_nand());
        assert!(VnishBootMode::NandPersistent.writes_nand());
        assert!(VnishBootMode::CVitekToolkit.writes_nand());
    }

    #[test]
    fn only_cvitek_requires_network_toolkit() {
        // The locked bootloader will revert on next boot if the network
        // Toolkit isn't reachable for re-injection.
        assert!(!VnishBootMode::SdCardLive.requires_toolkit_on_network());
        assert!(!VnishBootMode::NandPersistent.requires_toolkit_on_network());
        assert!(VnishBootMode::CVitekToolkit.requires_toolkit_on_network());
    }

    #[test]
    fn sd_card_max_size_pinned_to_32_gb() {
        // RE doc: "USB flash drive or SD card up to 32GB, formatted FAT32".
        assert_eq!(SD_CARD_MAX_SIZE_GB, 32);
        assert_eq!(SD_CARD_FILESYSTEM, "FAT32");
    }

    #[test]
    fn led_ready_indicator_is_green_solid() {
        // RE doc: "wait until green LED on front panel lights up and
        // stays on" — operator-facing boot-success signal.
        assert_eq!(LED_READY_INDICATOR, "green_solid");
    }

    #[test]
    fn sd_card_archive_filenames_pinned() {
        // Filename layout matters — Hashcore Toolkit expects exact
        // names at the FAT32 root.
        assert_eq!(SdCardArchiveFile::Boot1.filename(), "MLO");
        assert_eq!(SdCardArchiveFile::Uboot.filename(), "u-boot.img");
        assert_eq!(SdCardArchiveFile::KernelImage.filename(), "uImage");
        assert_eq!(SdCardArchiveFile::Rootfs.filename(), "rootfs.tar.gz");
        assert_eq!(SdCardArchiveFile::Manifest.filename(), "manifest.txt");
    }

    #[test]
    fn archive_file_order_matches_boot_load_sequence() {
        // First-stage MLO loads U-Boot, U-Boot loads kernel, kernel
        // mounts rootfs. Manifest is metadata-only at the end.
        assert_eq!(SD_CARD_ARCHIVE_FILES.len(), 5);
        assert_eq!(SD_CARD_ARCHIVE_FILES[0], SdCardArchiveFile::Boot1);
        assert_eq!(SD_CARD_ARCHIVE_FILES[1], SdCardArchiveFile::Uboot);
        assert_eq!(SD_CARD_ARCHIVE_FILES[2], SdCardArchiveFile::KernelImage);
        assert_eq!(SD_CARD_ARCHIVE_FILES[3], SdCardArchiveFile::Rootfs);
        assert_eq!(SD_CARD_ARCHIVE_FILES[4], SdCardArchiveFile::Manifest);
    }

    #[test]
    fn boot_mode_round_trips_through_serde() {
        for mode in [
            VnishBootMode::SdCardLive,
            VnishBootMode::NandPersistent,
            VnishBootMode::CVitekToolkit,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: VnishBootMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn recovery_strategy_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&RecoveryStrategy::RemoveSdCard).unwrap(),
            "\"remove_sd_card\""
        );
        assert_eq!(
            serde_json::to_string(&RecoveryStrategy::RestoreFromBackupPartition).unwrap(),
            "\"restore_from_backup_partition\""
        );
        assert_eq!(
            serde_json::to_string(&RecoveryStrategy::RestoreViaToolkit).unwrap(),
            "\"restore_via_toolkit\""
        );
    }

    #[test]
    fn boot_mode_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&VnishBootMode::SdCardLive).unwrap(),
            "\"sd_card_live\""
        );
        assert_eq!(
            serde_json::to_string(&VnishBootMode::NandPersistent).unwrap(),
            "\"nand_persistent\""
        );
        assert_eq!(
            serde_json::to_string(&VnishBootMode::CVitekToolkit).unwrap(),
            "\"c_vitek_toolkit\""
        );
    }

    #[test]
    fn archive_file_round_trips_through_serde() {
        for file in SD_CARD_ARCHIVE_FILES.iter().copied() {
            let json = serde_json::to_string(&file).unwrap();
            let back: SdCardArchiveFile = serde_json::from_str(&json).unwrap();
            assert_eq!(file, back);
        }
    }
}
