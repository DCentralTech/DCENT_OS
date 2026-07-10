use serde::{Deserialize, Serialize};

pub const CAPABILITY_SCHEMA_VERSION: u16 = 2;

pub const RUNTIME_CAPABILITY_VALUES: &[&str] = &[
    "detect",
    "inventory",
    "monitoring",
    "pools-read",
    "pools-rw",
    "config-read",
    "config-rw",
    "reboot",
    "logs-read",
    "backup",
    "restore",
    "flash-ota",
    "flash-otawww",
    "nvs-gen",
    "settings-patch",
    "asic-options",
    "statistics",
    "identify",
    "wifi-scan",
    "power-control",
];

pub const INSTALL_CAPABILITY_VALUES: &[&str] = &[
    "auth-ssh",
    "root-ssh",
    "persistent-install",
    "runtime-install",
    "restore-verified",
    "recovery-staged",
    "backup",
    "physical-sd-card",
    "luxos-uninstall",
    "vnish-cgminer-detected",
    "cv1835-emmc-proven",
    "bcb100-accept-unverified",
    "http-ota-or-usb-serial",
    "manifest-board-match",
    "stock-bmu-root-flasher",
];

pub const PLANNER_OUTCOME_VALUES: &[&str] =
    &["supported", "ota-supported", "runtime-only", "evidence-gap"];

pub const PROOF_SCOPE_VALUES: &[&str] = &[
    "exact_target",
    "exact_target_lab_only",
    "passthrough_only",
    "physical_media_required",
    "upload_only_boot_pending",
];

