//! Read-only reverse-engineering catalog REST endpoints.
//!
//! These routes expose static, HAL-free catalog data from
//! `dcentrald-api-types`. Handlers intentionally do not extract
//! `State`, open devices, call mining/control paths, or write config.

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use dcentrald_api_types::apw_dual_output::{
    ALL_OUTPUTS, APW12_AC_INPUT_COUNT, APW12_AC_VOLTAGE_MAX, APW12_AC_VOLTAGE_MIN,
    APW12_PFC_BUS_VOLTAGE_MAX, APW12_PFC_BUS_VOLTAGE_MIN,
};
use dcentrald_api_types::asic_command::{address_stride, AsicCommand, ColdBootStep};
use dcentrald_api_types::asic_register_map::{RegisterEntry, BM1387_REGISTERS, BM1397_REGISTERS};
use dcentrald_api_types::baud_switch::{
    is_forbidden_bm1362_value, BaudChipFamily, BaudPlan, BM1362_MISCCTRL_FRAME,
    BM1362_PREAMBLE_FRAME, FORBIDDEN_BM1362_MISCCTRL,
};
use dcentrald_api_types::boot_flow::{
    PhaseWindow, PlatformTier, AM1_ZYNQ_TIMELINE, AM2_ZYNQ_TIMELINE, AM3_AML_TIMELINE,
};
use dcentrald_api_types::boot_orchestration::{
    teardown_order, OrchestrationPhase, SubsystemDependency, SubsystemId, SUBSYSTEM_BRINGUP_ORDER,
    SUBSYSTEM_DEPENDENCIES,
};
use dcentrald_api_types::chip_init::ChipFamily;
use dcentrald_api_types::diode_voltage::{
    classify_diode_ohms, classify_voltage, , DiodeFamily, DiodePin,
};
use dcentrald_api_types::dspic_frame::{DspicOpcode, NAK_BYTE, PREAMBLE};
use dcentrald_api_types::eeprom_record::{chip_family_for_sku, BHB_SKU_CATALOG};
use dcentrald_api_types::fpga_register_map::{
    absolute_address, baud_from_divisor, divisor_from_baud, registers_for, S9ioWindow,
    S9IO_V102_CHAIN_BASES,
};
use dcentrald_api_types::psu_apw_protocol::{
    adc_raw_to_voltage, build_apw_frame, dac_code_to_voltage, voltage_to_dac_code, ApwCommand,
    APW_PREAMBLE_0, APW_PREAMBLE_1, DAC_OFFSET_PER_COUNT_V, DAC_REFERENCE_V,
};
use dcentrald_api_types::thermal_model::{
    FanMode, ThermalCompConfig, DEFAULT_DERATING_PER_C, DEFAULT_DERATING_THRESHOLD_C,
    DEFAULT_EMERGENCY_TEMP_C, DEFAULT_HYSTERESIS_BAND_C, DEFAULT_IMMERSION_OFFSET_C,
    DEFAULT_MIN_SCALE, DEFAULT_REFERENCE_TEMP_C, VNISH_LOWER_PROFILE_FAN_PWM_PERCENT,
    VNISH_LOWER_PROFILE_TEMP_C, VNISH_RAISE_PROFILE_FAN_PWM_PERCENT, VNISH_RAISE_PROFILE_TEMP_C,
    VNISH_SUSTAIN_WINDOW_SECONDS,
};
use dcentrald_api_types::uart_trans_layout::{
    is_chain_present, UartTransIoctl, ASIC_WORK_FRAME_LAYOUT, UART_TRANS_CHAIN_COUNT,
    UART_TRANS_DEV_PATH, UART_TRANS_IOCTL_MAGIC, UART_TRANS_MAJOR, UART_TRANS_MINOR,
    UART_TRANS_TTY_PATHS,
};

use crate::AppState;

const CATALOG_SCHEMA: &str = "dcentos.re.catalog.v1";
const BASE_PATH: &str = "/api/re/catalog";

/// Build the read-only RE catalog router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(BASE_PATH, get(get_index))
        .route("/api/re/catalog/index", get(get_index))
        .route("/api/re/catalog/asic-registers", get(get_asic_registers))
        .route(
            "/api/re/catalog/asic-registers/bm1387",
            get(get_asic_registers_bm1387),
        )
        .route(
            "/api/re/catalog/asic-registers/bm1397",
            get(get_asic_registers_bm1397),
        )
        .route("/api/re/catalog/asic-commands", get(get_asic_commands))
        .route("/api/re/catalog/boot-flow", get(get_boot_flow))
        .route(
            "/api/re/catalog/boot-orchestration",
            get(get_boot_orchestration),
        )
        .route("/api/re/catalog/thermal-model", get(get_thermal_model))
        .route(
            "/api/re/catalog/firmware-stratum-matrix",
            get(get_firmware_stratum_matrix),
        )
        .route(
            "/api/re/catalog/luxos-rest-commands",
            get(get_luxos_rest_commands),
        )
        .route(
            "/api/re/catalog/vnish-rest-endpoints",
            get(get_vnish_rest_endpoints),
        )
        .route(
            "/api/re/catalog/luxos-network-exposure",
            get(get_luxos_network_exposure),
        )
        .route("/api/re/catalog/eeprom/bhb-skus", get(get_eeprom_bhb_skus))
        .route("/api/re/catalog/dspic-frames", get(get_dspic_frames))
        .route("/api/re/catalog/apw-psu", get(get_apw_psu))
        .route("/api/re/catalog/fpga-registers", get(get_fpga_registers))
        .route("/api/re/catalog/bb-uart-trans", get(get_bb_uart_trans))
        .route(
            "/api/re/catalog/bm1362-baud-init",
            get(get_bm1362_baud_init),
        )
        .route("/api/re/catalog/diode-voltage", get(get_diode_voltage))
        .route(
            "/api/re/catalog/s21-fixture-production-warnings",
            get(get_s21_fixture_production_warnings),
        )
}

