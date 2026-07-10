use dcent_schema::capability::{
    AsicCapability, AsicFamily, BoardCapability, CapabilityReferences, ControlBoardCapability,
    DeviceCapabilityDescriptor, DeviceFamily, FailSafePolicy, FanControlMode, FanDescriptor,
    FanEnvelope, FanTopology, FrequencyEnvelope, HardwareIdentity, HashboardDescriptor,
    IdentityConfidence, InstallCapability, InstallCapabilityPlan, OperatingEnvelopes,
    PlannerOutcome, PowerCapability, ProofScope, PsuMode, RuntimeCapability, SafeDefaults,
    SupportTier, TempSensorClass, TempSensorDescriptor, ThermalCapability, TopologyCapability,
    VoltageEnvelope, CAPABILITY_SCHEMA_VERSION, READ_ONLY_RUNTIME_CAPABILITIES,
};
use dcentaxe_hal::board::{FanControllerKind, PowerControllerKind, TempSensorKind};

use crate::config::DcentAxeConfig;

pub fn build_esp_capability_descriptor(
    config: &DcentAxeConfig,
    mining_enabled: bool,
    min_frequency_mhz: f32,
    max_frequency_mhz: f32,
    min_voltage_mv: u16,
    max_voltage_mv: u16,
) -> DeviceCapabilityDescriptor {
    let board = config.board_config();
    let recognized =
        config.board_identity_recognized() && config.board_identity_family_consistent();

    if !recognized {
        return DeviceCapabilityDescriptor::unknown(
            DeviceFamily::Esp,
            "ESP board identity is unknown or inconsistent; runtime is read-only until board evidence is fixed",
        );
    }

    let mut runtime_caps = READ_ONLY_RUNTIME_CAPABILITIES.to_vec();
    runtime_caps.extend_from_slice(&[
        RuntimeCapability::PoolsRw,
        RuntimeCapability::ConfigRw,
        RuntimeCapability::Reboot,
        RuntimeCapability::Backup,
        RuntimeCapability::FlashOta,
        RuntimeCapability::FlashOtaWww,
        RuntimeCapability::SettingsPatch,
        RuntimeCapability::AsicOptions,
        RuntimeCapability::Identify,
        RuntimeCapability::WifiScan,
    ]);
    if board.model.has_voltage_control() {
        runtime_caps.push(RuntimeCapability::PowerControl);
    }

    let support = match config.support_status() {
        // ESP board-registry "supported" means the board profile is known and
        // host/live development has progressed. At the DCENT_OS multi-family
        // tier level it is still experimental until install/soak evidence is
        // promoted through the shared support matrix.
        "supported" | "experimental" => SupportTier::Experimental,
        "unknown" => SupportTier::Unknown,
        _ => SupportTier::Unsupported,
    };

    DeviceCapabilityDescriptor {
        schema_version: CAPABILITY_SCHEMA_VERSION,
        family: DeviceFamily::Esp,
        identity: HardwareIdentity {
            confidence: if !config.board_version.trim().is_empty() {
                IdentityConfidence::Exact
            } else {
                IdentityConfidence::High
            },
            sources: identity_sources(config),
            note: Some(format!(
                "ESP board registry reports '{}'; shared DCENT_OS tier remains {:?}",
                config.support_status(),
                support
            )),
            device_model: Some(board.device_model.clone()),
            board_target: Some(board.model.board_target().to_string()),
            board_version: Some(board.board_version.clone()),
            platform: Some("esp32-s3".to_string()),
        },
        support,
        board: BoardCapability {
            board_target: Some(board.model.board_target().to_string()),
            family: Some("esp-bitaxe".to_string()),
            control_board: Some(board.model.name().to_string()),
            fixture_refs: vec![
                "DCENT_OS_ESP/dcentaxe-hal/src/board.rs".to_string(),
                "DCENT_OS_ESP/dcentaxe/src/config.rs".to_string(),
            ],
        },
        control_board: ControlBoardCapability {
            soc: Some("esp32-s3".to_string()),
            control_board_id: Some(board.board_version.clone()),
            uio_model: None,
        },
        asic: AsicCapability {
            chip_model: Some(board.asic_model.clone()),
            asic_family: AsicFamily::BitmainBm13xx,
            chip_id: Some(board.model.expected_chip_id()),
            baud: Some(esp_runtime_baud(&board.asic_model)),
            cores_per_chip: Some(cores_per_chip(&board.asic_model)),
            nonce_attribution_cores: Some(nonce_attribution_cores(&board.asic_model)),
        },
        topology: TopologyCapability {
            chain_count: Some(1),
            chips_per_chain: Some(board.asic_count as u16),
            fan_count: Some(fan_count(board.fan_controller)),
            temp_sensors: temp_sensors(board.temp_sensor, board.power_controller),
            hashboards: vec![HashboardDescriptor {
                index: Some(0),
                chain_index: Some(0),
                chip_model: Some(board.asic_model.clone()),
                asic_family: AsicFamily::BitmainBm13xx,
                chip_id: Some(board.model.expected_chip_id()),
                chips_per_chain: Some(board.asic_count as u16),
                present: Some(board.mining_capable()),
                serial: None,
            }],
        },
        fan_topology: esp_fan_topology(board.fan_controller),
        temp_sensors: esp_temp_sensor_descriptors(board.temp_sensor, board.power_controller),
        thermal: ThermalCapability {
            runtime_caps: vec![RuntimeCapability::Monitoring],
            fail_closed_on_sensor_loss: board.temp_sensor != TempSensorKind::None,
        },
        power: PowerCapability {
            runtime_caps: if board.model.has_voltage_control() {
                vec![RuntimeCapability::PowerControl]
            } else {
                Vec::new()
            },
            voltage_control: Some(power_controller_label(board.power_controller).to_string()),
            psu_protocol: Some("board-regulator".to_string()),
            psu_mode: PsuMode::AutoDetect,
            psu_model: Some(power_controller_label(board.power_controller).to_string()),
            writes_enabled: board.model.has_voltage_control(),
        },
        controllers: Vec::new(),
        operating_envelopes: OperatingEnvelopes {
            frequency: Some(FrequencyEnvelope {
                min_mhz: Some(clamp_f32_to_u16(min_frequency_mhz, min_frequency_mhz, max_frequency_mhz)),
                max_mhz: Some(clamp_f32_to_u16(max_frequency_mhz, min_frequency_mhz, max_frequency_mhz)),
                step_mhz: None,
            }),
            voltage: Some(VoltageEnvelope {
                min_mv: Some(min_voltage_mv),
                max_mv: Some(max_voltage_mv),
                step_mv: None,
            }),
            fan: Some(FanEnvelope {
                min_pwm: Some(0),
                max_pwm: Some(100),
            }),
        },
        references: CapabilityReferences {
            fixture_refs: vec![
                "DCENT_OS_ESP/dcentaxe-hal/src/board.rs".to_string(),
                "DCENT_OS_ESP/".to_string(),
            ],
            sim_profile_ref: None,
            bench_checklist_ref: Some("BP-ESP-BOARD-SOAK".to_string()),
        },
        runtime_caps,
        install: InstallCapabilityPlan {
            planner_outcome: PlannerOutcome::OtaSupported,
            proof_scope: Some(ProofScope::UploadOnlyBootPending),
            required_capabilities: vec![
                InstallCapability::HttpOtaOrUsbSerial,
                InstallCapability::ManifestBoardMatch,
            ],
            missing_capabilities: vec![InstallCapability::RestoreVerified],
            recovery_route_id: Some("esp-ota-or-usb-serial".to_string()),
            note: Some(
                "ESP supports signed OTA/upload surfaces; boot/rollback proof remains operator-gated"
                    .to_string(),
            ),
        },
        safe_defaults: SafeDefaults {
            mining_enabled,
            fan_pwm_cap: 100,
            frequency_mhz: Some(
                clamp_f32_to_u16(board.default_frequency, min_frequency_mhz, max_frequency_mhz),
            ),
            voltage_mv: Some(board.default_voltage_mv.clamp(min_voltage_mv, max_voltage_mv)),
        },
        fail_safe: FailSafePolicy {
            read_only: false,
            mining_start_allowed: board.mining_capable(),
            mutating_routes_allowed: true,
            reason: "recognized ESP board profile; mutating routes still enforce their own auth and safety clamps"
                .to_string(),
        },
    }
}