pub const READ_ONLY_RUNTIME_CAPABILITIES: &[RuntimeCapability] = &[
    RuntimeCapability::Detect,
    RuntimeCapability::Inventory,
    RuntimeCapability::Monitoring,
    RuntimeCapability::PoolsRead,
    RuntimeCapability::ConfigRead,
    RuntimeCapability::LogsRead,
    RuntimeCapability::Statistics,
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuntimeCapability {
    #[serde(rename = "detect")]
    Detect,
    #[serde(rename = "inventory")]
    Inventory,
    #[serde(rename = "monitoring")]
    Monitoring,
    #[serde(rename = "pools-read")]
    PoolsRead,
    #[serde(rename = "pools-rw")]
    PoolsRw,
    #[serde(rename = "config-read")]
    ConfigRead,
    #[serde(rename = "config-rw")]
    ConfigRw,
    #[serde(rename = "reboot")]
    Reboot,
    #[serde(rename = "logs-read")]
    LogsRead,
    #[serde(rename = "backup")]
    Backup,
    #[serde(rename = "restore")]
    Restore,
    #[serde(rename = "flash-ota")]
    FlashOta,
    #[serde(rename = "flash-otawww")]
    FlashOtaWww,
    #[serde(rename = "nvs-gen")]
    NvsGen,
    #[serde(rename = "settings-patch")]
    SettingsPatch,
    #[serde(rename = "asic-options")]
    AsicOptions,
    #[serde(rename = "statistics")]
    Statistics,
    #[serde(rename = "identify")]
    Identify,
    #[serde(rename = "wifi-scan")]
    WifiScan,
    #[serde(rename = "power-control")]
    PowerControl,
}

impl RuntimeCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detect => "detect",
            Self::Inventory => "inventory",
            Self::Monitoring => "monitoring",
            Self::PoolsRead => "pools-read",
            Self::PoolsRw => "pools-rw",
            Self::ConfigRead => "config-read",
            Self::ConfigRw => "config-rw",
            Self::Reboot => "reboot",
            Self::LogsRead => "logs-read",
            Self::Backup => "backup",
            Self::Restore => "restore",
            Self::FlashOta => "flash-ota",
            Self::FlashOtaWww => "flash-otawww",
            Self::NvsGen => "nvs-gen",
            Self::SettingsPatch => "settings-patch",
            Self::AsicOptions => "asic-options",
            Self::Statistics => "statistics",
            Self::Identify => "identify",
            Self::WifiScan => "wifi-scan",
            Self::PowerControl => "power-control",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallCapability {
    #[serde(rename = "auth-ssh")]
    AuthSsh,
    #[serde(rename = "root-ssh")]
    RootSsh,
    #[serde(rename = "persistent-install")]
    PersistentInstall,
    #[serde(rename = "runtime-install")]
    RuntimeInstall,
    #[serde(rename = "restore-verified")]
    RestoreVerified,
    #[serde(rename = "recovery-staged")]
    RecoveryStaged,
    #[serde(rename = "backup")]
    Backup,
    #[serde(rename = "physical-sd-card")]
    PhysicalSdCard,
    #[serde(rename = "luxos-uninstall")]
    LuxosUninstall,
    #[serde(rename = "vnish-cgminer-detected")]
    VnishCgminerDetected,
    #[serde(rename = "cv1835-emmc-proven")]
    Cv1835EmmcProven,
    #[serde(rename = "bcb100-accept-unverified")]
    Bcb100AcceptUnverified,
    #[serde(rename = "http-ota-or-usb-serial")]
    HttpOtaOrUsbSerial,
    #[serde(rename = "manifest-board-match")]
    ManifestBoardMatch,
    #[serde(rename = "stock-bmu-root-flasher")]
    StockBmuRootFlasher,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlannerOutcome {
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "ota-supported")]
    OtaSupported,
    #[serde(rename = "runtime-only")]
    RuntimeOnly,
    #[serde(rename = "evidence-gap")]
    EvidenceGap,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProofScope {
    #[serde(rename = "exact_target")]
    ExactTarget,
    #[serde(rename = "exact_target_lab_only")]
    ExactTargetLabOnly,
    #[serde(rename = "passthrough_only")]
    PassthroughOnly,
    #[serde(rename = "physical_media_required")]
    PhysicalMediaRequired,
    #[serde(rename = "upload_only_boot_pending")]
    UploadOnlyBootPending,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceFamily {
    Antminer,
    Esp,
    Whatsminer,
    Avalon,
    Innosilicon,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SupportTier {
    Stable,
    Beta,
    Experimental,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AsicFamily {
    BitmainBm13xx,
    MicrobtKSeries,
    CanaanA31xx,
    InnosiliconGn,
    EspBitaxe,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FanControlMode {
    None,
    FixedPwm,
    PwmAndTach,
    Am2C49Pwm,
    Am2C52Pwm,
    Emc2101,
    Emc2103,
    Emc2302,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TempSensorClass {
    Die,
    BoardI2c,
    Lm75,
    Tmp42x,
    Xadc,
    VrController,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PsuMode {
    Bypass,
    AutoDetect,
    PmbusMonitor,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ControllerKind {
    Pic16f1704,
    Dspic33ep,
    TasNoPic,
    Eeprom,
    Psu,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityConfidence {
    Exact,
    High,
    Low,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HardwareIdentity {
    pub confidence: IdentityConfidence,
    pub sources: Vec<String>,
    pub note: Option<String>,
    #[serde(alias = "device_model")]
    pub device_model: Option<String>,
    #[serde(alias = "board_target")]
    pub board_target: Option<String>,
    #[serde(alias = "board_version")]
    pub board_version: Option<String>,
    pub platform: Option<String>,
}

impl HardwareIdentity {
    pub fn unknown(note: impl Into<String>) -> Self {
        Self {
            confidence: IdentityConfidence::Unknown,
            sources: Vec::new(),
            note: Some(note.into()),
            device_model: None,
            board_target: None,
            board_version: None,
            platform: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BoardCapability {
    #[serde(alias = "board_target")]
    pub board_target: Option<String>,
    pub family: Option<String>,
    #[serde(alias = "control_board")]
    pub control_board: Option<String>,
    #[serde(default, alias = "fixture_refs")]
    pub fixture_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ControlBoardCapability {
    pub soc: Option<String>,
    #[serde(alias = "control_board_id")]
    pub control_board_id: Option<String>,
    #[serde(alias = "uio_model")]
    pub uio_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsicCapability {
    #[serde(alias = "chip_model")]
    pub chip_model: Option<String>,
    #[serde(default, alias = "asic_family")]
    pub asic_family: AsicFamily,
    #[serde(alias = "chip_id")]
    pub chip_id: Option<u16>,
    pub baud: Option<u32>,
    #[serde(alias = "cores_per_chip")]
    pub cores_per_chip: Option<u32>,
    #[serde(alias = "nonce_attribution_cores")]
    pub nonce_attribution_cores: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HashboardDescriptor {
    pub index: Option<u8>,
    #[serde(alias = "chain_index")]
    pub chain_index: Option<u8>,
    #[serde(alias = "chip_model")]
    pub chip_model: Option<String>,
    #[serde(default, alias = "asic_family")]
    pub asic_family: AsicFamily,
    #[serde(alias = "chip_id")]
    pub chip_id: Option<u16>,
    #[serde(alias = "chips_per_chain")]
    pub chips_per_chain: Option<u16>,
    pub present: Option<bool>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TopologyCapability {
    #[serde(alias = "chain_count")]
    pub chain_count: Option<u8>,
    #[serde(alias = "chips_per_chain")]
    pub chips_per_chain: Option<u16>,
    #[serde(alias = "fan_count")]
    pub fan_count: Option<u8>,
    #[serde(default, alias = "temp_sensors")]
    pub temp_sensors: Vec<String>,
    #[serde(default)]
    pub hashboards: Vec<HashboardDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FanDescriptor {
    pub index: Option<u8>,
    #[serde(alias = "tach_channel")]
    pub tach_channel: Option<u8>,
    #[serde(alias = "pwm_channel")]
    pub pwm_channel: Option<u8>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FanTopology {
    #[serde(default, alias = "control_mode")]
    pub control_mode: FanControlMode,
    #[serde(alias = "fan_count")]
    pub fan_count: Option<u8>,
    #[serde(default, alias = "tach_channels")]
    pub tach_channels: Vec<u8>,
    #[serde(default, alias = "pwm_channels")]
    pub pwm_channels: Vec<u8>,
    #[serde(default, alias = "per_fan")]
    pub per_fan: Vec<FanDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TempSensorDescriptor {
    #[serde(default)]
    pub class: TempSensorClass,
    pub name: Option<String>,
    pub bus: Option<String>,
    pub address: Option<u8>,
    pub index: Option<u8>,
    #[serde(alias = "fallback_order")]
    pub fallback_order: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThermalCapability {
    #[serde(default, alias = "runtime_caps")]
    pub runtime_caps: Vec<RuntimeCapability>,
    #[serde(alias = "fail_closed_on_sensor_loss")]
    pub fail_closed_on_sensor_loss: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PowerCapability {
    #[serde(default, alias = "runtime_caps")]
    pub runtime_caps: Vec<RuntimeCapability>,
    #[serde(alias = "voltage_control")]
    pub voltage_control: Option<String>,
    #[serde(alias = "psu_protocol")]
    pub psu_protocol: Option<String>,
    #[serde(default, alias = "psu_mode")]
    pub psu_mode: PsuMode,
    #[serde(alias = "psu_model")]
    pub psu_model: Option<String>,
    #[serde(alias = "writes_enabled")]
    pub writes_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ControllerCapability {
    #[serde(default)]
    pub kind: ControllerKind,
    #[serde(alias = "fw_version")]
    pub fw_version: Option<String>,
    #[serde(default, alias = "write_denied_addrs")]
    pub write_denied_addrs: Vec<u8>,
    #[serde(default, alias = "degraded_fw_refuse")]
    pub degraded_fw_refuse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FrequencyEnvelope {
    #[serde(alias = "min_mhz")]
    pub min_mhz: Option<u16>,
    #[serde(alias = "max_mhz")]
    pub max_mhz: Option<u16>,
    #[serde(alias = "step_mhz")]
    pub step_mhz: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct VoltageEnvelope {
    #[serde(alias = "min_mv")]
    pub min_mv: Option<u16>,
    #[serde(alias = "max_mv")]
    pub max_mv: Option<u16>,
    #[serde(alias = "step_mv")]
    pub step_mv: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FanEnvelope {
    #[serde(alias = "min_pwm")]
    pub min_pwm: Option<u8>,
    #[serde(alias = "max_pwm")]
    pub max_pwm: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperatingEnvelopes {
    pub frequency: Option<FrequencyEnvelope>,
    pub voltage: Option<VoltageEnvelope>,
    pub fan: Option<FanEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReferences {
    #[serde(default, alias = "fixture_refs")]
    pub fixture_refs: Vec<String>,
    #[serde(alias = "sim_profile_ref")]
    pub sim_profile_ref: Option<String>,
    #[serde(alias = "bench_checklist_ref")]
    pub bench_checklist_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstallCapabilityPlan {
    #[serde(alias = "planner_outcome")]
    pub planner_outcome: PlannerOutcome,
    #[serde(alias = "proof_scope")]
    pub proof_scope: Option<ProofScope>,
    #[serde(default, alias = "required_capabilities")]
    pub required_capabilities: Vec<InstallCapability>,
    #[serde(default, alias = "missing_capabilities")]
    pub missing_capabilities: Vec<InstallCapability>,
    #[serde(alias = "recovery_route_id")]
    pub recovery_route_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SafeDefaults {
    #[serde(alias = "mining_enabled")]
    pub mining_enabled: bool,
    #[serde(alias = "fan_pwm_cap")]
    pub fan_pwm_cap: u8,
    #[serde(alias = "frequency_mhz")]
    pub frequency_mhz: Option<u16>,
    #[serde(alias = "voltage_mv")]
    pub voltage_mv: Option<u16>,
}

impl SafeDefaults {
    pub fn unknown_read_only() -> Self {
        Self {
            mining_enabled: false,
            fan_pwm_cap: 30,
            frequency_mhz: None,
            voltage_mv: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FailSafePolicy {
    #[serde(alias = "read_only")]
    pub read_only: bool,
    #[serde(alias = "mining_start_allowed")]
    pub mining_start_allowed: bool,
    #[serde(alias = "mutating_routes_allowed")]
    pub mutating_routes_allowed: bool,
    pub reason: String,
}

impl FailSafePolicy {
    pub fn evidence_gap(reason: impl Into<String>) -> Self {
        Self {
            read_only: true,
            mining_start_allowed: false,
            mutating_routes_allowed: false,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCapabilityDescriptor {
    #[serde(alias = "schema_version")]
    pub schema_version: u16,
    pub family: DeviceFamily,
    pub identity: HardwareIdentity,
    pub support: SupportTier,
    pub board: BoardCapability,
    #[serde(default, alias = "control_board")]
    pub control_board: ControlBoardCapability,
    pub asic: AsicCapability,
    pub topology: TopologyCapability,
    #[serde(default, alias = "fan_topology")]
    pub fan_topology: FanTopology,
    #[serde(default, alias = "temp_sensors")]
    pub temp_sensors: Vec<TempSensorDescriptor>,
    pub thermal: ThermalCapability,
    pub power: PowerCapability,
    #[serde(default)]
    pub controllers: Vec<ControllerCapability>,
    #[serde(default, alias = "operating_envelopes")]
    pub operating_envelopes: OperatingEnvelopes,
    #[serde(default)]
    pub references: CapabilityReferences,
    #[serde(default, alias = "runtime_caps")]
    pub runtime_caps: Vec<RuntimeCapability>,
    pub install: InstallCapabilityPlan,
    #[serde(alias = "safe_defaults")]
    pub safe_defaults: SafeDefaults,
    #[serde(alias = "fail_safe")]
    pub fail_safe: FailSafePolicy,
}

impl DeviceCapabilityDescriptor {
    pub fn unknown(family: DeviceFamily, note: impl Into<String>) -> Self {
        let note = note.into();
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            family,
            identity: HardwareIdentity::unknown(note.clone()),
            support: SupportTier::Unknown,
            board: BoardCapability {
                board_target: None,
                family: None,
                control_board: None,
                fixture_refs: Vec::new(),
            },
            control_board: ControlBoardCapability::default(),
            asic: AsicCapability {
                chip_model: None,
                asic_family: AsicFamily::Unknown,
                chip_id: None,
                baud: None,
                cores_per_chip: None,
                nonce_attribution_cores: None,
            },
            topology: TopologyCapability {
                chain_count: None,
                chips_per_chain: None,
                fan_count: None,
                temp_sensors: Vec::new(),
                hashboards: Vec::new(),
            },
            fan_topology: FanTopology::default(),
            temp_sensors: Vec::new(),
            thermal: ThermalCapability {
                runtime_caps: Vec::new(),
                fail_closed_on_sensor_loss: true,
            },
            power: PowerCapability {
                runtime_caps: Vec::new(),
                voltage_control: None,
                psu_protocol: None,
                psu_mode: PsuMode::Unknown,
                psu_model: None,
                writes_enabled: false,
            },
            controllers: Vec::new(),
            operating_envelopes: OperatingEnvelopes::default(),
            references: CapabilityReferences::default(),
            runtime_caps: READ_ONLY_RUNTIME_CAPABILITIES.to_vec(),
            install: InstallCapabilityPlan {
                planner_outcome: PlannerOutcome::EvidenceGap,
                proof_scope: None,
                required_capabilities: Vec::new(),
                missing_capabilities: Vec::new(),
                recovery_route_id: None,
                note: Some(note.clone()),
            },
            safe_defaults: SafeDefaults::unknown_read_only(),
            fail_safe: FailSafePolicy::evidence_gap(note),
        }
    }
}

pub type HardwareCapabilityDescriptor = DeviceCapabilityDescriptor;

pub trait CapabilityProvider {
    fn capability_descriptor(&self) -> HardwareCapabilityDescriptor;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityErrorKind {
    Unsupported,
    Conflict,
    UnknownHardware,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityError {
    #[serde(alias = "schema_version")]
    pub schema_version: u16,
    pub kind: CapabilityErrorKind,
    pub capability: Option<RuntimeCapability>,
    #[serde(alias = "http_status")]
    pub http_status: u16,
    pub message: String,
}

impl CapabilityError {
    pub fn unsupported(capability: RuntimeCapability, message: impl Into<String>) -> Self {
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            kind: CapabilityErrorKind::Unsupported,
            capability: Some(capability),
            http_status: 501,
            message: message.into(),
        }
    }

    pub fn conflict(capability: RuntimeCapability, message: impl Into<String>) -> Self {
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            kind: CapabilityErrorKind::Conflict,
            capability: Some(capability),
            http_status: 409,
            message: message.into(),
        }
    }

    pub fn unknown_hardware(message: impl Into<String>) -> Self {
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            kind: CapabilityErrorKind::UnknownHardware,
            capability: None,
            http_status: 409,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_capability_values_match_serde_strings() {
        let caps = [
            RuntimeCapability::Detect,
            RuntimeCapability::Inventory,
            RuntimeCapability::Monitoring,
            RuntimeCapability::PoolsRead,
            RuntimeCapability::PoolsRw,
            RuntimeCapability::ConfigRead,
            RuntimeCapability::ConfigRw,
            RuntimeCapability::Reboot,
            RuntimeCapability::LogsRead,
            RuntimeCapability::Backup,
            RuntimeCapability::Restore,
            RuntimeCapability::FlashOta,
            RuntimeCapability::FlashOtaWww,
            RuntimeCapability::NvsGen,
            RuntimeCapability::SettingsPatch,
            RuntimeCapability::AsicOptions,
            RuntimeCapability::Statistics,
            RuntimeCapability::Identify,
            RuntimeCapability::WifiScan,
            RuntimeCapability::PowerControl,
        ];
        assert_eq!(caps.len(), RUNTIME_CAPABILITY_VALUES.len());
        for (cap, expected) in caps.into_iter().zip(RUNTIME_CAPABILITY_VALUES) {
            assert_eq!(cap.as_str(), *expected);
            let json = serde_json::to_string(&cap).expect("serialize runtime cap");
            assert_eq!(json, format!("\"{expected}\""));
            let roundtrip: RuntimeCapability =
                serde_json::from_str(&json).expect("deserialize runtime cap");
            assert_eq!(roundtrip, cap);
        }
    }

    #[test]
    fn unknown_descriptor_is_read_only_and_fails_safe() {
        let descriptor =
            DeviceCapabilityDescriptor::unknown(DeviceFamily::Antminer, "chip identity unknown");
        assert_eq!(descriptor.schema_version, CAPABILITY_SCHEMA_VERSION);
        assert_eq!(CAPABILITY_SCHEMA_VERSION, 2);
        assert_eq!(descriptor.support, SupportTier::Unknown);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Unknown);
        assert_eq!(descriptor.asic.chip_model, None);
        assert_eq!(descriptor.asic.asic_family, AsicFamily::Unknown);
        assert_eq!(descriptor.safe_defaults.fan_pwm_cap, 30);
        assert!(!descriptor.safe_defaults.mining_enabled);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor.fail_safe.mining_start_allowed);
        assert!(!descriptor.fail_safe.mutating_routes_allowed);
        assert!(!descriptor.power.writes_enabled);
        assert_eq!(descriptor.power.psu_mode, PsuMode::Unknown);
        assert!(descriptor.controllers.is_empty());
        assert!(descriptor.control_board.soc.is_none());
        assert!(descriptor.topology.hashboards.is_empty());
        assert_eq!(
            descriptor.install.planner_outcome,
            PlannerOutcome::EvidenceGap
        );
        assert!(descriptor
            .runtime_caps
            .iter()
            .all(|cap| READ_ONLY_RUNTIME_CAPABILITIES.contains(cap)));
    }

    #[test]
    fn hardware_capability_descriptor_alias_preserves_wire_name() {
        let descriptor =
            DeviceCapabilityDescriptor::unknown(DeviceFamily::Unknown, "alias compile check");
        let alias: HardwareCapabilityDescriptor = descriptor.clone();
        assert_eq!(alias, descriptor);
    }

    #[test]
    fn v1_descriptor_json_parses_with_v2_defaults() {
        let mut value = serde_json::to_value(DeviceCapabilityDescriptor::unknown(
            DeviceFamily::Antminer,
            "v1 parse check",
        ))
        .expect("serialize descriptor");
        let obj = value.as_object_mut().expect("descriptor object");
        obj.insert("schemaVersion".to_string(), serde_json::json!(1));
        for key in [
            "controlBoard",
            "fanTopology",
            "tempSensors",
            "controllers",
            "operatingEnvelopes",
            "references",
        ] {
            obj.remove(key);
        }
        obj.get_mut("asic")
            .and_then(|asic| asic.as_object_mut())
            .expect("asic object")
            .remove("asicFamily");
        obj.get_mut("topology")
            .and_then(|topology| topology.as_object_mut())
            .expect("topology object")
            .remove("hashboards");
        let power = obj
            .get_mut("power")
            .and_then(|power| power.as_object_mut())
            .expect("power object");
        power.remove("psuMode");
        power.remove("psuModel");
        obj.get_mut("install")
            .and_then(|install| install.as_object_mut())
            .expect("install object")
            .remove("recoveryRouteId");

        let parsed: DeviceCapabilityDescriptor =
            serde_json::from_value(value).expect("v1-shaped JSON parses under v2");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.asic.asic_family, AsicFamily::Unknown);
        assert_eq!(parsed.power.psu_mode, PsuMode::Unknown);
        assert!(parsed.topology.hashboards.is_empty());
        assert!(parsed.controllers.is_empty());
        assert!(parsed.references.fixture_refs.is_empty());
        assert_eq!(parsed.safe_defaults.fan_pwm_cap, 30);
        assert!(!parsed.safe_defaults.mining_enabled);
        assert!(parsed.fail_safe.read_only);
    }

    #[test]
    fn v2_field_wire_names_are_camel_case() {
        let descriptor =
            DeviceCapabilityDescriptor::unknown(DeviceFamily::Unknown, "wire key check");
        let value = serde_json::to_value(descriptor).expect("serialize descriptor");
        let obj = value.as_object().expect("descriptor object");
        for key in [
            "controlBoard",
            "fanTopology",
            "tempSensors",
            "operatingEnvelopes",
            "safeDefaults",
            "failSafe",
        ] {
            assert!(obj.contains_key(key), "missing top-level key {key}");
        }
        assert!(obj["power"].as_object().unwrap().contains_key("psuMode"));
        assert!(obj["power"].as_object().unwrap().contains_key("psuModel"));
        assert!(obj["install"]
            .as_object()
            .unwrap()
            .contains_key("recoveryRouteId"));
    }

    #[test]
    fn runtime_and_install_capability_axes_remain_separate() {
        let runtime_backup =
            serde_json::to_string(&RuntimeCapability::Backup).expect("serialize runtime backup");
        let install_backup =
            serde_json::to_string(&InstallCapability::Backup).expect("serialize install backup");
        assert_eq!(runtime_backup, install_backup);
        assert_ne!(
            RUNTIME_CAPABILITY_VALUES.len(),
            INSTALL_CAPABILITY_VALUES.len(),
            "the two capability axes overlap on backup only; do not collapse them"
        );
    }

    #[test]
    fn capability_error_statuses_are_pinned() {
        let unsupported =
            CapabilityError::unsupported(RuntimeCapability::FlashOta, "OTA not supported");
        assert_eq!(unsupported.http_status, 501);
        assert_eq!(unsupported.kind, CapabilityErrorKind::Unsupported);
        let conflict = CapabilityError::conflict(RuntimeCapability::ConfigRw, "read-only mode");
        assert_eq!(conflict.http_status, 409);
        assert_eq!(conflict.kind, CapabilityErrorKind::Conflict);
        let unknown = CapabilityError::unknown_hardware("unknown chip");
        assert_eq!(unknown.http_status, 409);
        assert_eq!(unknown.kind, CapabilityErrorKind::UnknownHardware);
        assert_eq!(unknown.capability, None);
    }
}
