//!  luxos-K — LuxOS network attack-surface catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §11 (lines 399-417). Live `a lab unit` capture from 2026-04-29 of
//! LuxOS 1.38.1.
//!
//! Documents the 7 listening ports + their auth model + TLS posture
//! + risk classification. dcent-toolbox uses this catalog to:
//! - Render the dashboard's "LuxOS attack surface" advisory page.
//! - Refuse to install LuxOS without operator acknowledgement of the
//!   crit-ranked findings.
//!
//! HAZARDS pinned by tests (per §11 "Crit-ranked findings"):
//! 1. `/cgi-bin/uninstall.cgi` has NO auth — drive-by phishing image
//!    can brick fleet.
//! 2. NO TLS on any inbound port (4028/8080/9012/22/80 all plaintext).
//! 3. Default SSH creds (`root:root`) on port 22 — standard Bitmain
//!    BB criticism applies.
//! 4. Update fetched without code signing ( luxos-F covered).
//! 5. Audit log is mutable ( luxos-J covered).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Listen port catalog
// ---------------------------------------------------------------------------

/// One of 7 listening ports observed on LuxOS `a lab unit` per §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosListenPort {
    /// Port 22 — sshd. Bitmain BB default `root:root` creds.
    Ssh22,
    /// Port 80 — busybox httpd. Mixed auth: htdigest on /index.html,
    /// NONE on /recovery.html + /cgi-bin/*.
    Httpd80,
    /// Port 111 — rpcbind. Yocto cruft, never used by LuxOS.
    Rpcbind111,
    /// Port 4028 — luxminer CGMiner-compatible TCP API.
    Cgminer4028,
    /// Port 8080 — luxminer REST HTTP API.
    Luxminer8080,
    /// Port 9012 — luxupdate HTTP debug server (read-only).
    Luxupdate9012,
    /// Port 55065 — rpc.statd (NFS lockd). Yocto cruft, never used.
    RpcStatd55065,
}

impl LuxosListenPort {
    /// Numeric TCP port.
    pub fn port_number(&self) -> u16 {
        match self {
            Self::Ssh22 => 22,
            Self::Httpd80 => 80,
            Self::Rpcbind111 => 111,
            Self::Cgminer4028 => 4028,
            Self::Luxminer8080 => 8080,
            Self::Luxupdate9012 => 9012,
            Self::RpcStatd55065 => 55065,
        }
    }

    /// Process name that owns this port.
    pub fn process(&self) -> &'static str {
        match self {
            Self::Ssh22 => "sshd",
            Self::Httpd80 => "busybox httpd",
            Self::Rpcbind111 => "rpcbind",
            Self::Cgminer4028 => "luxminer",
            Self::Luxminer8080 => "luxminer",
            Self::Luxupdate9012 => "luxupdate",
            Self::RpcStatd55065 => "rpc.statd",
        }
    }

    /// Auth model exposed at the wire layer.
    pub fn auth_kind(&self) -> LuxosAuthKind {
        match self {
            Self::Ssh22 => LuxosAuthKind::RootRootDefault,
            Self::Httpd80 => LuxosAuthKind::HtdigestLighttpdMixed,
            Self::Rpcbind111 => LuxosAuthKind::None,
            Self::Cgminer4028 => LuxosAuthKind::ApiPassword,
            Self::Luxminer8080 => LuxosAuthKind::ApiPassword,
            Self::Luxupdate9012 => LuxosAuthKind::None,
            Self::RpcStatd55065 => LuxosAuthKind::None,
        }
    }

    /// True iff the wire is TLS-encrypted. **No inbound port has
    /// TLS** per §11 — the `a lab unit` finding is "No TLS on any port
    /// except outbound to GCS for updates and outbound to pool for
    /// stratum."
    pub fn has_tls(&self) -> bool {
        false
    }

    /// Operator-facing risk level per §11 risk classification.
    pub fn risk_level(&self) -> LuxosRiskLevel {
        match self {
            Self::Ssh22 => LuxosRiskLevel::Critical,
            Self::Httpd80 => LuxosRiskLevel::High,
            Self::Rpcbind111 => LuxosRiskLevel::Low,
            Self::Cgminer4028 => LuxosRiskLevel::Medium,
            Self::Luxminer8080 => LuxosRiskLevel::Medium,
            Self::Luxupdate9012 => LuxosRiskLevel::Low,
            Self::RpcStatd55065 => LuxosRiskLevel::Low,
        }
    }

    /// True iff this port should be disabled by default on a
    /// hardened install (Yocto cruft + open-internet exposure surfaces).
    pub fn should_disable_by_default(&self) -> bool {
        matches!(self, Self::Rpcbind111 | Self::RpcStatd55065)
    }
}

