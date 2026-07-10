//!  braiins-F — BraiinsOS+ NetworkService DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §9 NetworkService method table (lines 1165-1170) +
//! Appendix B gRPC API Changelog: GetNetworkInfo introduced v1.1.0
//! (line 2046).
//!
//!  braiins-A `braiinsos_grpc_catalog.rs` shipped the method
//! catalog; this module ships the typed payloads.
//!
//! Methods covered:
//! - `GetNetworkConfiguration` — current DHCP/static config + hostname.
//! - `SetNetworkConfiguration` — update DHCP/static config + hostname.
//! - `GetNetworkInfo` (v1.1.0+) — runtime interface state (MAC,
//!   IPv4/IPv6 addresses, gateway, DNS, kernel version).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// NetworkConfiguration message
// ---------------------------------------------------------------------------

/// Network mode per `GetNetworkConfiguration` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NetworkMode {
    /// Address obtained via DHCP.
    Dhcp,
    /// Operator-set static IP.
    Static,
}

/// `NetworkConfiguration` message — covers both DHCP and Static modes.
/// Static-only fields (`static_ip`, `netmask`, `gateway`) are
/// `Option<String>` so they can be absent in DHCP mode.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NetworkConfiguration {
    /// Mode selector.
    pub mode: Option<NetworkMode>,
    /// Operator-set hostname.
    pub hostname: String,
    /// Static IPv4 address (e.g. "203.0.113.100"). Required iff
    /// `mode == Static`.
    pub static_ip: Option<String>,
    /// Static IPv4 netmask (e.g. "255.255.255.0").
    pub netmask: Option<String>,
    /// Default gateway (e.g. "203.0.113.1").
    pub gateway: Option<String>,
    /// DNS servers in priority order.
    pub dns_servers: Vec<String>,
}

impl NetworkConfiguration {
    /// True iff every required field is populated for the selected mode.
    pub fn is_well_formed(&self) -> bool {
        // Hostname must always be present.
        if self.hostname.is_empty() {
            return false;
        }
        match self.mode {
            None => false,
            Some(NetworkMode::Dhcp) => true,
            Some(NetworkMode::Static) => {
                self.static_ip.is_some() && self.netmask.is_some() && self.gateway.is_some()
            }
        }
    }

    /// True iff the hostname is within the documented length budget
    /// (RFC 1035 §2.3.4: 63-byte label, 253-byte FQDN).
    pub fn hostname_length_ok(&self) -> bool {
        // Single-label hostname max = 63 chars; FQDN max = 253.
        // Bitmain stock sets a single label, so 63 is the practical
        // upper bound. We use 253 as the conservative limit.
        !self.hostname.is_empty() && self.hostname.len() <= MAX_HOSTNAME_LEN
    }
}

/// Practical upper bound on hostname length (RFC 1035 FQDN limit).
pub const MAX_HOSTNAME_LEN: usize = 253;

// ---------------------------------------------------------------------------
// NetworkInfo message (GetNetworkInfo, v1.1.0+)
// ---------------------------------------------------------------------------

/// `NetworkInfo` reply from `GetNetworkInfo` (v1.1.0+) — runtime
/// interface state.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NetworkInfo {
    /// Hardware MAC address (e.g. "AA:BB:CC:DD:EE:FF").
    pub mac: String,
    /// IPv4 addresses currently bound (multiple if multihomed).
    pub ipv4_addresses: Vec<String>,
    /// IPv6 addresses currently bound.
    pub ipv6_addresses: Vec<String>,
    /// Active default gateway.
    pub gateway: Option<String>,
    /// DNS servers currently in use.
    pub dns_servers: Vec<String>,
    /// Linux kernel version string (e.g. "4.14.95"). Introduced in
    /// v1.1.0 alongside GetNetworkInfo per Appendix B changelog.
    pub kernel_version: Option<String>,
}

