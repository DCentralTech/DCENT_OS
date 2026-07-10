//!  luxos-A — LuxOS web UI page + endpoint catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! (637 lines, live capture from S19j Pro `a lab unit` running LuxOS 1.38.1).
//!
//! LuxOS exposes a Vite + React 18 SPA on port 80 (busybox httpd) consuming
//! a REST API on port 8080 (luxminer). The page registry below catalogs
//! every page route + its companion REST commands so that:
//! - `dcent-toolbox` can compare DCENT_OS feature parity page-for-page;
//! - the dashboard's competitor-readiness widget can flag missing surfaces;
//! - install-time recovery flow can warn before destructive ops
//!   (uninstall LuxOS).
//!
//! HAZARD pinned by tests:
//! - `Uninstall` is a Destructive action — it `flash_erase`s mtd5 + mtd11
//!   then triggers `/proc/sysrq-trigger` power-off. PIN must require
//!   confirmation.
//! - `Reboot` and `RestartLuxminer` are Write actions, not Destructive
//!   (they do not erase flash).

use serde::{Deserialize, Serialize};

/// Action class for an action surface (page or endpoint). Read = no
/// state change. Write = persists configuration. Destructive = erases
/// flash, reboots, or otherwise requires operator confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosActionKind {
    Read,
    Write,
    Destructive,
}

/// Top-level page in the LuxOS SPA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosPage {
    /// `/dashboard` — KPIs, hashboard tiles, fan/pool status.
    Dashboard,
    /// `/presets` — Autotuner profile management.
    Presets,
    /// `/temperature` — Temperature & ATM (Advanced Thermal Management).
    Temperature,
    /// `/pools` — Pool/group manager + pool options.
    Pools,
    /// `/logs/current` — Live log tail.
    LogsCurrent,
    /// `/logs/history` — Log file browser/download.
    LogsHistory,
    /// `/logs/audit` — System audit ring buffer.
    LogsAudit,
    /// `/settings` — All persistable configuration.
    Settings,
    /// `/firmware/recovery.html` — Recovery-mode UI shell.
    Recovery,
}

/// Page descriptor with route + auth + action kind.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LuxosPageDescriptor {
    pub page: LuxosPage,
    pub path: &'static str,
    pub title: &'static str,
    pub requires_auth: bool,
    pub action: LuxosActionKind,
}

/// Canonical SPA page registry. Order matches the sidebar nav order from
/// `sidebar-Db9tOLXU.js` in J-web-ui.md §4.
pub const LUXOS_PAGES: &[LuxosPageDescriptor] = &[
    LuxosPageDescriptor {
        page: LuxosPage::Dashboard,
        path: "/dashboard",
        title: "Dashboard",
        requires_auth: true,
        action: LuxosActionKind::Read,
    },
    LuxosPageDescriptor {
        page: LuxosPage::Presets,
        path: "/presets",
        title: "Preset Profiles",
        requires_auth: true,
        action: LuxosActionKind::Write,
    },
    LuxosPageDescriptor {
        page: LuxosPage::Temperature,
        path: "/temperature",
        title: "Temperature & ATM",
        requires_auth: true,
        action: LuxosActionKind::Write,
    },
    LuxosPageDescriptor {
        page: LuxosPage::Pools,
        path: "/pools",
        title: "Pools",
        requires_auth: true,
        action: LuxosActionKind::Write,
    },
    LuxosPageDescriptor {
        page: LuxosPage::LogsCurrent,
        path: "/logs/current",
        title: "Logs — Current",
        requires_auth: true,
        action: LuxosActionKind::Read,
    },
    LuxosPageDescriptor {
        page: LuxosPage::LogsHistory,
        path: "/logs/history",
        title: "Logs — History",
        requires_auth: true,
        action: LuxosActionKind::Read,
    },
    LuxosPageDescriptor {
        page: LuxosPage::LogsAudit,
        path: "/logs/audit",
        title: "Logs — Audit",
        requires_auth: true,
        action: LuxosActionKind::Read,
    },
    LuxosPageDescriptor {
        page: LuxosPage::Settings,
        path: "/settings",
        title: "Settings",
        requires_auth: true,
        action: LuxosActionKind::Write,
    },
    LuxosPageDescriptor {
        page: LuxosPage::Recovery,
        path: "/firmware/recovery.html",
        title: "Recovery",
        requires_auth: true,
        action: LuxosActionKind::Destructive,
    },
];

