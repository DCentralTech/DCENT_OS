//! Wire types for the dcent-pack bridge contract.
//!
//! Field shapes are taken from the REAL firmware JSON emitters, not the spec
//! doc, wherever the two disagree:
//!
//! - The telemetry temperature object has **`external_temperature_c`** (f32)
//!   and NO `value_c` (`bridge_api.c:1369-1381`). The spec doc's "alias
//!   value_c" is on-the-wire fiction — we deserialize `external_temperature_c`.
//! - `/api/v1/health` exists and carries `product: "dcent-pack"`
//!   (`bridge_api.c:663-843`). We discriminate discovery on that field.
//! - Health does NOT expose `enrollment_open` or `bridge_name`
//!   (re-pair is driven off heartbeat `paired:false`, not health).
//!
//! Every `Deserialize` type uses `#[serde(default)]` at the container level so
//! that additive bridge fields (future waves) never break parsing — matching
//! the DCENT_OS `serde(default)` convention.

use serde::{Deserialize, Serialize};

// --------------------------------------------------------------------- /pair

/// Production `/pair` request body (HMAC-authenticated).
#[derive(Debug, Clone, Serialize)]
pub struct PairRequest {
    pub device_id: String,
    pub miner_mac: String,
    pub ts: u64,
    /// Lowercase-hex HMAC-SHA256, see [`crate::crypto::pair_hmac`].
    pub hmac: String,
    pub model: String,
    pub hostname: String,
    pub api_port: u16,
}

/// Successful `/pair` (200) response. The bridge may add fields in later
/// waves; `#[serde(default)]` keeps older clients forward-compatible.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PairResponse {
    pub ok: bool,
    pub bridge_name: String,
    pub proxy_url: String,
    pub telemetry_url: String,
}

// ----------------------------------------------------------------- heartbeat

/// `POST /api/v1/miner/heartbeat` request body.
///
/// The three required fields (`device_id` / `uptime_s` / `mode`) plus the
/// original two optionals (`miner_temperature_c` / `power_w`) are the
/// pre-Change-B shape. The remaining fields are the **V0.2 Change B** expanded
/// telemetry (`docs/V0.2_DCENTOS_CHANGES.md`, field names/units frozen in
/// `docs/MESH_MODULE.md`): all `Option` + `skip_serializing_if` so a request
/// with them all `None` serializes byte-identically to the old shape (an old
/// bridge ignores absent fields; a new bridge falls back to placeholders). This
/// keeps every compatibility-matrix pairing brick-free.
///
/// `Default` is derived only to keep call sites terse (fill the required fields,
/// `..Default::default()` for the optionals); it is never sent as-is.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HeartbeatRequest {
    pub device_id: String,
    pub uptime_s: u64,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miner_temperature_c: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_w: Option<u16>,
    // --- V0.2 Change B expanded fields (all optional, additive) -------------
    /// Pool-reported hashrate in TH/s (`RES hashrate`, `%.2f`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashrate_ths: Option<f64>,
    /// Cumulative accepted shares (`RES shares` acc half).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shares_accepted: Option<u64>,
    /// Cumulative rejected shares (`RES shares` rej half).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shares_rejected: Option<u64>,
    /// Best difficulty, free-form pre-formatted string, e.g. `"184.2M"`
    /// (`RES best_difficulty`, `%s`). Passed through verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_difficulty: Option<String>,
    /// Best-block height context (`RES block_height`, `%d`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_height: Option<u64>,
    /// Fan speed in RPM (`RES fan_speed`, `%d`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_speed_rpm: Option<u32>,
    /// COMMANDED ASIC frequency in MHz (`RES frequency`). Not a measured
    /// silicon frequency — the daemon's autotuner/config target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asic_frequency_mhz: Option<f32>,
    /// Measured ASIC voltage in V (`RES voltage`, `%.2f`). Stays `None` until a
    /// real measured source exists (MESH_MODULE.md §2) — never a commanded value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asic_voltage_v: Option<f32>,
    /// Height of a block THIS miner found — drives the mesh `found_block` BLK
    /// beacon (MESH_MODULE.md §2). Populated by W4.5 derivation, `None` for now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_found_height: Option<u64>,
}

/// Heartbeat response. `paired:false` after a 200 is the re-pair trigger
/// (`bridge_api.c` heartbeat handler — health does not expose enrollment).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HeartbeatResponse {
    pub ok: bool,
    pub paired: bool,
}

// --------------------------------------------------------------- /api/v1/health

/// `GET /api/v1/health` response — the no-auth discovery probe.
///
/// We only model the fields DCENT_OS consumes; everything else is dropped by
/// `serde(default)`. Discovery discriminates on `product == "dcent-pack"`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    /// Discovery discriminator. The genuine bridge always emits `"dcent-pack"`.
    pub product: String,
    pub uptime_s: u64,
    pub miner: HealthMiner,
}

/// `miner` sub-object of [`HealthResponse`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HealthMiner {
    pub paired: bool,
}

