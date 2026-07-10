//!  luxos-E — typed LuxOS REST response payload DTOs (HAL-free).
//!
//! Source RE evidence:
//! - `luxos/79-live-2026-04-29/analysis/E-rest-api-8080.md` §3.3
//!   (command catalog).
//! - `luxos/79-live-2026-04-29/captures/09-authenticated-api.txt`
//!   (live JSON capture from `a lab unit`).
//!
//! Every LuxOS reply wraps the command-specific payload in the
//! CGMiner-style envelope (`STATUS` array + payload arrays + `id`).
//! This module ships typed payload DTOs for the most-called commands
//! so dcent-toolbox + dashboard can decode without dynamic JSON walks.

use serde::{Deserialize, Serialize};

use crate::luxos_rest_envelope::LuxosStatus;

// ---------------------------------------------------------------------------
// `version` response
// ---------------------------------------------------------------------------

/// Single VERSION block content per E-rest-api-8080.md §3.3 row
/// "version" + 09-authenticated-api.txt observed shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosVersionEntry {
    /// Miner type (e.g. "Antminer S19j Pro").
    #[serde(rename = "Type", default)]
    pub miner_type: String,
    /// CGMiner-compat API version.
    #[serde(rename = "API", default)]
    pub api: String,
    /// Mining daemon version (e.g. "LUXminer 2026.4.3.192353-6ab4e5077").
    #[serde(rename = "Miner", default)]
    pub miner: String,
    /// Compile-time stamp.
    #[serde(rename = "CompileTime", default)]
    pub compile_time: String,
}

/// `version` response envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosVersionResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "VERSION", default)]
    pub version: Vec<LuxosVersionEntry>,
    #[serde(default)]
    pub id: u64,
}

// ---------------------------------------------------------------------------
// `summary` response
// ---------------------------------------------------------------------------

/// Single SUMMARY block per RE doc + cgminer canonical fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosSummaryEntry {
    /// Real-time hashrate (5-second average) in MH/s wire form.
    /// LuxOS / cgminer report MH/s on the wire; convert to TH/s in
    /// the dashboard.
    #[serde(rename = "MHS 5s", default)]
    pub mhs_5s: f64,
    #[serde(rename = "MHS av", default)]
    pub mhs_av: f64,
    #[serde(rename = "MHS 1m", default)]
    pub mhs_1m: f64,
    #[serde(rename = "MHS 5m", default)]
    pub mhs_5m: f64,
    #[serde(rename = "MHS 15m", default)]
    pub mhs_15m: f64,
    #[serde(rename = "Accepted", default)]
    pub accepted: u64,
    #[serde(rename = "Rejected", default)]
    pub rejected: u64,
    #[serde(rename = "Hardware Errors", default)]
    pub hardware_errors: u64,
    #[serde(rename = "Elapsed", default)]
    pub elapsed: u64,
    /// Power consumption in watts (LuxOS extension).
    #[serde(rename = "Power", default)]
    pub power: f64,
}

/// `summary` response envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosSummaryResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "SUMMARY", default)]
    pub summary: Vec<LuxosSummaryEntry>,
    #[serde(default)]
    pub id: u64,
}

// ---------------------------------------------------------------------------
// `devs` response — per-board status
// ---------------------------------------------------------------------------

/// Per-board entry in `devs`/`edevs` reply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosDevEntry {
    /// Logical ASC index (0-based).
    #[serde(rename = "ASC", default)]
    pub asc: u32,
    /// Stock cgminer-style board name.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// Board ID (per-chain index 0-N).
    #[serde(rename = "ID", default)]
    pub id: u32,
    /// Board status string ("Alive" / "Dead" / "Disabled" / "Initializing").
    #[serde(rename = "Status", default)]
    pub status: String,
    /// Real-time hashrate in MH/s.
    #[serde(rename = "MHS 5s", default)]
    pub mhs_5s: f64,
    /// Average hashrate.
    #[serde(rename = "MHS av", default)]
    pub mhs_av: f64,
    /// Maximum chip temperature on this board (°C).
    #[serde(rename = "Temperature", default)]
    pub temperature: f64,
    /// Frequency in MHz (LuxOS extension).
    #[serde(rename = "Frequency", default)]
    pub frequency: f64,
    /// Voltage in mV (LuxOS extension).
    #[serde(rename = "Voltage", default)]
    pub voltage: f64,
    /// Hardware errors on this board.
    #[serde(rename = "Hardware Errors", default)]
    pub hardware_errors: u64,
}

/// `devs` response envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosDevsResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "DEVS", default)]
    pub devs: Vec<LuxosDevEntry>,
    #[serde(default)]
    pub id: u64,
}

// ---------------------------------------------------------------------------
// `fans` response
// ---------------------------------------------------------------------------

