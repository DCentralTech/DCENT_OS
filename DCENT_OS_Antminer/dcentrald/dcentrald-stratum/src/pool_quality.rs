//! Pure pool-quality telemetry reducer.
//!
//! Mining front-ends receive [`StratumStatus`] events but publish
//! `PoolState` from different tasks. This module keeps the event-to-telemetry
//! mapping in one no-HAL place so dashboard fields are either real stratum
//! evidence or explicit honest defaults.

use crate::types::{HashrateSplitStatus, PoolFailoverStatus, StratumState, StratumStatus};

pub const REJECT_REASON_BUCKETS: usize = 6;
pub const ROLLING_ACCEPTANCE_EMPTY_PCT: f64 = 100.0;

/// Canonical source tags for pool-quality fields.
pub struct PoolQualitySource;

impl PoolQualitySource {
    /// Field value came from a real `StratumStatus` event.
    pub const STRATUM_STATUS: &'static str = "stratum_status";
    /// Field value came from configured intent, not live pool observation.
    pub const CONFIG: &'static str = "config";
    /// Field value is the honest fresh-boot/default state; no event observed yet.
    pub const HONEST_DEFAULT: &'static str = "honest_default";
    /// Field value came from a local mining-path share/accounting surface.
    pub const LOCAL_ACCOUNTING: &'static str = "local_accounting";
    /// Field is intentionally unsupported by this publishing path.
    pub const UNSUPPORTED: &'static str = "unsupported";
}

/// Stratum-native SV2 session snapshot. API crates can project this into their
/// wire DTO without making this no-HAL crate depend on `dcentrald-api`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSv2SessionSnapshot {
    pub cipher_suite: String,
    pub handshake_latency_ms: u64,
    pub pool_pubkey_fingerprint: String,
    pub certificate_valid_from: u64,
    pub certificate_not_after: u64,
    pub channel_id: Option<u32>,
    pub noise_nonce_tx: u64,
    pub noise_nonce_rx: u64,
    pub bytes_encrypted: u64,
    pub bytes_decrypted: u64,
    pub messages_sent: u64,
    pub messages_received: u64,
}

/// Per-field provenance for [`PoolQualitySnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolQualitySources {
    pub encrypted: &'static str,
    pub sv2_session: &'static str,
    pub donating: &'static str,
    pub auto_fallback: &'static str,
    pub failover: &'static str,
    pub hashrate_split: &'static str,
    pub latency_ms: &'static str,
    pub reject_reason_counts: &'static str,
    pub rolling_acceptance: &'static str,
}

impl Default for PoolQualitySources {
    fn default() -> Self {
        Self {
            encrypted: PoolQualitySource::HONEST_DEFAULT,
            sv2_session: PoolQualitySource::HONEST_DEFAULT,
            donating: PoolQualitySource::HONEST_DEFAULT,
            auto_fallback: PoolQualitySource::HONEST_DEFAULT,
            failover: PoolQualitySource::HONEST_DEFAULT,
            hashrate_split: PoolQualitySource::HONEST_DEFAULT,
            latency_ms: PoolQualitySource::HONEST_DEFAULT,
            reject_reason_counts: PoolQualitySource::HONEST_DEFAULT,
            rolling_acceptance: PoolQualitySource::HONEST_DEFAULT,
        }
    }
}

/// Pool-quality fields shared by daemon, serial, and hybrid publishers.
#[derive(Debug, Clone)]
pub struct PoolQualitySnapshot {
    pub encrypted: bool,
    pub sv2_session: Option<PoolSv2SessionSnapshot>,
    pub donating: bool,
    pub donation_percent: Option<f32>,
    pub donation_cycle_remaining_s: Option<u64>,
    pub donation_active_url: String,
    pub donation_active_worker: String,
    pub donation_pool_index: usize,
    pub auto_fallback_active: bool,
    pub auto_retry_sv2_after_s: Option<u64>,
    pub auto_fallback_reason: Option<String>,
    pub failover: PoolFailoverStatus,
    pub hashrate_split: HashrateSplitStatus,
    pub latency_ms: u64,
    pub reject_reason_counts: [u64; REJECT_REASON_BUCKETS],
    pub rolling_acceptance_pct_30min: f64,
    pub rolling_acceptance_count_30min: (u32, u32),
    /// Latest real Stratum connection state from a `StratumStatus::StateChanged`
    /// event. `None` = no event observed yet (publishers fall back to their own
    /// local heuristic). When `Some`, [`PoolState`] projects this onto its
    /// `status` field so the dashboard reflects the REAL connection state
    /// (connecting / authorized / mining / disconnected / auth_failed) instead
    /// of a `accepted()>0` proxy (FWT-2).
    ///
    /// [`PoolState`]: (see dcentrald-api)
    pub connection_state: Option<StratumState>,
    pub sources: PoolQualitySources,
}

