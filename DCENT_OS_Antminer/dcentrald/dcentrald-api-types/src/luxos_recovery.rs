//!  luxos-H — LuxOS recovery + uninstall flow DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §7 (Recovery mode — the user-facing escape hatch, lines 248-321).
//!
//! The LuxOS recovery server is **busybox httpd on port 80** (NOT the
//! luxupdate HTTP debug server on port 9012 —  luxos-F covered
//! that one). Doc root is `/firmware/`; CGI dir is `/firmware/cgi-bin/`.
//! Recovery is a separate static page at `/firmware/recovery.html`,
//! NOT the React SPA shell.
//!
//! Hard rules pinned by tests:
//! - **`LUXOS_RECOVERY_REQUIRES_AUTH = false`** — per §7b: no
//!   authentication on `/recovery.html` or its CGIs. Anyone on the LAN
//!   can call `/cgi-bin/uninstall.cgi` and trigger flash erase. This
//!   is a confirmed CSRF + open-network attack surface.
//! - **`UNINSTALL_SH_STEPS`** — exactly 6 steps in the order documented
//!   in §7d/§7e. Step 1 (`FlashEraseMtd5`) is load-bearing — if it
//!   fails the whole script aborts and LuxOS keeps running. Step 6
//!   (`FlashEraseMtd11`) is destructive (wipes the LuxOS rootfs).
//! - **`mtd10` is in the preserved list** — confirms the §3b finding
//!   that `mtd10="sig"` is unused stock leftover (Bitmain BB factory
//!   signature partition, untouched by LuxOS).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Recovery actions (operator-visible buttons on recovery.html)
// ---------------------------------------------------------------------------

/// Operator-visible recovery action per §7b "Recovery flow (read
/// directly from `recovery.html`)" — 4 actions plus the BrowseLogs
/// listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosRecoveryAction {
    /// `Restart miner` button → `GET /cgi-bin/reboot.cgi` →
    /// `shutdown -r now`.
    Reboot,
    /// `Uninstall LuxOS` button → `GET /cgi-bin/uninstall.cgi` →
    /// `/uninstall.sh 2>&1`. Destructive.
    Uninstall,
    /// Download `cgminer.conf` via
    /// `/cgi-bin/download_file.cgi?file=config/cgminer.conf`.
    DownloadCgminerConf,
    /// Download `luxminer.toml` via download_file.cgi.
    DownloadLuxminerToml,
    /// Browse logs via `/cgi-bin/get_logs.cgi` then per-file download.
    BrowseLogs,
}

impl LuxosRecoveryAction {
    /// True iff this action is destructive (flash erase / brick risk).
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Uninstall)
    }
}

// ---------------------------------------------------------------------------
// CGI endpoints
// ---------------------------------------------------------------------------

/// CGI dispatch endpoint per §7a `/firmware/cgi-bin/*.cgi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosRecoveryCgi {
    /// `/cgi-bin/reboot.cgi` — 4-line shell that runs `shutdown -r now`.
    Reboot,
    /// `/cgi-bin/uninstall.cgi` — 7-line shell that runs `/uninstall.sh`.
    Uninstall,
    /// `/cgi-bin/get_logs.cgi` — JSON list of files in 5 log groups.
    GetLogs,
    /// `/cgi-bin/download_file.cgi` — group-and-relative-path file
    /// download.
    DownloadFile,
}

impl LuxosRecoveryCgi {
    /// Path under `/firmware/`.
    pub fn path(&self) -> &'static str {
        match self {
            Self::Reboot => "/cgi-bin/reboot.cgi",
            Self::Uninstall => "/cgi-bin/uninstall.cgi",
            Self::GetLogs => "/cgi-bin/get_logs.cgi",
            Self::DownloadFile => "/cgi-bin/download_file.cgi",
        }
    }

    /// True iff the CGI triggers a destructive operation (flash erase).
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Uninstall)
    }
}

// ---------------------------------------------------------------------------
// Log group whitelist (download_file.cgi)
// ---------------------------------------------------------------------------

/// Group whitelist for `download_file.cgi` per §7c
/// `map_group_to_path()`. Five hard-coded values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosLogGroup {
    /// `ramdisklogs` → `/mnt/ramdisk/logs`.
    RamdiskLogs,
    /// `sdcardlogs` → `/mnt/sdcard/logs` (primary).
    SdcardLogs,
    /// `sdcardlogs2` → secondary SD-card log path.
    SdcardLogs2,
    /// `nandlogs` → `/persistent` NAND-backed logs.
    NandLogs,
    /// `config` → `/config/*`.
    Config,
}

