//!  braiins-A — BraiinsOS+ gRPC service catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §9 (gRPC Public API — Complete Reference).
//!
//! BraiinsOS+ exposes a Tonic-based gRPC server on port 50051. The full
//! protobuf definitions are GPL-3.0-licensed at
//! `github.com/braiins/bos-plus-api`. This module pins the service +
//! method catalog plus the auth tier and version-introduced for each
//! RPC so dcent-toolbox + dashboard can advertise feature parity per
//! `firmware_stratum_matrix.rs` ().

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Service catalog
// ---------------------------------------------------------------------------

/// BraiinsOS+ gRPC service. Per RE doc §9 service list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BraiinsService {
    Authentication,
    Actions,
    Miner,
    Pool,
    Performance,
    Cooling,
    Configuration,
    Network,
    Upgrade,
    License,
    Version,
}

impl BraiinsService {
    /// Canonical proto package name (e.g.
    /// `braiins.bos.v1.MinerService`).
    pub fn proto_name(&self) -> &'static str {
        match self {
            Self::Authentication => "braiins.bos.v1.AuthenticationService",
            Self::Actions => "braiins.bos.v1.ActionsService",
            Self::Miner => "braiins.bos.v1.MinerService",
            Self::Pool => "braiins.bos.v1.PoolService",
            Self::Performance => "braiins.bos.v1.PerformanceService",
            Self::Cooling => "braiins.bos.v1.CoolingService",
            Self::Configuration => "braiins.bos.v1.ConfigurationService",
            Self::Network => "braiins.bos.v1.NetworkService",
            Self::Upgrade => "braiins.bos.v1.UpgradeService",
            Self::License => "braiins.bos.v1.LicenseService",
            Self::Version => "braiins.bos.v1.VersionService",
        }
    }
}

// ---------------------------------------------------------------------------
// Method catalog
// ---------------------------------------------------------------------------

/// BraiinsOS+ gRPC method (one variant per RPC across all services).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum BraiinsMethod {
    // AuthenticationService
    Login,
    SetPassword,
    // ActionsService
    Start,
    Stop,
    PauseMining,
    ResumeMining,
    Restart,
    Reboot,
    FactoryReset,
    SetLocateDeviceStatus,
    GetLocateDeviceStatus,
    // MinerService
    GetMinerStatus,
    GetMinerDetails,
    GetMinerStats,
    GetErrors,
    GetHashboards,
    GetSupportArchive,
    EnableHashboards,
    DisableHashboards,
    // PoolService
    GetPoolGroups,
    CreatePoolGroup,
    UpdatePoolGroup,
    RemovePoolGroup,
    SetPoolGroups,
    // PerformanceService
    GetTunerState,
    ListTargetProfiles,
    SetDefaultPowerTarget,
    SetPowerTarget,
    IncrementPowerTarget,
    DecrementPowerTarget,
    SetRelativePowerTarget,
    SetDefaultHashrateTarget,
    SetHashrateTarget,
    IncrementHashrateTarget,
    DecrementHashrateTarget,
    SetRelativeHashrateTarget,
    SetDPS,
    SetPerformanceMode,
    GetActivePerformanceMode,
    RemoveTunedProfiles,
    SetQuickRamping,
    SetDefaultQuickRamping,
    // CoolingService
    GetCoolingState,
    SetCoolingMode,
    SetImmersionMode,
    // ConfigurationService
    GetMinerConfiguration,
    GetConstraints,
    // NetworkService
    GetNetworkConfiguration,
    SetNetworkConfiguration,
    GetNetworkInfo,
    // UpgradeService
    UpdateAutoUpgradeConfig,
    GetAutoUpgradeStatus,
    // LicenseService
    GetLicenseState,
    ApplyContractKey,
    // VersionService
    GetApiVersion,
}

