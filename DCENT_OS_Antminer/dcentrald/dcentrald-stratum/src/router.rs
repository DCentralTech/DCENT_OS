//! Stratum protocol router: selects V1 or V2 client based on configuration.
//!
//! The router is the top-level entry point for the stratum subsystem. It reads
//! the `protocol` field from `StratumConfig` and dispatches to the appropriate
//! client implementation:
//!
//! - `V1Only` (default): Uses `StratumV1Client` — standard Stratum V1 over TCP.
//! - `V2Only`: Uses `StratumV2Client` — encrypted SV2 with Noise_NX handshake.
//! - `Auto`: Resolves protocol from the active pool's own fields first, then
//!   falls back to the global setting.
//!
//! Both clients share the same channel interface (job_tx, share_rx, status_tx),
//! so the daemon doesn't need to know which protocol is in use.
//!
//! # Backward Compatibility
//!
//! If no `protocol` field is present in config (or it's set to "sv1"/"v1"),
//! the router defaults to V1. Existing configs with no SV2 fields work unchanged.
//!
//! # Future Work
//!
//! - Cross-protocol failover between pool endpoints within one long-lived client
//! - Connection quality metrics for protocol switching decisions

use crate::types::{JobTemplate, StratumConfig, StratumStatus, ValidShare};
use crate::StratumV1Client;
#[cfg(feature = "sv2")]
use crate::StratumV2Client;
use tokio::sync::mpsc;
#[cfg(all(feature = "sv2", feature = "jd"))]
use tokio::sync::watch;
use tracing::{info, warn};

/// Hard ceiling for SV2 Standard channels.
///
/// Above this nominal hashrate, the pool-owned coinbase/extranonce space gets
/// exhausted before the channel can keep up — see
///  (S9 13 TH/s exhausts Standard in 2.5s).
/// Operators on multi-TH/s miners must use Extended/JD or fall back to V1.
const SV2_STANDARD_CHANNEL_MAX_HASHRATE_GHS: f32 = 1_000.0;

/// W5.3: at or above this nominal hashrate, Auto/V2 modes prefer
/// `OpenExtendedMiningChannel` over `OpenStandardMiningChannel`. The number
/// is set conservatively below the Standard-exhaustion ceiling so the
/// Extended preference kicks in well before any pool-side throttling.
///
/// Per-platform tuning is exposed through `StratumConfig::nominal_hashrate_ghs`.
/// The daemon owns wiring this from the active silicon profile (Phase 2 will
/// pipe the value from the BM1387/BM1366/BM1368/BM1362/BM1370/etc. profile's
/// `expected_hashrate_ghs` instead of the current placeholder 0.0). Until the
/// daemon plumbs the live value, this hint stays conservative — 0.0 fails the
/// check so SV2 channel-open decisions fall through to existing logic.
const SV2_EXTENDED_CHANNEL_PREFER_HASHRATE_GHS: f32 = 5_000.0;

/// Protocol selection mode, derived from config at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolMode {
    /// Stratum V1 only (default, backward compatible).
    V1Only,
    /// Stratum V2 only (encrypted, Noise_NX transport).
    V2Only,
    /// Auto-detect: try V2 if sv2_url is configured, else V1.
    Auto,
}

impl std::fmt::Display for ProtocolMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolMode::V1Only => write!(f, "v1"),
            ProtocolMode::V2Only => write!(f, "v2"),
            ProtocolMode::Auto => write!(f, "auto"),
        }
    }
}

/// Stratum protocol router.
///
/// Selects and runs the appropriate stratum client based on the configured
/// protocol mode. Acts as a drop-in replacement for direct `StratumV1Client`
/// usage in the daemon.
pub struct StratumRouter {
    config: StratumConfig,
    protocol_mode: ProtocolMode,
    #[cfg(all(feature = "sv2", feature = "jd"))]
    jd_status_rx: Option<watch::Receiver<crate::v2::jd::JdStatus>>,
}

impl StratumRouter {
    fn parse_protocol_mode(protocol: Option<&str>) -> Option<ProtocolMode> {
        match protocol {
            Some("sv2") | Some("v2") => Some(ProtocolMode::V2Only),
            Some("auto") => Some(ProtocolMode::Auto),
            Some("sv1") | Some("v1") => Some(ProtocolMode::V1Only),
            _ => None,
        }
    }

