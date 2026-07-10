//! `BridgeConfig` — the `[bridge]` section of `dcentrald.toml`.
//!
//! Mirrors the DCENT_OS config conventions (sibling `HomeModeConfig`):
//! `#[serde(default)]` per field with `default_*` fns, `#[serde(deny_unknown_fields)]`,
//! and a hand-written `Default` impl. A missing `[bridge]` block deserializes
//! to `BridgeConfig::default()` (enabled = false) so existing configs and
//! `validate()` are unaffected.

use serde::{Deserialize, Serialize};

/// Configuration for the DCENT Expansion Pack bridge client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Master enable. Default `false` — the task returns immediately when off,
    /// so a stock miner with no bridge does nothing.
    #[serde(default)]
    pub enabled: bool,

    /// Optional gateway IP override. When set, the discovery probe fires when
    /// the default gateway equals this value instead of `10.77.0.1`.
    #[serde(default)]
    pub gateway_override: Option<String>,

    /// Heartbeat cadence in seconds (spec §5.1).
    #[serde(default = "default_heartbeat_interval_s")]
    pub heartbeat_interval_s: u64,

    /// Telemetry poll cadence while heating (spec §3.1).
    #[serde(default = "default_telemetry_poll_heating_s")]
    pub telemetry_poll_heating_s: u64,

    /// Telemetry poll cadence while idle / monitoring (spec §3.1).
    #[serde(default = "default_telemetry_poll_idle_s")]
    pub telemetry_poll_idle_s: u64,

    /// Whether a usable external temperature is fed into the thermal PID via
    /// the shared `room_temp_c10` atomic. Default `true`.
    #[serde(default = "default_feed_thermal")]
    pub feed_thermal: bool,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            gateway_override: None,
            heartbeat_interval_s: default_heartbeat_interval_s(),
            telemetry_poll_heating_s: default_telemetry_poll_heating_s(),
            telemetry_poll_idle_s: default_telemetry_poll_idle_s(),
            feed_thermal: default_feed_thermal(),
        }
    }
}

fn default_heartbeat_interval_s() -> u64 {
    60
}

fn default_telemetry_poll_heating_s() -> u64 {
    10
}

fn default_telemetry_poll_idle_s() -> u64 {
    60
}

fn default_feed_thermal() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn bridge_config_toml_parse_never_panics_on_arbitrary_text(text in ".{0,2048}") {
            let _ = toml::from_str::<BridgeConfig>(&text);
        }
    }

    #[test]
    fn default_is_disabled_with_spec_cadences() {
        let c = BridgeConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.heartbeat_interval_s, 60);
        assert_eq!(c.telemetry_poll_heating_s, 10);
        assert_eq!(c.telemetry_poll_idle_s, 60);
        assert!(c.feed_thermal);
        assert!(c.gateway_override.is_none());
    }

    #[test]
    fn empty_toml_section_uses_defaults() {
        let c: BridgeConfig = toml::from_str("").expect("empty section is valid");
        assert!(!c.enabled);
        assert_eq!(c.heartbeat_interval_s, 60);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: BridgeConfig =
            toml::from_str("enabled = true\ngateway_override = \"10.99.0.1\"\n").expect("parse");
        assert!(c.enabled);
        assert_eq!(c.gateway_override.as_deref(), Some("10.99.0.1"));
        // Unspecified fields take their defaults.
        assert_eq!(c.telemetry_poll_heating_s, 10);
        assert!(c.feed_thermal);
    }

    #[test]
    fn unknown_field_rejected() {
        let err = toml::from_str::<BridgeConfig>("enabled = true\nbogus = 1\n");
        assert!(err.is_err(), "deny_unknown_fields should reject `bogus`");
    }
}
