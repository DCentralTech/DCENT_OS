// DCENT_axe Stratum V1 Types
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Pool wire protocol selected from the configured endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StratumProtocol {
    V1,
    V2,
}

/// Pool connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StratumConfig {
    /// Pool hostname or IP (e.g., "solo.ckpool.org")
    pub url: String,

    /// Pool port (e.g., 3333)
    pub port: u16,

    /// Worker name (e.g., "bc1q...worker1")
    pub worker_name: String,

    /// Pool password (typically "x")
    pub password: String,

    /// Suggested initial difficulty (0 = let pool decide)
    pub suggest_difficulty: u32,

    /// Enable version rolling / ASICBoost (BIP 310)
    pub version_rolling: bool,
}

impl Default for StratumConfig {
    fn default() -> Self {
        Self {
            url: "public-pool.io".into(),
            port: 21496,
            worker_name: "dcentaxe".into(),
            password: "x".into(),
            suggest_difficulty: 0,
            version_rolling: true,
        }
    }
}

impl StratumConfig {
    /// Infer the wire protocol from the stored URL scheme.
    ///
    /// Legacy configs store a bare hostname. Dashboard/API updates can store
    /// `stratum+tcp://host` for V1 or `stratum2+tcp://host` for V2 without
    /// changing the persisted config schema.
    pub fn protocol(&self) -> StratumProtocol {
        let url = self.url.trim().to_ascii_lowercase();
        if url.starts_with("stratum2+tcp://")
            || url.starts_with("stratum2://")
            || url.starts_with("sv2://")
        {
            StratumProtocol::V2
        } else {
            StratumProtocol::V1
        }
    }

    /// Hostname/IP suitable for DNS resolution.
    pub fn endpoint_host(&self) -> String {
        endpoint_host_from_url(&self.url)
    }

    /// Return a copy with any known Stratum URL scheme stripped from `url`.
    pub fn with_endpoint_host(mut self) -> Self {
        self.url = self.endpoint_host();
        self
    }
}

pub fn endpoint_host_from_url(url: &str) -> String {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("stratum2+tcp://")
        .or_else(|| trimmed.strip_prefix("stratum2://"))
        .or_else(|| trimmed.strip_prefix("sv2://"))
        .or_else(|| trimmed.strip_prefix("stratum+tcp://"))
        .or_else(|| trimmed.strip_prefix("stratum://"))
        .or_else(|| trimmed.strip_prefix("tcp://"))
        .unwrap_or(trimmed);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split('@')
        .last()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .trim()
        .to_string()
}

/// Live stratum runtime status shared with API/reporting layers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StratumStatus {
    pub pool_index: u8,
    pub connected: bool,
    pub authorized: bool,
    pub extranonce_subscribe_requested: bool,
    pub extranonce_subscribe_accepted: bool,
    pub failover_active: bool,
    pub primary_failback_state: PrimaryFailbackState,
    pub primary_failback_detail: String,
    pub last_primary_reprobe_unix_ms: u64,
    pub last_primary_failback_unix_ms: u64,
    pub configured_url: String,
    pub configured_port: u16,
    pub active_url: String,
    pub active_port: u16,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_pending: u32,
    pub shares_unresolved: u64,
    pub oldest_pending_submit_age_ms: u64,
    pub difficulty_accepted: f64,
    pub difficulty_rejected: f64,
    pub jobs_received: u64,
    pub difficulty: f64,
    pub last_share_response_ms: f64,
    pub last_share_time: u64,
    pub last_share_submit_unix_ms: u64,
    pub last_share_response_unix_ms: u64,
    #[serde(default)]
    pub last_share_accepted_unix_ms: u64,
    #[serde(default)]
    pub last_share_rejected_unix_ms: u64,
    pub last_share_difficulty: f64,
    /// Number of version-rolling bits the pool granted in the last
    /// mining.configure negotiation (0 = rolling declined/disabled).
    #[serde(default)]
    pub negotiated_version_bits: u32,
    pub last_reject_reason: String,
    pub last_connect_cause: String,
    pub last_disconnect_cause: String,
    pub last_reconnect_cause: String,
    pub last_connect_unix_ms: u64,
    pub last_disconnect_unix_ms: u64,
    pub consecutive_failures: u32,
    pub backoff_secs: u64,
    pub recent_events: Vec<StratumEventRecord>,
    pub reject_reason_counts: Vec<RejectReasonCount>,
}