impl Default for PoolQualitySnapshot {
    fn default() -> Self {
        Self {
            encrypted: false,
            sv2_session: None,
            donating: false,
            donation_percent: None,
            donation_cycle_remaining_s: None,
            donation_active_url: String::new(),
            donation_active_worker: String::new(),
            donation_pool_index: 0,
            auto_fallback_active: false,
            auto_retry_sv2_after_s: None,
            auto_fallback_reason: None,
            failover: PoolFailoverStatus::default(),
            hashrate_split: HashrateSplitStatus::default(),
            latency_ms: 0,
            reject_reason_counts: [0; REJECT_REASON_BUCKETS],
            rolling_acceptance_pct_30min: ROLLING_ACCEPTANCE_EMPTY_PCT,
            rolling_acceptance_count_30min: (0, 0),
            connection_state: None,
            sources: PoolQualitySources::default(),
        }
    }
}

impl PoolQualitySnapshot {
    pub fn record_reject(&mut self, error_code: i64, error_msg: &str) {
        let idx = classify_reject_reason(error_code, error_msg);
        if let Some(slot) = self.reject_reason_counts.get_mut(idx) {
            *slot = slot.saturating_add(1);
        }
        self.sources.reject_reason_counts = PoolQualitySource::STRATUM_STATUS;
    }

    pub fn set_rolling_acceptance(&mut self, pct: f64, count: (u32, u32)) {
        self.rolling_acceptance_pct_30min = pct;
        self.rolling_acceptance_count_30min = count;
        self.sources.rolling_acceptance = PoolQualitySource::LOCAL_ACCOUNTING;
    }
}