fn identity_sources(config: &DcentAxeConfig) -> Vec<String> {
    let mut sources = Vec::new();
    if !config.board_version.trim().is_empty() {
        sources.push(format!(
            "config.board_version:{}",
            config.board_version.trim()
        ));
    }
    if !config.board_model.trim().is_empty() {
        sources.push(format!("config.board_model:{}", config.board_model.trim()));
    }
    if !config.asic_model.trim().is_empty() {
        sources.push(format!("config.asic_model:{}", config.asic_model.trim()));
    }
    sources
}

fn esp_runtime_baud(chip_model: &str) -> u32 {
    match chip_model.trim() {
        "BM1397" => 3_125_000,
        "BM1366" | "BM1368" | "BM1370" => 1_000_000,
        _ => 115_200,
    }
}

fn cores_per_chip(chip_model: &str) -> u32 {
    match chip_model.trim() {
        "BM1397" => 168,
        "BM1366" => 112,
        "BM1368" => 80,
        "BM1370" => 128,
        _ => 0,
    }
}

fn nonce_attribution_cores(chip_model: &str) -> u32 {
    match chip_model.trim() {
        "BM1397" => 672,
        "BM1366" => 894,
        "BM1368" => 1276,
        "BM1370" => 2040,
        _ => 0,
    }
}