/// Per-fan entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosFanEntry {
    /// Fan index (0-based).
    #[serde(rename = "FAN", default)]
    pub fan: u32,
    /// RPM reading.
    #[serde(rename = "RPM", default)]
    pub rpm: u32,
    /// Speed percent (0-100).
    #[serde(rename = "Speed", default)]
    pub speed: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosFansResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "FANS", default)]
    pub fans: Vec<LuxosFanEntry>,
    #[serde(default)]
    pub id: u64,
}

// ---------------------------------------------------------------------------
// `tempctrl` response
// ---------------------------------------------------------------------------

/// Single TEMPCTRL entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosTempCtrlEntry {
    /// Mode string (typically "Automatic").
    #[serde(rename = "Mode", default)]
    pub mode: String,
    /// Target chip temp (°C).
    #[serde(rename = "Target", default)]
    pub target: f64,
    /// Hot threshold (°C).
    #[serde(rename = "Hot", default)]
    pub hot: f64,
    /// Panic threshold (°C).
    #[serde(rename = "Panic", default)]
    pub panic: f64,
    /// Water inlet (°C, hydro models only — 0 otherwise).
    #[serde(rename = "WaterInlet", default)]
    pub water_inlet: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosTempCtrlResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "TEMPCTRL", default)]
    pub tempctrl: Vec<LuxosTempCtrlEntry>,
    #[serde(default)]
    pub id: u64,
}

// ---------------------------------------------------------------------------
// `atm` response
// ---------------------------------------------------------------------------

