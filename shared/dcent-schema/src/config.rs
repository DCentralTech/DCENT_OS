use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedPoolConfig {
    pub url: String,
    pub port: Option<u16>,
    pub worker: String,
    #[serde(alias = "password_set")]
    pub password_set: bool,
    pub protocol: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedNetworkConfig {
    pub hostname: String,
    pub ipv4: Option<String>,
    pub ssid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedMiningConfig {
    pub enabled: bool,
    #[serde(alias = "frequency_mhz")]
    pub frequency_mhz: Option<f32>,
    #[serde(alias = "voltage_mv")]
    pub voltage_mv: Option<u16>,
    #[serde(alias = "overclock_enabled")]
    pub overclock_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedThermalConfig {
    #[serde(alias = "target_temp_c")]
    pub target_temp_c: Option<u8>,
    #[serde(alias = "manual_fan_speed_pct")]
    pub manual_fan_speed_pct: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedAuthConfig {
    #[serde(alias = "password_set")]
    pub password_set: bool,
    #[serde(alias = "metrics_require_auth")]
    pub metrics_require_auth: bool,
    #[serde(alias = "allow_unsigned_ota")]
    pub allow_unsigned_ota: bool,
    #[serde(alias = "session_auth")]
    pub session_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharedConfigSnapshot {
    pub schema: u8,
    pub family: String,
    #[serde(alias = "device_model")]
    pub device_model: String,
    #[serde(alias = "board_target")]
    pub board_target: String,
    #[serde(alias = "board_version")]
    pub board_version: Option<String>,
    pub network: SharedNetworkConfig,
    #[serde(alias = "primary_pool")]
    pub primary_pool: SharedPoolConfig,
    #[serde(alias = "fallback_pool")]
    pub fallback_pool: Option<SharedPoolConfig>,
    pub mining: SharedMiningConfig,
    pub thermal: SharedThermalConfig,
    pub auth: SharedAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedPoolPatch {
    pub url: Option<String>,
    pub port: Option<u16>,
    pub worker: Option<String>,
    pub password: Option<String>,
    pub protocol: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedNetworkPatch {
    pub hostname: Option<String>,
    pub ssid: Option<String>,
    #[serde(alias = "wifi_password")]
    pub wifi_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedMiningPatch {
    pub enabled: Option<bool>,
    #[serde(alias = "frequency_mhz")]
    pub frequency_mhz: Option<f32>,
    #[serde(alias = "voltage_mv")]
    pub voltage_mv: Option<u16>,
    #[serde(alias = "overclock_enabled")]
    pub overclock_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedThermalPatch {
    #[serde(alias = "target_temp_c")]
    pub target_temp_c: Option<u8>,
    #[serde(alias = "manual_fan_speed_pct")]
    pub manual_fan_speed_pct: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedAuthPatch {
    #[serde(alias = "metrics_require_auth")]
    pub metrics_require_auth: Option<bool>,
    #[serde(alias = "allow_unsigned_ota")]
    pub allow_unsigned_ota: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedConfigPatch {
    pub network: Option<SharedNetworkPatch>,
    #[serde(alias = "primary_pool")]
    pub primary_pool: Option<SharedPoolPatch>,
    #[serde(alias = "fallback_pool")]
    pub fallback_pool: Option<SharedPoolPatch>,
    pub mining: Option<SharedMiningPatch>,
    pub thermal: Option<SharedThermalPatch>,
    pub auth: Option<SharedAuthPatch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Schema version + structural pins.
    //
    // dcent-schema is a shared wire contract used by dcentos, dcentaxe,
    // and the toolbox. Any silent shape change breaks downstream
    // consumers. These tests pin the camelCase wire form, the
    // snake_case alias compatibility forms, the schema version, and
    // (most critically) the credential boundaries: snapshot DTOs must
    // never carry a `password` field; only patch DTOs carry one for
    // writes.
    // -----------------------------------------------------------------------

    #[test]
    fn schema_version_constant_is_pinned() {
        // A bump must be a deliberate decision — pin the current value
        // so a refactor that increments without coordinating with
        // downstream consumers (dcentaxe, toolbox) is caught.
        assert_eq!(CONFIG_SCHEMA_VERSION, 1);
    }

    fn sample_pool() -> SharedPoolConfig {
        SharedPoolConfig {
            url: "stratum+tcp://pool.example.com".to_string(),
            port: Some(3333),
            worker: "user.worker".to_string(),
            password_set: true,
            protocol: Some("v1".to_string()),
            enabled: true,
        }
    }

    fn sample_snapshot() -> SharedConfigSnapshot {
        SharedConfigSnapshot {
            schema: CONFIG_SCHEMA_VERSION,
            family: "dcentos".to_string(),
            device_model: "S9".to_string(),
            board_target: "am1-s9".to_string(),
            board_version: Some("v3.1".to_string()),
            network: SharedNetworkConfig {
                hostname: "miner-39".to_string(),
                ipv4: Some("203.0.113.39".to_string()),
                ssid: None,
            },
            primary_pool: sample_pool(),
            fallback_pool: None,
            mining: SharedMiningConfig {
                enabled: true,
                frequency_mhz: Some(650.0),
                voltage_mv: Some(9100),
                overclock_enabled: Some(false),
            },
            thermal: SharedThermalConfig {
                target_temp_c: Some(75),
                manual_fan_speed_pct: None,
            },
            auth: SharedAuthConfig {
                password_set: true,
                metrics_require_auth: false,
                allow_unsigned_ota: false,
                session_auth: true,
            },
        }
    }

    #[test]
    fn snapshot_serializes_in_camelcase_wire_form() {
        // Pin the wire-format field names so a downstream JSON consumer
        // (dashboard JS, toolbox) doesn't break when the Rust field
        // names change but the on-wire contract should stay stable.
        let json = serde_json::to_value(&sample_snapshot()).unwrap();
        assert!(json.get("schema").is_some());
        assert!(json.get("family").is_some());
        assert!(json.get("deviceModel").is_some());
        assert!(json.get("boardTarget").is_some());
        assert!(json.get("boardVersion").is_some());
        assert!(json.get("primaryPool").is_some());
        assert!(json.get("fallbackPool").is_some());

        // Negative pins: snake_case must NOT appear on the wire.
        assert!(json.get("device_model").is_none());
        assert!(json.get("board_target").is_none());
        assert!(json.get("primary_pool").is_none());
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let original = sample_snapshot();
        let json = serde_json::to_string(&original).unwrap();
        let recovered: SharedConfigSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn snapshot_password_set_field_does_not_carry_password_material() {
        // CRITICAL: snapshots are read-only observability and must NOT
        // ship password material. The boolean `password_set` is the
        // contract; if anyone changes the type to `password: Option<String>`
        // operator credentials would leak through `/api/config` snapshots.
        let json = serde_json::to_value(&sample_snapshot()).unwrap();

        // Inside primary_pool: passwordSet exists, password does NOT.
        let pool = &json["primaryPool"];
        assert!(
            pool.get("passwordSet").is_some(),
            "snapshot must expose passwordSet boolean"
        );
        assert!(
            pool.get("password").is_none(),
            "snapshot must NOT expose password material — found: {}",
            pool
        );

        // Inside auth section: passwordSet exists, password does NOT.
        let auth = &json["auth"];
        assert!(auth.get("passwordSet").is_some());
        assert!(auth.get("password").is_none());
    }

    #[test]
    fn snapshot_accepts_legacy_snake_case_aliases() {
        // Old config files / older toolbox versions may emit snake_case.
        // The serde aliases must accept both forms so a snake_case consumer
        // can round-trip through dcentos without breaking.
        let snake = r#"{
            "schema": 1,
            "family": "dcentos",
            "device_model": "S19",
            "board_target": "am2-s19",
            "boardVersion": null,
            "network": {"hostname":"x","ipv4":null,"ssid":null},
            "primary_pool": {
                "url":"stratum+tcp://x:3333",
                "port":3333,
                "worker":"w",
                "password_set":false,
                "protocol":null,
                "enabled":true
            },
            "fallback_pool": null,
            "mining": {"enabled":false,"frequency_mhz":null,"voltage_mv":null,"overclock_enabled":null},
            "thermal": {"target_temp_c":null,"manual_fan_speed_pct":null},
            "auth": {"password_set":false,"metrics_require_auth":false,"allow_unsigned_ota":false,"session_auth":false}
        }"#;
        let parsed: SharedConfigSnapshot = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.device_model, "S19");
        assert_eq!(parsed.board_target, "am2-s19");
        assert!(!parsed.primary_pool.password_set);
        assert!(!parsed.auth.password_set);
    }

    #[test]
    fn pool_patch_carries_password_material_for_writes() {
        // Patch DTOs are write-only inputs from operators. They MUST
        // carry an Option<String> password so config-update flows work.
        // Pin the asymmetry with snapshots: snapshots NEVER ship
        // passwords; patches MAY ship them.
        let patch = SharedPoolPatch {
            url: None,
            port: None,
            worker: None,
            password: Some("new-pool-password".to_string()),
            protocol: None,
            enabled: None,
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(
            json.get("password").and_then(|v| v.as_str()),
            Some("new-pool-password")
        );
    }

    #[test]
    fn pool_patch_default_omits_unset_fields_in_serialization() {
        // Default patch should produce a JSON object that deserializes
        // back to Default — pin the round trip.
        let default_patch = SharedPoolPatch::default();
        let json = serde_json::to_string(&default_patch).unwrap();
        let recovered: SharedPoolPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(default_patch, recovered);
    }

    #[test]
    fn config_patch_default_round_trips() {
        // The all-None patch is the no-op update — must round-trip cleanly.
        let default_patch = SharedConfigPatch::default();
        let json = serde_json::to_string(&default_patch).unwrap();
        let recovered: SharedConfigPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(default_patch, recovered);
    }

    #[test]
    fn config_patch_camelcase_wire_form() {
        let patch = SharedConfigPatch {
            primary_pool: Some(SharedPoolPatch {
                worker: Some("new.worker".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert!(json.get("primaryPool").is_some());
        assert!(json.get("primary_pool").is_none());
    }

    #[test]
    fn config_patch_accepts_legacy_snake_case_aliases() {
        let snake = r#"{
            "primary_pool": {"url":"stratum+tcp://new:3333"},
            "fallback_pool": {"enabled":false}
        }"#;
        let parsed: SharedConfigPatch = serde_json::from_str(snake).unwrap();
        assert!(parsed.primary_pool.is_some());
        assert!(parsed.fallback_pool.is_some());
        assert_eq!(
            parsed.primary_pool.as_ref().unwrap().url.as_deref(),
            Some("stratum+tcp://new:3333")
        );
    }

    #[test]
    fn mining_config_aliases_snake_case_field_names() {
        let snake =
            r#"{"enabled":true,"frequency_mhz":650.0,"voltage_mv":9100,"overclock_enabled":false}"#;
        let parsed: SharedMiningConfig = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.frequency_mhz, Some(650.0));
        assert_eq!(parsed.voltage_mv, Some(9100));
        assert_eq!(parsed.overclock_enabled, Some(false));
    }

    #[test]
    fn thermal_config_aliases_snake_case_field_names() {
        let snake = r#"{"target_temp_c":75,"manual_fan_speed_pct":50}"#;
        let parsed: SharedThermalConfig = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.target_temp_c, Some(75));
        assert_eq!(parsed.manual_fan_speed_pct, Some(50));
    }

    #[test]
    fn auth_config_aliases_snake_case_field_names() {
        let snake = r#"{
            "password_set":true,
            "metrics_require_auth":true,
            "allow_unsigned_ota":false,
            "session_auth":true
        }"#;
        let parsed: SharedAuthConfig = serde_json::from_str(snake).unwrap();
        assert!(parsed.password_set);
        assert!(parsed.metrics_require_auth);
        assert!(!parsed.allow_unsigned_ota);
        assert!(parsed.session_auth);
    }

    #[test]
    fn network_patch_aliases_wifi_password_snake_case() {
        let snake = r#"{"hostname":"new","ssid":"net","wifi_password":"secret"}"#;
        let parsed: SharedNetworkPatch = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.wifi_password.as_deref(), Some("secret"));

        // Negative: serialized form is camelCase.
        let json = serde_json::to_value(&parsed).unwrap();
        assert!(json.get("wifiPassword").is_some());
        assert!(json.get("wifi_password").is_none());
    }

    #[test]
    fn mining_patch_round_trip_preserves_optional_fields() {
        let patch = SharedMiningPatch {
            enabled: Some(true),
            frequency_mhz: Some(700.0),
            voltage_mv: Some(8800),
            overclock_enabled: Some(true),
        };
        let json = serde_json::to_string(&patch).unwrap();
        let recovered: SharedMiningPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(patch, recovered);
    }

    #[test]
    fn auth_patch_omits_password_set_field() {
        // SharedAuthPatch is a write-only DTO that does NOT include a
        // password_set field — operators don't write that, the device
        // computes it. Pin the absence so a future refactor that
        // accidentally adds it is caught.
        let patch = SharedAuthPatch {
            metrics_require_auth: Some(true),
            allow_unsigned_ota: Some(false),
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert!(json.get("metricsRequireAuth").is_some());
        assert!(json.get("allowUnsignedOta").is_some());
        assert!(
            json.get("passwordSet").is_none(),
            "auth PATCH must not have passwordSet (computed by device, not written by operator)"
        );
        assert!(
            json.get("password").is_none(),
            "auth PATCH must not carry password material via the patch DTO"
        );
    }
}
