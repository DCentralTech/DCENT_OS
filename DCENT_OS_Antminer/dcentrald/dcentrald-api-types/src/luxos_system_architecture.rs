//!  luxos-L — LuxOS system architecture catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §1 (Boot Chain — MTD partition layout, lines 63-92) + §2 (Init
//! System — inittab, boot sequence, init-script catalog, lines
//! 96-205).
//!
//! Pins the on-flash layout + boot sequence for an S19j Pro BB
//! running LuxOS 1.38.1 (live `a lab unit` capture). Cross-references:
//! -  luxos-F `LUXUPDATE_RAMDISK_SIZE_MB=128`.
//! -  luxos-H `UNINSTALL_SH_STEPS` + `preserved_partitions()`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MTD partition catalog (12 entries)
// ---------------------------------------------------------------------------

/// One of 12 MTD partitions on a LuxOS-on-Bitmain-BB unit per §1
/// MTD Partition Layout table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosMtdPartition {
    /// mtd0 — Primary SPL (MLO).
    Spl0,
    /// mtd1 — SPL backup #1 (BootROM tries 4 slots).
    Spl1,
    /// mtd2 — SPL backup #2.
    Spl2,
    /// mtd3 — SPL backup #3.
    Spl3,
    /// mtd4 — U-Boot.
    UBoot,
    /// mtd5 — U-Boot environment. Wiped by `/uninstall.sh`.
    Bootenv,
    /// mtd6 — Kernel device tree blob (FDT).
    Fdt,
    /// mtd7 — uImage (Linux kernel).
    Kernel,
    /// mtd8 — Factory rootfs (kept for revert-to-stock recovery).
    FactoryRoot,
    /// mtd9 — `/config` jffs2 — luxminer.toml + drop-ins + auth.
    Config,
    /// mtd10 — Signature partition (Bitmain BB factory leftover —
    /// NOT used by LuxOS per  luxos-F §3b finding).
    Sig,
    /// mtd11 — Active rootfs + `/persistent/` logs. Wiped by
    /// uninstall.sh.
    Nvdata,
}

impl LuxosMtdPartition {
    /// Numeric mtd index (0..=11).
    pub fn mtd_index(&self) -> u8 {
        match self {
            Self::Spl0 => 0,
            Self::Spl1 => 1,
            Self::Spl2 => 2,
            Self::Spl3 => 3,
            Self::UBoot => 4,
            Self::Bootenv => 5,
            Self::Fdt => 6,
            Self::Kernel => 7,
            Self::FactoryRoot => 8,
            Self::Config => 9,
            Self::Sig => 10,
            Self::Nvdata => 11,
        }
    }

    /// Partition size in bytes per §1 table.
    pub fn partition_size_bytes(&self) -> u64 {
        match self {
            Self::Spl0 | Self::Spl1 | Self::Spl2 | Self::Spl3 => 128 * 1024,
            Self::UBoot => 1_700 * 1024, // 1.7 MB
            Self::Bootenv => 128 * 1024,
            Self::Fdt => 128 * 1024,
            Self::Kernel => 5 * 1024 * 1024,
            Self::FactoryRoot => 20 * 1024 * 1024,
            Self::Config => 2 * 1024 * 1024,
            Self::Sig => 2 * 1024 * 1024,
            Self::Nvdata => 96 * 1024 * 1024,
        }
    }

    /// Partition `name=` field as it appears in `/proc/mtd`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Spl0 => "spl",
            Self::Spl1 => "spl.bak1",
            Self::Spl2 => "spl.bak2",
            Self::Spl3 => "spl.bak3",
            Self::UBoot => "u-boot",
            Self::Bootenv => "bootenv",
            Self::Fdt => "fdt",
            Self::Kernel => "kernel",
            Self::FactoryRoot => "root",
            Self::Config => "config",
            Self::Sig => "sig",
            Self::Nvdata => "nvdata",
        }
    }

    /// Architectural role.
    pub fn role(&self) -> LuxosMtdRole {
        match self {
            Self::Spl0 | Self::Spl1 | Self::Spl2 | Self::Spl3 => LuxosMtdRole::SplBackup,
            Self::UBoot => LuxosMtdRole::Bootloader,
            Self::Bootenv => LuxosMtdRole::BootEnv,
            Self::Fdt => LuxosMtdRole::DeviceTree,
            Self::Kernel => LuxosMtdRole::Kernel,
            Self::FactoryRoot => LuxosMtdRole::FactoryRootfs,
            Self::Config => LuxosMtdRole::Config,
            Self::Sig => LuxosMtdRole::Signature,
            Self::Nvdata => LuxosMtdRole::ActiveRootfs,
        }
    }
}