#[derive(Debug, Clone, Serialize)]
struct ReadOnlyMeta {
    schema: &'static str,
    read_only: bool,
    hardware_reads: bool,
    hardware_writes: bool,
    config_writes: bool,
    mining_control: bool,
    source_crate: &'static str,
}

impl Default for ReadOnlyMeta {
    fn default() -> Self {
        Self {
            schema: CATALOG_SCHEMA,
            read_only: true,
            hardware_reads: false,
            hardware_writes: false,
            config_writes: false,
            mining_control: false,
            source_crate: "dcentrald-api-types",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CatalogEndpoint {
    name: &'static str,
    path: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct CatalogIndex {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    base_path: &'static str,
    catalogs: Vec<CatalogEndpoint>,
}

fn catalog_index() -> CatalogIndex {
    CatalogIndex {
        meta: ReadOnlyMeta::default(),
        base_path: BASE_PATH,
        catalogs: vec![
            CatalogEndpoint {
                name: "asic_registers",
                path: "/api/re/catalog/asic-registers",
                description: "BM1387 and BM1397+ ASIC register maps.",
            },
            CatalogEndpoint {
                name: "asic_commands",
                path: "/api/re/catalog/asic-commands",
                description: "Family-specific ASIC command bytes and cold-boot step requirements.",
            },
            CatalogEndpoint {
                name: "boot_flow",
                path: "/api/re/catalog/boot-flow",
                description: "Static boot phase timing windows by platform tier.",
            },
            CatalogEndpoint {
                name: "boot_orchestration",
                path: "/api/re/catalog/boot-orchestration",
                description: "Static subsystem dependencies, bring-up order, and teardown order.",
            },
            CatalogEndpoint {
                name: "thermal_model",
                path: "/api/re/catalog/thermal-model",
                description: "Thermal derating constants, VNish thresholds, and fan safety caps.",
            },
            CatalogEndpoint {
                name: "firmware_stratum_matrix",
                path: "/api/re/catalog/firmware-stratum-matrix",
                description: "Cross-firmware SV1/SV2, version-rolling, and devfee capability matrix.",
            },
            CatalogEndpoint {
                name: "luxos_rest_commands",
                path: "/api/re/catalog/luxos-rest-commands",
                description: "Full LuxOS REST command catalog with auth, parameter shape, and destructive classification.",
            },
            CatalogEndpoint {
                name: "vnish_rest_endpoints",
                path: "/api/re/catalog/vnish-rest-endpoints",
                description: "VNish REST endpoint catalog with auth and destructive confirmation classification.",
            },
            CatalogEndpoint {
                name: "luxos_network_exposure",
                path: "/api/re/catalog/luxos-network-exposure",
                description: "LuxOS listen-port, auth, TLS, and default-disable risk catalog.",
            },
            CatalogEndpoint {
                name: "eeprom_bhb_skus",
                path: "/api/re/catalog/eeprom/bhb-skus",
                description: "EEPROM parser preambles and static BHB/A3HB SKU-to-chip-family catalog.",
            },
            CatalogEndpoint {
                name: "dspic_frames",
                path: "/api/re/catalog/dspic-frames",
                description: "dsPIC/PIC/APW frame preamble, opcode table, and destructive classification.",
            },
            CatalogEndpoint {
                name: "apw_psu",
                path: "/api/re/catalog/apw-psu",
                description: "APW PSU I2C protocol catalog plus APW12 dual-output rail behavior.",
            },
            CatalogEndpoint {
                name: "fpga_registers",
                path: "/api/re/catalog/fpga-registers",
                description: "BraiinsOS s9io FPGA chain bases, windows, registers, and baud divisor helpers.",
            },
            CatalogEndpoint {
                name: "bb_uart_trans",
                path: "/api/re/catalog/bb-uart-trans",
                description: "BB-platform uart_trans work-frame layout, ioctl IDs, char device, and chain map.",
            },
            CatalogEndpoint {
                name: "bm1362_baud_init",
                path: "/api/re/catalog/bm1362-baud-init",
                description: "BM1362 baud/init plan with byte-pinned frames and forbidden 0x40C100B7 hazard.",
            },
            CatalogEndpoint {
                name: "diode_voltage",
                path: "/api/re/catalog/diode-voltage",
                description: "Manual diode-voltage reference catalog and classifier input/output shape.",
            },
            CatalogEndpoint {
                name: "s21_fixture_production_warnings",
                path: "/api/re/catalog/s21-fixture-production-warnings",
                description: "Static warnings separating S21/AMTC fixture evidence from production cold-boot rules.",
            },
        ],
    }
}

async fn get_index() -> Json<CatalogIndex> {
    Json(catalog_index())
}

#[derive(Debug, Clone, Serialize)]
struct RegisterFamilyCatalog {
    family: ChipFamily,
    label: &'static str,
    registers: &'static [RegisterEntry],
}

#[derive(Debug, Clone, Serialize)]
struct AsicRegisterCatalog {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    families: Vec<RegisterFamilyCatalog>,
}

fn asic_register_catalog() -> AsicRegisterCatalog {
    AsicRegisterCatalog {
        meta: ReadOnlyMeta::default(),
        families: vec![
            RegisterFamilyCatalog {
                family: ChipFamily::Bm1387,
                label: "BM1387",
                registers: BM1387_REGISTERS,
            },
            RegisterFamilyCatalog {
                family: ChipFamily::Bm1397,
                label: "BM1397+",
                registers: BM1397_REGISTERS,
            },
        ],
    }
}

async fn get_asic_registers() -> Json<AsicRegisterCatalog> {
    Json(asic_register_catalog())
}

async fn get_asic_registers_bm1387() -> Json<RegisterFamilyCatalog> {
    Json(RegisterFamilyCatalog {
        family: ChipFamily::Bm1387,
        label: "BM1387",
        registers: BM1387_REGISTERS,
    })
}

async fn get_asic_registers_bm1397() -> Json<RegisterFamilyCatalog> {
    Json(RegisterFamilyCatalog {
        family: ChipFamily::Bm1397,
        label: "BM1397+",
        registers: BM1397_REGISTERS,
    })
}

#[derive(Debug, Clone, Serialize)]
struct CommandCatalogEntry {
    command: AsicCommand,
    bm1387_byte: Option<u8>,
    bm1397plus_byte: Option<u8>,
    broadcast: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ColdBootStepCatalogEntry {
    step: ColdBootStep,
    bm1387_required: bool,
    bm1397plus_required: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AddressStrideCatalog {
    bm1387_stride: u32,
    bm1397plus_formula: &'static str,
    bm1397plus_examples: Vec<AddressStrideExample>,
}

#[derive(Debug, Clone, Serialize)]
struct AddressStrideExample {
    chip_count: u32,
    stride: u32,
}

#[derive(Debug, Clone, Serialize)]
struct AsicCommandCatalog {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    commands: Vec<CommandCatalogEntry>,
    cold_boot_steps: Vec<ColdBootStepCatalogEntry>,
    address_stride: AddressStrideCatalog,
}

fn asic_command_catalog() -> AsicCommandCatalog {
    let commands = [
        AsicCommand::SetChipAddress,
        AsicCommand::GetAddress,
        AsicCommand::ChainInactive,
        AsicCommand::SetConfig,
    ]
    .into_iter()
    .map(|command| CommandCatalogEntry {
        command,
        bm1387_byte: command.byte_for_family(ChipFamily::Bm1387),
        bm1397plus_byte: command.byte_for_family(ChipFamily::Bm1397),
        broadcast: command.is_broadcast(),
    })
    .collect();

    let cold_boot_steps = [
        ColdBootStep::FpgaReset,
        ColdBootStep::OpenUart115200,
        ColdBootStep::GetAddress,
        ColdBootStep::ChainInactiveTriple,
        ColdBootStep::SetChipAddressSeq,
        ColdBootStep::FamilyPreamble,
        ColdBootStep::PllSetup,
        ColdBootStep::MiscCtrlBaudUpgrade,
        ColdBootStep::TicketMaskConfig,
        ColdBootStep::OpenCore,
        ColdBootStep::HashCounting,
        ColdBootStep::FrequencyRamp,
    ]
    .into_iter()
    .map(|step| ColdBootStepCatalogEntry {
        step,
        bm1387_required: step.is_required(ChipFamily::Bm1387),
        bm1397plus_required: step.is_required(ChipFamily::Bm1397),
    })
    .collect();

    AsicCommandCatalog {
        meta: ReadOnlyMeta::default(),
        commands,
        cold_boot_steps,
        address_stride: AddressStrideCatalog {
            bm1387_stride: address_stride(ChipFamily::Bm1387, 63),
            bm1397plus_formula: "256 / chip_count",
            bm1397plus_examples: [114, 108, 77]
                .into_iter()
                .map(|chip_count| AddressStrideExample {
                    chip_count,
                    stride: address_stride(ChipFamily::Bm1397, chip_count),
                })
                .collect(),
        },
    }
}

async fn get_asic_commands() -> Json<AsicCommandCatalog> {
    Json(asic_command_catalog())
}

#[derive(Debug, Clone, Serialize)]
struct BootTimelineCatalog {
    tier: PlatformTier,
    timeline: &'static [PhaseWindow],
}

#[derive(Debug, Clone, Serialize)]
struct BootFlowCatalog {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    timelines: Vec<BootTimelineCatalog>,
}

fn boot_flow_catalog() -> BootFlowCatalog {
    BootFlowCatalog {
        meta: ReadOnlyMeta::default(),
        timelines: vec![
            BootTimelineCatalog {
                tier: PlatformTier::Am1Zynq,
                timeline: AM1_ZYNQ_TIMELINE,
            },
            BootTimelineCatalog {
                tier: PlatformTier::Am2Zynq,
                timeline: AM2_ZYNQ_TIMELINE,
            },
            BootTimelineCatalog {
                tier: PlatformTier::Am3Aml,
                timeline: AM3_AML_TIMELINE,
            },
            BootTimelineCatalog {
                tier: PlatformTier::Am3Bb,
                timeline: AM3_AML_TIMELINE,
            },
        ],
    }
}

async fn get_boot_flow() -> Json<BootFlowCatalog> {
    Json(boot_flow_catalog())
}

#[derive(Debug, Clone, Serialize)]
struct BootOrchestrationCatalog {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    phases: [OrchestrationPhase; 9],
    subsystem_bringup_order: &'static [SubsystemId],
    subsystem_teardown_order: Vec<SubsystemId>,
    subsystem_dependencies: &'static [SubsystemDependency],
}

fn boot_orchestration_catalog() -> BootOrchestrationCatalog {
    BootOrchestrationCatalog {
        meta: ReadOnlyMeta::default(),
        phases: OrchestrationPhase::canonical_order(),
        subsystem_bringup_order: SUBSYSTEM_BRINGUP_ORDER,
        subsystem_teardown_order: teardown_order(),
        subsystem_dependencies: SUBSYSTEM_DEPENDENCIES,
    }
}

async fn get_boot_orchestration() -> Json<BootOrchestrationCatalog> {
    Json(boot_orchestration_catalog())
}

#[derive(Debug, Clone, Serialize)]
struct ThermalConstantsCatalog {
    : f32,
    derating_threshold_c: f32,
    derating_per_c: f32,
    emergency_temp_c: f32,
    hysteresis_band_c: f32,
    min_scale: f32,
    immersion_offset_c: f32,
}

#[derive(Debug, Clone, Serialize)]
struct VnishThermalCatalog {
    lower_profile_temp_c: f32,
    lower_profile_fan_pwm_percent: u8,
    raise_profile_temp_c: f32,
    raise_profile_fan_pwm_percent: u8,
    sustain_window_seconds: u32,
}

#[derive(Debug, Clone, Serialize)]
struct FanModeCatalog {
    mode: FanMode,
    display: &'static str,
    max_pwm: u8,
    safety_cap_pwm: u8,
}

#[derive(Debug, Clone, Serialize)]
struct ThermalModelCatalog {
    #[serde(flatten)]
    meta: ReadOnlyMeta,
    defaults: ThermalCompConfig,
    constants: ThermalConstantsCatalog,
    vnish_profile_switching: VnishThermalCatalog,
    fan_safety_caps: Vec<FanModeCatalog>,
}

fn thermal_model_catalog() -> ThermalModelCatalog {
    ThermalModelCatalog {
        meta: ReadOnlyMeta::default(),
        defaults: ThermalCompConfig::default(),
        constants: ThermalConstantsCatalog {
            : DEFAULT_REFERENCE_TEMP_C,
            derating_threshold_c: DEFAULT_DERATING_THRESHOLD_C,
            derating_per_c: DEFAULT_DERATING_PER_C,
            emergency_temp_c: DEFAULT_EMERGENCY_TEMP_C,
            hysteresis_band_c: DEFAULT_HYSTERESIS_BAND_C,
            min_scale: DEFAULT_MIN_SCALE,
            immersion_offset_c: DEFAULT_IMMERSION_OFFSET_C,
        },
        vnish_profile_switching: VnishThermalCatalog {
            lower_profile_temp_c: VNISH_LOWER_PROFILE_TEMP_C,
            lower_profile_fan_pwm_percent: VNISH_LOWER_PROFILE_FAN_PWM_PERCENT,
            raise_profile_temp_c: VNISH_RAISE_PROFILE_TEMP_C,
            raise_profile_fan_pwm_percent: VNISH_RAISE_PROFILE_FAN_PWM_PERCENT,
            sustain_window_seconds: VNISH_SUSTAIN_WINDOW_SECONDS,
        },
        fan_safety_caps: [
            FanMode::QuietHome,
            FanMode::Home,
            FanMode::Balanced,
            FanMode::Advanced,
            FanMode::HashrateMax,
        ]
        .into_iter()
        .map(|mode| FanModeCatalog {
            mode,
            display: mode.display(),
            max_pwm: mode.max_pwm(),
            safety_cap_pwm: mode.safety_cap_pwm(),
        })
        .collect(),
    }
}

async fn get_thermal_model() -> Json<ThermalModelCatalog> {
    Json(thermal_model_catalog())
}

async fn get_firmware_stratum_matrix() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "capabilities": dcentrald_api_types::firmware_stratum_matrix::FIRMWARE_CAPABILITIES,
    }))
}

async fn get_luxos_rest_commands() -> Json<serde_json::Value> {
    let commands: Vec<serde_json::Value> =
        dcentrald_api_types::luxos_rest_command::ALL_COMMANDS
            .iter()
            .copied()
            .map(|command| {
                let descriptor = dcentrald_api_types::luxos_rest_command::descriptor(command);
                serde_json::json!({
                    "command": command,
                    "name": descriptor.name,
                    "auth": descriptor.auth,
                    "kind": descriptor.kind,
                    "parameter_shape": descriptor.parameter_shape,
                    "verified_in_spa": descriptor.verified_in_spa,
                    "destructive": dcentrald_api_types::luxos_rest_command::is_destructive(command),
                    "requires_session": dcentrald_api_types::luxos_rest_command::requires_session(command),
                })
            })
            .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "count": commands.len(),
        "commands": commands,
    }))
}

