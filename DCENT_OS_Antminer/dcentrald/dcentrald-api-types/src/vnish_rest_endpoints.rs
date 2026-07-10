//!  vnish-A — VNish REST API endpoint catalog (HAL-free).
//!
//! Source RE evidence:
//!  §3.1
//! (verified against pyasic VNishWebAPI source).
//!
//! VNish 1.2.x ships a Bearer-token REST API on port 80 at
//! `/api/v1/...`. Authentication: `POST /api/v1/unlock` with
//! `{"pw":"<password>"}` returns `{"token":"<JWT>"}`. Subsequent calls
//! use `Authorization: Bearer <token>`.
//!
//!: the default web-UI
//! password is `admin` on VNish 1.2.6 (corrects an old RE doc claiming
//! `root`).
//!
//! This catalog is HAL-free; `dcent-toolbox` uses it for the VNish
//! source-firmware install adapter, and the dashboard's competitive-
//! readiness widget compares feature parity against it.

use serde::{Deserialize, Serialize};

/// HTTP method used by an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum VnishMethod {
    Get,
    Post,
    Put,
    Delete,
}

/// Endpoint behavior class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishEndpointKind {
    /// No state change (GET endpoints, queries).
    Read,
    /// Persists configuration (settings/pools/autotune).
    Write,
    /// Restarts mining or device — operator-visible.
    Lifecycle,
    /// Erases settings, restores stock firmware, or otherwise needs
    /// explicit confirmation.
    Destructive,
}

/// Auth requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishAuth {
    /// `/api/v1/unlock` is the auth gateway — no Bearer required.
    None,
    /// All other endpoints require `Authorization: Bearer <token>`.
    Bearer,
}

/// Top-level VNish REST endpoint we have evidence on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishEndpoint {
    Unlock,
    Info,
    Summary,
    PerfSummary,
    Chips,
    Layout,
    Status,
    SettingsGet,
    SettingsSet,
    PoolsGet,
    PoolsSet,
    AutotunePresetsGet,
    AutotunePresetsSet,
    Metrics,
    FactoryInfo,
    MiningRestart,
    MiningPause,
    MiningResume,
    MiningStop,
    MiningStart,
    SystemReboot,
    FindMiner,
    Upgrade,
    FactoryReset,
    RestoreStock,
}

/// Endpoint descriptor with HTTP method + path + auth + kind.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct VnishEndpointDescriptor {
    pub endpoint: VnishEndpoint,
    pub method: VnishMethod,
    pub path: &'static str,
    pub auth: VnishAuth,
    pub kind: VnishEndpointKind,
}

/// Look up the canonical descriptor.
pub fn descriptor(endpoint: VnishEndpoint) -> VnishEndpointDescriptor {
    use VnishAuth::{Bearer as Bearer_, None as None_};
    use VnishEndpoint::*;
    use VnishEndpointKind::*;
    use VnishMethod::*;
    let (method, path, auth, kind) = match endpoint {
        Unlock => (Post, "/api/v1/unlock", None_, Read),
        Info => (Get, "/api/v1/info", Bearer_, Read),
        Summary => (Get, "/api/v1/summary", Bearer_, Read),
        PerfSummary => (Get, "/api/v1/perf-summary", Bearer_, Read),
        Chips => (Get, "/api/v1/chips", Bearer_, Read),
        Layout => (Get, "/api/v1/layout", Bearer_, Read),
        Status => (Get, "/api/v1/status", Bearer_, Read),
        SettingsGet => (Get, "/api/v1/settings", Bearer_, Read),
        SettingsSet => (Post, "/api/v1/settings", Bearer_, Write),
        PoolsGet => (Get, "/api/v1/pools", Bearer_, Read),
        PoolsSet => (Post, "/api/v1/pools", Bearer_, Write),
        AutotunePresetsGet => (Get, "/api/v1/autotune/presets", Bearer_, Read),
        AutotunePresetsSet => (Post, "/api/v1/autotune/presets", Bearer_, Write),
        Metrics => (Get, "/api/v1/metrics", Bearer_, Read),
        FactoryInfo => (Get, "/api/v1/factory-info", Bearer_, Read),
        MiningRestart => (Post, "/api/v1/mining/restart", Bearer_, Lifecycle),
        MiningPause => (Post, "/api/v1/mining/pause", Bearer_, Lifecycle),
        MiningResume => (Post, "/api/v1/mining/resume", Bearer_, Lifecycle),
        MiningStop => (Post, "/api/v1/mining/stop", Bearer_, Lifecycle),
        MiningStart => (Post, "/api/v1/mining/start", Bearer_, Lifecycle),
        SystemReboot => (Post, "/api/v1/system/reboot", Bearer_, Lifecycle),
        FindMiner => (Post, "/api/v1/find-miner", Bearer_, Write),
        // Upgrade: writes a new firmware to flash. Without successful
        // signing-oracle verification this can brick — classify
        // Destructive so the dashboard requires confirmation.
        Upgrade => (Post, "/api/v1/upgrade", Bearer_, Destructive),
        FactoryReset => (Post, "/api/v1/factory-reset", Bearer_, Destructive),
        RestoreStock => (Post, "/api/v1/restore-stock", Bearer_, Destructive),
    };
    VnishEndpointDescriptor {
        endpoint,
        method,
        path,
        auth,
        kind,
    }
}