/// Single ATM (Advanced Thermal Management) entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosAtmEntry {
    #[serde(rename = "Enabled", default)]
    pub enabled: bool,
    #[serde(rename = "StartupMinutes", default)]
    pub startup_minutes: u32,
    #[serde(rename = "PostRampMinutes", default)]
    pub post_ramp_minutes: u32,
    #[serde(rename = "TempWindow", default)]
    pub temp_window: f64,
    #[serde(rename = "ChipTempWindow", default)]
    pub chip_temp_window: f64,
    #[serde(rename = "MinProfile", default)]
    pub min_profile: String,
    #[serde(rename = "MaxProfile", default)]
    pub max_profile: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuxosAtmResponse {
    #[serde(rename = "STATUS", default)]
    pub status: Vec<LuxosStatus>,
    #[serde(rename = "ATM", default)]
    pub atm: Vec<LuxosAtmEntry>,
    #[serde(default)]
    pub id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::luxos_rest_envelope::{codes, LuxosStatusSeverity};

    fn ok_status(code: u16, msg: &str) -> LuxosStatus {
        LuxosStatus {
            severity: LuxosStatusSeverity::Success,
            code,
            msg: msg.to_string(),
            description: "LUXminer test".to_string(),
            when: 0,
        }
    }

    #[test]
    fn version_response_uses_capitalised_field_names() {
        // CGMiner-compat envelope: Type/API/Miner/CompileTime are
        // capitalised on the wire. Pin so a refactor doesn't lowercase.
        let v = LuxosVersionEntry {
            miner_type: "Antminer S19j Pro".into(),
            api: "3.7".into(),
            miner: "LUXminer 2026.4.3".into(),
            compile_time: "2026-04-03".into(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert!(json.get("Type").is_some());
        assert!(json.get("API").is_some());
        assert!(json.get("Miner").is_some());
        assert!(json.get("CompileTime").is_some());
        // Negative pin: snake_case must NOT appear.
        assert!(json.get("miner_type").is_none());
        assert!(json.get("api").is_none());
    }

    #[test]
    fn summary_field_names_match_cgminer_canonical() {
        let s = LuxosSummaryEntry {
            mhs_5s: 110_000.0,
            mhs_av: 109_500.0,
            mhs_1m: 110_200.0,
            mhs_5m: 110_100.0,
            mhs_15m: 110_000.0,
            accepted: 15_023,
            rejected: 5,
            hardware_errors: 12,
            elapsed: 86_400,
            power: 3_250.0,
        };
        let json = serde_json::to_value(&s).unwrap();
        // Pin every cgminer-canonical field name verbatim.
        assert!(json.get("MHS 5s").is_some());
        assert!(json.get("MHS av").is_some());
        assert!(json.get("MHS 1m").is_some());
        assert!(json.get("MHS 5m").is_some());
        assert!(json.get("MHS 15m").is_some());
        assert!(json.get("Accepted").is_some());
        assert!(json.get("Rejected").is_some());
        assert!(json.get("Hardware Errors").is_some());
        assert!(json.get("Elapsed").is_some());
        assert!(json.get("Power").is_some());
    }

    #[test]
    fn dev_entry_carries_per_board_status() {
        let d = LuxosDevEntry {
            asc: 0,
            name: "ASC".into(),
            id: 0,
            status: "Alive".into(),
            mhs_5s: 36_500.0,
            mhs_av: 36_700.0,
            temperature: 72.0,
            frequency: 675.0,
            voltage: 13800.0,
            hardware_errors: 5,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["Name"], "ASC");
        assert_eq!(json["Status"], "Alive");
        assert_eq!(json["Frequency"], 675.0);
        assert_eq!(json["Voltage"], 13800.0);
    }

    #[test]
    fn version_response_round_trips_through_serde() {
        let original = LuxosVersionResponse {
            status: vec![ok_status(codes::SESSION_INFO, "Session information")],
            version: vec![LuxosVersionEntry {
                miner_type: "Antminer S19j Pro".into(),
                api: "3.7".into(),
                miner: "LUXminer".into(),
                compile_time: "2026-04-03".into(),
            }],
            id: 1,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosVersionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn devs_response_carries_status_and_devs_arrays() {
        let r = LuxosDevsResponse {
            status: vec![ok_status(codes::SESSION_INFO, "ok")],
            devs: vec![
                LuxosDevEntry {
                    asc: 0,
                    name: "ASC".into(),
                    id: 0,
                    status: "Alive".into(),
                    ..LuxosDevEntry::default()
                },
                LuxosDevEntry {
                    asc: 1,
                    name: "ASC".into(),
                    id: 1,
                    status: "Alive".into(),
                    ..LuxosDevEntry::default()
                },
                LuxosDevEntry {
                    asc: 2,
                    name: "ASC".into(),
                    id: 2,
                    status: "Dead".into(),
                    ..LuxosDevEntry::default()
                },
            ],
            id: 2,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("STATUS").is_some());
        assert!(json.get("DEVS").is_some());
        assert_eq!(json["DEVS"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn fans_response_round_trips() {
        let original = LuxosFansResponse {
            status: vec![ok_status(
                crate::cgminer_status_codes::CgminerStatusCode::LuxFans.code(),
                "Fans",
            )],
            fans: vec![
                LuxosFanEntry {
                    fan: 0,
                    rpm: 4_200,
                    speed: 70,
                },
                LuxosFanEntry {
                    fan: 1,
                    rpm: 4_180,
                    speed: 70,
                },
            ],
            id: 3,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosFansResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn tempctrl_uses_capitalised_mode_target_hot_panic() {
        let t = LuxosTempCtrlEntry {
            mode: "Automatic".into(),
            target: 75.0,
            hot: 85.0,
            panic: 90.0,
            water_inlet: 0.0,
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("Mode").is_some());
        assert!(json.get("Target").is_some());
        assert!(json.get("Hot").is_some());
        assert!(json.get("Panic").is_some());
        assert!(json.get("WaterInlet").is_some());
    }

    #[test]
    fn atm_response_field_names_match_re_doc() {
        let a = LuxosAtmEntry {
            enabled: true,
            startup_minutes: 30,
            post_ramp_minutes: 10,
            temp_window: 5.0,
            chip_temp_window: 8.0,
            min_profile: "default".into(),
            max_profile: "+2".into(),
        };
        let json = serde_json::to_value(&a).unwrap();
        // RE doc §5.3: enabled, startup_minutes, post_ramp_minutes,
        // temp_window, chip_temp_window, min_profile, max_profile.
        // CGMiner-style — capitalised first letter per word.
        for field in [
            "Enabled",
            "StartupMinutes",
            "PostRampMinutes",
            "TempWindow",
            "ChipTempWindow",
            "MinProfile",
            "MaxProfile",
        ] {
            assert!(
                json.get(field).is_some(),
                "AtmEntry must expose '{}'",
                field
            );
        }
    }

    #[test]
    fn summary_default_initializes_to_zero_fields() {
        let s = LuxosSummaryEntry::default();
        assert_eq!(s.mhs_5s, 0.0);
        assert_eq!(s.accepted, 0);
        assert_eq!(s.rejected, 0);
        assert_eq!(s.hardware_errors, 0);
        assert_eq!(s.elapsed, 0);
        assert_eq!(s.power, 0.0);
    }

    #[test]
    fn dev_status_strings_pinned_to_canonical_words() {
        // J-web-ui.md §5.1 lists the canonical board states. Pin a few
        // common ones so a refactor can't typo them.
        for state in ["Alive", "Dead", "Initializing", "Disabled", "Disconnected"] {
            let d = LuxosDevEntry {
                status: state.to_string(),
                ..LuxosDevEntry::default()
            };
            let json = serde_json::to_value(&d).unwrap();
            assert_eq!(json["Status"], state);
        }
    }

    #[test]
    fn version_response_default_is_empty_with_zero_id() {
        let r = LuxosVersionResponse::default();
        assert!(r.status.is_empty());
        assert!(r.version.is_empty());
        assert_eq!(r.id, 0);
    }

    #[test]
    fn summary_mhs_fields_are_floats() {
        // Hashrate is fractional (e.g. 110_245.5 MH/s). Pin the type
        // so a refactor doesn't accidentally make them u64 (truncating
        // pool-reported precision).
        let s = LuxosSummaryEntry {
            mhs_5s: 110_245.5,
            ..LuxosSummaryEntry::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(json["MHS 5s"].is_f64());
        assert_eq!(json["MHS 5s"].as_f64(), Some(110_245.5));
    }
}