async fn get_vnish_rest_endpoints() -> Json<serde_json::Value> {
    let endpoints: Vec<serde_json::Value> =
        dcentrald_api_types::vnish_rest_endpoints::ALL_ENDPOINTS
            .iter()
            .copied()
            .map(|endpoint| {
                let descriptor = dcentrald_api_types::vnish_rest_endpoints::descriptor(endpoint);
                serde_json::json!({
                    "endpoint": endpoint,
                    "method": descriptor.method,
                    "path": descriptor.path,
                    "auth": descriptor.auth,
                    "kind": descriptor.kind,
                    "requires_confirmation": dcentrald_api_types::vnish_rest_endpoints::requires_confirmation(endpoint),
                })
            })
            .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "api_prefix": dcentrald_api_types::vnish_rest_endpoints::VNISH_API_PREFIX,
        "count": endpoints.len(),
        "endpoints": endpoints,
    }))
}

async fn get_luxos_network_exposure() -> Json<serde_json::Value> {
    let ports: Vec<serde_json::Value> =
        dcentrald_api_types::luxos_network_exposure::ALL_LUXOS_PORTS
            .iter()
            .copied()
            .map(|port| {
                serde_json::json!({
                    "port": port,
                    "port_number": port.port_number(),
                    "process": port.process(),
                    "auth_kind": port.auth_kind(),
                    "has_tls": port.has_tls(),
                    "risk_level": port.risk_level(),
                    "should_disable_by_default": port.should_disable_by_default(),
                })
            })
            .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "count": ports.len(),
        "ports": ports,
    }))
}