/// Architectural role of an mtd partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosMtdRole {
    SplBackup,
    Bootloader,
    BootEnv,
    DeviceTree,
    Kernel,
    FactoryRootfs,
    Config,
    Signature,
    ActiveRootfs,
}

/// All 12 MTD partitions in canonical order.
pub const ALL_LUXOS_MTD_PARTITIONS: &[LuxosMtdPartition] = &[
    LuxosMtdPartition::Spl0,
    LuxosMtdPartition::Spl1,
    LuxosMtdPartition::Spl2,
    LuxosMtdPartition::Spl3,
    LuxosMtdPartition::UBoot,
    LuxosMtdPartition::Bootenv,
    LuxosMtdPartition::Fdt,
    LuxosMtdPartition::Kernel,
    LuxosMtdPartition::FactoryRoot,
    LuxosMtdPartition::Config,
    LuxosMtdPartition::Sig,
    LuxosMtdPartition::Nvdata,
];

// ---------------------------------------------------------------------------
// LuxOS-specific init scripts
// ---------------------------------------------------------------------------

/// LuxOS-introduced init scripts per §2 "LuxOS-Specific Init Scripts"
/// (the only 4 scripts that aren't stock Yocto / Bitmain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosInitScript {
    /// `S00mount-config-partition.sh` — early-boot mount of mtd9
    /// `/config` jffs2.
    S00MountConfigPartition,
    /// `S20httpd-init` — busybox httpd recovery server on port 80
    /// ( luxos-H `LUXOS_HTTPD_PORT`).
    S20HttpdInit,
    /// `S89ramdisk.sh` — tmpfs `/mnt/ramdisk` 128 MB ( luxos-F
    /// `LUXUPDATE_RAMDISK_SIZE_MB`).
    S89Ramdisk,
    /// `S90luxminer-init` — start luxupdate watcher (which forks
    /// luxminer, see  luxos-F §1b).
    S90LuxminerInit,
}

impl LuxosInitScript {
    /// Filename in `/etc/init.d/`.
    pub fn filename(&self) -> &'static str {
        match self {
            Self::S00MountConfigPartition => "S00mount-config-partition.sh",
            Self::S20HttpdInit => "S20httpd-init",
            Self::S89Ramdisk => "S89ramdisk.sh",
            Self::S90LuxminerInit => "S90luxminer-init",
        }
    }
}

/// All 4 LuxOS-specific init scripts per §2 catalog.
pub const LUXOS_SPECIFIC_INIT_SCRIPTS: &[LuxosInitScript] = &[
    LuxosInitScript::S00MountConfigPartition,
    LuxosInitScript::S20HttpdInit,
    LuxosInitScript::S89Ramdisk,
    LuxosInitScript::S90LuxminerInit,
];

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Live `a lab unit` kernel uname tag per §1 Kernel Tag.
pub const LUXOS_KERNEL_VERSION_TAG: &str = "5.4.242-v2023.11.1-u0-58-g9f143660-dirty-bone66";

/// Kernel platform identifier — `bone66` is the Bitmain BB control
/// board family (BeagleBone-AI / AM335x-class).
pub const LUXOS_KERNEL_PLATFORM: &str = "bone66";

/// Linux kernel major.minor.patch baseline.
pub const LUXOS_KERNEL_VERSION: &str = "5.4.242";

/// `/mnt/ramdisk` tmpfs size in MB. Cross-references  luxos-F
/// `LUXUPDATE_RAMDISK_SIZE_MB`.
pub const LUXOS_RAMDISK_TMPFS_SIZE_MB: u32 = 128;

/// Default hostname per §2 boot sequence.
pub const LUXOS_DEFAULT_HOSTNAME: &str = "Antminer";

/// Inittab default runlevel per §2.
pub const LUXOS_INITTAB_DEFAULT_RUNLEVEL: u8 = 5;

