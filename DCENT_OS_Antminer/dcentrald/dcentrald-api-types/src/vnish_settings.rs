//!  vnish-D — VNish typed REST response payloads (HAL-free).
//!
//! Source RE evidence:
//!  §3.1
//! (REST endpoint catalog with verbatim JSON examples).
//!
//!  vnish-A pinned the endpoint catalog (`/api/v1/...`). This
//! module ships the typed payload shapes for the most-used endpoints:
//! - `VnishSettingsResponse` ← `GET /api/v1/settings`
//! - `VnishChipsResponse` ← `GET /api/v1/chips`
//! - `VnishFactoryInfoResponse` ← `GET /api/v1/factory-info`
//!
//! Field names match the verbatim JSON examples from VNISH_REVERSE_ENGINEERING.md
//! lines 487-672. dcent-toolbox uses these for parity comparisons; the
//! competitive-readiness widget reads them to render "VNish features
//! supported by DCENT_OS" tables.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Settings response
// ---------------------------------------------------------------------------

/// `GET /api/v1/settings` response shape per VNISH_REVERSE_ENGINEERING.md
/// lines 580-597. Field names match the wire form verbatim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishSettingsResponse {
    /// Active overclock preset name (e.g. `"S19Pro-Performance"`).
    #[serde(default)]
    pub preset: String,
    /// Mode label (`"autotune"` / `"manual"`).
    #[serde(default)]
    pub mode: String,
    /// Power-limit watts. `None` = no limit.
    #[serde(default)]
    pub power_limit: Option<u32>,
    /// Cooling mode (`"air"` / `"hydro"` / `"immersion"`).
    #[serde(default)]
    pub cooling_mode: String,
    /// Fan mode (`"auto"` / `"manual"`).
    #[serde(default)]
    pub fan_mode: String,
    /// Current fan speed percent (0-100).
    #[serde(default)]
    pub fan_speed: u8,
    /// Minimum allowed fan speed (0-100).
    #[serde(default)]
    pub fan_min: u8,
    /// Maximum allowed fan speed (0-100).
    #[serde(default)]
    pub fan_max: u8,
    /// Target chip temperature (°C).
    #[serde(default)]
    pub temp_target: i16,
    /// Critical chip temperature trip (°C).
    #[serde(default)]
    pub temp_critical: i16,
    /// Startup mining delay (seconds).
    #[serde(default)]
    pub startup_delay: u32,
    /// Minimum boards required for mining to start.
    #[serde(default)]
    pub min_boards: u8,
    /// Immersion-cooling mode flag.
    #[serde(default)]
    pub immersion_mode: bool,
    /// Operator override — ignore on-chip temperature sensors.
    ///: dangerous; use with care.
    #[serde(default)]
    pub ignore_chip_temp_sensors: bool,
    /// Operator override — skip broken temperature sensors silently.
    #[serde(default)]
    pub skip_broken_temp_sensors: bool,
}

// ---------------------------------------------------------------------------
// Chips response
// ---------------------------------------------------------------------------

/// Per-chip status entry inside `VnishChipsResponse`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishChipEntry {
    /// 0-based chip index on this board.
    #[serde(default)]
    pub id: u32,
    /// Chip-internal frequency in MHz.
    #[serde(default)]
    pub frequency: f32,
    /// Real-time hashrate (TH/s).
    #[serde(default)]
    pub hashrate: f32,
    /// Chip temperature (°C).
    #[serde(default)]
    pub temperature: f32,
    /// Hardware errors observed.
    #[serde(default)]
    pub hw_errors: u64,
    /// Status string (`"ok"` / `"throttled"` / `"failed"`).
    #[serde(default)]
    pub status: String,
    /// True iff thermal-throttled.
    #[serde(default)]
    pub throttled: bool,
    /// Chip power consumption in watts.
    #[serde(default)]
    pub power_consumption: f32,
}

/// Per-board entry — N chips per board.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishChainEntry {
    /// 0-based board index.
    #[serde(default)]
    pub index: u32,
    /// All chips on this board.
    #[serde(default)]
    pub chips: Vec<VnishChipEntry>,
}

/// `GET /api/v1/chips` response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishChipsResponse {
    /// One entry per detected hashboard.
    #[serde(default)]
    pub chains: Vec<VnishChainEntry>,
}

// ---------------------------------------------------------------------------
// Factory info response
// ---------------------------------------------------------------------------

