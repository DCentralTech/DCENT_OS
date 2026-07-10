//!  vnish-B — VNish 1.2.7 overlay-on-stock layout DTOs (HAL-free).
//!
//! Source RE evidence:
//! - `mining-bible-v1/_canonical/system-orchestration-bible.md` Family C
//!   (lines 74-124).
//! - `vnish/VNISH_REVERSE_ENGINEERING.md` §1.3 (Boot Sequence).
//!
//! VNish 1.2.7 is the dominant pattern across modern miners (S19k Pro,
//! S19j Pro, L7, L9). It does NOT replace stock firmware — it
//! parasitizes it. The stock Bitmain BootROM + FSBL + U-Boot + kernel
//! all run UNTOUCHED; VNish injects a tarball overlay over the running
//! rootfs via `bootos.sh`. Recovery from corruption is trivial:
//! `rm -rf /nvdata/anthillos` removes the overlay entirely.
//!
//! This module pins the canonical filesystem layout, the bootos.sh
//! takeover sequence (14 steps), the AnthillOS service start order, and
//! the killalled stock services so dcent-toolbox / install-time
//! adapters / dashboard recovery flows can work the surface
//! losslessly.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Canonical paths
// ---------------------------------------------------------------------------

/// Canonical VNish 1.2.7 path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishOverlayPath {
    /// Root of the AnthillOS installation. `rm -rf` removes the whole overlay.
    NvdataAnthillOs,
    /// AnthillOS persistent config (pool, network, autotune).
    NvdataConfig,
    /// Original stock `/config` preserved bind-mount (mac, sn, network.conf).
    ConfigStock,
    /// Active config bind-mount (consumed by AnthillOS services).
    Config,
    /// The overlay tarball that is dropped on top of stock rootfs.
    OverlayTarball,
    /// `/scripts/boot` ELF copied into place by `runme.sh`.
    ScriptsBoot,
    /// AnthillOS bootos.sh entry point (~92 lines).
    NvdataAnthillOsScriptsBootos,
    /// AnthillOS update entry point.
    NvdataAnthillOsScriptsUpdateos,
    /// S15 timezone init script.
    EtcInitDS15Timezone,
    /// S12 hwscan (model probe + def-conf).
    EtcInitDS12Hwscan,
    /// S80 dashd (Web UI on :80, talks cgminer api :4029).
    EtcInitDS80Dashd,
    /// S70 miner (cgminer Vnish patched on :4028+:4029).
    EtcInitDS70Miner,
    /// S71 monitor (supervises cgminer).
    EtcInitDS71Monitor,
    /// S50 dropbear (replaces stock sshd).
    EtcInitDS50Dropbear,
    /// S13 restore — copies missing factory configs.
    EtcInitDS13Restore,
}

impl VnishOverlayPath {
    /// Filesystem path string (canonical).
    pub fn path(&self) -> &'static str {
        match self {
            Self::NvdataAnthillOs => "/nvdata/anthillos",
            Self::NvdataConfig => "/nvdata/anthillos/config",
            Self::ConfigStock => "/config-stock",
            Self::Config => "/config",
            Self::OverlayTarball => "/nvdata/anthillos/overlay.tar.gz",
            Self::ScriptsBoot => "/scripts/boot",
            Self::NvdataAnthillOsScriptsBootos => "/nvdata/anthillos/scripts/bootos.sh",
            Self::NvdataAnthillOsScriptsUpdateos => "/nvdata/anthillos/scripts/updateos.sh",
            Self::EtcInitDS15Timezone => "/etc/init.d/S15timezone",
            Self::EtcInitDS12Hwscan => "/etc/init.d/S12hwscan",
            Self::EtcInitDS80Dashd => "/etc/init.d/S80dashd",
            Self::EtcInitDS70Miner => "/etc/init.d/S70miner",
            Self::EtcInitDS71Monitor => "/etc/init.d/S71monitor",
            Self::EtcInitDS50Dropbear => "/etc/init.d/S50dropbear",
            Self::EtcInitDS13Restore => "/etc/init.d/S13restore",
        }
    }
}

// ---------------------------------------------------------------------------
// Stock services killalled by bootos.sh
// ---------------------------------------------------------------------------