/// True iff this endpoint requires explicit confirmation (Destructive).
pub fn requires_confirmation(endpoint: VnishEndpoint) -> bool {
    descriptor(endpoint).kind == VnishEndpointKind::Destructive
}

/// VNish 1.2.6 default web UI password (per
/// ). Lab-environment
/// constant only; production flow always asks the operator.
pub const VNISH_DEFAULT_PASSWORD: &str = "admin";

/// Base URL prefix for all VNish REST endpoints.
pub const VNISH_API_PREFIX: &str = "/api/v1/";

/// All endpoints in stable iteration order.
pub const ALL_ENDPOINTS: &[VnishEndpoint] = &[
    VnishEndpoint::Unlock,
    VnishEndpoint::Info,
    VnishEndpoint::Summary,
    VnishEndpoint::PerfSummary,
    VnishEndpoint::Chips,
    VnishEndpoint::Layout,
    VnishEndpoint::Status,
    VnishEndpoint::SettingsGet,
    VnishEndpoint::SettingsSet,
    VnishEndpoint::PoolsGet,
    VnishEndpoint::PoolsSet,
    VnishEndpoint::AutotunePresetsGet,
    VnishEndpoint::AutotunePresetsSet,
    VnishEndpoint::Metrics,
    VnishEndpoint::FactoryInfo,
    VnishEndpoint::MiningRestart,
    VnishEndpoint::MiningPause,
    VnishEndpoint::MiningResume,
    VnishEndpoint::MiningStop,
    VnishEndpoint::MiningStart,
    VnishEndpoint::SystemReboot,
    VnishEndpoint::FindMiner,
    VnishEndpoint::Upgrade,
    VnishEndpoint::FactoryReset,
    VnishEndpoint::RestoreStock,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_count_matches_re_doc() {
        // VNISH_REVERSE_ENGINEERING.md §3.1 lists 25 distinct REST endpoints
        // (1 auth + 14 GET + 7 POST + 3 system).
        assert_eq!(ALL_ENDPOINTS.len(), 25);
    }

    #[test]
    fn unlock_is_only_endpoint_without_bearer() {
        // /api/v1/unlock is the gateway — every other endpoint requires
        // Bearer.
        for endpoint in ALL_ENDPOINTS.iter().copied() {
            let d = descriptor(endpoint);
            if endpoint == VnishEndpoint::Unlock {
                assert_eq!(d.auth, VnishAuth::None);
            } else {
                assert_eq!(
                    d.auth,
                    VnishAuth::Bearer,
                    "{:?} should require Bearer",
                    endpoint
                );
            }
        }
    }

    #[test]
    fn destructive_endpoints_pinned() {
        // Three canonical destructive endpoints: upgrade, factory-reset,
        // restore-stock.
        let destructives: Vec<VnishEndpoint> = ALL_ENDPOINTS
            .iter()
            .copied()
            .filter(|e| requires_confirmation(*e))
            .collect();
        assert_eq!(destructives.len(), 3);
        assert!(destructives.contains(&VnishEndpoint::Upgrade));
        assert!(destructives.contains(&VnishEndpoint::FactoryReset));
        assert!(destructives.contains(&VnishEndpoint::RestoreStock));
    }

    #[test]
    fn mining_endpoints_classify_as_lifecycle() {
        for endpoint in [
            VnishEndpoint::MiningRestart,
            VnishEndpoint::MiningPause,
            VnishEndpoint::MiningResume,
            VnishEndpoint::MiningStop,
            VnishEndpoint::MiningStart,
            VnishEndpoint::SystemReboot,
        ] {
            let d = descriptor(endpoint);
            assert_eq!(d.kind, VnishEndpointKind::Lifecycle);
            assert_eq!(d.method, VnishMethod::Post);
        }
    }

    #[test]
    fn every_path_starts_with_api_v1_prefix() {
        for endpoint in ALL_ENDPOINTS.iter().copied() {
            let d = descriptor(endpoint);
            assert!(
                d.path.starts_with(VNISH_API_PREFIX),
                "{:?} path '{}' does not start with {}",
                endpoint,
                d.path,
                VNISH_API_PREFIX
            );
        }
    }

    #[test]
    fn vnish_api_prefix_is_pinned() {
        // pyasic + dashboard + dcent-toolbox all rely on this prefix.
        assert_eq!(VNISH_API_PREFIX, "/api/v1/");
    }

    #[test]
    fn vnish_default_password_pinned_per_reference_doc() {
        // : VNish 1.2.6 default
        // is `admin` (NOT `root` as old RE doc claimed).
        assert_eq!(VNISH_DEFAULT_PASSWORD, "admin");
    }

    #[test]
    fn settings_get_and_set_share_path_but_differ_in_method() {
        let g = descriptor(VnishEndpoint::SettingsGet);
        let s = descriptor(VnishEndpoint::SettingsSet);
        assert_eq!(g.path, s.path);
        assert_eq!(g.method, VnishMethod::Get);
        assert_eq!(s.method, VnishMethod::Post);
        assert_eq!(g.kind, VnishEndpointKind::Read);
        assert_eq!(s.kind, VnishEndpointKind::Write);
    }

    #[test]
    fn pools_get_and_set_share_path_but_differ_in_method() {
        let g = descriptor(VnishEndpoint::PoolsGet);
        let s = descriptor(VnishEndpoint::PoolsSet);
        assert_eq!(g.path, s.path);
        assert_eq!(g.method, VnishMethod::Get);
        assert_eq!(s.method, VnishMethod::Post);
    }

    #[test]
    fn read_endpoints_use_get_method() {
        for endpoint in ALL_ENDPOINTS.iter().copied() {
            let d = descriptor(endpoint);
            if d.kind == VnishEndpointKind::Read && endpoint != VnishEndpoint::Unlock {
                assert_eq!(
                    d.method,
                    VnishMethod::Get,
                    "{:?} kind=Read should use GET",
                    endpoint
                );
            }
        }
    }

    #[test]
    fn unlock_uses_post_despite_being_read_class() {
        // /api/v1/unlock is POST because it carries a password body, but
        // doesn't change persistent state — classify Read.
        let d = descriptor(VnishEndpoint::Unlock);
        assert_eq!(d.method, VnishMethod::Post);
        assert_eq!(d.kind, VnishEndpointKind::Read);
    }

    #[test]
    fn vnish_method_serializes_uppercase() {
        assert_eq!(serde_json::to_string(&VnishMethod::Get).unwrap(), "\"GET\"");
        assert_eq!(
            serde_json::to_string(&VnishMethod::Post).unwrap(),
            "\"POST\""
        );
    }

    #[test]
    fn endpoint_round_trips_through_serde() {
        for endpoint in ALL_ENDPOINTS.iter().copied() {
            let json = serde_json::to_string(&endpoint).unwrap();
            let back: VnishEndpoint = serde_json::from_str(&json).unwrap();
            assert_eq!(endpoint, back);
        }
    }

    #[test]
    fn endpoint_kind_serializes_in_snake_case() {
        for (kind, expected) in [
            (VnishEndpointKind::Read, "\"read\""),
            (VnishEndpointKind::Write, "\"write\""),
            (VnishEndpointKind::Lifecycle, "\"lifecycle\""),
            (VnishEndpointKind::Destructive, "\"destructive\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), expected);
        }
    }

    #[test]
    fn factory_info_is_read_only() {
        // /api/v1/factory-info exposes serial/board-type/PSU info — read.
        let d = descriptor(VnishEndpoint::FactoryInfo);
        assert_eq!(d.kind, VnishEndpointKind::Read);
        assert_eq!(d.method, VnishMethod::Get);
    }

    #[test]
    fn restore_stock_is_destructive_not_lifecycle() {
        // /api/v1/restore-stock rolls back to Bitmain firmware — flash
        // erase happens. MUST be Destructive, not Lifecycle.
        let d = descriptor(VnishEndpoint::RestoreStock);
        assert_eq!(d.kind, VnishEndpointKind::Destructive);
        assert!(requires_confirmation(VnishEndpoint::RestoreStock));
    }
}