impl StratumStatus {
    /// Clone status for API/MCP hot paths while keeping only the newest event
    /// records. Counters and connection state are preserved exactly.
    pub fn snapshot_with_recent_event_limit(&self, recent_event_limit: usize) -> Self {
        let event_start = self.recent_events.len().saturating_sub(recent_event_limit);
        Self {
            pool_index: self.pool_index,
            connected: self.connected,
            authorized: self.authorized,
            extranonce_subscribe_requested: self.extranonce_subscribe_requested,
            extranonce_subscribe_accepted: self.extranonce_subscribe_accepted,
            failover_active: self.failover_active,
            primary_failback_state: self.primary_failback_state,
            primary_failback_detail: self.primary_failback_detail.clone(),
            last_primary_reprobe_unix_ms: self.last_primary_reprobe_unix_ms,
            last_primary_failback_unix_ms: self.last_primary_failback_unix_ms,
            configured_url: self.configured_url.clone(),
            configured_port: self.configured_port,
            active_url: self.active_url.clone(),
            active_port: self.active_port,
            shares_submitted: self.shares_submitted,
            shares_accepted: self.shares_accepted,
            shares_rejected: self.shares_rejected,
            shares_pending: self.shares_pending,
            shares_unresolved: self.shares_unresolved,
            oldest_pending_submit_age_ms: self.oldest_pending_submit_age_ms,
            difficulty_accepted: self.difficulty_accepted,
            difficulty_rejected: self.difficulty_rejected,
            jobs_received: self.jobs_received,
            difficulty: self.difficulty,
            last_share_response_ms: self.last_share_response_ms,
            last_share_time: self.last_share_time,
            last_share_submit_unix_ms: self.last_share_submit_unix_ms,
            last_share_response_unix_ms: self.last_share_response_unix_ms,
            last_share_accepted_unix_ms: self.last_share_accepted_unix_ms,
            last_share_rejected_unix_ms: self.last_share_rejected_unix_ms,
            last_share_difficulty: self.last_share_difficulty,
            negotiated_version_bits: self.negotiated_version_bits,
            last_reject_reason: self.last_reject_reason.clone(),
            last_connect_cause: self.last_connect_cause.clone(),
            last_disconnect_cause: self.last_disconnect_cause.clone(),
            last_reconnect_cause: self.last_reconnect_cause.clone(),
            last_connect_unix_ms: self.last_connect_unix_ms,
            last_disconnect_unix_ms: self.last_disconnect_unix_ms,
            consecutive_failures: self.consecutive_failures,
            backoff_secs: self.backoff_secs,
            recent_events: self.recent_events[event_start..].to_vec(),
            reject_reason_counts: self.reject_reason_counts.clone(),
        }
    }

    /// Maintain compact primary failback state from the bounded event stream.
    ///
    /// This is connection/routing evidence only. It does not prove accepted
    /// shares, hashrate, or restored mining after failback.
    pub fn update_primary_failback_from_event(
        &mut self,
        kind: StratumEventKind,
        detail: impl Into<String>,
        ts_unix_ms: u64,
    ) {
        match kind {
            StratumEventKind::FailoverEntered => {
                self.primary_failback_state = PrimaryFailbackState::FallbackActive;
                self.primary_failback_detail = detail.into();
            }
            StratumEventKind::PrimaryReprobeStarted => {
                self.primary_failback_state = PrimaryFailbackState::ReprobeStarted;
                self.primary_failback_detail = detail.into();
                self.last_primary_reprobe_unix_ms = ts_unix_ms;
            }
            StratumEventKind::PrimaryReprobeFailed => {
                self.primary_failback_state = PrimaryFailbackState::ReprobeFailed;
                self.primary_failback_detail = detail.into();
                self.last_primary_reprobe_unix_ms = ts_unix_ms;
            }
            StratumEventKind::PrimaryReprobeReady => {
                self.primary_failback_state = PrimaryFailbackState::ReprobeReady;
                self.primary_failback_detail = detail.into();
                self.last_primary_reprobe_unix_ms = ts_unix_ms;
            }
            StratumEventKind::PrimaryFailbackEntered => {
                self.primary_failback_state = PrimaryFailbackState::FailbackEntered;
                self.primary_failback_detail = detail.into();
                self.last_primary_failback_unix_ms = ts_unix_ms;
            }
            _ => {}
        }
    }
}

pub type SharedStratumStatus = Arc<Mutex<StratumStatus>>;

