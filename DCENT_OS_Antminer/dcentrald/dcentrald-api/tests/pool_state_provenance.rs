use dcentrald_api::{MinerState, OperatingMode, PoolQualitySource, PoolState};
use dcentrald_stratum::pool_quality::{
    apply_stratum_status, stratum_state_status_str, PoolQualitySnapshot,
};
use dcentrald_stratum::types::{StratumState, StratumStatus};
use serde_json::{json, Value};

fn legacy_pool_payload() -> Value {
    json!({
        "url": "stratum+tcp://pool.example:3333",
        "worker": "worker.1",
        "status": "Alive",
        "difficulty": 2048.0,
        "last_share_at": 0,
        "encrypted": false,
        "donating": false,
        "latency_ms": 0,
        "reject_reason_counts": [0, 0, 0, 0, 0, 0],
        "rolling_acceptance_pct_30min": 100.0,
        "rolling_acceptance_count_30min": [0, 0]
    })
}

#[test]
fn legacy_pool_state_json_deserializes_without_source_labels() {
    let pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");

    assert_eq!(pool.url, "stratum+tcp://pool.example:3333");
    assert_eq!(pool.protocol, "sv1");
    assert!(pool.encrypted_source.is_none());
    assert!(pool.donating_source.is_none());
    assert!(pool.latency_ms_source.is_none());
    assert!(pool.reject_reason_counts_source.is_none());
    assert!(pool.rolling_acceptance_source.is_none());
    assert!(pool.sv2_session_source.is_none());
    assert!(pool.auto_fallback_source.is_none());
    assert!(pool.failover_source.is_none());
    assert!(pool.hashrate_split_source.is_none());
}

#[test]
fn pool_state_serializes_additive_source_labels_when_present() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");
    let tag = Some(PoolQualitySource::HONEST_DEFAULT.to_string());

    pool.encrypted_source = tag.clone();
    pool.donating_source = tag.clone();
    pool.latency_ms_source = tag.clone();
    pool.reject_reason_counts_source = tag.clone();
    pool.rolling_acceptance_source = tag.clone();
    pool.sv2_session_source = tag.clone();
    pool.auto_fallback_source = tag.clone();
    pool.failover_source = tag.clone();
    pool.hashrate_split_source = tag;

    let value = serde_json::to_value(pool).expect("serialize pool");
    for field in [
        "encrypted_source",
        "donating_source",
        "latency_ms_source",
        "reject_reason_counts_source",
        "rolling_acceptance_source",
        "sv2_session_source",
        "auto_fallback_source",
        "failover_source",
        "hashrate_split_source",
    ] {
        assert_eq!(
            value[field].as_str(),
            Some(PoolQualitySource::HONEST_DEFAULT),
            "{field}"
        );
    }
}

#[test]
fn empty_miner_state_labels_pool_quality_defaults_as_honest_default() {
    let pool = MinerState::empty(OperatingMode::Standard).pool;

    for source in [
        pool.encrypted_source.as_deref(),
        pool.donating_source.as_deref(),
        pool.latency_ms_source.as_deref(),
        pool.reject_reason_counts_source.as_deref(),
        pool.rolling_acceptance_source.as_deref(),
        pool.sv2_session_source.as_deref(),
        pool.auto_fallback_source.as_deref(),
        pool.failover_source.as_deref(),
        pool.hashrate_split_source.as_deref(),
    ] {
        assert_eq!(source, Some(PoolQualitySource::HONEST_DEFAULT));
    }
}

#[test]
fn quality_snapshot_projection_clears_stale_sv2_on_v1_fallback() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");
    pool.protocol = "sv2".to_string();
    pool.encrypted = true;

    let mut quality = PoolQualitySnapshot::default();
    apply_stratum_status(
        &mut quality,
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
        &mut quality,
        &StratumStatus::AutoFallbackStateChanged {
            active: true,
            retry_after_s: 30,
            reason: "sv2 handshake failed".to_string(),
        },
    );

    pool.apply_quality_snapshot(&quality);

    assert_eq!(pool.protocol, "sv1");
    assert!(!pool.encrypted);
    assert!(pool.sv2_session.is_none());
    assert_eq!(
        pool.encrypted_source.as_deref(),
        Some(PoolQualitySource::STRATUM_STATUS)
    );
    assert_eq!(
        pool.sv2_session_source.as_deref(),
        Some(PoolQualitySource::STRATUM_STATUS)
    );
}