/// All 7 documented ports in stable iteration order.
pub const ALL_LUXOS_PORTS: &[LuxosListenPort] = &[
    LuxosListenPort::Ssh22,
    LuxosListenPort::Httpd80,
    LuxosListenPort::Rpcbind111,
    LuxosListenPort::Cgminer4028,
    LuxosListenPort::Luxminer8080,
    LuxosListenPort::Luxupdate9012,
    LuxosListenPort::RpcStatd55065,
];

// ---------------------------------------------------------------------------
// Auth kind
// ---------------------------------------------------------------------------

/// Wire-layer authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosAuthKind {
    /// No authentication required.
    None,
    /// Default Bitmain BB credentials (`root:root`).
    RootRootDefault,
    /// lighttpd htdigest on `/index.html` only — `/recovery.html` +
    /// `/cgi-bin/*` are unauth'd.
    HtdigestLighttpdMixed,
    /// API password configured via `[app.api.tcp]` /
    /// `[app.api.http]` in luxminer.toml.
    ApiPassword,
}

// ---------------------------------------------------------------------------
// Risk level
// ---------------------------------------------------------------------------

/// Operator-facing risk classification per §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_numbers_match_re_doc_table() {
        // §11 verbatim port numbers.
        assert_eq!(LuxosListenPort::Ssh22.port_number(), 22);
        assert_eq!(LuxosListenPort::Httpd80.port_number(), 80);
        assert_eq!(LuxosListenPort::Rpcbind111.port_number(), 111);
        assert_eq!(LuxosListenPort::Cgminer4028.port_number(), 4028);
        assert_eq!(LuxosListenPort::Luxminer8080.port_number(), 8080);
        assert_eq!(LuxosListenPort::Luxupdate9012.port_number(), 9012);
        assert_eq!(LuxosListenPort::RpcStatd55065.port_number(), 55065);
    }

    #[test]
    fn process_names_match_re_doc_table() {
        assert_eq!(LuxosListenPort::Ssh22.process(), "sshd");
        assert_eq!(LuxosListenPort::Httpd80.process(), "busybox httpd");
        assert_eq!(LuxosListenPort::Cgminer4028.process(), "luxminer");
        assert_eq!(LuxosListenPort::Luxminer8080.process(), "luxminer");
        assert_eq!(LuxosListenPort::Luxupdate9012.process(), "luxupdate");
    }

    #[test]
    fn ssh_22_is_critical_with_root_root_default() {
        // §11 "CRITICAL if default" + "Confirmed default creds known
        // in field." This is the #3 crit-ranked finding.
        assert_eq!(
            LuxosListenPort::Ssh22.risk_level(),
            LuxosRiskLevel::Critical
        );
        assert_eq!(
            LuxosListenPort::Ssh22.auth_kind(),
            LuxosAuthKind::RootRootDefault
        );
    }

    #[test]
    fn httpd_80_is_high_with_mixed_htdigest_auth() {
        // §11 finding #1: /cgi-bin/uninstall.cgi has no auth → HIGH.
        assert_eq!(LuxosListenPort::Httpd80.risk_level(), LuxosRiskLevel::High);
        assert_eq!(
            LuxosListenPort::Httpd80.auth_kind(),
            LuxosAuthKind::HtdigestLighttpdMixed
        );
    }

    #[test]
    fn no_inbound_port_has_tls() {
        // §11 finding #2: "No TLS on any port except outbound to GCS
        // for updates and outbound to pool for stratum. All API
        // traffic on 4028/8080 is plaintext over LAN."
        for port in ALL_LUXOS_PORTS.iter().copied() {
            assert!(!port.has_tls(), "{:?} unexpectedly has inbound TLS", port);
        }
    }

    #[test]
    fn cgminer_and_luxminer_apis_use_api_password() {
        // §11 row 4 + 5: API password configured via
        // `app.api.tcp` (4028) and `app.api.http` (8080).
        assert_eq!(
            LuxosListenPort::Cgminer4028.auth_kind(),
            LuxosAuthKind::ApiPassword
        );
        assert_eq!(
            LuxosListenPort::Luxminer8080.auth_kind(),
            LuxosAuthKind::ApiPassword
        );
        assert_eq!(
            LuxosListenPort::Cgminer4028.risk_level(),
            LuxosRiskLevel::Medium
        );
        assert_eq!(
            LuxosListenPort::Luxminer8080.risk_level(),
            LuxosRiskLevel::Medium
        );
    }

    #[test]
    fn luxupdate_9012_is_low_risk_read_only_debug() {
        // §11 row 6: read-only debug, but info leak.
        assert_eq!(
            LuxosListenPort::Luxupdate9012.risk_level(),
            LuxosRiskLevel::Low
        );
        assert_eq!(
            LuxosListenPort::Luxupdate9012.auth_kind(),
            LuxosAuthKind::None
        );
    }

    #[test]
    fn yocto_cruft_ports_should_disable_by_default() {
        // §11: rpcbind 111 + rpc.statd 55065 are "Yocto cruft, never
        // used". Pin disable-by-default flag.
        assert!(LuxosListenPort::Rpcbind111.should_disable_by_default());
        assert!(LuxosListenPort::RpcStatd55065.should_disable_by_default());
        // Operational ports stay on.
        for port in [
            LuxosListenPort::Ssh22,
            LuxosListenPort::Httpd80,
            LuxosListenPort::Cgminer4028,
            LuxosListenPort::Luxminer8080,
            LuxosListenPort::Luxupdate9012,
        ] {
            assert!(
                !port.should_disable_by_default(),
                "{:?} should NOT be disabled by default",
                port
            );
        }
    }

    #[test]
    fn rpcbind_and_rpcstatd_are_low_risk() {
        // Even though they're attack surface, they're "never used by
        // LuxOS" so risk lands at Low — they should just be off.
        assert_eq!(
            LuxosListenPort::Rpcbind111.risk_level(),
            LuxosRiskLevel::Low
        );
        assert_eq!(
            LuxosListenPort::RpcStatd55065.risk_level(),
            LuxosRiskLevel::Low
        );
    }

    #[test]
    fn risk_level_ordering_is_canonical() {
        // Critical > High > Medium > Low.
        assert!(LuxosRiskLevel::Critical > LuxosRiskLevel::High);
        assert!(LuxosRiskLevel::High > LuxosRiskLevel::Medium);
        assert!(LuxosRiskLevel::Medium > LuxosRiskLevel::Low);
    }

    #[test]
    fn all_listen_ports_count_matches_re_doc() {
        // §11 table has exactly 7 rows.
        assert_eq!(ALL_LUXOS_PORTS.len(), 7);
    }

    #[test]
    fn port_round_trips_through_serde() {
        for port in ALL_LUXOS_PORTS.iter().copied() {
            let json = serde_json::to_string(&port).unwrap();
            let back: LuxosListenPort = serde_json::from_str(&json).unwrap();
            assert_eq!(port, back);
        }
    }

    #[test]
    fn auth_kind_round_trips_through_serde() {
        for kind in [
            LuxosAuthKind::None,
            LuxosAuthKind::RootRootDefault,
            LuxosAuthKind::HtdigestLighttpdMixed,
            LuxosAuthKind::ApiPassword,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: LuxosAuthKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn risk_level_serializes_in_snake_case() {
        for (level, expected) in [
            (LuxosRiskLevel::Low, "\"low\""),
            (LuxosRiskLevel::Medium, "\"medium\""),
            (LuxosRiskLevel::High, "\"high\""),
            (LuxosRiskLevel::Critical, "\"critical\""),
        ] {
            assert_eq!(serde_json::to_string(&level).unwrap(), expected);
        }
    }

    #[test]
    fn port_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosListenPort::Ssh22).unwrap(),
            "\"ssh22\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosListenPort::Cgminer4028).unwrap(),
            "\"cgminer4028\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosListenPort::RpcStatd55065).unwrap(),
            "\"rpc_statd55065\""
        );
    }

    #[test]
    fn auth_kind_none_serializes_as_lowercase_none() {
        assert_eq!(
            serde_json::to_string(&LuxosAuthKind::None).unwrap(),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosAuthKind::ApiPassword).unwrap(),
            "\"api_password\""
        );
    }

    #[test]
    fn ssh_default_creds_are_critical_finding_3() {
        // §11 crit-ranked finding #3: "Default SSH creds. Standard
        // Bitmain BB criticism applies." Pin as Critical.
        let p = LuxosListenPort::Ssh22;
        assert!(matches!(p.auth_kind(), LuxosAuthKind::RootRootDefault));
        assert_eq!(p.risk_level(), LuxosRiskLevel::Critical);
    }
}