async fn get_eeprom_bhb_skus() -> Json<serde_json::Value> {
    let lookup_examples: Vec<serde_json::Value> = [
        "BHB42601",
        "BHB42801",
        "BHB42811",
        "BHB42831",
        "BHB56902",
        "BHB68xxx",
        "A3HB7xxxx",
    ]
    .iter()
    .map(|sku| {
        let chip_family = chip_family_for_sku(sku);
        serde_json::json!({
            "sku": sku,
            "chip_family": chip_family,
            "recognized": chip_family.is_some(),
        })
    })
    .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "parser_boundary": {
            "input": "post-cipher EEPROM plaintext bytes",
            "hardware_reads": false,
            "decrypts_eeprom": false,
            "writes_eeprom": false,
        },
        "preambles": [
            {
                "bytes": [0x04u8, 0x11],
                "variant": "x19_plain_or_x19_j",
                "families": "BHB42xxx, including BHB426xx and BHB428xx",
            },
            {
                "bytes": [0x05u8, 0x11],
                "variant": "x21_aes",
                "families": "BHB56xxx, BHB68xxx, A3HB7xxxx",
            },
            {
                "bytes": [b'B', b'r'],
                "variant": "braiinsminer",
                "families": "BMM100/BMM101",
            },
        ],
        "sku_catalog": BHB_SKU_CATALOG,
        "lookup_examples": lookup_examples,
    }))
}