/// `GET /api/v1/factory-info` response. Carries the factory-burned
/// identifiers operator sees on the unit's serial sticker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishFactoryInfoResponse {
    /// Miner model (e.g. `"S19 Pro"`).
    #[serde(default)]
    pub model: String,
    /// Miner serial number.
    #[serde(default)]
    pub serial: String,
    /// True iff the unit has PIC voltage controllers.
    ///: S21 family is NoPic.
    #[serde(default)]
    pub has_pics: bool,
    /// PSU model (`"APW12"`, `"APW3"`, etc.).
    #[serde(default)]
    pub psu_model: String,
    /// PSU serial number.
    #[serde(default)]
    pub psu_serial: String,
    /// Control-board type (`"AML"` / `"BB"` / `"Zynq"` / `"CV"`).
    #[serde(default)]
    pub board_type: String,
    /// MAC address.
    #[serde(default)]
    pub mac: String,
}

// ---------------------------------------------------------------------------
// Info response (`GET /api/v1/info`)
// ---------------------------------------------------------------------------

/// `GET /api/v1/info` response per VNISH_REVERSE_ENGINEERING.md lines
/// 487-501.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishInfoResponse {
    #[serde(default)]
    pub miner_type: String,
    #[serde(default)]
    pub firmware_version: String,
    #[serde(default)]
    pub build_uuid: String,
    #[serde(default)]
    pub build_name: String,
    #[serde(default)]
    pub mac_address: String,
    #[serde(default)]
    pub ip_address: String,
    #[serde(default)]
    pub uptime: u64,
    #[serde(default)]
    pub hostname: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_response_field_names_match_re_doc() {
        // VNISH_REVERSE_ENGINEERING.md lines 580-597: every field name
        // is verbatim from the JSON example. Pin so a refactor doesn't
        // accidentally rename `temp_target` → `target_temperature`.
        let s = VnishSettingsResponse {
            preset: "S19Pro-Performance".into(),
            mode: "autotune".into(),
            power_limit: None,
            cooling_mode: "air".into(),
            fan_mode: "auto".into(),
            fan_speed: 80,
            fan_min: 20,
            fan_max: 100,
            temp_target: 75,
            temp_critical: 87,
            startup_delay: 0,
            min_boards: 3,
            immersion_mode: false,
            ignore_chip_temp_sensors: false,
            skip_broken_temp_sensors: false,
        };
        let json = serde_json::to_value(&s).unwrap();
        for field in [
            "preset",
            "mode",
            "power_limit",
            "cooling_mode",
            "fan_mode",
            "fan_speed",
            "fan_min",
            "fan_max",
            "temp_target",
            "temp_critical",
            "startup_delay",
            "min_boards",
            "immersion_mode",
            "ignore_chip_temp_sensors",
            "skip_broken_temp_sensors",
        ] {
            assert!(
                json.get(field).is_some(),
                "VnishSettingsResponse missing field {}",
                field
            );
        }
    }

    #[test]
    fn settings_decodes_canonical_re_doc_example() {
        // Decode the exact JSON example from VNISH_REVERSE_ENGINEERING.md
        // lines 581-597 (paraphrased — the original has trailing
        // booleans the wire form may or may not include).
        let raw = r#"{
            "preset":"S19Pro-Performance",
            "mode":"autotune",
            "power_limit":null,
            "cooling_mode":"air",
            "fan_mode":"auto",
            "fan_speed":80,
            "fan_min":20,
            "fan_max":100,
            "temp_target":75,
            "temp_critical":87,
            "startup_delay":0,
            "min_boards":3,
            "immersion_mode":false,
            "ignore_chip_temp_sensors":false,
            "skip_broken_temp_sensors":false
        }"#;
        let s: VnishSettingsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(s.preset, "S19Pro-Performance");
        assert_eq!(s.mode, "autotune");
        assert!(s.power_limit.is_none());
        assert_eq!(s.fan_speed, 80);
        assert_eq!(s.temp_target, 75);
        assert_eq!(s.min_boards, 3);
        assert!(!s.immersion_mode);
    }

    #[test]
    fn chips_response_carries_chains_array_with_chip_arrays() {
        let r = VnishChipsResponse {
            chains: vec![VnishChainEntry {
                index: 0,
                chips: vec![
                    VnishChipEntry {
                        id: 0,
                        frequency: 650.0,
                        hashrate: 0.49,
                        temperature: 72.0,
                        hw_errors: 0,
                        status: "ok".into(),
                        throttled: false,
                        power_consumption: 12.5,
                    },
                    VnishChipEntry {
                        id: 1,
                        status: "throttled".into(),
                        throttled: true,
                        ..VnishChipEntry::default()
                    },
                ],
            }],
        };
        let json = serde_json::to_value(&r).unwrap();
        let chains = json["chains"].as_array().unwrap();
        assert_eq!(chains.len(), 1);
        let chips = chains[0]["chips"].as_array().unwrap();
        assert_eq!(chips.len(), 2);
        assert_eq!(chips[0]["status"], "ok");
        assert_eq!(chips[1]["status"], "throttled");
        assert_eq!(chips[1]["throttled"], true);
    }

    #[test]
    fn factory_info_response_field_names_match_re_doc() {
        // VNISH_REVERSE_ENGINEERING.md lines 660-670.
        let f = VnishFactoryInfoResponse {
            model: "S19 Pro".into(),
            serial: "ABC123".into(),
            has_pics: true,
            psu_model: "APW12".into(),
            psu_serial: "DEF456".into(),
            board_type: "AML".into(),
            mac: "AA:BB:CC:DD:EE:FF".into(),
        };
        let json = serde_json::to_value(&f).unwrap();
        for field in [
            "model",
            "serial",
            "has_pics",
            "psu_model",
            "psu_serial",
            "board_type",
            "mac",
        ] {
            assert!(
                json.get(field).is_some(),
                "VnishFactoryInfoResponse missing field {}",
                field
            );
        }
    }

    #[test]
    fn factory_info_carries_has_pics_for_s21_nopic_distinction() {
        // S21 family is NoPic — has_pics=false. S19 Pro has PICs.
        let s21 = VnishFactoryInfoResponse {
            model: "S21".into(),
            has_pics: false,
            board_type: "AML".into(),
            ..VnishFactoryInfoResponse::default()
        };
        let s19_pro = VnishFactoryInfoResponse {
            model: "S19 Pro".into(),
            has_pics: true,
            board_type: "AML".into(),
            ..VnishFactoryInfoResponse::default()
        };
        assert!(!s21.has_pics);
        assert!(s19_pro.has_pics);
    }

    #[test]
    fn info_response_round_trips_through_serde() {
        let original = VnishInfoResponse {
            miner_type: "Antminer S19 Pro".into(),
            firmware_version: "1.2.7".into(),
            build_uuid: "abc123".into(),
            build_name: "vnish-s19pro-1.2.7-autotune".into(),
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            ip_address: "203.0.113.100".into(),
            uptime: 86_400,
            hostname: "miner-001".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: VnishInfoResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn settings_default_initializes_safely() {
        let s = VnishSettingsResponse::default();
        assert!(s.preset.is_empty());
        assert!(s.power_limit.is_none());
        assert_eq!(s.fan_speed, 0);
        assert_eq!(s.temp_target, 0);
        assert!(!s.immersion_mode);
        assert!(!s.ignore_chip_temp_sensors);
    }

    #[test]
    fn chip_entry_default_initializes_safely() {
        let c = VnishChipEntry::default();
        assert_eq!(c.id, 0);
        assert_eq!(c.frequency, 0.0);
        assert_eq!(c.hashrate, 0.0);
        assert!(!c.throttled);
        assert!(c.status.is_empty());
    }

    #[test]
    fn chips_response_default_is_empty_chain_list() {
        let r = VnishChipsResponse::default();
        assert!(r.chains.is_empty());
    }

    #[test]
    fn settings_uses_snake_case_field_names_not_camel() {
        // Pin negative: NO camelCase fields.
        let s = VnishSettingsResponse::default();
        let json = serde_json::to_value(&s).unwrap();
        for forbidden in [
            "fanSpeed",
            "fanMin",
            "fanMax",
            "tempTarget",
            "tempCritical",
            "startupDelay",
            "minBoards",
            "immersionMode",
            "ignoreChipTempSensors",
            "skipBrokenTempSensors",
        ] {
            assert!(
                json.get(forbidden).is_none(),
                "VnishSettingsResponse must NOT use camelCase {}",
                forbidden
            );
        }
    }

    #[test]
    fn chip_entry_throttled_field_is_bool() {
        let c = VnishChipEntry {
            throttled: true,
            ..VnishChipEntry::default()
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(json["throttled"].is_boolean());
        assert_eq!(json["throttled"], true);
    }

    #[test]
    fn factory_info_round_trips_through_serde() {
        let original = VnishFactoryInfoResponse {
            model: "S21".into(),
            serial: "S21-XYZ".into(),
            has_pics: false,
            psu_model: "APW17".into(),
            psu_serial: "APW17-001".into(),
            board_type: "AML".into(),
            mac: "00:11:22:33:44:55".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: VnishFactoryInfoResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }
}