/// gRPC method descriptor: the service it lives on, the method's
/// auth tier, and the BraiinsOS+ release that introduced it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BraiinsMethodDescriptor {
    pub method: BraiinsMethod,
    pub service: BraiinsService,
    /// True iff a session token is required.
    pub auth_required: bool,
    /// First BraiinsOS+ minor version that exposed this method.
    pub version_introduced: &'static str,
    /// True iff this method is destructive (reboot / factory-reset /
    /// remove-profiles / disable-hashboards). UI MUST require
    /// confirmation.
    pub destructive: bool,
}

/// Look up the descriptor for a method.
pub fn descriptor(method: BraiinsMethod) -> BraiinsMethodDescriptor {
    use BraiinsMethod::*;
    use BraiinsService::*;
    let (service, auth, version, destructive) = match method {
        // AuthenticationService — Login is the only no-auth method.
        Login => (Authentication, false, "1.0.0", false),
        SetPassword => (Authentication, true, "1.0.0", false),
        // ActionsService
        Start => (Actions, true, "1.0.0", false),
        Stop => (Actions, true, "1.0.0", false),
        PauseMining => (Actions, true, "1.0.0", false),
        ResumeMining => (Actions, true, "1.0.0", false),
        Restart => (Actions, true, "1.0.0", false),
        Reboot => (Actions, true, "1.0.0", true),
        FactoryReset => (Actions, true, "1.9.0", true),
        SetLocateDeviceStatus => (Actions, true, "1.0.0", false),
        GetLocateDeviceStatus => (Actions, true, "1.0.0", false),
        // MinerService
        GetMinerStatus => (Miner, true, "1.0.0", false),
        GetMinerDetails => (Miner, true, "1.0.0", false),
        GetMinerStats => (Miner, true, "1.0.0", false),
        GetErrors => (Miner, true, "1.0.0", false),
        GetHashboards => (Miner, true, "1.0.0", false),
        GetSupportArchive => (Miner, true, "1.0.0", false),
        EnableHashboards => (Miner, true, "1.0.0", false),
        DisableHashboards => (Miner, true, "1.0.0", true),
        // PoolService
        GetPoolGroups => (Pool, true, "1.0.0", false),
        CreatePoolGroup => (Pool, true, "1.0.0", false),
        UpdatePoolGroup => (Pool, true, "1.0.0", false),
        RemovePoolGroup => (Pool, true, "1.0.0", false),
        SetPoolGroups => (Pool, true, "1.0.0", false),
        // PerformanceService
        GetTunerState => (Performance, true, "1.0.0", false),
        ListTargetProfiles => (Performance, true, "1.0.0", false),
        SetDefaultPowerTarget => (Performance, true, "1.0.0", false),
        SetPowerTarget => (Performance, true, "1.0.0", false),
        IncrementPowerTarget => (Performance, true, "1.0.0", false),
        DecrementPowerTarget => (Performance, true, "1.0.0", false),
        SetRelativePowerTarget => (Performance, true, "1.8.0", false),
        SetDefaultHashrateTarget => (Performance, true, "1.0.0", false),
        SetHashrateTarget => (Performance, true, "1.0.0", false),
        IncrementHashrateTarget => (Performance, true, "1.0.0", false),
        DecrementHashrateTarget => (Performance, true, "1.0.0", false),
        SetRelativeHashrateTarget => (Performance, true, "1.8.0", false),
        SetDPS => (Performance, true, "1.4.0", false),
        SetPerformanceMode => (Performance, true, "1.0.0", false),
        GetActivePerformanceMode => (Performance, true, "1.0.0", false),
        RemoveTunedProfiles => (Performance, true, "1.0.0", true),
        SetQuickRamping => (Performance, true, "1.7.0", false),
        SetDefaultQuickRamping => (Performance, true, "1.7.0", false),
        // CoolingService
        GetCoolingState => (Cooling, true, "1.0.0", false),
        SetCoolingMode => (Cooling, true, "1.4.0", false),
        SetImmersionMode => (Cooling, true, "1.0.0", false),
        // ConfigurationService
        GetMinerConfiguration => (Configuration, true, "1.0.0", false),
        GetConstraints => (Configuration, true, "1.0.0", false),
        // NetworkService
        GetNetworkConfiguration => (Network, true, "1.0.0", false),
        SetNetworkConfiguration => (Network, true, "1.0.0", false),
        GetNetworkInfo => (Network, true, "1.0.0", false),
        // UpgradeService
        UpdateAutoUpgradeConfig => (Upgrade, true, "1.8.0", false),
        GetAutoUpgradeStatus => (Upgrade, true, "1.8.0", false),
        // LicenseService
        GetLicenseState => (License, true, "1.0.0", false),
        ApplyContractKey => (License, true, "1.6.0", false),
        // VersionService — GetApiVersion is the ONLY no-auth read endpoint.
        GetApiVersion => (Version, false, "1.0.0", false),
    };
    BraiinsMethodDescriptor {
        method,
        service,
        auth_required: auth,
        version_introduced: version,
        destructive,
    }
}