/// Each stock-Bitmain service that bootos.sh terminates before laying
/// down the overlay. Kept stable so the install-time adapter can
/// pre-validate that these services are present (or warn if the unit
/// is already not stock).
pub const STOCK_KILLED_PROCESSES: &[&str] = &[
    "lighttpd",
    "syslogd",
    "daemons",
    "S52miner_act",
    "S71monitorcg",
];

/// Each stock-Bitmain init-script that bootos.sh disables.
pub const STOCK_DISABLED_INIT_SCRIPTS: &[&str] = &[
    "/etc/init.d/S70cgminer",
    "/etc/init.d/S65monitor-ipsig",
    "/etc/init.d/S66monitor-recobtn",
    "/etc/init.d/S52miner_act",
    "/etc/init.d/S71monitorcg",
];

// ---------------------------------------------------------------------------
// bootos.sh phase ordering
// ---------------------------------------------------------------------------

/// Stages of `bootos.sh` in execution order (from system-orchestration-bible.md
/// Family C lines 86-105). Each stage maps to one or more shell-script lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootosPhase {
    /// 1) `killall -9` stock processes (lighttpd/syslogd/daemons/etc).
    KillStockProcesses,
    /// 2) Stop stock init scripts (S70cgminer / S65monitor-ipsig / S66monitor-recobtn).
    StopStockInitScripts,
    /// 3) `rm -f` cgminer/bmminer binaries (replaced from overlay).
    RemoveStockBinaries,
    /// 4) `rm -f` stock init script symlinks.
    RemoveStockInitScripts,
    /// 5) GPIO 412 PSU disable + 427/429/431/433 chain reset (Amlogic).
    GpioPsuAndChainReset,
    /// 6) Disable kernel exception trace.
    DisableExceptionTrace,
    /// 7) Bind-mount stock `/config` to `/config-stock` (preserve mac/network/sn).
    BindMountStockConfig,
    /// 8) Bind-mount AnthillOS config to `/config`.
    BindMountAnthillConfig,
    /// 9) Extract `/nvdata/anthillos/overlay.tar.gz` over rootfs.
    ExtractOverlay,
    /// 10) Symlink stock-preserved files (mac, network.conf, sn) into new /config.
    SymlinkPreservedConfigs,
    /// 11) `S13restore start` to copy missing factory configs.
    RestoreFactoryConfigs,
    /// 12) `cp /etc/shadow.factory /etc/shadow` (reset password).
    ResetShadowPassword,
    /// 13) `rmmod uart_trans` if present (cv-only path).
    RmmodUartTransIfCv,
    /// 14) Start AnthillOS services (S15→S12→S80→S70→S71→S50).
    StartAnthillServices,
}

/// All `bootos.sh` phases in canonical execution order.
pub const BOOTOS_SH_PHASES: &[BootosPhase] = &[
    BootosPhase::KillStockProcesses,
    BootosPhase::StopStockInitScripts,
    BootosPhase::RemoveStockBinaries,
    BootosPhase::RemoveStockInitScripts,
    BootosPhase::GpioPsuAndChainReset,
    BootosPhase::DisableExceptionTrace,
    BootosPhase::BindMountStockConfig,
    BootosPhase::BindMountAnthillConfig,
    BootosPhase::ExtractOverlay,
    BootosPhase::SymlinkPreservedConfigs,
    BootosPhase::RestoreFactoryConfigs,
    BootosPhase::ResetShadowPassword,
    BootosPhase::RmmodUartTransIfCv,
    BootosPhase::StartAnthillServices,
];

// ---------------------------------------------------------------------------
// AnthillOS service start order (the LAST bootos.sh phase)
// ---------------------------------------------------------------------------

/// AnthillOS service start order, as invoked at the end of `bootos.sh`.
/// Order matters: dashd depends on hwscan; miner depends on dashd
/// reading `/config` first; monitor wraps miner; dropbear last.
pub const ANTHILL_SERVICE_START_ORDER: &[&str] = &[
    "/etc/init.d/S15timezone",
    "/etc/init.d/S12hwscan",
    "/etc/init.d/S80dashd",
    "/etc/init.d/S70miner",
    "/etc/init.d/S71monitor",
    "/etc/init.d/S50dropbear",
];

// ---------------------------------------------------------------------------
// Recovery contract
// ---------------------------------------------------------------------------

/// Canonical command to fully remove the VNish overlay and revert to
/// stock behavior on next reboot.
pub const RECOVERY_REMOVE_OVERLAY_COMMAND: &str = "rm -rf /nvdata/anthillos";