#[test]
fn quality_snapshot_projection_preserves_rolling_acceptance_source() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");
    let mut quality = PoolQualitySnapshot::default();

    apply_stratum_status(
        &mut quality,
        &StratumStatus::RollingAcceptanceUpdated {
            pct: 50.0,
            accepted: 1,
            total: 2,
        },
    );
    pool.apply_quality_snapshot(&quality);

    assert_eq!(pool.rolling_acceptance_pct_30min, 50.0);
    assert_eq!(pool.rolling_acceptance_count_30min, (1, 2));
    assert_eq!(
        pool.rolling_acceptance_source.as_deref(),
        Some(PoolQualitySource::LOCAL_ACCOUNTING)
    );
}

// PRV-12: honest `status` at pool-connect — the dashboard must never render a
// proxy/optimistic connection state (e.g. an `accepted()>0` heuristic from
// fresh defaults) as if it were a measured one. The Stratum clients emit
// `StateChanged(Connecting)` at connection initiation (v1/client.rs:1781,
// v2/client.rs:329/426/651) — strictly BEFORE `run_session`, where the first
// share event can fire. These tests pin the resulting truth contract on the
// pure no-HAL projection so it cannot silently regress.

/// (b) Status never claims `authorized`/`mining` before a `StateChanged` is
/// observed. With `connection_state == None`, `apply_quality_snapshot` must
/// leave the publisher's honest fallback `status` untouched — it must not
/// fabricate an "authorized"/"mining" projection out of share-count defaults.
#[test]
fn prv12_projection_does_not_claim_authorized_or_mining_before_state_changed() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");
    // A publisher's pre-projection fallback (honest "connecting" placeholder).
    pool.status = "connecting".to_string();

    let quality = PoolQualitySnapshot::default();
    assert!(
        quality.connection_state.is_none(),
        "fresh snapshot must have observed no StateChanged yet"
    );

    pool.apply_quality_snapshot(&quality);

    // No StateChanged observed -> projection must not upgrade the status.
    assert_eq!(pool.status, "connecting");
    assert_ne!(pool.status, "authorized");
    assert_ne!(pool.status, "mining");
}

/// (a)+(c) The FIRST `StateChanged` to reach the snapshot is recorded as the
/// real `connection_state`, and the projected `PoolState.status` matches that
/// recorded state EXACTLY. Connection initiation (`Connecting`) lands before
/// any `ShareAccepted`/`ShareRejected`, so the dashboard shows "connecting"
/// first — not an optimistic "mining" derived from a stale heuristic.
#[test]
fn prv12_first_state_changed_is_connecting_and_projects_exactly() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");
    pool.status = "stale_heuristic".to_string();

    let mut quality = PoolQualitySnapshot::default();
    // Connection-initiation event — the first status a client emits.
    apply_stratum_status(
        &mut quality,
        &StratumStatus::StateChanged(StratumState::Connecting),
    );
    assert_eq!(quality.connection_state, Some(StratumState::Connecting));

    pool.apply_quality_snapshot(&quality);
    // Projection is exact: recorded Connecting -> "connecting".
    assert_eq!(
        pool.status,
        stratum_state_status_str(&StratumState::Connecting)
    );
    assert_eq!(pool.status, "connecting");
}

/// (a) An accept event arriving AFTER the connection-time `StateChanged` must
/// not rewrite the connection state, and a later real `StateChanged` (e.g.
/// `Mining`) projects exactly. This pins that share accounting never overrides
/// the authoritative connection-state projection.
#[test]
fn prv12_share_events_do_not_override_connection_state_projection() {
    let mut pool: PoolState =
        serde_json::from_value(legacy_pool_payload()).expect("legacy pool payload");

    let mut quality = PoolQualitySnapshot::default();

    // 1) Connection initiation.
    apply_stratum_status(
        &mut quality,
        &StratumStatus::StateChanged(StratumState::Connecting),
    );
    // 2) A reject event (share accounting) must not touch connection_state.
    apply_stratum_status(
        &mut quality,
        &StratumStatus::ShareRejected {
            job_id: "1".to_string(),
            error_code: 23,
            error_msg: "Low difficulty share".to_string(),
            meta: None,
        },
    );
    assert_eq!(
        quality.connection_state,
        Some(StratumState::Connecting),
        "a share event must not rewrite the recorded connection state"
    );

    // 3) A later real transition to Mining projects exactly.
    apply_stratum_status(
        &mut quality,
        &StratumStatus::StateChanged(StratumState::Mining),
    );
    pool.apply_quality_snapshot(&quality);
    assert_eq!(pool.status, stratum_state_status_str(&StratumState::Mining));
    assert_eq!(pool.status, "mining");
}