fn fan_count(kind: FanControllerKind) -> u8 {
    match kind {
        FanControllerKind::None => 0,
        FanControllerKind::Emc2101 | FanControllerKind::Emc2103 => 1,
        FanControllerKind::Emc2302 => 2,
    }
}

fn esp_fan_topology(kind: FanControllerKind) -> FanTopology {
    let count = fan_count(kind);
    let per_fan: Vec<FanDescriptor> = (0..count)
        .map(|index| FanDescriptor {
            index: Some(index),
            tach_channel: Some(index),
            pwm_channel: Some(index),
            label: Some(format!("fan{index}")),
        })
        .collect();

    FanTopology {
        control_mode: match kind {
            FanControllerKind::None => FanControlMode::None,
            FanControllerKind::Emc2101 => FanControlMode::Emc2101,
            FanControllerKind::Emc2103 => FanControlMode::Emc2103,
            FanControllerKind::Emc2302 => FanControlMode::Emc2302,
        },
        fan_count: Some(count),
        tach_channels: per_fan.iter().filter_map(|fan| fan.tach_channel).collect(),
        pwm_channels: per_fan.iter().filter_map(|fan| fan.pwm_channel).collect(),
        per_fan,
    }
}

fn temp_sensors(temp: TempSensorKind, power: PowerControllerKind) -> Vec<String> {
    let mut sensors = Vec::new();
    match temp {
        TempSensorKind::None => {}
        TempSensorKind::Emc2101 => sensors.push("emc2101".to_string()),
        TempSensorKind::Tmp1075 => sensors.push("tmp1075".to_string()),
        TempSensorKind::Emc2103 => sensors.push("emc2103".to_string()),
    }
    if power == PowerControllerKind::Tps546 {
        sensors.push("tps546-vr".to_string());
    }
    sensors
}

fn esp_temp_sensor_descriptors(
    temp: TempSensorKind,
    power: PowerControllerKind,
) -> Vec<TempSensorDescriptor> {
    temp_sensors(temp, power)
        .into_iter()
        .enumerate()
        .map(|(index, name)| TempSensorDescriptor {
            class: match name.as_str() {
                "tps546-vr" => TempSensorClass::VrController,
                _ => TempSensorClass::BoardI2c,
            },
            name: Some(name),
            bus: Some("i2c".to_string()),
            address: None,
            index: u8::try_from(index).ok(),
            fallback_order: u8::try_from(index).ok(),
        })
        .collect()
}

fn power_controller_label(kind: PowerControllerKind) -> &'static str {
    match kind {
        PowerControllerKind::None => "fixed-or-none",
        PowerControllerKind::Tps546 => "tps546",
        PowerControllerKind::Ds4432u => "ds4432u",
    }
}

fn clamp_f32_to_u16(value: f32, min: f32, max: f32) -> u16 {
    value.clamp(min, max).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_gamma_descriptor_is_experimental_not_beta_or_stable() {
        let cfg = DcentAxeConfig::default();
        let desc = build_esp_capability_descriptor(&cfg, true, 50.0, 650.0, 850, 1350);

        assert_eq!(desc.family, DeviceFamily::Esp);
        assert_eq!(desc.support, SupportTier::Experimental);
        assert_eq!(desc.identity.confidence, IdentityConfidence::Exact);
        assert_eq!(desc.board.board_target.as_deref(), Some("bitaxe-gamma"));
        assert_eq!(desc.asic.chip_model.as_deref(), Some("BM1370"));
        assert_eq!(desc.asic.baud, Some(1_000_000));
        assert!(desc.runtime_caps.contains(&RuntimeCapability::FlashOta));
        assert_eq!(desc.install.planner_outcome, PlannerOutcome::OtaSupported);
        assert_eq!(desc.safe_defaults.fan_pwm_cap, 100);
        assert!(desc.fail_safe.mutating_routes_allowed);
    }

    #[test]
    fn unknown_board_identity_fails_read_only() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_version = "unknown-board-version".to_string();

        let desc = build_esp_capability_descriptor(&cfg, true, 50.0, 650.0, 850, 1350);

        assert_eq!(desc.support, SupportTier::Unknown);
        assert_eq!(desc.identity.confidence, IdentityConfidence::Unknown);
        assert_eq!(desc.runtime_caps, READ_ONLY_RUNTIME_CAPABILITIES);
        assert!(desc.fail_safe.read_only);
        assert!(!desc.fail_safe.mining_start_allowed);
        assert!(!desc.fail_safe.mutating_routes_allowed);
        assert!(!desc.safe_defaults.mining_enabled);
    }

    #[test]
    fn esp_baud_pins_keep_bm136x_bm1370_at_one_mbaud() {
        assert_eq!(esp_runtime_baud("BM1366"), 1_000_000);
        assert_eq!(esp_runtime_baud("BM1368"), 1_000_000);
        assert_eq!(esp_runtime_baud("BM1370"), 1_000_000);
        assert_eq!(esp_runtime_baud("BM1397"), 3_125_000);
    }
}