pub fn new_shared_stratum_status(config: &StratumConfig, pool_index: u8) -> SharedStratumStatus {
    Arc::new(Mutex::new(StratumStatus {
        pool_index,
        configured_url: config.url.clone(),
        configured_port: config.port,
        active_url: config.url.clone(),
        active_port: config.port,
        ..Default::default()
    }))
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryFailbackState {
    #[default]
    Idle,
    FallbackActive,
    ReprobeStarted,
    ReprobeFailed,
    ReprobeReady,
    FailbackEntered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RejectReasonCount {
    pub key: String,
    pub count: u32,
    pub last_seen_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StratumEventRecord {
    pub ts_unix_ms: u64,
    pub kind: StratumEventKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StratumEventKind {
    Connect,
    Disconnect,
    ReconnectRequested,
    ReconnectBackoff,
    FailoverEntered,
    FailoverSkipped,
    PrimaryReprobeStarted,
    PrimaryReprobeFailed,
    PrimaryReprobeReady,
    PrimaryFailbackEntered,
    ShareSubmitted,
    ShareAccepted,
    ShareRejected,
    DifficultyChanged,
    PoolMessage,
}

// ---------------------------------------------------------------------------
// Stratum Job (from mining.notify)
// ---------------------------------------------------------------------------

/// A mining job received from the pool via mining.notify.
///
/// Contains all data needed to construct block headers and dispatch work.
#[derive(Debug, Clone)]
pub struct StratumJob {
    /// Pool-assigned job identifier (hex string, e.g., "bf", "1a3f")
    pub job_id: String,

    /// Previous block hash (hex string, 64 chars, pool byte order)
    pub prev_hash: String,

    /// First part of coinbase transaction (hex string)
    pub coinbase1: String,

    /// Second part of coinbase transaction (hex string)
    pub coinbase2: String,

    /// Merkle branch hashes (hex strings, 64 chars each)
    pub merkle_branches: Vec<String>,

    /// Block version (hex string, 8 chars, e.g., "20000000")
    pub version: String,

    /// Compact difficulty target / nbits (hex string, 8 chars)
    pub nbits: String,

    /// Cached block height (extracted from coinbase1 per BIP34)
    pub block_height: u32,

    /// Block timestamp (hex string, 8 chars)
    pub ntime: String,

    /// If true, discard all pending work immediately
    pub clean_jobs: bool,
}

// ---------------------------------------------------------------------------
// Share Submission
// ---------------------------------------------------------------------------

/// A share to submit to the pool via mining.submit.
#[derive(Debug, Clone)]
pub struct ShareSubmission {
    /// Job ID from the original mining.notify
    pub job_id: String,

    /// Miner-generated extranonce2 (hex string)
    pub extranonce2: String,

    /// Block timestamp (hex string, 8 chars)
    pub ntime: String,

    /// The nonce that produced a valid hash (hex string, 8 chars)
    pub nonce: String,

    /// Full block version used for this share.
    pub version: u32,

    /// Version bits from version rolling (hex string, 8 chars, optional)
    pub version_bits: Option<String>,

    /// Achieved share difficulty for truthful pool-side accounting.
    pub difficulty: f64,
}

// ---------------------------------------------------------------------------
// Session State
// ---------------------------------------------------------------------------

/// Default Stratum V1 extranonce2 byte width used when a pool omits the value.
pub const DEFAULT_EXTRANONCE2_SIZE: usize = 4;

/// Maximum extranonce2 byte width accepted from Stratum V1 pools.
///
/// The work builder stores the extranonce2 counter as a u64, so larger pool
/// values cannot be represented honestly and would also create unbounded heap
/// allocations when formatting the coinbase.
pub const MAX_EXTRANONCE2_SIZE: usize = 8;

/// True when a pool-provided extranonce2 byte width is representable and safe.
pub fn is_valid_extranonce2_size(size: usize) -> bool {
    (1..=MAX_EXTRANONCE2_SIZE).contains(&size)
}

/// Session state after mining.subscribe handshake.
#[derive(Debug, Clone, Default)]
pub struct SessionState {
    /// Pool-assigned extranonce1 (hex string)
    pub extranonce1: String,

    /// Size of miner-controlled extranonce2 in bytes
    pub extranonce2_size: usize,

    /// Current pool difficulty
    pub difficulty: f64,

    /// Difficulty update announced by the pool that applies to the next job.
    pub pending_difficulty: Option<f64>,

    /// Version rolling mask (0 = no rolling)
    pub version_mask: u32,

    /// Extranonce update announced by the pool that applies to the next job.
    pub pending_extranonce: Option<(String, usize)>,

    /// Whether we are authorized to submit shares
    pub authorized: bool,

    /// Whether we requested mining.extranonce.subscribe this session.
    pub extranonce_subscribe_requested: bool,

    /// Whether the pool accepted mining.extranonce.subscribe this session.
    pub extranonce_subscribe_accepted: bool,

    /// Subscription ID (opaque string from pool)
    pub subscription_id: String,
}

// ---------------------------------------------------------------------------
// JSON-RPC Messages
// ---------------------------------------------------------------------------

/// Stratum JSON-RPC message types from the pool.
#[derive(Debug)]
pub enum StratumMessage {
    /// mining.notify — new job from pool
    Notify(StratumJob),

    /// mining.set_difficulty — pool difficulty change
    SetDifficulty(f64),

    /// mining.set_version_mask — version mask update
    SetVersionMask(u32),

    /// mining.set_extranonce — extranonce rotation mid-session
    SetExtranonce {
        extranonce1: String,
        extranonce2_size: usize,
    },

    /// mining.ping — keepalive from pool
    Ping(u64),

    /// client.reconnect — pool requests reconnection
    Reconnect {
        host: String,
        port: u16,
        wait_seconds: u32,
    },

    /// client.get_version — pool asks for our version
    GetVersion(u64),

    /// client.show_message — informational message from pool
    ShowMessage(String),

    /// Response to one of our requests
    Response {
        id: u64,
        result: Option<serde_json::Value>,
        error: Option<serde_json::Value>,
    },

    /// Unrecognized message
    Unknown(String),
}

// ---------------------------------------------------------------------------
// Difficulty
// ---------------------------------------------------------------------------

/// Pool difficulty 1 target (pdiff):
/// 0x00000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
/// = 2^224 - 1
pub const PDIFF1_TARGET: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// Client event — sent from the stratum thread to the mining thread.
#[derive(Debug)]
pub enum StratumEvent {
    /// New job received from pool
    NewJob(StratumJob),

    /// Pool difficulty changed
    DifficultyChanged(f64),

    /// Version mask changed
    VersionMaskChanged(u32),

    /// Extranonce changed mid-session
    ExtranonceChanged {
        extranonce1: String,
        extranonce2_size: usize,
    },

    /// Pre-built mining work (from SV2 — pool pre-computes merkle root).
    /// Bypasses the WorkBuilder pipeline entirely.
    PrebuiltWork {
        work: crate::work::MiningWork,
        clean_jobs: bool,
    },

    /// Connection lost — mining should continue with last job
    Disconnected,

    /// Connection re-established
    Reconnected,
}

/// Mining event — sent from the mining thread to the stratum thread.
#[derive(Debug)]
pub enum MiningEvent {
    /// Valid share ready for submission
    SubmitShare(ShareSubmission),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(idx: u64) -> StratumEventRecord {
        StratumEventRecord {
            ts_unix_ms: idx,
            kind: StratumEventKind::ShareSubmitted,
            detail: format!("event-{idx}"),
        }
    }

    #[test]
    fn status_snapshot_caps_recent_events_to_newest_tail() {
        let mut status = StratumStatus {
            shares_submitted: 42,
            shares_accepted: 40,
            shares_rejected: 2,
            shares_pending: 1,
            shares_unresolved: 3,
            oldest_pending_submit_age_ms: 250,
            last_share_submit_unix_ms: 1000,
            last_share_response_unix_ms: 1100,
            last_share_accepted_unix_ms: 1100,
            last_share_rejected_unix_ms: 0,
            difficulty: 1024.0,
            ..Default::default()
        };
        status.recent_events = (0..12).map(event).collect();

        let snapshot = status.snapshot_with_recent_event_limit(5);

        assert_eq!(snapshot.shares_submitted, 42);
        assert_eq!(snapshot.shares_accepted, 40);
        assert_eq!(snapshot.shares_rejected, 2);
        assert_eq!(snapshot.shares_pending, 1);
        assert_eq!(snapshot.shares_unresolved, 3);
        assert_eq!(snapshot.oldest_pending_submit_age_ms, 250);
        assert_eq!(snapshot.last_share_submit_unix_ms, 1000);
        assert_eq!(snapshot.last_share_response_unix_ms, 1100);
        assert_eq!(snapshot.last_share_accepted_unix_ms, 1100);
        assert_eq!(snapshot.last_share_rejected_unix_ms, 0);
        assert_eq!(snapshot.difficulty, 1024.0);
        assert_eq!(snapshot.recent_events.len(), 5);
        assert_eq!(snapshot.recent_events[0].detail, "event-7");
        assert_eq!(snapshot.recent_events[4].detail, "event-11");
        assert_eq!(status.recent_events.len(), 12);
    }

    #[test]
    fn status_snapshot_allows_zero_or_full_recent_events() {
        let mut status = StratumStatus::default();
        status.recent_events = (0..3).map(event).collect();

        assert!(status
            .snapshot_with_recent_event_limit(0)
            .recent_events
            .is_empty());
        assert_eq!(
            status
                .snapshot_with_recent_event_limit(8)
                .recent_events
                .len(),
            3
        );
    }

    #[test]
    fn primary_failback_events_serialize_as_status_strings() {
        assert_eq!(
            serde_json::to_string(&StratumEventKind::PrimaryReprobeStarted).unwrap(),
            r#""primary_reprobe_started""#
        );
        assert_eq!(
            serde_json::to_string(&StratumEventKind::PrimaryReprobeFailed).unwrap(),
            r#""primary_reprobe_failed""#
        );
        assert_eq!(
            serde_json::to_string(&StratumEventKind::PrimaryReprobeReady).unwrap(),
            r#""primary_reprobe_ready""#
        );
        assert_eq!(
            serde_json::to_string(&StratumEventKind::PrimaryFailbackEntered).unwrap(),
            r#""primary_failback_entered""#
        );
    }

    #[test]
    fn primary_failback_states_serialize_as_pool_truth_values() {
        let cases = [
            (PrimaryFailbackState::Idle, r#""idle""#),
            (PrimaryFailbackState::FallbackActive, r#""fallback_active""#),
            (PrimaryFailbackState::ReprobeStarted, r#""reprobe_started""#),
            (PrimaryFailbackState::ReprobeFailed, r#""reprobe_failed""#),
            (PrimaryFailbackState::ReprobeReady, r#""reprobe_ready""#),
            (
                PrimaryFailbackState::FailbackEntered,
                r#""failback_entered""#,
            ),
        ];

        for (state, expected) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), expected);
        }
    }

    #[test]
    fn status_json_carries_primary_failback_routing_contract() {
        let status = StratumStatus {
            primary_failback_state: PrimaryFailbackState::ReprobeReady,
            primary_failback_detail: "primary authorized with job proof".to_string(),
            last_primary_reprobe_unix_ms: 1234,
            last_primary_failback_unix_ms: 5678,
            shares_accepted: 0,
            ..Default::default()
        };

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["primary_failback_state"], "reprobe_ready");
        assert_eq!(
            json["primary_failback_detail"],
            "primary authorized with job proof"
        );
        assert_eq!(json["last_primary_reprobe_unix_ms"], 1234);
        assert_eq!(json["last_primary_failback_unix_ms"], 5678);
        assert_eq!(json["shares_accepted"], 0);
    }

    #[test]
    fn primary_failback_state_tracks_bounded_events_without_share_proof() {
        let mut status = StratumStatus::default();

        status.update_primary_failback_from_event(
            StratumEventKind::FailoverEntered,
            "switched to fallback fallback.example:4444",
            1000,
        );
        assert_eq!(
            status.primary_failback_state,
            PrimaryFailbackState::FallbackActive
        );
        assert_eq!(status.last_primary_reprobe_unix_ms, 0);
        assert_eq!(status.last_primary_failback_unix_ms, 0);

        status.update_primary_failback_from_event(
            StratumEventKind::PrimaryReprobeReady,
            "primary primary.example:3333 authorized with job proof",
            2000,
        );
        assert_eq!(
            status.primary_failback_state,
            PrimaryFailbackState::ReprobeReady
        );
        assert_eq!(status.last_primary_reprobe_unix_ms, 2000);
        assert_eq!(status.last_primary_failback_unix_ms, 0);
        assert_eq!(status.shares_accepted, 0);

        status.update_primary_failback_from_event(
            StratumEventKind::PrimaryFailbackEntered,
            "switching back to primary primary.example:3333",
            3000,
        );
        assert_eq!(
            status.primary_failback_state,
            PrimaryFailbackState::FailbackEntered
        );
        assert_eq!(status.last_primary_failback_unix_ms, 3000);
        assert_eq!(status.shares_accepted, 0);
    }
}