/// REST API endpoint exposed by LuxOS. Most ride the `?command=`
/// dispatcher on `:8080`; a small set of CGI paths live on `:80`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosApiEndpoint {
    /// `?command=stats` — dashboard hashboard polling.
    Stats,
    /// `?command=pools` — pool list.
    Pools,
    /// `?command=summary` — overall hashrate summary.
    Summary,
    /// `?command=tempctrl` — temperature control config.
    TempCtrl,
    /// `?command=tempctrlset` — set temperature targets.
    TempCtrlSet,
    /// `?command=fans` — read fan status.
    Fans,
    /// `?command=fanset` — set fan speed/min/max/power-off.
    FanSet,
    /// `?command=atm` — ATM auto-stepper status.
    Atm,
    /// `?command=atmset` — set ATM parameters.
    AtmSet,
    /// `?command=profiles` — enumerate autotuner profiles.
    Profiles,
    /// `?command=profileset` — apply a profile.
    ProfileSet,
    /// `?command=profilenew` / `profilerem` — create/remove profile.
    ProfileNew,
    ProfileRem,
    /// `?command=switchpool` — change active pool.
    SwitchPool,
    /// `?command=addpool` / `removepool` / `enablepool` / `disablepool`.
    AddPool,
    RemovePool,
    EnablePool,
    DisablePool,
    /// `?command=pooloptsset` — pool options.
    PoolOptsSet,
    /// `?command=netset` — network configuration.
    NetSet,
    /// `?command=updateset` / `updaterun` — firmware updater config + go.
    UpdateSet,
    UpdateRun,
    /// `?command=logset` — set log level.
    LogSet,
    /// `?command=psuset` — PSU bypass configuration.
    PsuSet,
    /// `?command=resetminer` / `reboot` — restart commands.
    ResetMiner,
    Reboot,
    /// `?command=curtail` — power state (active / sleep).
    Curtail,
    /// `?command=autotunerset` — enable / disable autotuner.
    AutotunerSet,
    /// `?command=ledset` — LED mode.
    LedSet,
    /// `PUT /api/settings` — API access (TCP/HTTP ports + password).
    ApiSettings,
    /// `?command=uninstallluxos` — destructive: erases mtd5+mtd11, reboots.
    UninstallLuxos,
    /// `/cgi-bin/reboot.cgi` — direct CGI reboot.
    CgiReboot,
    /// `/cgi-bin/uninstall.cgi` — destructive CGI uninstall trigger.
    CgiUninstall,
    /// `/cgi-bin/get_logs.cgi` — log retrieval.
    CgiGetLogs,
    /// `/cgi-bin/download_file.cgi` — generic file download.
    CgiDownloadFile,
}

/// Endpoint descriptor with HTTP path + action kind + companion page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LuxosApiDescriptor {
    pub endpoint: LuxosApiEndpoint,
    pub path: &'static str,
    pub action: LuxosActionKind,
    pub requires_auth: bool,
}

/// Returns `true` iff the endpoint is destructive and MUST require an
/// explicit confirmation pin (operator typed PIN, or web confirm modal).
pub fn requires_confirmation(endpoint: LuxosApiEndpoint) -> bool {
    matches!(
        endpoint,
        LuxosApiEndpoint::UninstallLuxos | LuxosApiEndpoint::CgiUninstall
    )
}

/// Returns the canonical descriptor for an endpoint.
pub fn endpoint_descriptor(endpoint: LuxosApiEndpoint) -> LuxosApiDescriptor {
    use LuxosActionKind::*;
    use LuxosApiEndpoint::*;
    let (path, action) = match endpoint {
        Stats => ("/api?command=stats", Read),
        Pools => ("/api?command=pools", Read),
        Summary => ("/api?command=summary", Read),
        TempCtrl => ("/api?command=tempctrl", Read),
        TempCtrlSet => ("/api?command=tempctrlset", Write),
        Fans => ("/api?command=fans", Read),
        FanSet => ("/api?command=fanset", Write),
        Atm => ("/api?command=atm", Read),
        AtmSet => ("/api?command=atmset", Write),
        Profiles => ("/api?command=profiles", Read),
        ProfileSet => ("/api?command=profileset", Write),
        ProfileNew => ("/api?command=profilenew", Write),
        ProfileRem => ("/api?command=profilerem", Write),
        SwitchPool => ("/api?command=switchpool", Write),
        AddPool => ("/api?command=addpool", Write),
        RemovePool => ("/api?command=removepool", Write),
        EnablePool => ("/api?command=enablepool", Write),
        DisablePool => ("/api?command=disablepool", Write),
        PoolOptsSet => ("/api?command=pooloptsset", Write),
        NetSet => ("/api?command=netset", Write),
        UpdateSet => ("/api?command=updateset", Write),
        UpdateRun => ("/api?command=updaterun", Write),
        LogSet => ("/api?command=logset", Write),
        PsuSet => ("/api?command=psuset", Write),
        ResetMiner => ("/api?command=resetminer", Write),
        Reboot => ("/api?command=reboot", Write),
        Curtail => ("/api?command=curtail", Write),
        AutotunerSet => ("/api?command=autotunerset", Write),
        LedSet => ("/api?command=ledset", Write),
        ApiSettings => ("/api/settings", Write),
        UninstallLuxos => ("/api?command=uninstallluxos", Destructive),
        CgiReboot => ("/cgi-bin/reboot.cgi", Write),
        CgiUninstall => ("/cgi-bin/uninstall.cgi", Destructive),
        CgiGetLogs => ("/cgi-bin/get_logs.cgi", Read),
        CgiDownloadFile => ("/cgi-bin/download_file.cgi", Read),
    };
    LuxosApiDescriptor {
        endpoint,
        path,
        action,
        requires_auth: true,
    }
}