    fn protocol_mode_for_pool(&self, pool: &crate::types::PoolConfig) -> ProtocolMode {
        match Self::parse_protocol_mode(pool.protocol.as_deref()) {
            Some(ProtocolMode::Auto) => {
                if pool.sv2_url.is_some() {
                    ProtocolMode::V2Only
                } else {
                    ProtocolMode::V1Only
                }
            }
            Some(mode) => mode,
            None => match self.protocol_mode {
                ProtocolMode::Auto => {
                    if pool.sv2_url.is_some() {
                        ProtocolMode::V2Only
                    } else {
                        ProtocolMode::V1Only
                    }
                }
                ref mode => mode.clone(),
            },
        }
    }

    /// Create a new router from stratum configuration.
    ///
    /// The protocol mode is determined by the `protocol` field in config:
    /// - `"sv2"` or `"v2"` -> V2Only
    /// - `"auto"` -> Auto (resolve from the active pool endpoint)
    /// - `"sv1"` or `"v1"` -> V1Only (operator-pinned legacy)
    /// - absent / unrecognized -> Auto (W5.3 new default)
    ///
    /// W5.3: previously the absent/unrecognized default was V1Only. Operators
    /// running fresh installs now get Auto, which only flips a session to V2
    /// when the active pool actually advertises an SV2 endpoint
    /// (`pool.sv2_url` set), so legacy V1-only pool configs keep mining over
    /// V1 with no behavioral change. Operators with explicit `protocol = "sv1"`
    /// in their existing configs are still pinned to V1Only — only the
    /// silent default moves.
    pub fn new(config: StratumConfig) -> Self {
        let protocol_mode =
            Self::parse_protocol_mode(config.protocol.as_deref()).unwrap_or(ProtocolMode::Auto);

        info!(
            mode = %protocol_mode,
            "Stratum router initialized"
        );

        Self {
            config,
            protocol_mode,
            #[cfg(all(feature = "sv2", feature = "jd"))]
            jd_status_rx: None,
        }
    }

    #[cfg(all(feature = "sv2", feature = "jd"))]
    pub fn with_job_declaration_status_rx(
        mut self,
        rx: watch::Receiver<crate::v2::jd::JdStatus>,
    ) -> Self {
        self.jd_status_rx = Some(rx);
        self
    }

    /// Get the selected protocol mode.
    pub fn protocol_mode(&self) -> &ProtocolMode {
        &self.protocol_mode
    }