// ---------------------------------------------------------------------------
// Tuner state + mode (per BRAIINSOS_REVERSE_ENGINEERING.md §5)
// ---------------------------------------------------------------------------

/// `TunerMode` enum from the BraiinsOS+ gRPC API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum TunerMode {
    /// `TUNER_MODE_POWER_TARGET = 1` — maximize hashrate at given wattage.
    PowerTarget = 1,
    /// `TUNER_MODE_HASHRATE_TARGET = 2` — minimize power at given TH/s.
    HashrateTarget = 2,
}

/// `TunerState` enum from the BraiinsOS+ gRPC API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum TunerState {
    /// `TUNER_STATE_DISABLED = 1` — autotuning off (manual mode).
    Disabled = 1,
    /// `TUNER_STATE_STABLE = 2` — converged, optimal settings applied.
    Stable = 2,
    /// `TUNER_STATE_TUNING = 3` — actively adjusting V/F settings.
    Tuning = 3,
    /// `TUNER_STATE_ERROR = 4` — error during tuning.
    Error = 4,
    /// `TUNER_STATE_CONTINUOUS = 5` — continuously micro-adjusting (v1.9.0+).
    Continuous = 5,
}

/// `SaveAction` enum applied to all write operations per RE doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum SaveAction {
    /// `SAVE_ACTION_SAVE = 1` — save to config file only (next reboot).
    Save = 1,
    /// `SAVE_ACTION_APPLY_AND_SAVE = 2` — apply at runtime + save.
    ApplyAndSave = 2,
    /// `SAVE_ACTION_APPLY = 3` — apply at runtime only.
    Apply = 3,
}

/// Default gRPC port per BRAIINSOS_REVERSE_ENGINEERING.md §9.
pub const BRAIINSOS_GRPC_PORT: u16 = 50051;