/// Returns `true` iff a page exists for the given path.
pub fn page_for_path(path: &str) -> Option<&'static LuxosPageDescriptor> {
    LUXOS_PAGES.iter().find(|d| d.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_includes_canonical_dashboard() {
        let dash = page_for_path("/dashboard").expect("dashboard registered");
        assert_eq!(dash.page, LuxosPage::Dashboard);
        assert_eq!(dash.title, "Dashboard");
        assert!(dash.requires_auth);
        assert_eq!(dash.action, LuxosActionKind::Read);
    }

    #[test]
    fn registry_count_matches_re_doc_pages() {
        // J-web-ui.md §5 lists 9 distinct pages including recovery.
        assert_eq!(LUXOS_PAGES.len(), 9);
    }

    #[test]
    fn settings_is_write_action() {
        let s = page_for_path("/settings").unwrap();
        assert_eq!(s.action, LuxosActionKind::Write);
    }

    #[test]
    fn recovery_page_is_destructive() {
        // Recovery page hosts the Uninstall LuxOS UI which flash_erase's
        // mtd5+mtd11. Mark Destructive at the page level so callers can
        // gate it before showing.
        let r = page_for_path("/firmware/recovery.html").unwrap();
        assert_eq!(r.action, LuxosActionKind::Destructive);
    }

    #[test]
    fn unknown_path_returns_none() {
        assert!(page_for_path("/i-do-not-exist").is_none());
        assert!(page_for_path("/dashboard/extra").is_none());
    }

    #[test]
    fn requires_confirmation_only_on_destructive_endpoints() {
        // J-web-ui.md §5.6: uninstall LuxOS is the canonical destructive
        // action; reboots / restartminer are NOT destructive (no flash erase).
        assert!(requires_confirmation(LuxosApiEndpoint::UninstallLuxos));
        assert!(requires_confirmation(LuxosApiEndpoint::CgiUninstall));
        assert!(!requires_confirmation(LuxosApiEndpoint::Reboot));
        assert!(!requires_confirmation(LuxosApiEndpoint::CgiReboot));
        assert!(!requires_confirmation(LuxosApiEndpoint::ResetMiner));
        assert!(!requires_confirmation(LuxosApiEndpoint::Curtail));
        assert!(!requires_confirmation(LuxosApiEndpoint::Stats));
    }

    #[test]
    fn endpoint_descriptor_path_uses_command_dispatcher() {
        let d = endpoint_descriptor(LuxosApiEndpoint::Stats);
        assert_eq!(d.path, "/api?command=stats");
        assert_eq!(d.action, LuxosActionKind::Read);
        let d = endpoint_descriptor(LuxosApiEndpoint::AtmSet);
        assert_eq!(d.path, "/api?command=atmset");
        assert_eq!(d.action, LuxosActionKind::Write);
    }

    #[test]
    fn api_settings_uses_rest_path_not_command_dispatcher() {
        // J-web-ui.md §5.6 — the API access page (TCP+HTTP port + pwd)
        // uses `PUT /api/settings`, NOT the `?command=` route.
        let d = endpoint_descriptor(LuxosApiEndpoint::ApiSettings);
        assert_eq!(d.path, "/api/settings");
    }

    #[test]
    fn uninstall_endpoints_classify_as_destructive() {
        let a = endpoint_descriptor(LuxosApiEndpoint::UninstallLuxos);
        assert_eq!(a.action, LuxosActionKind::Destructive);
        let b = endpoint_descriptor(LuxosApiEndpoint::CgiUninstall);
        assert_eq!(b.action, LuxosActionKind::Destructive);
    }

    #[test]
    fn cgi_paths_live_under_cgi_bin() {
        for endpoint in [
            LuxosApiEndpoint::CgiReboot,
            LuxosApiEndpoint::CgiUninstall,
            LuxosApiEndpoint::CgiGetLogs,
            LuxosApiEndpoint::CgiDownloadFile,
        ] {
            let d = endpoint_descriptor(endpoint);
            assert!(
                d.path.starts_with("/cgi-bin/"),
                "{:?} path should start with /cgi-bin/, got {}",
                endpoint,
                d.path
            );
        }
    }

    #[test]
    fn every_page_descriptor_serializes_explicit_path() {
        for page in LUXOS_PAGES {
            let json = serde_json::to_value(page).unwrap();
            assert!(json.get("page").is_some());
            assert!(json.get("path").is_some());
            assert!(json.get("title").is_some());
            assert!(json.get("requires_auth").is_some());
            assert!(json.get("action").is_some());
        }
    }

    #[test]
    fn action_kind_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosActionKind::Read).unwrap(),
            "\"read\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosActionKind::Write).unwrap(),
            "\"write\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosActionKind::Destructive).unwrap(),
            "\"destructive\""
        );
    }

    #[test]
    fn luxos_page_round_trips_through_serde() {
        for descriptor in LUXOS_PAGES {
            let json = serde_json::to_string(&descriptor.page).unwrap();
            let back: LuxosPage = serde_json::from_str(&json).unwrap();
            assert_eq!(back, descriptor.page);
        }
    }
}