/// Apply one stratum event to the pool-quality snapshot.
pub fn apply_stratum_status(snapshot: &mut PoolQualitySnapshot, status: &StratumStatus) {
    match status {
        StratumStatus::PoolFailoverUpdated(failover) => {
            snapshot.failover = failover.clone();
            snapshot.sources.failover = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::HashrateSplitUpdated(split) => {
            snapshot.hashrate_split = split.clone();
            snapshot.sources.hashrate_split = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::Latency(ms) => {
            snapshot.latency_ms = *ms;
            snapshot.sources.latency_ms = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::DonationStateChanged {
            active,
            percent,
            cycle_remaining_s,
            active_url,
            active_worker,
            pool_index,
        } => {
            snapshot.donating = *active;
            snapshot.donation_percent = Some(*percent);
            snapshot.donation_cycle_remaining_s = Some(*cycle_remaining_s);
            snapshot.donation_active_url = active_url.clone();
            snapshot.donation_active_worker = active_worker.clone();
            snapshot.donation_pool_index = *pool_index;
            snapshot.sources.donating = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::AutoFallbackStateChanged {
            active,
            retry_after_s,
            reason,
        } => {
            snapshot.auto_fallback_active = *active;
            snapshot.auto_retry_sv2_after_s = active.then_some(*retry_after_s);
            snapshot.auto_fallback_reason = active.then_some(reason.clone());
            if *active {
                snapshot.encrypted = false;
                snapshot.sv2_session = None;
                snapshot.sources.encrypted = PoolQualitySource::STRATUM_STATUS;
                snapshot.sources.sv2_session = PoolQualitySource::STRATUM_STATUS;
            }
            snapshot.sources.auto_fallback = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::RollingAcceptanceUpdated {
            pct,
            accepted,
            total,
        } => {
            snapshot.set_rolling_acceptance(*pct, (*accepted, *total));
        }
        StratumStatus::Sv2SessionUpdated {
            cipher_suite,
            handshake_latency_ms,
            pool_pubkey_fingerprint,
            certificate_valid_from,
            certificate_not_after,
            channel_id,
            noise_nonce_tx,
            noise_nonce_rx,
            bytes_encrypted,
            bytes_decrypted,
            messages_sent,
            messages_received,
        } => {
            snapshot.encrypted = true;
            snapshot.sv2_session = Some(PoolSv2SessionSnapshot {
                cipher_suite: cipher_suite.clone(),
                handshake_latency_ms: *handshake_latency_ms,
                pool_pubkey_fingerprint: pool_pubkey_fingerprint.clone(),
                certificate_valid_from: *certificate_valid_from,
                certificate_not_after: *certificate_not_after,
                channel_id: *channel_id,
                noise_nonce_tx: *noise_nonce_tx,
                noise_nonce_rx: *noise_nonce_rx,
                bytes_encrypted: *bytes_encrypted,
                bytes_decrypted: *bytes_decrypted,
                messages_sent: *messages_sent,
                messages_received: *messages_received,
            });
            snapshot.sources.encrypted = PoolQualitySource::STRATUM_STATUS;
            snapshot.sources.sv2_session = PoolQualitySource::STRATUM_STATUS;
        }
        StratumStatus::ShareRejected {
            error_code,
            error_msg,
            ..
        } => {
            snapshot.record_reject(*error_code, error_msg);
        }
        StratumStatus::StateChanged(state) => {
            // FWT-2: record the REAL connection state so publishers can project
            // it onto `PoolState.status` instead of an `accepted()>0` proxy that
            // mislabels a 100%-rejecting or auth-failing pool.
            snapshot.connection_state = Some(state.clone());
        }
        _ => {}
    }
}

/// Map a real [`StratumState`] to the canonical lowercase `PoolState.status`
/// string the dashboard renders. Pure + no-HAL so it is unit-tested here and
/// reused by the API projection (FWT-2).
///
/// `"authorized"` (pool accepted credentials, awaiting/working jobs) is kept
/// distinct from `"mining"` (shares flowing) and from `"connecting"` (pre-auth)
/// to honor the project's Wave-9D9 truth contract (connecting ≠ connected ≠
/// mining). `"auth_failed"` is the actionable wrong-worker/banned-wallet signal.
pub fn stratum_state_status_str(state: &StratumState) -> &'static str {
    match state {
        StratumState::Disconnected => "disconnected",
        StratumState::Connecting => "connecting",
        StratumState::Authorized => "authorized",
        StratumState::Mining => "mining",
        StratumState::Donating => "donating",
        StratumState::AuthFailed => "auth_failed",
    }
}

/// Classify a pool share-reject `(error_code, error_msg)` into a fixed bucket.
/// Index order matches the API `REJECT_REASON_LABELS` contract.
pub fn classify_reject_reason(error_code: i64, error_msg: &str) -> usize {
    match error_code {
        21 => return 1,      // Job not found / stale
        22 => return 2,      // Duplicate share
        23 => return 0,      // Low difficulty share
        24 | 25 => return 4, // Unauthorized worker / Not subscribed
        _ => {}
    }
    let m = error_msg.to_ascii_lowercase();
    if m.contains("low difficulty") || m.contains("low diff") || m.contains("below target") {
        0
    } else if m.contains("job not found") || m.contains("stale") || m.contains("unknown job") {
        1
    } else if m.contains("duplicate") {
        2
    } else if m.contains("above target")
        || m.contains("high hash")
        || m.contains("above the target")
    {
        3
    } else if m.contains("unauthorized")
        || m.contains("not authorized")
        || m.contains("not subscribed")
    {
        4
    } else {
        5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StratumStatus;

    #[test]
    fn defaults_are_honest_empty_values() {
        let snap = PoolQualitySnapshot::default();

        assert!(!snap.encrypted);
        assert!(snap.sv2_session.is_none());
        assert!(!snap.donating);
        assert_eq!(snap.donation_percent, None);
        assert_eq!(snap.donation_cycle_remaining_s, None);
        assert!(snap.donation_active_url.is_empty());
        assert!(snap.donation_active_worker.is_empty());
        assert_eq!(snap.donation_pool_index, 0);
        assert!(!snap.auto_fallback_active);
        assert_eq!(snap.auto_retry_sv2_after_s, None);
        assert_eq!(snap.auto_fallback_reason, None);
        assert!(!snap.failover.enabled);
        assert!(!snap.hashrate_split.enabled);
        assert_eq!(snap.latency_ms, 0);
        assert_eq!(snap.reject_reason_counts, [0; REJECT_REASON_BUCKETS]);
        assert_eq!(snap.rolling_acceptance_pct_30min, 100.0);
        assert_eq!(snap.rolling_acceptance_count_30min, (0, 0));
        assert_eq!(snap.sources.encrypted, PoolQualitySource::HONEST_DEFAULT);
        assert_eq!(snap.sources.sv2_session, PoolQualitySource::HONEST_DEFAULT);
        assert_eq!(snap.sources.donating, PoolQualitySource::HONEST_DEFAULT);
        assert_eq!(
            snap.sources.auto_fallback,
            PoolQualitySource::HONEST_DEFAULT
        );
        assert_eq!(snap.sources.failover, PoolQualitySource::HONEST_DEFAULT);
        assert_eq!(
            snap.sources.hashrate_split,
            PoolQualitySource::HONEST_DEFAULT
        );
        assert_eq!(snap.sources.latency_ms, PoolQualitySource::HONEST_DEFAULT);
        assert_eq!(
            snap.sources.reject_reason_counts,
            PoolQualitySource::HONEST_DEFAULT
        );
        assert_eq!(
            snap.sources.rolling_acceptance,
            PoolQualitySource::HONEST_DEFAULT
        );
    }

    #[test]
    fn latency_event_updates_latency_only() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(&mut snap, &StratumStatus::Latency(42));

        assert_eq!(snap.latency_ms, 42);
        assert_eq!(snap.sources.latency_ms, PoolQualitySource::STRATUM_STATUS);
        assert!(!snap.encrypted);
    }

    #[test]
    fn connection_state_default_is_none() {
        // FWT-2: a fresh snapshot has observed no StateChanged yet, so
        // publishers fall back to their own local heuristic.
        assert!(PoolQualitySnapshot::default().connection_state.is_none());
    }

    #[test]
    fn state_changed_event_records_real_connection_state() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::StateChanged(StratumState::Mining),
        );
        assert_eq!(snap.connection_state, Some(StratumState::Mining));

        // A later auth-failure event must overwrite, not be swallowed.
        apply_stratum_status(
            &mut snap,
            &StratumStatus::StateChanged(StratumState::AuthFailed),
        );
        assert_eq!(snap.connection_state, Some(StratumState::AuthFailed));
    }

    #[test]
    fn stratum_state_status_str_maps_every_variant_distinctly() {
        // FWT-2/FWT-3: each real state maps to a distinct, honest status string;
        // connecting ≠ authorized ≠ mining, and auth_failed is its own signal.
        assert_eq!(
            stratum_state_status_str(&StratumState::Disconnected),
            "disconnected"
        );
        assert_eq!(
            stratum_state_status_str(&StratumState::Connecting),
            "connecting"
        );
        assert_eq!(
            stratum_state_status_str(&StratumState::Authorized),
            "authorized"
        );
        assert_eq!(stratum_state_status_str(&StratumState::Mining), "mining");
        assert_eq!(
            stratum_state_status_str(&StratumState::Donating),
            "donating"
        );
        assert_eq!(
            stratum_state_status_str(&StratumState::AuthFailed),
            "auth_failed"
        );
    }

    #[test]
    fn donation_event_updates_route_and_source() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::DonationStateChanged {
                active: true,
                percent: 2.0,
                cycle_remaining_s: 60,
                active_url: "stratum+tcp://donate.example:3333".to_string(),
                active_worker: "worker".to_string(),
                pool_index: 1,
            },
        );

        assert!(snap.donating);
        assert_eq!(snap.donation_percent, Some(2.0));
        assert_eq!(snap.donation_cycle_remaining_s, Some(60));
        assert_eq!(
            snap.donation_active_url,
            "stratum+tcp://donate.example:3333"
        );
        assert_eq!(snap.donation_active_worker, "worker");
        assert_eq!(snap.donation_pool_index, 1);
        assert_eq!(snap.sources.donating, PoolQualitySource::STRATUM_STATUS);
    }

    #[test]
    fn auto_fallback_inactive_clears_optional_fields_with_real_source() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::AutoFallbackStateChanged {
                active: true,
                retry_after_s: 30,
                reason: "sv2 handshake failed".to_string(),
            },
        );
        apply_stratum_status(
            &mut snap,
            &StratumStatus::AutoFallbackStateChanged {
                active: false,
                retry_after_s: 30,
                reason: "recovered".to_string(),
            },
        );

        assert!(!snap.auto_fallback_active);
        assert_eq!(snap.auto_retry_sv2_after_s, None);
        assert_eq!(snap.auto_fallback_reason, None);
        assert_eq!(
            snap.sources.auto_fallback,
            PoolQualitySource::STRATUM_STATUS
        );
    }

    #[test]
    fn active_auto_fallback_clears_stale_sv2_transport_truth() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::Sv2SessionUpdated {
                cipher_suite: "Noise_NX_25519_ChaChaPoly_SHA256".to_string(),
                handshake_latency_ms: 11,
                pool_pubkey_fingerprint: "abcd".to_string(),
                certificate_valid_from: 1,
                certificate_not_after: 2,
                channel_id: Some(9),
                noise_nonce_tx: 3,
                noise_nonce_rx: 4,
                bytes_encrypted: 5,
                bytes_decrypted: 6,
                messages_sent: 7,
                messages_received: 8,
            },
        );
        apply_stratum_status(
            &mut snap,
            &StratumStatus::AutoFallbackStateChanged {
                active: true,
                retry_after_s: 30,
                reason: "sv2 handshake failed".to_string(),
            },
        );

        assert!(!snap.encrypted);
        assert!(snap.sv2_session.is_none());
        assert_eq!(snap.sources.encrypted, PoolQualitySource::STRATUM_STATUS);
        assert_eq!(snap.sources.sv2_session, PoolQualitySource::STRATUM_STATUS);
    }

    #[test]
    fn rolling_acceptance_event_updates_window_with_local_accounting_source() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::RollingAcceptanceUpdated {
                pct: 50.0,
                accepted: 1,
                total: 2,
            },
        );

        assert_eq!(snap.rolling_acceptance_pct_30min, 50.0);
        assert_eq!(snap.rolling_acceptance_count_30min, (1, 2));
        assert_eq!(
            snap.sources.rolling_acceptance,
            PoolQualitySource::LOCAL_ACCOUNTING
        );
    }

    #[test]
    fn failover_and_hashrate_split_events_clone_status() {
        let mut snap = PoolQualitySnapshot::default();
        let failover = PoolFailoverStatus {
            enabled: true,
            active_pool_index: 1,
            active_pool_priority: 2,
            active_pool_url: "stratum+tcp://backup.example:3333".to_string(),
            switch_count: 7,
            event: "switch".to_string(),
            ..PoolFailoverStatus::default()
        };
        let split = HashrateSplitStatus {
            enabled: true,
            active: true,
            active_route: "secondary".to_string(),
            active_pool_index: 1,
            active_pool_priority: 2,
            secondary_bps: 2500,
            ..HashrateSplitStatus::default()
        };

        apply_stratum_status(&mut snap, &StratumStatus::PoolFailoverUpdated(failover));
        apply_stratum_status(&mut snap, &StratumStatus::HashrateSplitUpdated(split));

        assert!(snap.failover.enabled);
        assert_eq!(
            snap.failover.active_pool_url,
            "stratum+tcp://backup.example:3333"
        );
        assert!(snap.hashrate_split.enabled);
        assert_eq!(snap.hashrate_split.active_route, "secondary");
        assert_eq!(snap.sources.failover, PoolQualitySource::STRATUM_STATUS);
        assert_eq!(
            snap.sources.hashrate_split,
            PoolQualitySource::STRATUM_STATUS
        );
    }

    #[test]
    fn sv2_session_event_marks_transport_encrypted() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::Sv2SessionUpdated {
                cipher_suite: "Noise_NX_25519_ChaChaPoly_SHA256".to_string(),
                handshake_latency_ms: 11,
                pool_pubkey_fingerprint: "abcd".to_string(),
                certificate_valid_from: 1,
                certificate_not_after: 2,
                channel_id: Some(9),
                noise_nonce_tx: 3,
                noise_nonce_rx: 4,
                bytes_encrypted: 5,
                bytes_decrypted: 6,
                messages_sent: 7,
                messages_received: 8,
            },
        );

        let session = snap.sv2_session.as_ref().expect("session");
        assert!(snap.encrypted);
        assert_eq!(session.channel_id, Some(9));
        assert_eq!(session.bytes_encrypted, 5);
        assert_eq!(snap.sources.encrypted, PoolQualitySource::STRATUM_STATUS);
        assert_eq!(snap.sources.sv2_session, PoolQualitySource::STRATUM_STATUS);
    }

    #[test]
    fn share_reject_increments_classified_bucket() {
        let mut snap = PoolQualitySnapshot::default();

        apply_stratum_status(
            &mut snap,
            &StratumStatus::ShareRejected {
                job_id: "1".to_string(),
                error_code: 21,
                error_msg: "job not found".to_string(),
                meta: None,
            },
        );
        apply_stratum_status(
            &mut snap,
            &StratumStatus::ShareRejected {
                job_id: "2".to_string(),
                error_code: 0,
                error_msg: "Low difficulty share".to_string(),
                meta: None,
            },
        );

        assert_eq!(snap.reject_reason_counts[0], 1);
        assert_eq!(snap.reject_reason_counts[1], 1);
        assert_eq!(
            snap.sources.reject_reason_counts,
            PoolQualitySource::STRATUM_STATUS
        );
    }
}