impl LuxosLogGroup {
    /// Wire-form group token (matches the regex `^[a-zA-Z0-9_-]+$` per
    /// §7c safety analysis).
    pub fn wire_token(&self) -> &'static str {
        match self {
            Self::RamdiskLogs => "ramdisklogs",
            Self::SdcardLogs => "sdcardlogs",
            Self::SdcardLogs2 => "sdcardlogs2",
            Self::NandLogs => "nandlogs",
            Self::Config => "config",
        }
    }

    /// All 5 documented groups.
    pub const ALL: [Self; 5] = [
        Self::RamdiskLogs,
        Self::SdcardLogs,
        Self::SdcardLogs2,
        Self::NandLogs,
        Self::Config,
    ];
}

// ---------------------------------------------------------------------------
// uninstall.sh sequence
// ---------------------------------------------------------------------------

/// One step of the uninstall.sh script per §7d (verbatim shell) and
/// §7e (line-by-line analysis). Order is canonical and load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosUninstallStep {
    /// 1. `flash_erase /dev/mtd5 0 0` — wipe bootenv. LOAD-BEARING:
    ///    if this fails the whole script aborts and LuxOS keeps
    ///    running.
    FlashEraseMtd5,
    /// 2. `trap 'echo b > /proc/sysrq-trigger' EXIT` — kernel-level
    ///    panic-reboot if anything below fails.
    SetSysrqTrap,
    /// 3. `rm -f /config/luxminer.toml ... /config/partner_info.toml`
    ///    — wipe top-level config files.
    RmConfigToplevel,
    /// 4. `rm -rf /config/backup/ /config/profiles/
    ///    /config/luxminer.conf.d/ /backup/` — wipe drop-in configs.
    ///    DANGEROUS: no diff/preserve.
    RmConfigSubdirs,
    /// 5. `sync` — flush dirty pages.
    Sync,
    /// 6. `flash_erase /dev/mtd11 0 0` — WIPE THE ENTIRE LUXOS ROOTFS.
    ///    Destructive, irreversible without re-flashing.
    FlashEraseMtd11,
}

/// Canonical 6-step uninstall sequence per §7d shell verbatim.
pub const UNINSTALL_SH_STEPS: &[LuxosUninstallStep] = &[
    LuxosUninstallStep::FlashEraseMtd5,
    LuxosUninstallStep::SetSysrqTrap,
    LuxosUninstallStep::RmConfigToplevel,
    LuxosUninstallStep::RmConfigSubdirs,
    LuxosUninstallStep::Sync,
    LuxosUninstallStep::FlashEraseMtd11,
];