const DSPIC_OPCODES: &[DspicOpcode] = &[
    DspicOpcode::SetPicFlashPointer,
    DspicOpcode::SendData,
    DspicOpcode::ReadData,
    DspicOpcode::Jump,
    DspicOpcode::Reset,
    DspicOpcode::SetVoltage,
    DspicOpcode::Enable,
    DspicOpcode::Heartbeat,
    DspicOpcode::GetVersion,
    DspicOpcode::GetVoltage,
    DspicOpcode::Measure,
    DspicOpcode::GetV2,
    DspicOpcode::PsuWatchdog,
    DspicOpcode::PsuSetVoltage,
    DspicOpcode::PsuHeartbeat,
];

async fn get_dspic_frames() -> Json<serde_json::Value> {
    let opcodes: Vec<serde_json::Value> = DSPIC_OPCODES
        .iter()
        .copied()
        .map(|opcode| {
            serde_json::json!({
                "opcode": opcode,
                "code": opcode.as_u8(),
                "destructive": opcode.is_destructive(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "preamble": PREAMBLE,
        "nak_byte": NAK_BYTE,
        "wire_forms": [
            {
                "name": "bare",
                "shape": "[0x55, 0xAA, CMD, payload...]",
                "families": "S9/L3+ PIC16F1704 bare runtime path",
            },
            {
                "name": "framed_sum",
                "shape": "[0x55, 0xAA, LEN, CMD, payload..., CKSUM]",
                "families": "AM2/Amlogic dsPIC and APW PSU framed path",
            },
            {
                "name": "framed_short",
                "shape": "[0x55, 0xAA, CMD]",
                "families": "fw=0x86 GET_VERSION special case",
            },
        ],
        "opcodes": opcodes,
    }))
}

const APW_COMMANDS: &[ApwCommand] = &[
    ApwCommand::GetFwVersion,
    ApwCommand::GetHwVersion,
    ApwCommand::GetVoltage,
    ApwCommand::MeasureVoltage,
    ApwCommand::ReadState,
    ApwCommand::ReadCal,
    ApwCommand::Watchdog,
    ApwCommand::SetVoltage,
    ApwCommand::WriteCal,
];

async fn get_apw_psu() -> Json<serde_json::Value> {
    let commands: Vec<serde_json::Value> = APW_COMMANDS
        .iter()
        .copied()
        .map(|command| {
            serde_json::json!({
                "command": command,
                "code": command.code(),
                "echo_ack": command.is_echo_ack(),
                "destructive": command.is_destructive(),
            })
        })
        .collect();

    let outputs: Vec<serde_json::Value> = ALL_OUTPUTS
        .iter()
        .copied()
        .map(|output| {
            let (voltage_min_v, voltage_max_v) = output.voltage_range();
            serde_json::json!({
                "output": output,
                "connector": output.connector(),
                "role": output.role(),
                "watchdog_can_cut": output.watchdog_can_cut(),
                "voltage_min_v": voltage_min_v,
                "voltage_max_v": voltage_max_v,
                "max_current_a": output.max_current_a(),
                "max_power_w": output.max_power_w(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "frame": {
            "preamble": [APW_PREAMBLE_0, APW_PREAMBLE_1],
            "shape": "[0x55, 0xAA, length, cmd, payload..., checksum]",
            "checksum": "8-bit sum over length + cmd + payload",
        },
        "commands": commands,
        "frame_examples": {
            "watchdog_disable": build_apw_frame(ApwCommand::Watchdog, &[0x00]),
            "watchdog_enable": build_apw_frame(ApwCommand::Watchdog, &[0x01]),
            "set_12_5v": build_apw_frame(ApwCommand::SetVoltage, &[0xC8]),
        },
        "dac_formula": {
            "": DAC_REFERENCE_V,
            "offset_per_count_v": DAC_OFFSET_PER_COUNT_V,
            "dac_0xc8_voltage_v": dac_code_to_voltage(0xC8),
            "voltage_12_5_dac_code": voltage_to_dac_code(12.5),
            "adc_raw_800_voltage_v": adc_raw_to_voltage(800),
        },
        "apw12_dual_output": {
            "ac_input_count": APW12_AC_INPUT_COUNT,
            "ac_voltage_min_v": APW12_AC_VOLTAGE_MIN,
            "ac_voltage_max_v": APW12_AC_VOLTAGE_MAX,
            "pfc_bus_voltage_min_v": APW12_PFC_BUS_VOLTAGE_MIN,
            "pfc_bus_voltage_max_v": APW12_PFC_BUS_VOLTAGE_MAX,
            "outputs": outputs,
        },
    }))
}

const FPGA_WINDOWS: &[S9ioWindow] = &[
    S9ioWindow::Common,
    S9ioWindow::Cmd,
    S9ioWindow::WorkRx,
    S9ioWindow::WorkTx,
];

async fn get_fpga_registers() -> Json<serde_json::Value> {
    let windows: Vec<serde_json::Value> = FPGA_WINDOWS
        .iter()
        .copied()
        .map(|window| {
            let registers = registers_for(window);
            let chain0_first_address = registers
                .first()
                .and_then(|register| absolute_address(0, window, register.offset));
            serde_json::json!({
                "window": window,
                "base_offset": window.base_offset(),
                "registers": registers,
                "chain0_first_absolute_address": chain0_first_address,
            })
        })
        .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "bitstream": "BraiinsOS s9io v1.0.2",
        "chain_bases": S9IO_V102_CHAIN_BASES,
        "windows": windows,
        "baud_examples": {
            "divisor_0x6c_baud": baud_from_divisor(0x6C),
            "divisor_0x03_baud": baud_from_divisor(0x03),
            "divisor_for_3125000_baud": divisor_from_baud(3_125_000),
        },
    }))
}

const UART_TRANS_IOCTL_OPS: &[UartTransIoctl] = &[
    UartTransIoctl::MmapConfig,
    UartTransIoctl::SetBaud,
    UartTransIoctl::GetBaud,
    UartTransIoctl::ResetFifo,
    UartTransIoctl::FlushTx,
    UartTransIoctl::FlushRx,
    UartTransIoctl::GetNonce,
    UartTransIoctl::SendWork,
];

async fn get_bb_uart_trans() -> Json<serde_json::Value> {
    let ioctls: Vec<serde_json::Value> = UART_TRANS_IOCTL_OPS
        .iter()
        .copied()
        .map(|op| {
            serde_json::json!({
                "op": op,
                "nr": op.nr(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "device": {
            "path": UART_TRANS_DEV_PATH,
            "major": UART_TRANS_MAJOR,
            "minor": UART_TRANS_MINOR,
        },
        "ioctl_magic": {
            "byte": UART_TRANS_IOCTL_MAGIC,
            "ascii": (UART_TRANS_IOCTL_MAGIC as char).to_string(),
        },
        "layout": ASIC_WORK_FRAME_LAYOUT,
        "ioctls": ioctls,
        "chain_map": {
            "chain_count": UART_TRANS_CHAIN_COUNT,
            "tty_paths": UART_TRANS_TTY_PATHS,
            "bitmap_example_0b0101": [
                is_chain_present(0b0101, 0),
                is_chain_present(0b0101, 1),
                is_chain_present(0b0101, 2),
                is_chain_present(0b0101, 3),
            ],
        },
    }))
}

async fn get_bm1362_baud_init() -> Json<serde_json::Value> {
    let baud_plan = BaudPlan::canonical(BaudChipFamily::Bm1362);
    let init_spec = dcentrald_api_types::chip_init::init_spec(ChipFamily::Bm1362);

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "chip_family": ChipFamily::Bm1362,
        "init_spec": init_spec,
        "baud_plan": baud_plan,
        "byte_pinned_frames": {
            "fast_uart_preamble_register_0x28": BM1362_PREAMBLE_FRAME,
            "miscctrl_register_0x18_triple_write": BM1362_MISCCTRL_FRAME,
        },
        "forbidden_miscctrl": {
            "value": FORBIDDEN_BM1362_MISCCTRL,
            "value_hex": "0x40C100B7",
            "is_forbidden": is_forbidden_bm1362_value(FORBIDDEN_BM1362_MISCCTRL),
            "why": "bit 16 is already set in MISC_CONTROL_INIT; OR-ing it again is a no-op and can leave the chain at 115200",
        },
        "guardrails": [
            "broadcast 0x28 = 0x00003011 before the 0x18 triple-write",
            "write 0x18 = 0x00C100B0 three times with 5 ms spacing",
            "switch host UART to 3.125 Mbaud only after the third write and settle",
            "do not use 0x40C100B7 for BM1362",
        ],
    }))
}

