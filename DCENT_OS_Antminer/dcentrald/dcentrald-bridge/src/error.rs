//! Error types for the bridge client (spec §7).
//!
//! `PairError` carries the per-status-code policy outcomes from `/pair`
//! (spec §2.5); `BridgeError` is the broader client error for heartbeat /
//! telemetry / OTA paths.

/// Errors from a single `/pair` attempt, carrying the firmware status policy.
#[derive(Debug, thiserror::Error)]
pub enum PairError {
    /// 503 `time_not_synced` — bridge SNTP gate (spec §2.5). Retry after 15 s.
    #[error("bridge clock not synced (HTTP 503)")]
    TimeNotSynced,

    /// 401 — HMAC mismatch, `ts` skew, or missing proof. Do not blind-retry.
    #[error("HMAC mismatch or skew (HTTP 401): {0}")]
    AuthFailed(String),

    /// 400 — malformed body. Fix the body; do not retry as-is.
    #[error("malformed pair request (HTTP 400): {0}")]
    BadRequest(String),

    /// 403 — enrollment closed; operator must long-press the setup button.
    #[error("enrollment locked (HTTP 403); operator must press setup button")]
    EnrollmentLocked,

    /// 409 `replay` — identical `(device_id, miner_mac, ts)` was just sent.
    /// Regenerate `ts` and re-sign before retrying (spec §2.5 / firmware).
    #[error("replay rejected (HTTP 409); regenerate ts and re-sign")]
    Replay,

    /// Any other non-2xx HTTP status.
    #[error("bridge returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Network / socket / decode transport failure.
    #[error("bridge transport error: {0}")]
    Transport(String),

    /// Catch-all (e.g. clock-before-epoch, JSON decode of a 200 body).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl PairError {
    /// Whether the retry wrapper should keep trying after this error.
    ///
    /// Fast-fail (no retry): 400 / 401 / 403.
    /// Retry: 503, 5xx, transport, and 409-replay (after a `ts` refresh).
    pub fn is_retryable(&self) -> bool {
        match self {
            PairError::BadRequest(_) | PairError::AuthFailed(_) | PairError::EnrollmentLocked => {
                false
            }
            PairError::TimeNotSynced | PairError::Replay | PairError::Transport(_) => true,
            PairError::Http { status, .. } => *status >= 500,
            PairError::Other(_) => false,
        }
    }
}

/// Broader client error for the heartbeat / telemetry / OTA surfaces.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// HTTP non-2xx with the captured status + body.
    #[error("bridge returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Network / socket / timeout / decode transport failure.
    #[error("bridge transport error: {0}")]
    Transport(String),

    /// RESERVED — not currently produced. The 200 + `paired:false` re-pair
    /// signal flows through `HeartbeatOutcome::NeedsRepair` (client.rs::heartbeat),
    /// NOT this variant — a repo-wide search finds `NotPaired` only at this
    /// definition site. Kept for a possible future direct-error path; do NOT
    /// `match` on it expecting the re-pair condition to arrive here. (gap-swarm
    /// no-HAL hunt #9: the prior doc-comment implied this fires on paired:false,
    /// which it does not.)
    #[error("bridge reports not paired; re-pair required")]
    NotPaired,

    /// 403 on heartbeat — bridge paired to a different miner.
    #[error("bridge paired to a different miner (HTTP 403)")]
    WrongMiner,

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Map a `reqwest::Error` into a `PairError::Transport`.
impl From<reqwest::Error> for PairError {
    fn from(e: reqwest::Error) -> Self {
        PairError::Transport(e.to_string())
    }
}

/// Map a `reqwest::Error` into a `BridgeError::Transport`.
impl From<reqwest::Error> for BridgeError {
    fn from(e: reqwest::Error) -> Self {
        BridgeError::Transport(e.to_string())
    }
}