/// First BraiinsOS+ minor version that introduced GetNetworkInfo +
/// the `kernel_version` field per Appendix B (line 2046).
pub const NETWORK_INFO_INTRODUCED_VERSION: &str = "1.1.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_mode_serializes_in_screaming_snake_case() {
        // Proto3 wire form — pin both variants.
        assert_eq!(
            serde_json::to_string(&NetworkMode::Dhcp).unwrap(),
            "\"DHCP\""
        );
        assert_eq!(
            serde_json::to_string(&NetworkMode::Static).unwrap(),
            "\"STATIC\""
        );
    }

    #[test]
    fn network_mode_round_trips_through_serde() {
        for mode in [NetworkMode::Dhcp, NetworkMode::Static] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: NetworkMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn dhcp_config_does_not_require_static_fields() {
        let cfg = NetworkConfiguration {
            mode: Some(NetworkMode::Dhcp),
            hostname: "miner-001".to_string(),
            static_ip: None,
            netmask: None,
            gateway: None,
            dns_servers: vec![],
        };
        assert!(cfg.is_well_formed());
    }

    #[test]
    fn static_config_requires_ip_netmask_gateway() {
        // All three present → well-formed.
        let good = NetworkConfiguration {
            mode: Some(NetworkMode::Static),
            hostname: "miner-001".to_string(),
            static_ip: Some("203.0.113.100".to_string()),
            netmask: Some("255.255.255.0".to_string()),
            gateway: Some("203.0.113.1".to_string()),
            dns_servers: vec!["1.1.1.1".to_string()],
        };
        assert!(good.is_well_formed());

        // Missing static_ip → not well-formed.
        let no_ip = NetworkConfiguration {
            static_ip: None,
            ..good.clone()
        };
        assert!(!no_ip.is_well_formed());

        // Missing netmask → not well-formed.
        let no_mask = NetworkConfiguration {
            netmask: None,
            ..good.clone()
        };
        assert!(!no_mask.is_well_formed());

        // Missing gateway → not well-formed.
        let no_gw = NetworkConfiguration {
            gateway: None,
            ..good
        };
        assert!(!no_gw.is_well_formed());
    }

    #[test]
    fn empty_hostname_is_not_well_formed() {
        let cfg = NetworkConfiguration {
            mode: Some(NetworkMode::Dhcp),
            hostname: String::new(),
            ..NetworkConfiguration::default()
        };
        assert!(!cfg.is_well_formed());
    }

    #[test]
    fn missing_mode_is_not_well_formed() {
        let cfg = NetworkConfiguration {
            mode: None,
            hostname: "miner-001".to_string(),
            ..NetworkConfiguration::default()
        };
        assert!(!cfg.is_well_formed());
    }

    #[test]
    fn hostname_length_validates_against_rfc_1035_limit() {
        let mut cfg = NetworkConfiguration {
            mode: Some(NetworkMode::Dhcp),
            hostname: "a".repeat(253),
            ..NetworkConfiguration::default()
        };
        assert!(cfg.hostname_length_ok());

        cfg.hostname = "a".repeat(254);
        assert!(!cfg.hostname_length_ok());

        cfg.hostname = String::new();
        assert!(!cfg.hostname_length_ok());

        cfg.hostname = "miner-001".to_string();
        assert!(cfg.hostname_length_ok());
    }

    #[test]
    fn max_hostname_len_pinned_to_rfc_1035() {
        // RFC 1035 §2.3.4: 253-byte FQDN.
        assert_eq!(MAX_HOSTNAME_LEN, 253);
    }

    #[test]
    fn network_configuration_round_trips_through_serde() {
        let original = NetworkConfiguration {
            mode: Some(NetworkMode::Static),
            hostname: "test-miner".to_string(),
            static_ip: Some("203.0.113.50".to_string()),
            netmask: Some("255.255.255.0".to_string()),
            gateway: Some("203.0.113.1".to_string()),
            dns_servers: vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: NetworkConfiguration = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn network_info_round_trips_through_serde() {
        let original = NetworkInfo {
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
            ipv4_addresses: vec!["203.0.113.100".to_string()],
            ipv6_addresses: vec!["fe80::1".to_string()],
            gateway: Some("203.0.113.1".to_string()),
            dns_servers: vec!["1.1.1.1".to_string()],
            kernel_version: Some("4.14.95".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: NetworkInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn network_info_introduced_version_pinned_to_1_1_0() {
        // Per RE doc Appendix B (line 2046): "1.1.0 | 2024-05-09 |
        // NetworkService.GetNetworkInfo, kernel_version".
        assert_eq!(NETWORK_INFO_INTRODUCED_VERSION, "1.1.0");
    }

    #[test]
    fn network_info_default_is_empty_safe() {
        let info = NetworkInfo::default();
        assert!(info.mac.is_empty());
        assert!(info.ipv4_addresses.is_empty());
        assert!(info.ipv6_addresses.is_empty());
        assert!(info.gateway.is_none());
        assert!(info.dns_servers.is_empty());
        assert!(info.kernel_version.is_none());
    }

    #[test]
    fn network_configuration_default_is_empty_dhcp_unknown() {
        let cfg = NetworkConfiguration::default();
        assert!(cfg.mode.is_none()); // unknown — must be set explicitly
        assert!(cfg.hostname.is_empty());
        assert!(cfg.static_ip.is_none());
        assert!(cfg.netmask.is_none());
        assert!(cfg.gateway.is_none());
        assert!(cfg.dns_servers.is_empty());
        assert!(!cfg.is_well_formed());
    }

    #[test]
    fn dns_servers_preserve_priority_order() {
        // dns_servers is Vec<String> (ordered). Pin via round-trip
        // that two DNS entries come back in the same order.
        let cfg = NetworkConfiguration {
            mode: Some(NetworkMode::Dhcp),
            hostname: "x".to_string(),
            dns_servers: vec![
                "1.1.1.1".to_string(),
                "9.9.9.9".to_string(),
                "8.8.8.8".to_string(),
            ],
            ..NetworkConfiguration::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: NetworkConfiguration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dns_servers[0], "1.1.1.1");
        assert_eq!(back.dns_servers[1], "9.9.9.9");
        assert_eq!(back.dns_servers[2], "8.8.8.8");
    }

    #[test]
    fn ipv4_and_ipv6_address_lists_are_separate() {
        // GetNetworkInfo returns IPv4 + IPv6 separately. Pin via
        // round-trip and field-name pinning.
        let info = NetworkInfo {
            mac: "0:1:2:3:4:5".into(),
            ipv4_addresses: vec!["203.0.113.1".into()],
            ipv6_addresses: vec!["::1".into()],
            ..NetworkInfo::default()
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("ipv4_addresses").is_some());
        assert!(json.get("ipv6_addresses").is_some());
        assert_eq!(json["ipv4_addresses"][0], "203.0.113.1");
        assert_eq!(json["ipv6_addresses"][0], "::1");
    }

    #[test]
    fn config_uses_snake_case_field_names_no_camel() {
        let cfg = NetworkConfiguration {
            mode: Some(NetworkMode::Static),
            hostname: "x".into(),
            static_ip: Some("1.1.1.1".into()),
            netmask: Some("255.255.255.0".into()),
            gateway: Some("1.1.1.0".into()),
            dns_servers: vec![],
        };
        let json = serde_json::to_value(&cfg).unwrap();
        // Pin every snake_case field.
        for field in [
            "mode",
            "hostname",
            "static_ip",
            "netmask",
            "gateway",
            "dns_servers",
        ] {
            assert!(
                json.get(field).is_some(),
                "NetworkConfiguration must expose {}",
                field
            );
        }
        // No camelCase forms.
        assert!(json.get("staticIp").is_none());
        assert!(json.get("dnsServers").is_none());
    }
}