/// `mtd*` partitions NOT touched by uninstall.sh per §7e bullet
/// "What's preserved". Ordered to match the RE doc list.
pub fn preserved_partitions() -> &'static [&'static str] {
    &[
        "mtd0",  // SPL backup 0
        "mtd1",  // SPL backup 1
        "mtd2",  // SPL backup 2
        "mtd3",  // SPL backup 3
        "mtd4",  // u-boot
        "mtd6",  // fdt
        "mtd7",  // kernel
        "mtd8",  // factory rootfs
        "mtd9",  // config (partition-level — files inside are rm'd)
        "mtd10", // sig — UNTOUCHED (Bitmain BB factory leftover, NOT used by LuxOS)
    ]
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Busybox httpd document root per §7a.
pub const LUXOS_RECOVERY_DOC_ROOT: &str = "/firmware";

/// Recovery server port. Distinct from the luxupdate HTTP debug
/// server on port 9012 ( `luxos_update::LUXOS_HTTP_DEBUG_PORT`).
pub const LUXOS_HTTPD_PORT: u16 = 80;

/// Load-bearing finding from §7b: `/recovery.html` and its CGIs have
/// NO authentication. Confirmed CSRF + open-network attack surface.
pub const LUXOS_RECOVERY_REQUIRES_AUTH: bool = false;

/// Recovery static page filename per §7a.
pub const LUXOS_RECOVERY_HTML_PATH: &str = "/firmware/recovery.html";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_actions_destructive_classification() {
        // Only Uninstall is destructive. Reboot / DownloadCgminerConf
        // / DownloadLuxminerToml / BrowseLogs are NOT.
        assert!(LuxosRecoveryAction::Uninstall.is_destructive());
        assert!(!LuxosRecoveryAction::Reboot.is_destructive());
        assert!(!LuxosRecoveryAction::DownloadCgminerConf.is_destructive());
        assert!(!LuxosRecoveryAction::DownloadLuxminerToml.is_destructive());
        assert!(!LuxosRecoveryAction::BrowseLogs.is_destructive());
    }

    #[test]
    fn cgi_paths_match_re_doc_literally() {
        // Pin every CGI path verbatim — pyasic + dashboard call by
        // exact path.
        assert_eq!(LuxosRecoveryCgi::Reboot.path(), "/cgi-bin/reboot.cgi");
        assert_eq!(LuxosRecoveryCgi::Uninstall.path(), "/cgi-bin/uninstall.cgi");
        assert_eq!(LuxosRecoveryCgi::GetLogs.path(), "/cgi-bin/get_logs.cgi");
        assert_eq!(
            LuxosRecoveryCgi::DownloadFile.path(),
            "/cgi-bin/download_file.cgi"
        );
    }

    #[test]
    fn cgi_destructive_only_for_uninstall() {
        for cgi in [
            LuxosRecoveryCgi::Reboot,
            LuxosRecoveryCgi::GetLogs,
            LuxosRecoveryCgi::DownloadFile,
        ] {
            assert!(!cgi.is_destructive(), "{:?} not destructive", cgi);
        }
        assert!(LuxosRecoveryCgi::Uninstall.is_destructive());
    }

    #[test]
    fn log_group_whitelist_has_exactly_5_entries() {
        // §7c `map_group_to_path()` hard-coded list.
        assert_eq!(LuxosLogGroup::ALL.len(), 5);
    }

    #[test]
    fn log_group_wire_tokens_match_re_doc() {
        for (group, expected) in [
            (LuxosLogGroup::RamdiskLogs, "ramdisklogs"),
            (LuxosLogGroup::SdcardLogs, "sdcardlogs"),
            (LuxosLogGroup::SdcardLogs2, "sdcardlogs2"),
            (LuxosLogGroup::NandLogs, "nandlogs"),
            (LuxosLogGroup::Config, "config"),
        ] {
            assert_eq!(group.wire_token(), expected);
        }
    }

    #[test]
    fn log_group_wire_tokens_pass_safety_regex() {
        // §7c regex: `^[a-zA-Z0-9_-]+$` (safe). Verify every documented
        // group token matches.
        for group in LuxosLogGroup::ALL {
            let token = group.wire_token();
            for ch in token.chars() {
                assert!(
                    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-',
                    "group token '{}' contains unsafe char '{}'",
                    token,
                    ch
                );
            }
            assert!(!token.is_empty());
        }
    }

    #[test]
    fn uninstall_sequence_has_exactly_6_steps() {
        // §7d shell has 6 explicit steps.
        assert_eq!(UNINSTALL_SH_STEPS.len(), 6);
    }

    #[test]
    fn uninstall_step_1_is_flash_erase_mtd5_load_bearing() {
        // §7e: "Line 1: flash_erase /dev/mtd5 0 0 — wipe the bootenv
        // partition. ... Safety: this is the LOAD-BEARING line for
        // revert-to-stock. If this fails (e.g., write protection),
        // the script aborts."
        assert_eq!(UNINSTALL_SH_STEPS[0], LuxosUninstallStep::FlashEraseMtd5);
    }

    #[test]
    fn uninstall_step_2_is_sysrq_trap() {
        assert_eq!(UNINSTALL_SH_STEPS[1], LuxosUninstallStep::SetSysrqTrap);
    }

    #[test]
    fn uninstall_step_6_is_flash_erase_mtd11_destructive() {
        // §7e: "Line 6: flash_erase /dev/mtd11 0 0 — WIPE THE ENTIRE
        // LUXOS ROOTFS."
        assert_eq!(UNINSTALL_SH_STEPS[5], LuxosUninstallStep::FlashEraseMtd11);
    }

    #[test]
    fn config_wipes_run_between_traps_and_destructive_erase() {
        // RmConfigToplevel + RmConfigSubdirs MUST run AFTER the trap
        // is set (so a panic-reboot fires if they crash) and BEFORE
        // FlashEraseMtd11 (which would orphan a config-wipe failure).
        let pos = |s: LuxosUninstallStep| {
            UNINSTALL_SH_STEPS
                .iter()
                .position(|x| *x == s)
                .expect("step missing")
        };
        assert!(pos(LuxosUninstallStep::SetSysrqTrap) < pos(LuxosUninstallStep::RmConfigToplevel));
        assert!(
            pos(LuxosUninstallStep::RmConfigToplevel) < pos(LuxosUninstallStep::RmConfigSubdirs)
        );
        assert!(pos(LuxosUninstallStep::RmConfigSubdirs) < pos(LuxosUninstallStep::Sync));
        assert!(pos(LuxosUninstallStep::Sync) < pos(LuxosUninstallStep::FlashEraseMtd11));
    }

    #[test]
    fn preserved_partitions_match_re_doc_bullet() {
        // §7e "What's preserved" bullet point.
        let preserved = preserved_partitions();
        // Pin specific entries we documented.
        assert!(preserved.contains(&"mtd0"));
        assert!(preserved.contains(&"mtd4")); // u-boot
        assert!(preserved.contains(&"mtd7")); // kernel
        assert!(preserved.contains(&"mtd8")); // factory rootfs (the revert-to-stock target)
        assert!(preserved.contains(&"mtd9")); // config partition
        assert!(preserved.contains(&"mtd10")); // sig — Bitmain stock leftover
    }

    #[test]
    fn mtd5_and_mtd11_not_in_preserved_list() {
        // Both are destructively wiped; pin them as ABSENT from the
        // preserved list.
        let preserved = preserved_partitions();
        assert!(!preserved.contains(&"mtd5"));
        assert!(!preserved.contains(&"mtd11"));
    }

    #[test]
    fn mtd10_sig_partition_pinned_as_unused() {
        // Cross-reference §3b: `mtd10="sig"` is NOT used by LuxOS.
        // If uninstall.sh wiped it, that would imply LuxOS depends on
        // it. The fact it's preserved confirms the unused finding.
        assert!(preserved_partitions().contains(&"mtd10"));
    }

    #[test]
    fn recovery_doc_root_pinned() {
        // §7a: HTTPROOT="/firmware".
        assert_eq!(LUXOS_RECOVERY_DOC_ROOT, "/firmware");
    }

    #[test]
    fn recovery_httpd_port_is_80_not_9012() {
        // Recovery is on busybox httpd port 80, NOT the luxupdate HTTP
        // debug server on 9012 ( luxos-F).
        assert_eq!(LUXOS_HTTPD_PORT, 80);
        // Cross-check against  constant (not directly imported,
        // but the port number is documented and load-bearing).
        assert_ne!(LUXOS_HTTPD_PORT, 9012);
    }

    #[test]
    fn recovery_does_not_require_auth_csrf_surface_finding() {
        // §7b LOAD-BEARING finding: "No authentication on
        // /recovery.html or its CGIs. Anyone on the LAN who can reach
        // port 80 can call /cgi-bin/uninstall.cgi and brick LuxOS."
        // Pin so a refactor cannot silently set this true.
        assert!(!LUXOS_RECOVERY_REQUIRES_AUTH);
    }

    #[test]
    fn recovery_html_path_pinned() {
        assert_eq!(LUXOS_RECOVERY_HTML_PATH, "/firmware/recovery.html");
    }

    #[test]
    fn enums_round_trip_through_serde() {
        for action in [
            LuxosRecoveryAction::Reboot,
            LuxosRecoveryAction::Uninstall,
            LuxosRecoveryAction::DownloadCgminerConf,
            LuxosRecoveryAction::DownloadLuxminerToml,
            LuxosRecoveryAction::BrowseLogs,
        ] {
            let json = serde_json::to_string(&action).unwrap();
            let back: LuxosRecoveryAction = serde_json::from_str(&json).unwrap();
            assert_eq!(action, back);
        }
        for cgi in [
            LuxosRecoveryCgi::Reboot,
            LuxosRecoveryCgi::Uninstall,
            LuxosRecoveryCgi::GetLogs,
            LuxosRecoveryCgi::DownloadFile,
        ] {
            let json = serde_json::to_string(&cgi).unwrap();
            let back: LuxosRecoveryCgi = serde_json::from_str(&json).unwrap();
            assert_eq!(cgi, back);
        }
        for group in LuxosLogGroup::ALL {
            let json = serde_json::to_string(&group).unwrap();
            let back: LuxosLogGroup = serde_json::from_str(&json).unwrap();
            assert_eq!(group, back);
        }
        for step in UNINSTALL_SH_STEPS.iter().copied() {
            let json = serde_json::to_string(&step).unwrap();
            let back: LuxosUninstallStep = serde_json::from_str(&json).unwrap();
            assert_eq!(step, back);
        }
    }
}