/// Serial console getty baud per §2.
pub const LUXOS_SERIAL_CONSOLE_BAUD: u32 = 115_200;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtd_table_has_12_partitions() {
        // §1 table: mtd0..=mtd11.
        assert_eq!(ALL_LUXOS_MTD_PARTITIONS.len(), 12);
    }

    #[test]
    fn mtd_indices_strictly_increasing() {
        for window in ALL_LUXOS_MTD_PARTITIONS.windows(2) {
            assert!(window[0].mtd_index() < window[1].mtd_index());
        }
        assert_eq!(ALL_LUXOS_MTD_PARTITIONS[0].mtd_index(), 0);
        assert_eq!(ALL_LUXOS_MTD_PARTITIONS.last().unwrap().mtd_index(), 11);
    }

    #[test]
    fn mtd_sizes_match_re_doc_table() {
        // §1 verbatim sizes.
        assert_eq!(LuxosMtdPartition::Spl0.partition_size_bytes(), 128 * 1024);
        assert_eq!(
            LuxosMtdPartition::UBoot.partition_size_bytes(),
            1_700 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::Bootenv.partition_size_bytes(),
            128 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::Kernel.partition_size_bytes(),
            5 * 1024 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::FactoryRoot.partition_size_bytes(),
            20 * 1024 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::Config.partition_size_bytes(),
            2 * 1024 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::Sig.partition_size_bytes(),
            2 * 1024 * 1024
        );
        assert_eq!(
            LuxosMtdPartition::Nvdata.partition_size_bytes(),
            96 * 1024 * 1024
        );
    }

    #[test]
    fn factory_rootfs_at_mtd8_is_revert_to_stock_target() {
        // §7e cross-reference: uninstall.sh DOES NOT touch mtd8 — it's
        // the factory rootfs that comes back online when LuxOS is
        // wiped from mtd11.
        let p = LuxosMtdPartition::FactoryRoot;
        assert_eq!(p.mtd_index(), 8);
        assert_eq!(p.role(), LuxosMtdRole::FactoryRootfs);
        assert_eq!(p.name(), "root");
    }

    #[test]
    fn sig_partition_at_mtd10_is_unused_per_wave_25_finding() {
        //  luxos-F §3b cross-reference: mtd10 "sig" is Bitmain
        // BB factory leftover, NOT used by LuxOS. Pin the role +
        // index here so the cross-reference holds.
        let p = LuxosMtdPartition::Sig;
        assert_eq!(p.mtd_index(), 10);
        assert_eq!(p.role(), LuxosMtdRole::Signature);
        assert_eq!(p.name(), "sig");
    }

    #[test]
    fn bootenv_at_mtd5_is_uninstall_step_1() {
        //  luxos-H cross-reference: uninstall.sh step 1 is
        // `flash_erase /dev/mtd5` (load-bearing).
        let p = LuxosMtdPartition::Bootenv;
        assert_eq!(p.mtd_index(), 5);
        assert_eq!(p.role(), LuxosMtdRole::BootEnv);
    }

    #[test]
    fn nvdata_at_mtd11_is_uninstall_step_6() {
        //  luxos-H cross-reference: uninstall.sh step 6 is
        // `flash_erase /dev/mtd11` (destructive — wipes LuxOS rootfs).
        let p = LuxosMtdPartition::Nvdata;
        assert_eq!(p.mtd_index(), 11);
        assert_eq!(p.role(), LuxosMtdRole::ActiveRootfs);
        assert_eq!(p.name(), "nvdata");
    }

    #[test]
    fn four_spl_backups_have_same_size() {
        // §1 BootROM tries 4 SPL slots — all 128 KB each.
        for p in [
            LuxosMtdPartition::Spl0,
            LuxosMtdPartition::Spl1,
            LuxosMtdPartition::Spl2,
            LuxosMtdPartition::Spl3,
        ] {
            assert_eq!(p.partition_size_bytes(), 128 * 1024);
            assert_eq!(p.role(), LuxosMtdRole::SplBackup);
        }
    }

    #[test]
    fn names_match_proc_mtd_literals() {
        // `/proc/mtd` shows `spl`, `spl.bak1`, etc. — pin the literals.
        assert_eq!(LuxosMtdPartition::Spl0.name(), "spl");
        assert_eq!(LuxosMtdPartition::Spl1.name(), "spl.bak1");
        assert_eq!(LuxosMtdPartition::Spl2.name(), "spl.bak2");
        assert_eq!(LuxosMtdPartition::Spl3.name(), "spl.bak3");
        assert_eq!(LuxosMtdPartition::UBoot.name(), "u-boot");
        assert_eq!(LuxosMtdPartition::Bootenv.name(), "bootenv");
        assert_eq!(LuxosMtdPartition::Fdt.name(), "fdt");
    }

    #[test]
    fn luxos_specific_scripts_count_pinned_to_4() {
        // §2 "LuxOS-Specific Init Scripts (Summary)" — exactly 4
        // scripts that aren't stock Yocto / Bitmain.
        assert_eq!(LUXOS_SPECIFIC_INIT_SCRIPTS.len(), 4);
    }

    #[test]
    fn luxos_specific_scripts_filenames_match_re_doc() {
        assert_eq!(
            LuxosInitScript::S00MountConfigPartition.filename(),
            "S00mount-config-partition.sh"
        );
        assert_eq!(LuxosInitScript::S20HttpdInit.filename(), "S20httpd-init");
        assert_eq!(LuxosInitScript::S89Ramdisk.filename(), "S89ramdisk.sh");
        assert_eq!(
            LuxosInitScript::S90LuxminerInit.filename(),
            "S90luxminer-init"
        );
    }

    #[test]
    fn ramdisk_size_matches_wave25_constant() {
        //  luxos-F LUXUPDATE_RAMDISK_SIZE_MB=128. Pin the
        // cross-reference invariant — both modules must agree.
        assert_eq!(LUXOS_RAMDISK_TMPFS_SIZE_MB, 128);
    }

    #[test]
    fn kernel_constants_match_re_doc() {
        // §1 Kernel Tag: "5.4.242-v2023.11.1-u0-58-g9f143660-dirty-bone66".
        assert_eq!(
            LUXOS_KERNEL_VERSION_TAG,
            "5.4.242-v2023.11.1-u0-58-g9f143660-dirty-bone66"
        );
        assert_eq!(LUXOS_KERNEL_VERSION, "5.4.242");
        assert_eq!(LUXOS_KERNEL_PLATFORM, "bone66");
        // Sanity: full tag contains both major-version and platform.
        assert!(LUXOS_KERNEL_VERSION_TAG.contains(LUXOS_KERNEL_VERSION));
        assert!(LUXOS_KERNEL_VERSION_TAG.contains(LUXOS_KERNEL_PLATFORM));
    }

    #[test]
    fn inittab_constants_match_re_doc() {
        // §2 inittab: default runlevel 5, serial console 115200.
        assert_eq!(LUXOS_INITTAB_DEFAULT_RUNLEVEL, 5);
        assert_eq!(LUXOS_SERIAL_CONSOLE_BAUD, 115_200);
    }

    #[test]
    fn default_hostname_is_antminer() {
        // §2 boot sequence: hostname=Antminer.
        assert_eq!(LUXOS_DEFAULT_HOSTNAME, "Antminer");
    }

    #[test]
    fn partitions_round_trip_through_serde() {
        for p in ALL_LUXOS_MTD_PARTITIONS.iter().copied() {
            let json = serde_json::to_string(&p).unwrap();
            let back: LuxosMtdPartition = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn role_round_trip_through_serde() {
        for r in [
            LuxosMtdRole::SplBackup,
            LuxosMtdRole::Bootloader,
            LuxosMtdRole::BootEnv,
            LuxosMtdRole::DeviceTree,
            LuxosMtdRole::Kernel,
            LuxosMtdRole::FactoryRootfs,
            LuxosMtdRole::Config,
            LuxosMtdRole::Signature,
            LuxosMtdRole::ActiveRootfs,
        ] {
            let json = serde_json::to_string(&r).unwrap();
            let back: LuxosMtdRole = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn init_script_round_trip_through_serde() {
        for s in LUXOS_SPECIFIC_INIT_SCRIPTS.iter().copied() {
            let json = serde_json::to_string(&s).unwrap();
            let back: LuxosInitScript = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }
}