const DIODE_FAMILIES: &[DiodeFamily] = &[
    DiodeFamily::S17Family,
    DiodeFamily::S17eFamily,
    DiodeFamily::S19Family,
];

async fn get_diode_voltage() -> Json<serde_json::Value> {
    let families: Vec<serde_json::Value> = DIODE_FAMILIES
        .iter()
        .copied()
        .map(|family| {
            serde_json::json!({
                "family": family,
                "reference": (family),
            })
        })
        .collect();

    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "measurement_mode": {
            "operator_supplied": true,
            "hardware_reads": false,
            "hardware_writes": false,
            "instrument": "manual multimeter measurement",
        },
        "families": families,
        "classifier_shape": {
            "voltage": {
                "input": {
                    "family": DiodeFamily::S19Family,
                    "pin": DiodePin::Clk,
                    "measured_v": 0.8,
                },
                "verdict": classify_voltage(DiodeFamily::S19Family, DiodePin::Clk, 0.8),
            },
            "diode_ohms": {
                "input": {
                    "family": DiodeFamily::S19Family,
                    "pin": DiodePin::BiBo,
                    "measured_ohms": 1220,
                },
                "verdict": classify_diode_ohms(DiodeFamily::S19Family, DiodePin::BiBo, 1220),
            },
        },
    }))
}