impl HealthResponse {
    /// True when this response identifies a genuine dcent-pack bridge.
    pub fn is_dcent_pack(&self) -> bool {
        self.product == "dcent-pack"
    }
}

// -------------------------------------------------------------- /api/v1/telemetry

/// Temperature sub-object of bridge telemetry.
///
/// NOTE: the field is `external_temperature_c`. There is NO `value_c` on the
/// wire (`bridge_api.c:1369-1381`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BridgeTemperature {
    pub sensor: String,
    pub present: bool,
    /// `"ok" | "missing" | "stale" | "fault"`.
    pub status: String,
    pub external_temperature_c: f32,
    pub last_sample_age_ms: u64,
}

/// `accessories.temperature_feedback` sub-object.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TemperatureFeedback {
    pub enabled: bool,
}

/// `accessories` sub-object (only the part DCENT_OS reads).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BridgeAccessories {
    pub temperature_feedback: TemperatureFeedback,
}

/// `pairing` sub-object of telemetry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TelemetryPairing {
    pub paired: bool,
}

/// `GET <telemetry_url>` response (subset DCENT_OS consumes).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BridgeTelemetry {
    pub ok: bool,
    pub bridge_name: String,
    pub firmware_version: String,
    pub temperature: BridgeTemperature,
    pub accessories: BridgeAccessories,
    pub pairing: TelemetryPairing,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn bridge_json_protocol_decoders_never_panic_on_arbitrary_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..4096)
        ) {
            let _ = serde_json::from_slice::<BridgeTelemetry>(&data);
            let _ = serde_json::from_slice::<HealthResponse>(&data);
            let _ = serde_json::from_slice::<HeartbeatResponse>(&data);
            let _ = serde_json::from_slice::<PairResponse>(&data);
        }
    }

    // A telemetry body copied from the real firmware JSON emitter shape
    // (bridge_api.c:1357-1381). Trimmed to the fields above plus a couple of
    // extra sub-objects to prove forward-compat tolerance.
    const REAL_TELEMETRY: &str = r#"{
        "ok": true,
        "bridge_name": "dcent-pack-1234",
        "firmware_version": "0.1.0-dev",
        "wifi": {"state": "connected", "rssi_dbm": -55, "ssid_hash": null},
        "ethernet": {"link": true, "client_ip": "10.77.0.2"},
        "router": {"nat_enabled": true},
        "network": {"dhcp": {"lease_count": 1}},
        "pairing": {"paired": true, "device_id": "dcentos-test", "miner_ip": "10.77.0.2", "api_port": 80},
        "miner": {"last_seen_age_ms": 1200, "mode": "mining_heating", "reported_temperature_c": 62.5, "uptime_s": 3600},
        "mining": {"power_w": 3250},
        "time": {"synced": true, "epoch_s": 1735689600, "synced_at_uptime_s": 12},
        "temperature": {
            "sensor": "TMP102",
            "present": true,
            "status": "ok",
            "external_temperature_c": 23.44,
            "last_sample_age_ms": 850
        },
        "accessories": {
            "oled": {"enabled": false, "profile": "standard", "page": "home"},
            "touch": {"enabled": false, "transport": "bap_uart", "brightness_percent": 0, "rail_v": 3.3, "last_event": null},
            "temperature_feedback": {"enabled": true}
        }
    }"#;

    #[test]
    fn telemetry_parses_external_temperature_c() {
        let t: BridgeTelemetry = serde_json::from_str(REAL_TELEMETRY).expect("parse telemetry");
        assert!(t.ok);
        assert_eq!(t.bridge_name, "dcent-pack-1234");
        assert_eq!(t.firmware_version, "0.1.0-dev");
        assert_eq!(t.temperature.status, "ok");
        assert!((t.temperature.external_temperature_c - 23.44).abs() < 0.001);
        assert_eq!(t.temperature.last_sample_age_ms, 850);
        assert_eq!(t.temperature.sensor, "TMP102");
        assert!(t.temperature.present);
        assert!(t.accessories.temperature_feedback.enabled);
        assert!(t.pairing.paired);
    }

    #[test]
    fn telemetry_has_no_value_c_field() {
        // Confirm the on-the-wire schema uses external_temperature_c only:
        // a body that ONLY has value_c must leave external_temperature_c at
        // its serde default (0.0), proving we are NOT reading value_c.
        let body = r#"{"temperature":{"status":"ok","value_c":99.9,"last_sample_age_ms":10}}"#;
        let t: BridgeTelemetry = serde_json::from_str(body).expect("parse");
        assert_eq!(t.temperature.external_temperature_c, 0.0);
        assert_eq!(t.temperature.status, "ok");
    }

    #[test]
    fn health_parses_product_discriminator() {
        let body = r#"{
            "ok": true,
            "version": "dcent-pack-0.1.0-dev",
            "product": "dcent-pack",
            "uptime_s": 12345,
            "heap": {"free": 100000},
            "miner": {"paired": false, "last_seen_age_s": null}
        }"#;
        let h: HealthResponse = serde_json::from_str(body).expect("parse health");
        assert!(h.is_dcent_pack());
        assert_eq!(h.version, "dcent-pack-0.1.0-dev");
        assert_eq!(h.uptime_s, 12345);
        assert!(!h.miner.paired);
    }

    #[test]
    fn health_non_dcent_pack_rejected() {
        // A random router that happens to serve JSON must NOT look like a bridge.
        let body = r#"{"ok": true, "product": "some-router", "version": "1.2"}"#;
        let h: HealthResponse = serde_json::from_str(body).expect("parse");
        assert!(!h.is_dcent_pack());
    }

    #[test]
    fn heartbeat_response_paired_false() {
        let body = r#"{"ok": true, "paired": false}"#;
        let h: HeartbeatResponse = serde_json::from_str(body).expect("parse");
        assert!(h.ok);
        assert!(!h.paired);
    }

    #[test]
    fn heartbeat_request_omits_none_optionals() {
        let req = HeartbeatRequest {
            device_id: "d".into(),
            uptime_s: 10,
            mode: "idle".into(),
            miner_temperature_c: None,
            power_w: None,
            ..Default::default()
        };
        let j = serde_json::to_string(&req).expect("serialize");
        assert!(!j.contains("miner_temperature_c"));
        assert!(!j.contains("power_w"));
        assert!(j.contains("device_id"));
    }

    #[test]
    fn heartbeat_request_includes_some_optionals() {
        let req = HeartbeatRequest {
            device_id: "d".into(),
            uptime_s: 10,
            mode: "mining_heating".into(),
            miner_temperature_c: Some(62.5),
            power_w: Some(3250),
            ..Default::default()
        };
        let j = serde_json::to_string(&req).expect("serialize");
        assert!(j.contains("\"miner_temperature_c\":62.5"));
        assert!(j.contains("\"power_w\":3250"));
    }

    #[test]
    fn heartbeat_request_all_change_b_none_is_byte_identical_to_legacy_shape() {
        // Old-wire compatibility: with every Change-B field None, the serialized
        // bytes MUST equal the pre-Change-B shape (device_id/uptime_s/mode +
        // the two original optionals, in declaration order). This is the
        // no-brick guarantee for old-bridge + new-DCENT_OS.
        let req = HeartbeatRequest {
            device_id: "dcentos-test".into(),
            uptime_s: 3600,
            mode: "mining_heating".into(),
            miner_temperature_c: Some(62.5),
            power_w: Some(3250),
            ..Default::default()
        };
        let got = serde_json::to_string(&req).expect("serialize");
        let legacy = r#"{"device_id":"dcentos-test","uptime_s":3600,"mode":"mining_heating","miner_temperature_c":62.5,"power_w":3250}"#;
        assert_eq!(got, legacy);
    }

    #[test]
    fn heartbeat_request_fully_populated_contains_every_change_b_key() {
        let req = HeartbeatRequest {
            device_id: "d".into(),
            uptime_s: 1,
            mode: "mining".into(),
            miner_temperature_c: Some(60.0),
            power_w: Some(3000),
            hashrate_ths: Some(21.3),
            shares_accepted: Some(12044),
            shares_rejected: Some(7),
            best_difficulty: Some("184.2M".into()),
            block_height: Some(873221),
            fan_speed_rpm: Some(5400),
            asic_frequency_mhz: Some(525.0),
            asic_voltage_v: Some(1.15),
            block_found_height: Some(873222),
        };
        let j = serde_json::to_string(&req).expect("serialize");
        for key in [
            "hashrate_ths",
            "shares_accepted",
            "shares_rejected",
            "best_difficulty",
            "block_height",
            "fan_speed_rpm",
            "asic_frequency_mhz",
            "asic_voltage_v",
            "block_found_height",
        ] {
            assert!(
                j.contains(&format!("\"{key}\"")),
                "expected key `{key}` in serialized heartbeat: {j}"
            );
        }
    }

    #[test]
    fn heartbeat_request_best_difficulty_passes_free_form_string_through() {
        // best_difficulty is a pre-formatted string (e.g. "184.2M") — the daemon
        // must not coerce/parse it; it goes on the wire verbatim.
        let req = HeartbeatRequest {
            device_id: "d".into(),
            uptime_s: 1,
            mode: "mining".into(),
            best_difficulty: Some("184.2M".into()),
            ..Default::default()
        };
        let j = serde_json::to_string(&req).expect("serialize");
        assert!(j.contains(r#""best_difficulty":"184.2M""#));
    }

    #[test]
    fn pair_response_round_trip() {
        let body = r#"{
            "ok": true,
            "bridge_name": "dcent-pack-1234",
            "proxy_url": "http://dcent-pack-1234.local/",
            "telemetry_url": "http://10.77.0.1/api/v1/telemetry"
        }"#;
        let p: PairResponse = serde_json::from_str(body).expect("parse pair response");
        assert!(p.ok);
        assert_eq!(p.bridge_name, "dcent-pack-1234");
        assert_eq!(p.proxy_url, "http://dcent-pack-1234.local/");
        assert_eq!(p.telemetry_url, "http://10.77.0.1/api/v1/telemetry");
    }
}