    fn sv2_standard_channel_block_reason(config: &StratumConfig) -> Option<&'static str> {
        if config.nominal_hashrate_ghs > SV2_STANDARD_CHANNEL_MAX_HASHRATE_GHS
            && !config.sv2_extended_channel
        {
            Some("SV2 Standard channels exhaust nonce space above 1 TH/s; enable Extended/JD or use V1")
        } else {
            None
        }
    }

    /// W5.3: returns true when the configured nominal hashrate sits at or above
    /// the Extended-channel preference threshold.
    ///
    /// The router uses this hint to log the Extended-channel preference and to
    /// help future SV2 channel-open code path-pick `OpenExtendedMiningChannel`
    /// before falling back to Standard. The decision is deliberately keyed off
    /// `StratumConfig::nominal_hashrate_ghs`, which the daemon now fills from
    /// the active silicon profile (`silicon_profile.expected_hashrate_ghs`).
    ///
    /// This is independent of `sv2_standard_channel_block_reason`: the block
    /// reason is the hard "Standard cannot work here" gate at >1 TH/s; this
    /// hint is the soft "Extended is the smarter open" preference at >=5 TH/s.
    pub fn sv2_should_prefer_extended_channel(config: &StratumConfig) -> bool {
        config.nominal_hashrate_ghs >= SV2_EXTENDED_CHANNEL_PREFER_HASHRATE_GHS
    }

    /// Run the stratum client with protocol selection.
    ///
    /// This is the main entry point — spawn as a tokio task. The router
    /// selects the appropriate client and delegates to it. The selected
    /// client runs forever with internal reconnection logic.
    ///
    /// # Channel Interface
    /// Same as `StratumV1Client::run()` and `StratumV2Client::run()`:
    /// - `job_tx`: Sends `JobTemplate` when new mining jobs arrive from pool
    /// - `share_rx`: Receives `ValidShare` from the work dispatcher for submission
    /// - `status_tx`: Sends `StratumStatus` updates (state changes, share results)
    pub async fn run(
        self,
        job_tx: mpsc::Sender<JobTemplate>,
        share_rx: mpsc::Receiver<ValidShare>,
        status_tx: mpsc::Sender<StratumStatus>,
    ) {
        match self.protocol_mode {
            ProtocolMode::V1Only => {
                info!(
                    pool = %self.config.pool1.url,
                    "Starting Stratum V1 client"
                );
                let client = StratumV1Client::new(self.config, job_tx, share_rx, status_tx);
                client.run().await;
            }

            #[cfg(feature = "sv2")]
            ProtocolMode::V2Only => {
                if let Some(reason) = Self::sv2_standard_channel_block_reason(&self.config) {
                    warn!(
                        nominal_hashrate_ghs = self.config.nominal_hashrate_ghs,
                        reason, "SV2 Standard channel refused; falling back to Stratum V1"
                    );
                    let client = StratumV1Client::new(self.config, job_tx, share_rx, status_tx);
                    client.run().await;
                    return;
                }
                // V2Only is single-pool: it reconnects to pool1 forever (capped
                // backoff) and has NO multi-pool failover and NO V1 fallback
                // (unlike Auto). Warn the operator so a configured-but-unused
                // pool2/pool3 isn't mistaken for resilience. (Structural SV2
                // multi-pool failover is a tracked follow-up — see router rustdoc.)
                if self.config.pool2.is_some() || self.config.pool3.is_some() {
                    warn!(
                        "protocol=\"sv2\" (V2Only) is single-pool: backup pool2/pool3 are NOT used \
                         for failover and there is no V1 fallback on this mode. A dead SV2 pool1 is \
                         retried forever with capped backoff. For multi-pool resilience use \
                         protocol=\"auto\" (falls back to V1) or list backups as V1 endpoints."
                    );
                }
                let sv2_url = self
                    .config
                    .pool1
                    .sv2_url
                    .as_deref()
                    .unwrap_or(&self.config.pool1.url);
                let prefer_extended = Self::sv2_should_prefer_extended_channel(&self.config);
                // W1.4: mask wallet-shaped worker.
                info!(
                    sv2_url = %sv2_url,
                    nominal_hashrate_ghs = self.config.nominal_hashrate_ghs,
                    sv2_extended_channel = self.config.sv2_extended_channel,
                    prefer_extended_channel = prefer_extended,
                    worker = %dcentrald_common::wallet_mask::mask_wallet(&self.config.pool1.worker),
                    "Starting Stratum V2 client (encrypted, Noise_NX)"
                );
                let client = StratumV2Client::new(
                    self.config.clone(),
                    self.config.nominal_hashrate_ghs,
                    job_tx,
                    share_rx,
                    status_tx,
                );
                #[cfg(feature = "jd")]
                let client = if let Some(rx) = self.jd_status_rx {
                    client.with_job_declaration_status_rx(rx)
                } else {
                    client
                };
                client.run().await;
            }

            #[cfg(feature = "sv2")]
            ProtocolMode::Auto => {
                // Auto mode resolves against the active pool endpoint first.
                // This keeps old configs working while letting pool-specific
                // protocol hints override the global Auto mode.
                match self.protocol_mode_for_pool(&self.config.pool1) {
                    ProtocolMode::V2Only => {
                        if let Some(reason) = Self::sv2_standard_channel_block_reason(&self.config)
                        {
                            warn!(
                                nominal_hashrate_ghs = self.config.nominal_hashrate_ghs,
                                reason, "Auto mode: SV2 Standard channel refused; using Stratum V1"
                            );
                            let client =
                                StratumV1Client::new(self.config, job_tx, share_rx, status_tx);
                            client.run().await;
                            return;
                        }
                        let sv2_url = self
                            .config
                            .pool1
                            .sv2_url
                            .as_deref()
                            .unwrap_or(&self.config.pool1.url);
                        let prefer_extended =
                            Self::sv2_should_prefer_extended_channel(&self.config);
                        info!(
                            sv2_url = %sv2_url,
                            v1_url = %self.config.pool1.url,
                            pool_protocol = ?self.config.pool1.protocol,
                            nominal_hashrate_ghs = self.config.nominal_hashrate_ghs,
                            sv2_extended_channel = self.config.sv2_extended_channel,
                            prefer_extended_channel = prefer_extended,
                            "Auto mode: active pool resolves to Stratum V2"
                        );

                        let client = StratumV2Client::new(
                            self.config.clone(),
                            self.config.nominal_hashrate_ghs,
                            job_tx,
                            share_rx,
                            status_tx,
                        );
                        #[cfg(feature = "jd")]
                        let client = if let Some(rx) = self.jd_status_rx {
                            client.with_job_declaration_status_rx(rx)
                        } else {
                            client
                        };
                        client.run_auto_with_v1_fallback().await;
                    }
                    ProtocolMode::V1Only | ProtocolMode::Auto => {
                        info!(
                            pool = %self.config.pool1.url,
                            pool_protocol = ?self.config.pool1.protocol,
                            "Auto mode: active pool resolves to Stratum V1"
                        );
                        let client = StratumV1Client::new(self.config, job_tx, share_rx, status_tx);
                        client.run().await;
                    }
                }
            }

            // When sv2 feature is not compiled in but V2/Auto was requested
            #[cfg(not(feature = "sv2"))]
            ProtocolMode::V2Only | ProtocolMode::Auto => {
                warn!(
                    "SV2 requested but not compiled in (feature 'sv2' disabled), falling back to V1"
                );
                let client = StratumV1Client::new(self.config, job_tx, share_rx, status_tx);
                client.run().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DonationConfig, PoolConfig};

    fn make_test_config() -> StratumConfig {
        StratumConfig {
            pool1: PoolConfig {
                url: "stratum+tcp://pool.example.com:3333".into(),
                worker: "test.worker".into(),
                password: "x".into(),
                sv2_url: None,
                protocol: None,
                split_bps: None,
            },
            pool2: None,
            pool3: None,
            routing_mode: "failover".into(),
            split_cycle_duration_s: 1800,
            primary_return_stability_secs: 900,
            no_notify_failover_secs: 300,
            reject_rate_failover_pct: 0,
            reject_rate_failover_min_samples: 100,
            smart_failover_enabled: false,
            smart_failover_drive: false,
            sv2_max_inbound_frame_bytes: 1_048_576,
            v1_max_inbound_line_bytes: 65_536,
            donation: DonationConfig::default(),
            version_rolling: true,
            version_rolling_mask: crate::types::default_version_rolling_mask(),
            suggest_difficulty: Some(256),
            hash_on_disconnect: true,
            nominal_hashrate_ghs: 13500.0,
            sv2_extended_channel: false,
            protocol: None,
        }
    }

    #[test]
    fn test_default_protocol_is_auto() {
        // W5.3: configs that omit `protocol` now default to Auto. Auto only
        // flips to V2 when the active pool advertises an SV2 endpoint, so
        // legacy V1-only operators keep mining over V1 unchanged. This
        // pin replaces the previous V1Only default contract.
        let config = make_test_config();
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::Auto);
    }

    #[test]
    fn test_unknown_protocol_falls_back_to_auto_default() {
        // W5.3: an unrecognized protocol string must fall back to the new
        // Auto default — not silently get pinned to V1Only — so a typo
        // doesn't accidentally pin SV2-capable operators to V1.
        let mut config = make_test_config();
        config.protocol = Some("garbage-2".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::Auto);
    }

    #[test]
    fn test_explicit_v1_still_pins_v1only_after_default_flip() {
        // Operators who explicitly opted into "v1" or "sv1" must still get
        // V1Only after the W5.3 default flip — only the silent default
        // changes, not the explicit-pin contract.
        for alias in ["v1", "sv1"] {
            let mut config = make_test_config();
            config.protocol = Some(alias.into());
            let router = StratumRouter::new(config);
            assert_eq!(
                *router.protocol_mode(),
                ProtocolMode::V1Only,
                "explicit alias {alias} must still pin V1Only"
            );
        }
    }

    #[test]
    fn sv2_should_prefer_extended_channel_at_5_ths() {
        let mut config = make_test_config();
        // Below threshold: prefer Standard (assuming the standard-block gate
        // also passes — separate concern).
        config.nominal_hashrate_ghs = 4_999.0;
        assert!(!StratumRouter::sv2_should_prefer_extended_channel(&config));
        // At threshold: prefer Extended.
        config.nominal_hashrate_ghs = 5_000.0;
        assert!(StratumRouter::sv2_should_prefer_extended_channel(&config));
        // Multi-TH/s S9-class: prefer Extended.
        config.nominal_hashrate_ghs = 13_500.0;
        assert!(StratumRouter::sv2_should_prefer_extended_channel(&config));
    }

    #[test]
    fn sv2_extended_preference_is_independent_of_extended_channel_flag() {
        // The "should prefer" hint is informational and keys off hashrate.
        // The actual `sv2_extended_channel` config flag is a separate switch
        // (the consumer of the hint) — pin that the hint does not depend on
        // it so callers can use the hint to decide whether to flip the flag.
        let mut config = make_test_config();
        config.nominal_hashrate_ghs = 13_500.0;
        config.sv2_extended_channel = false;
        assert!(StratumRouter::sv2_should_prefer_extended_channel(&config));
        config.sv2_extended_channel = true;
        assert!(StratumRouter::sv2_should_prefer_extended_channel(&config));
    }

    #[test]
    fn test_explicit_sv1_protocol() {
        let mut config = make_test_config();
        config.protocol = Some("sv1".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::V1Only);
    }

    #[test]
    fn test_explicit_v1_protocol() {
        let mut config = make_test_config();
        config.protocol = Some("v1".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::V1Only);
    }

    #[test]
    fn test_sv2_protocol() {
        let mut config = make_test_config();
        config.protocol = Some("sv2".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::V2Only);
    }

    #[test]
    fn sv2_standard_channel_blocked_for_multi_ths_hashrate() {
        let mut config = make_test_config();
        config.nominal_hashrate_ghs = 13_500.0;
        config.sv2_extended_channel = false;

        assert!(StratumRouter::sv2_standard_channel_block_reason(&config).is_some());
    }

    #[test]
    fn sv2_standard_channel_allowed_below_one_ths() {
        let mut config = make_test_config();
        config.nominal_hashrate_ghs = 500.0;
        config.sv2_extended_channel = false;

        assert!(StratumRouter::sv2_standard_channel_block_reason(&config).is_none());
    }

    #[test]
    fn sv2_extended_channel_allows_multi_ths_hashrate() {
        let mut config = make_test_config();
        config.nominal_hashrate_ghs = 13_500.0;
        config.sv2_extended_channel = true;

        assert!(StratumRouter::sv2_standard_channel_block_reason(&config).is_none());
    }

    #[test]
    fn test_v2_protocol() {
        let mut config = make_test_config();
        config.protocol = Some("v2".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::V2Only);
    }

    #[test]
    fn test_auto_protocol() {
        let mut config = make_test_config();
        config.protocol = Some("auto".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::Auto);
    }

    #[test]
    fn test_unknown_protocol_defaults_to_auto() {
        // W5.3 default flip: unknown protocol values now resolve to Auto
        // (was V1Only). See test_unknown_protocol_falls_back_to_auto_default
        // above for the duplicated coverage that pins the contract change.
        let mut config = make_test_config();
        config.protocol = Some("garbage".into());
        let router = StratumRouter::new(config);
        assert_eq!(*router.protocol_mode(), ProtocolMode::Auto);
    }

    #[test]
    fn test_display_impl() {
        assert_eq!(format!("{}", ProtocolMode::V1Only), "v1");
        assert_eq!(format!("{}", ProtocolMode::V2Only), "v2");
        assert_eq!(format!("{}", ProtocolMode::Auto), "auto");
    }

    #[test]
    fn test_auto_uses_pool_sv2_url_when_pool_protocol_absent() {
        let mut config = make_test_config();
        config.protocol = Some("auto".into());
        config.pool1.sv2_url = Some("stratum2+tcp://v2.pool.example.com:3336".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V2Only
        );
    }

    #[test]
    fn test_auto_respects_pool_level_sv1_override() {
        let mut config = make_test_config();
        config.protocol = Some("auto".into());
        config.pool1.sv2_url = Some("stratum2+tcp://v2.pool.example.com:3336".into());
        config.pool1.protocol = Some("sv1".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V1Only
        );
    }

    #[test]
    fn test_auto_respects_pool_level_sv2_override() {
        let mut config = make_test_config();
        config.protocol = Some("auto".into());
        config.pool1.protocol = Some("sv2".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V2Only
        );
    }

    // -----------------------------------------------------------------------
    // Pool-level Auto resolution + protocol-mode parser edge cases.
    //
    // Existing tests cover the global protocol mode + a handful of pool
    // override combinations, but several boundary cases are unpinned —
    // particularly the "pool-level auto" + sv2_url interaction, the
    // global-V2-with-pool-V1-override path, and the parser's
    // case-sensitivity behavior.
    // -----------------------------------------------------------------------

    #[test]
    fn test_pool_level_auto_with_sv2_url_resolves_to_v2() {
        let mut config = make_test_config();
        config.protocol = Some("v1".into()); // global = V1
        config.pool1.protocol = Some("auto".into()); // pool = auto
        config.pool1.sv2_url = Some("stratum2+tcp://v2.pool.example.com:3336".into());
        let router = StratumRouter::new(config);
        // Pool-level "auto" with sv2_url present beats global V1.
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V2Only
        );
    }

    #[test]
    fn test_pool_level_auto_without_sv2_url_resolves_to_v1() {
        let mut config = make_test_config();
        config.protocol = Some("v2".into()); // global = V2
        config.pool1.protocol = Some("auto".into()); // pool = auto
        config.pool1.sv2_url = None;
        let router = StratumRouter::new(config);
        // Pool-level "auto" without sv2_url falls back to V1, even when
        // the global mode is V2. Pool's auto resolution wins.
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V1Only
        );
    }

    #[test]
    fn test_global_v2_with_pool_level_v1_override_uses_v1() {
        // Pool-level explicit override beats global V2.
        let mut config = make_test_config();
        config.protocol = Some("v2".into());
        config.pool1.protocol = Some("v1".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V1Only
        );
    }

    #[test]
    fn test_global_v1_with_pool_level_v2_override_uses_v2() {
        // Symmetric: pool-level V2 override beats global V1.
        let mut config = make_test_config();
        config.protocol = Some("v1".into());
        config.pool1.protocol = Some("v2".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V2Only
        );
    }

    #[test]
    fn test_garbage_pool_protocol_falls_back_to_global_mode() {
        // Pool ships a misspelled protocol — the parser returns None and
        // resolution falls back to the global protocol mode.
        let mut config = make_test_config();
        config.protocol = Some("v2".into());
        config.pool1.protocol = Some("not-a-protocol".into());
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V2Only
        );
    }

    #[test]
    fn test_global_auto_without_pool_sv2_url_resolves_to_v1() {
        // Global "auto", no pool override, no sv2_url → V1.
        let mut config = make_test_config();
        config.protocol = Some("auto".into());
        config.pool1.sv2_url = None;
        let router = StratumRouter::new(config);
        assert_eq!(
            router.protocol_mode_for_pool(&router.config.pool1),
            ProtocolMode::V1Only
        );
    }

    #[test]
    fn parse_protocol_mode_accepts_all_aliases() {
        assert_eq!(
            StratumRouter::parse_protocol_mode(Some("sv1")),
            Some(ProtocolMode::V1Only)
        );
        assert_eq!(
            StratumRouter::parse_protocol_mode(Some("v1")),
            Some(ProtocolMode::V1Only)
        );
        assert_eq!(
            StratumRouter::parse_protocol_mode(Some("sv2")),
            Some(ProtocolMode::V2Only)
        );
        assert_eq!(
            StratumRouter::parse_protocol_mode(Some("v2")),
            Some(ProtocolMode::V2Only)
        );
        assert_eq!(
            StratumRouter::parse_protocol_mode(Some("auto")),
            Some(ProtocolMode::Auto)
        );
    }

    #[test]
    fn parse_protocol_mode_rejects_none_and_unknown() {
        // None must produce None so the caller's `.unwrap_or(V1Only)`
        // applies (backward-compat default).
        assert_eq!(StratumRouter::parse_protocol_mode(None), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("garbage")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("v3")), None);
    }

    #[test]
    fn parse_protocol_mode_is_case_sensitive() {
        // Pin the case-sensitivity contract: uppercase variants must NOT
        // match. A refactor that added case-insensitive matching would
        // be caught here so the protocol-string contract stays explicit.
        assert_eq!(StratumRouter::parse_protocol_mode(Some("SV1")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("V1")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("Sv2")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("AUTO")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("Auto")), None);
    }

    #[test]
    fn parse_protocol_mode_does_not_trim_whitespace() {
        // Whitespace around a valid mode is NOT trimmed. Pin so a refactor
        // that added trim() would change the behavior — operators relying
        // on strict-match config validation expect this.
        assert_eq!(StratumRouter::parse_protocol_mode(Some(" v1")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some("v1 ")), None);
        assert_eq!(StratumRouter::parse_protocol_mode(Some(" sv2 ")), None);
    }

    #[test]
    fn protocol_mode_display_strings_match_config_aliases() {
        // The Display impl produces strings that round-trip through
        // parse_protocol_mode. Pin so a refactor of either side stays
        // self-consistent.
        for mode in [
            ProtocolMode::V1Only,
            ProtocolMode::V2Only,
            ProtocolMode::Auto,
        ] {
            let s = format!("{}", mode);
            let recovered = StratumRouter::parse_protocol_mode(Some(&s));
            assert_eq!(
                recovered.as_ref(),
                Some(&mode),
                "Display(\"{s}\") must round-trip through parse_protocol_mode"
            );
        }
    }
}