async fn get_s21_fixture_production_warnings() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "meta": ReadOnlyMeta::default(),
        "scope": "S21/S21-class and AMTC fixture evidence that must not be promoted to production cold-boot behavior without a lab gate.",
        "warnings": [
            {
                "id": "fixture_pre_open_core_voltage",
                "fixture_evidence": "AMTC S21 fixture uses Pre_Open_Core_Voltage=1500 (15.0 V).",
                "production_rule": "Treat 15.0 V pre-open as fixture/pattern-test evidence, not normal production cold-boot evidence.",
                "operator_gate": "Explicit lab flag required before any implementation may use 15.0 V pre-open.",
                "source": "D-Central internal reverse-engineering",
            },
            {
                "id": "bm1370_tail_block_not_bm1362_default",
                "fixture_evidence": "BM1370/S21 fixture paths include a B9/54/B9/3C tail block.",
                "production_rule": "Do not make that tail block normal for S19j Pro BM1362 AM2; keep it diagnostic-only.",
                "operator_gate": "Diagnostic/lab path only.",
                "source": "D-Central internal reverse-engineering",
            },
            {
                "id": "fixture_core_counts_are_not_voltage_policy",
                "fixture_evidence": "S21 fixture RE helps pin BM1368 core layout and PLL behavior.",
                "production_rule": "Use it as register/profile evidence only; voltage policy must come from the target production platform.",
                "operator_gate": "No automatic voltage lift from fixture-only constants.",
                "source": "D-Central internal reverse-engineering",
            },
        ],
        "read_only_catalog_only": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lists_stable_catalog_paths() {
        let index = catalog_index();
        assert!(index.meta.read_only);
        assert!(!index.meta.hardware_reads);
        assert!(!index.meta.hardware_writes);
        assert!(!index.meta.config_writes);
        assert!(!index.meta.mining_control);
        assert_eq!(index.base_path, BASE_PATH);

        let paths: Vec<&str> = index.catalogs.iter().map(|c| c.path).collect();
        for expected in [
            "/api/re/catalog/asic-registers",
            "/api/re/catalog/asic-commands",
            "/api/re/catalog/boot-flow",
            "/api/re/catalog/boot-orchestration",
            "/api/re/catalog/thermal-model",
            "/api/re/catalog/firmware-stratum-matrix",
            "/api/re/catalog/luxos-rest-commands",
            "/api/re/catalog/vnish-rest-endpoints",
            "/api/re/catalog/luxos-network-exposure",
            "/api/re/catalog/eeprom/bhb-skus",
            "/api/re/catalog/dspic-frames",
            "/api/re/catalog/apw-psu",
            "/api/re/catalog/fpga-registers",
            "/api/re/catalog/bb-uart-trans",
            "/api/re/catalog/bm1362-baud-init",
            "/api/re/catalog/diode-voltage",
            "/api/re/catalog/s21-fixture-production-warnings",
        ] {
            assert!(paths.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn asic_register_catalog_exposes_bm1387_and_bm1397plus() {
        let catalog = asic_register_catalog();
        assert_eq!(catalog.families.len(), 2);
        assert_eq!(catalog.families[0].family, ChipFamily::Bm1387);
        assert_eq!(catalog.families[0].registers, BM1387_REGISTERS);
        assert_eq!(catalog.families[1].family, ChipFamily::Bm1397);
        assert_eq!(catalog.families[1].registers, BM1397_REGISTERS);
    }

    #[test]
    fn asic_command_catalog_pins_family_specific_get_address_bytes() {
        let catalog = asic_command_catalog();
        let get_address = catalog
            .commands
            .iter()
            .find(|entry| entry.command == AsicCommand::GetAddress)
            .expect("GetAddress entry");

        assert_eq!(get_address.bm1387_byte, Some(0x54));
        assert_eq!(get_address.bm1397plus_byte, Some(0x52));
        assert!(get_address.broadcast);
    }

    #[test]
    fn thermal_catalog_keeps_home_safety_cap_at_pwm_30() {
        let catalog = thermal_model_catalog();
        let home = catalog
            .fan_safety_caps
            .iter()
            .find(|entry| entry.mode == FanMode::Home)
            .expect("Home fan mode");

        assert_eq!(home.max_pwm, 30);
        assert_eq!(home.safety_cap_pwm, 30);
    }

    #[tokio::test]
    async fn rest_command_catalog_exposes_all_luxos_commands_read_only() {
        let Json(body) = get_luxos_rest_commands().await;
        assert_eq!(
            body["count"].as_u64(),
            Some(dcentrald_api_types::luxos_rest_command::ALL_COMMANDS.len() as u64)
        );
        assert_eq!(body["meta"]["read_only"].as_bool(), Some(true));
        assert_eq!(body["meta"]["hardware_writes"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn vnish_catalog_marks_restore_stock_as_confirmation_required() {
        let Json(body) = get_vnish_rest_endpoints().await;
        let endpoints = body["endpoints"].as_array().expect("endpoints array");
        let restore = endpoints
            .iter()
            .find(|entry| entry["path"] == "/api/v1/restore-stock")
            .expect("restore-stock endpoint");

        assert_eq!(restore["requires_confirmation"].as_bool(), Some(true));
    }

    fn assert_json_catalog_is_read_only(body: &serde_json::Value) {
        assert_eq!(body["meta"]["read_only"].as_bool(), Some(true));
        assert_eq!(body["meta"]["hardware_reads"].as_bool(), Some(false));
        assert_eq!(body["meta"]["hardware_writes"].as_bool(), Some(false));
        assert_eq!(body["meta"]["config_writes"].as_bool(), Some(false));
        assert_eq!(body["meta"]["mining_control"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn new_static_catalog_endpoints_are_read_only_and_hal_free() {
        let bodies = vec![
            get_eeprom_bhb_skus().await.0,
            get_dspic_frames().await.0,
            get_apw_psu().await.0,
            get_fpga_registers().await.0,
            get_bb_uart_trans().await.0,
            get_bm1362_baud_init().await.0,
            get_diode_voltage().await.0,
            get_s21_fixture_production_warnings().await.0,
        ];

        for body in bodies {
            assert_json_catalog_is_read_only(&body);
        }
    }

    #[tokio::test]
    async fn eeprom_catalog_exposes_bhb428_as_bm1366() {
        let Json(body) = get_eeprom_bhb_skus().await;
        let examples = body["lookup_examples"]
            .as_array()
            .expect("lookup examples array");
        let bhb42801 = examples
            .iter()
            .find(|entry| entry["sku"] == "BHB42801")
            .expect("BHB42801 example");

        assert_eq!(bhb42801["chip_family"].as_str(), Some("BM1366"));
    }

    #[tokio::test]
    async fn bm1362_catalog_pins_forbidden_miscctrl_hazard() {
        let Json(body) = get_bm1362_baud_init().await;
        assert_eq!(
            body["forbidden_miscctrl"]["value_hex"].as_str(),
            Some("0x40C100B7")
        );
        assert_eq!(
            body["forbidden_miscctrl"]["is_forbidden"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["baud_plan"]["register_value"].as_u64(),
            Some(0x00C1_00B0)
        );
    }
}