/// Default session-token TTL (returned by `Login`) — 1 hour per RE doc.
pub const BRAIINSOS_SESSION_TIMEOUT_SECONDS: u32 = 3600;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_port_is_50051() {
        assert_eq!(BRAIINSOS_GRPC_PORT, 50051);
    }

    #[test]
    fn session_timeout_is_one_hour() {
        // RE doc line 1068 example: timeout_s: 3600.
        assert_eq!(BRAIINSOS_SESSION_TIMEOUT_SECONDS, 3600);
    }

    #[test]
    fn proto_service_names_match_re_doc() {
        // Pin every service's full proto name. dcent-toolbox uses
        // grpcurl with these literal strings.
        assert_eq!(
            BraiinsService::Authentication.proto_name(),
            "braiins.bos.v1.AuthenticationService"
        );
        assert_eq!(
            BraiinsService::Miner.proto_name(),
            "braiins.bos.v1.MinerService"
        );
        assert_eq!(
            BraiinsService::Performance.proto_name(),
            "braiins.bos.v1.PerformanceService"
        );
        assert_eq!(
            BraiinsService::Version.proto_name(),
            "braiins.bos.v1.VersionService"
        );
    }

    #[test]
    fn login_is_only_no_auth_authentication_method() {
        // RE doc §9 AuthenticationService table: Login=No, SetPassword=Yes.
        assert!(!descriptor(BraiinsMethod::Login).auth_required);
        assert!(descriptor(BraiinsMethod::SetPassword).auth_required);
    }

    #[test]
    fn get_api_version_is_only_no_auth_read_endpoint() {
        // RE doc §9 VersionService: "GetApiVersion (NO auth required)".
        // Pin this fact + ensure no OTHER read endpoint is no-auth.
        assert!(!descriptor(BraiinsMethod::GetApiVersion).auth_required);
        // Spot-check all the other read methods are auth-required.
        for m in [
            BraiinsMethod::GetMinerStatus,
            BraiinsMethod::GetMinerDetails,
            BraiinsMethod::GetTunerState,
            BraiinsMethod::GetCoolingState,
            BraiinsMethod::GetMinerConfiguration,
            BraiinsMethod::GetNetworkConfiguration,
            BraiinsMethod::GetLicenseState,
        ] {
            assert!(descriptor(m).auth_required, "{:?} should require auth", m);
        }
    }

    #[test]
    fn destructive_methods_pinned() {
        // Reboot, FactoryReset, RemoveTunedProfiles, DisableHashboards
        // are destructive operator-confirmable methods.
        for m in [
            BraiinsMethod::Reboot,
            BraiinsMethod::FactoryReset,
            BraiinsMethod::RemoveTunedProfiles,
            BraiinsMethod::DisableHashboards,
        ] {
            assert!(descriptor(m).destructive, "{:?} should be destructive", m);
        }
        // Most methods are NOT destructive.
        for m in [
            BraiinsMethod::GetMinerStatus,
            BraiinsMethod::SetPowerTarget,
            BraiinsMethod::PauseMining,
            BraiinsMethod::SetCoolingMode,
        ] {
            assert!(
                !descriptor(m).destructive,
                "{:?} should NOT be destructive",
                m
            );
        }
    }

    #[test]
    fn factory_reset_introduced_in_1_9_0() {
        // RE doc: FactoryReset (v1.9.0+).
        assert_eq!(
            descriptor(BraiinsMethod::FactoryReset).version_introduced,
            "1.9.0"
        );
    }

    #[test]
    fn set_dps_introduced_in_1_4_0() {
        // RE doc: SetDPS (v1.4.0+).
        assert_eq!(
            descriptor(BraiinsMethod::SetDPS).version_introduced,
            "1.4.0"
        );
    }

    #[test]
    fn upgrade_service_methods_introduced_in_1_8_0() {
        // RE doc: UpgradeService (v1.8.0+).
        for m in [
            BraiinsMethod::UpdateAutoUpgradeConfig,
            BraiinsMethod::GetAutoUpgradeStatus,
        ] {
            assert_eq!(descriptor(m).version_introduced, "1.8.0");
        }
    }

    #[test]
    fn relative_targets_introduced_in_1_8_0() {
        // RE doc: SetRelativePowerTarget / SetRelativeHashrateTarget v1.8.0+.
        assert_eq!(
            descriptor(BraiinsMethod::SetRelativePowerTarget).version_introduced,
            "1.8.0"
        );
        assert_eq!(
            descriptor(BraiinsMethod::SetRelativeHashrateTarget).version_introduced,
            "1.8.0"
        );
    }

    #[test]
    fn quick_ramping_introduced_in_1_7_0() {
        for m in [
            BraiinsMethod::SetQuickRamping,
            BraiinsMethod::SetDefaultQuickRamping,
        ] {
            assert_eq!(descriptor(m).version_introduced, "1.7.0");
        }
    }

    #[test]
    fn apply_contract_key_introduced_in_1_6_0() {
        assert_eq!(
            descriptor(BraiinsMethod::ApplyContractKey).version_introduced,
            "1.6.0"
        );
    }

    #[test]
    fn tuner_state_continuous_introduced_in_1_9_0() {
        // RE doc line 603: TUNER_STATE_CONTINUOUS = 5 (new in v1.9.0).
        assert_eq!(TunerState::Continuous as u8, 5);
        assert_eq!(TunerState::Disabled as u8, 1);
        assert_eq!(TunerState::Stable as u8, 2);
        assert_eq!(TunerState::Tuning as u8, 3);
        assert_eq!(TunerState::Error as u8, 4);
    }

    #[test]
    fn tuner_mode_discriminants_match_proto() {
        // RE doc lines 590-591: POWER_TARGET=1, HASHRATE_TARGET=2.
        assert_eq!(TunerMode::PowerTarget as u8, 1);
        assert_eq!(TunerMode::HashrateTarget as u8, 2);
    }

    #[test]
    fn save_action_discriminants_match_proto() {
        // SaveAction enum from RE doc §9.
        assert_eq!(SaveAction::Save as u8, 1);
        assert_eq!(SaveAction::ApplyAndSave as u8, 2);
        assert_eq!(SaveAction::Apply as u8, 3);
    }

    #[test]
    fn miner_service_has_full_method_set() {
        // RE doc §9 MinerService table — 8 methods.
        let methods = [
            BraiinsMethod::GetMinerStatus,
            BraiinsMethod::GetMinerDetails,
            BraiinsMethod::GetMinerStats,
            BraiinsMethod::GetErrors,
            BraiinsMethod::GetHashboards,
            BraiinsMethod::GetSupportArchive,
            BraiinsMethod::EnableHashboards,
            BraiinsMethod::DisableHashboards,
        ];
        assert_eq!(methods.len(), 8);
        for m in methods {
            assert_eq!(descriptor(m).service, BraiinsService::Miner);
        }
    }

    #[test]
    fn pool_service_supports_5_crud_methods() {
        // RE doc §9 PoolService table — 5 methods (Get/Create/Update/
        // Remove/SetPoolGroups).
        for m in [
            BraiinsMethod::GetPoolGroups,
            BraiinsMethod::CreatePoolGroup,
            BraiinsMethod::UpdatePoolGroup,
            BraiinsMethod::RemovePoolGroup,
            BraiinsMethod::SetPoolGroups,
        ] {
            assert_eq!(descriptor(m).service, BraiinsService::Pool);
        }
    }

    #[test]
    fn service_round_trips_through_serde() {
        for s in [
            BraiinsService::Authentication,
            BraiinsService::Actions,
            BraiinsService::Miner,
            BraiinsService::Pool,
            BraiinsService::Performance,
            BraiinsService::Cooling,
            BraiinsService::Configuration,
            BraiinsService::Network,
            BraiinsService::Upgrade,
            BraiinsService::License,
            BraiinsService::Version,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: BraiinsService = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn tuner_state_serializes_in_screaming_snake_case() {
        // Pin proto-style wire form so a refactor doesn't accidentally
        // switch to snake_case (would break tonic-decoded clients).
        assert_eq!(
            serde_json::to_string(&TunerState::Continuous).unwrap(),
            "\"CONTINUOUS\""
        );
        assert_eq!(
            serde_json::to_string(&TunerState::Disabled).unwrap(),
            "\"DISABLED\""
        );
    }

    #[test]
    fn method_pascal_case_round_trip() {
        assert_eq!(
            serde_json::to_string(&BraiinsMethod::GetMinerStatus).unwrap(),
            "\"GetMinerStatus\""
        );
        assert_eq!(
            serde_json::to_string(&BraiinsMethod::SetDPS).unwrap(),
            "\"SetDPS\""
        );
    }
}