/// True iff the overlay-extract phase touches a path INSIDE
/// `/nvdata/anthillos` (i.e. is reversible by the recovery command).
pub fn is_in_recoverable_root(path: &str) -> bool {
    path.starts_with("/nvdata/anthillos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvdata_anthillos_is_overlay_root() {
        assert_eq!(
            VnishOverlayPath::NvdataAnthillOs.path(),
            "/nvdata/anthillos"
        );
    }

    #[test]
    fn config_stock_preserves_factory_files() {
        // /config-stock is the bind-mount of the original stock /config —
        // pinned because the install-time adapter relies on this exact
        // location to read mac / sn / network.conf.
        assert_eq!(VnishOverlayPath::ConfigStock.path(), "/config-stock");
    }

    #[test]
    fn overlay_tarball_path_is_canonical() {
        assert_eq!(
            VnishOverlayPath::OverlayTarball.path(),
            "/nvdata/anthillos/overlay.tar.gz"
        );
    }

    #[test]
    fn init_d_paths_use_anthillos_naming() {
        for (variant, expected) in [
            (
                VnishOverlayPath::EtcInitDS15Timezone,
                "/etc/init.d/S15timezone",
            ),
            (VnishOverlayPath::EtcInitDS12Hwscan, "/etc/init.d/S12hwscan"),
            (VnishOverlayPath::EtcInitDS80Dashd, "/etc/init.d/S80dashd"),
            (VnishOverlayPath::EtcInitDS70Miner, "/etc/init.d/S70miner"),
            (
                VnishOverlayPath::EtcInitDS71Monitor,
                "/etc/init.d/S71monitor",
            ),
            (
                VnishOverlayPath::EtcInitDS50Dropbear,
                "/etc/init.d/S50dropbear",
            ),
        ] {
            assert_eq!(variant.path(), expected);
        }
    }

    #[test]
    fn stock_killed_processes_match_re_doc() {
        // Family C bootos.sh §1: lighttpd, syslogd, daemons, S52miner_act,
        // S71monitorcg.
        assert_eq!(STOCK_KILLED_PROCESSES.len(), 5);
        for proc in &[
            "lighttpd",
            "syslogd",
            "daemons",
            "S52miner_act",
            "S71monitorcg",
        ] {
            assert!(
                STOCK_KILLED_PROCESSES.contains(proc),
                "expected '{}' in killed list",
                proc
            );
        }
    }

    #[test]
    fn stock_disabled_init_scripts_match_re_doc() {
        assert_eq!(STOCK_DISABLED_INIT_SCRIPTS.len(), 5);
        for path in &[
            "/etc/init.d/S70cgminer",
            "/etc/init.d/S65monitor-ipsig",
            "/etc/init.d/S66monitor-recobtn",
            "/etc/init.d/S52miner_act",
            "/etc/init.d/S71monitorcg",
        ] {
            assert!(
                STOCK_DISABLED_INIT_SCRIPTS.contains(path),
                "expected '{}' in disabled init list",
                path
            );
        }
    }

    #[test]
    fn bootos_sh_has_14_phases_in_canonical_order() {
        assert_eq!(BOOTOS_SH_PHASES.len(), 14);
        // First phase always kills stock processes; last phase always
        // starts AnthillOS services.
        assert_eq!(
            BOOTOS_SH_PHASES.first(),
            Some(&BootosPhase::KillStockProcesses)
        );
        assert_eq!(
            BOOTOS_SH_PHASES.last(),
            Some(&BootosPhase::StartAnthillServices)
        );
    }

    #[test]
    fn extract_overlay_runs_after_config_bind_mounts() {
        // bootos.sh §7→§8→§9: stock config bind, anthill config bind,
        // THEN extract overlay. Order matters — extracting before the
        // bind mounts would clobber preserved files.
        let pos = |p: BootosPhase| {
            BOOTOS_SH_PHASES
                .iter()
                .position(|x| *x == p)
                .expect("phase missing")
        };
        assert!(pos(BootosPhase::BindMountStockConfig) < pos(BootosPhase::ExtractOverlay));
        assert!(pos(BootosPhase::BindMountAnthillConfig) < pos(BootosPhase::ExtractOverlay));
        // And the anthill bind comes after the stock bind so the
        // preserved /config-stock is captured first.
        assert!(pos(BootosPhase::BindMountStockConfig) < pos(BootosPhase::BindMountAnthillConfig));
    }

    #[test]
    fn symlink_preserved_configs_runs_after_overlay_extract() {
        // §10: relink mac/network.conf/sn into new /config — must come
        // AFTER §9 ExtractOverlay otherwise the new /config is empty.
        let pos = |p: BootosPhase| BOOTOS_SH_PHASES.iter().position(|x| *x == p).unwrap();
        assert!(pos(BootosPhase::ExtractOverlay) < pos(BootosPhase::SymlinkPreservedConfigs));
    }

    #[test]
    fn shadow_password_reset_runs_before_dropbear_start() {
        // §12 ResetShadowPassword must precede §14 StartAnthillServices
        // (which starts S50dropbear).
        let pos = |p: BootosPhase| BOOTOS_SH_PHASES.iter().position(|x| *x == p).unwrap();
        assert!(pos(BootosPhase::ResetShadowPassword) < pos(BootosPhase::StartAnthillServices));
    }

    #[test]
    fn anthill_service_start_order_is_correct() {
        // S15 → S12 → S80 → S70 → S71 → S50.
        assert_eq!(ANTHILL_SERVICE_START_ORDER.len(), 6);
        assert_eq!(ANTHILL_SERVICE_START_ORDER[0], "/etc/init.d/S15timezone");
        assert_eq!(ANTHILL_SERVICE_START_ORDER[1], "/etc/init.d/S12hwscan");
        assert_eq!(ANTHILL_SERVICE_START_ORDER[2], "/etc/init.d/S80dashd");
        assert_eq!(ANTHILL_SERVICE_START_ORDER[3], "/etc/init.d/S70miner");
        assert_eq!(ANTHILL_SERVICE_START_ORDER[4], "/etc/init.d/S71monitor");
        assert_eq!(ANTHILL_SERVICE_START_ORDER[5], "/etc/init.d/S50dropbear");
    }

    #[test]
    fn dashd_starts_before_miner_and_monitor() {
        // Order matters: S80dashd sets up :80 + reads /config; S70miner
        // depends on it; S71monitor wraps miner.
        let pos = |s: &str| {
            ANTHILL_SERVICE_START_ORDER
                .iter()
                .position(|x| *x == s)
                .unwrap()
        };
        assert!(pos("/etc/init.d/S80dashd") < pos("/etc/init.d/S70miner"));
        assert!(pos("/etc/init.d/S70miner") < pos("/etc/init.d/S71monitor"));
    }

    #[test]
    fn recovery_command_targets_anthillos_root() {
        // Pin the canonical recovery — pyasic's VNish removal flow uses
        // exactly this command.
        assert_eq!(RECOVERY_REMOVE_OVERLAY_COMMAND, "rm -rf /nvdata/anthillos");
    }

    #[test]
    fn paths_inside_anthillos_classify_as_recoverable() {
        // All overlay-introduced paths are under /nvdata/anthillos and
        // therefore reversible by the recovery command. Stock-preserved
        // paths (/config-stock, /etc/init.d/*) are NOT inside the
        // recoverable root — they survive removal.
        assert!(is_in_recoverable_root("/nvdata/anthillos/overlay.tar.gz"));
        assert!(is_in_recoverable_root(
            "/nvdata/anthillos/scripts/bootos.sh"
        ));
        assert!(is_in_recoverable_root("/nvdata/anthillos/config"));
        assert!(!is_in_recoverable_root("/config-stock"));
        assert!(!is_in_recoverable_root("/etc/init.d/S80dashd"));
        // The fact that S80dashd paths are NOT inside the recoverable
        // root is by design: removing the overlay leaves the symlinks
        // dangling, which next boot ignores because stock S70cgminer
        // is back in PID 1's init runlevel.
    }

    #[test]
    fn variant_round_trips_through_serde() {
        for variant in [
            VnishOverlayPath::NvdataAnthillOs,
            VnishOverlayPath::ConfigStock,
            VnishOverlayPath::OverlayTarball,
            VnishOverlayPath::EtcInitDS80Dashd,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: VnishOverlayPath = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn bootos_phase_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&BootosPhase::KillStockProcesses).unwrap(),
            "\"kill_stock_processes\""
        );
        assert_eq!(
            serde_json::to_string(&BootosPhase::StartAnthillServices).unwrap(),
            "\"start_anthill_services\""
        );
    }
}
