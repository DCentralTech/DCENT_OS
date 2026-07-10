//!  luxos-M — LuxOS pool-failover state machine + smart switch
//! + backoff schedule (HAL-free).
//!
//! Source RE evidence:
//!
//! §2 (Pool Failover State Machine, lines 122-217).
//!
//! Live `a lab unit` capture confirms the documented behavior: pool
//! transitions through 5 states (Idle / Connecting / Subscribed /
//! Alive / Dead), 10 failover triggers escalate via per-pool error
//! counter (default `max_errors=5`), smart-switch background task
//! probes higher-priority pools every `smart_switch_secs=60`, and
//! the backoff schedule on TCP/URL/IO failure is linear:
//! `min(attempt, 4)` seconds.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pool state
// ---------------------------------------------------------------------------

/// Per-pool runtime state per §2 diagram. The binary explicitly
/// enumerates `Connecting`, `Alive`, `Dead`; we ship the full 5-state
/// machine including `Idle` (pre-connect) and `Subscribed`
/// (handshake-mid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosPoolState {
    /// Pre-connect: pool list empty or daemon just started.
    Idle,
    /// TCP connection in progress.
    Connecting,
    /// TCP connected; mining.configure / subscribe / authorize in flight.
    Subscribed,
    /// Pool ready: receiving notify / set_difficulty / version_mask.
    Alive,
    /// Pool has reached the failover threshold or hard-disconnected.
    Dead,
}

impl LuxosPoolState {
    /// True iff the pool can accept share submissions (only `Alive`).
    pub fn is_mining_ready(&self) -> bool {
        matches!(self, Self::Alive)
    }

    /// True iff the pool is in a transient handshake state (still
    /// progressing toward Alive).
    pub fn is_in_handshake(&self) -> bool {
        matches!(self, Self::Connecting | Self::Subscribed)
    }
}

/// All 5 pool states in canonical order.
pub const ALL_POOL_STATES: &[LuxosPoolState] = &[
    LuxosPoolState::Idle,
    LuxosPoolState::Connecting,
    LuxosPoolState::Subscribed,
    LuxosPoolState::Alive,
    LuxosPoolState::Dead,
];

// ---------------------------------------------------------------------------
// Failover triggers (10 documented in §2.1)
// ---------------------------------------------------------------------------

/// One of the 10 documented failover triggers per §2.1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosFailoverTrigger {
    /// `timeout_secs=5` exceeded during TCP connect.
    TcpConnectTimeout,
    /// "missing port" / "invalid url schema: missing host".
    UrlParseError,
    /// I/O error connecting (POSIX errno via runner.rs).
    IoError,
    /// `mining.authorize` returned false.
    AuthError,
    /// TLS handshake failure.
    TlsError,
    /// `Stratum read failure ... Disconnected` (pool.rs).
    StratumReadFailure,
    /// `Too many rejections in a row` (pool.rs force-close).
    TooManyRejections,
    /// `connection dropped due to pool inactivity` (net.rs — 60s
    /// no TX from server side).
    PoolInactivity,
    /// Operator called `disablepool` via API.
    PoolDisabledByOperator,
    /// Operator called `removepool` via API.
    PoolRemovedByOperator,
}

impl LuxosFailoverTrigger {
    /// True iff this trigger increments the per-pool `error_inc`
    /// counter (escalates to NextPool when `error_inc >= max_errors`).
    pub fn increments_error_counter(&self) -> bool {
        matches!(
            self,
            Self::TcpConnectTimeout
                | Self::UrlParseError
                | Self::IoError
                | Self::AuthError
                | Self::TlsError
        )
    }

    /// True iff this trigger reconnects the SAME pool (vs advancing
    /// to the next pool in the list).
    pub fn reconnects_same_pool(&self) -> bool {
        matches!(
            self,
            Self::StratumReadFailure | Self::TooManyRejections | Self::PoolInactivity
        )
    }

    /// True iff this trigger drops the pool from the list entirely
    /// (operator-removed pool).
    pub fn drops_from_list(&self) -> bool {
        matches!(self, Self::PoolRemovedByOperator)
    }
}

/// All 10 documented failover triggers in stable order.
pub const ALL_FAILOVER_TRIGGERS: &[LuxosFailoverTrigger] = &[
    LuxosFailoverTrigger::TcpConnectTimeout,
    LuxosFailoverTrigger::UrlParseError,
    LuxosFailoverTrigger::IoError,
    LuxosFailoverTrigger::AuthError,
    LuxosFailoverTrigger::TlsError,
    LuxosFailoverTrigger::StratumReadFailure,
    LuxosFailoverTrigger::TooManyRejections,
    LuxosFailoverTrigger::PoolInactivity,
    LuxosFailoverTrigger::PoolDisabledByOperator,
    LuxosFailoverTrigger::PoolRemovedByOperator,
];

// ---------------------------------------------------------------------------
// Failover config
// ---------------------------------------------------------------------------

/// Pool-failover configuration per §2.1 + §2.3 + smart-switch §2.2.
/// Defaults match live `a lab unit` capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuxosPoolFailoverConfig {
    /// TCP connect timeout (seconds).
    pub timeout_secs: u32,
    /// Errors per pool before advancing to NextPool.
    pub max_errors: u32,
    /// Smart-switch background task tick interval (seconds).
    pub smart_switch_secs: u32,
    /// True = linear backoff on TCP/URL/IO failure (default).
    pub backoff_on_error: bool,
}

impl Default for LuxosPoolFailoverConfig {
    fn default() -> Self {
        // Live `a lab unit` defaults per §2.1 + §2.2.
        Self {
            timeout_secs: 5,
            max_errors: 5,
            smart_switch_secs: 60,
            backoff_on_error: true,
        }
    }
}

/// Linear backoff schedule per §2.3: `min(attempt, 4)` seconds.
/// `attempt` is 1-indexed (first retry = 1).
pub fn linear_backoff_seconds(attempt: u32) -> u32 {
    attempt.min(4)
}

// ---------------------------------------------------------------------------
// Smart-switch state strings
// ---------------------------------------------------------------------------

/// Smart-switch background-task state per §2.2 binary strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosSmartSwitchState {
    /// "Starting smart switch pool checker"
    CheckerStarted,
    /// "Smart switch: checking pool connection"
    ProbeChecking,
    /// "Smart switch: pool is back online; sending switch signal"
    ProbeOnline,
    /// "Smart switch: pool is still offline"
    ProbeOffline,
    /// "A pool with a higher prio is back online, triggering reconnect process"
    HigherPrioOnline,
    /// "Smart switch checker finished"
    CheckerFinished,
    /// "All higher priority pools are disabled, smart switch will be
    ///  disabled for current pool"
    AllHigherDisabled,
}

/// All 7 documented smart-switch states per §2.2 binary strings.
pub const ALL_SMART_SWITCH_STATES: &[LuxosSmartSwitchState] = &[
    LuxosSmartSwitchState::CheckerStarted,
    LuxosSmartSwitchState::ProbeChecking,
    LuxosSmartSwitchState::ProbeOnline,
    LuxosSmartSwitchState::ProbeOffline,
    LuxosSmartSwitchState::HigherPrioOnline,
    LuxosSmartSwitchState::CheckerFinished,
    LuxosSmartSwitchState::AllHigherDisabled,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_state_count_pinned_to_5() {
        // Idle / Connecting / Subscribed / Alive / Dead.
        assert_eq!(ALL_POOL_STATES.len(), 5);
    }

    #[test]
    fn only_alive_is_mining_ready() {
        assert!(LuxosPoolState::Alive.is_mining_ready());
        for s in [
            LuxosPoolState::Idle,
            LuxosPoolState::Connecting,
            LuxosPoolState::Subscribed,
            LuxosPoolState::Dead,
        ] {
            assert!(!s.is_mining_ready(), "{:?} should not be mining-ready", s);
        }
    }

    #[test]
    fn handshake_states_are_connecting_and_subscribed() {
        assert!(LuxosPoolState::Connecting.is_in_handshake());
        assert!(LuxosPoolState::Subscribed.is_in_handshake());
        for s in [
            LuxosPoolState::Idle,
            LuxosPoolState::Alive,
            LuxosPoolState::Dead,
        ] {
            assert!(!s.is_in_handshake(), "{:?} not in handshake", s);
        }
    }

    #[test]
    fn failover_trigger_count_pinned_to_10() {
        // §2.1 table has 10 rows.
        assert_eq!(ALL_FAILOVER_TRIGGERS.len(), 10);
    }

    #[test]
    fn error_counter_increment_set_matches_re_doc() {
        // Per §2.1: TCP timeout / URL parse / I/O / Auth / TLS all
        // increment error_inc. Stratum read / too many rejections /
        // inactivity reconnect SAME pool. Operator pool-disable /
        // pool-remove are operator-driven (no error counter).
        for t in [
            LuxosFailoverTrigger::TcpConnectTimeout,
            LuxosFailoverTrigger::UrlParseError,
            LuxosFailoverTrigger::IoError,
            LuxosFailoverTrigger::AuthError,
            LuxosFailoverTrigger::TlsError,
        ] {
            assert!(
                t.increments_error_counter(),
                "{:?} should increment error counter",
                t
            );
        }
        for t in [
            LuxosFailoverTrigger::StratumReadFailure,
            LuxosFailoverTrigger::TooManyRejections,
            LuxosFailoverTrigger::PoolInactivity,
            LuxosFailoverTrigger::PoolDisabledByOperator,
            LuxosFailoverTrigger::PoolRemovedByOperator,
        ] {
            assert!(
                !t.increments_error_counter(),
                "{:?} should NOT increment error counter",
                t
            );
        }
    }

    #[test]
    fn reconnects_same_pool_set_matches_re_doc() {
        // Stratum read failure / too many rejections / pool inactivity
        // all reconnect the SAME pool.
        for t in [
            LuxosFailoverTrigger::StratumReadFailure,
            LuxosFailoverTrigger::TooManyRejections,
            LuxosFailoverTrigger::PoolInactivity,
        ] {
            assert!(t.reconnects_same_pool(), "{:?} same-pool", t);
        }
        // Operator-removed pool is the only one that drops from list.
        assert!(LuxosFailoverTrigger::PoolRemovedByOperator.drops_from_list());
        assert!(!LuxosFailoverTrigger::PoolDisabledByOperator.drops_from_list());
    }

    #[test]
    fn default_config_matches_live_79_capture() {
        let cfg = LuxosPoolFailoverConfig::default();
        assert_eq!(cfg.timeout_secs, 5);
        assert_eq!(cfg.max_errors, 5);
        assert_eq!(cfg.smart_switch_secs, 60);
        assert!(cfg.backoff_on_error);
    }

    #[test]
    fn linear_backoff_schedule_matches_re_doc() {
        // §2.3 documented schedule:
        // attempt 1 → 1s
        // attempt 2 → 2s
        // attempt 3 → 3s
        // attempt 4 → 4s
        // attempt 5 → 4s (capped)
        assert_eq!(linear_backoff_seconds(1), 1);
        assert_eq!(linear_backoff_seconds(2), 2);
        assert_eq!(linear_backoff_seconds(3), 3);
        assert_eq!(linear_backoff_seconds(4), 4);
        assert_eq!(linear_backoff_seconds(5), 4);
        assert_eq!(linear_backoff_seconds(100), 4);
    }

    #[test]
    fn smart_switch_state_count_pinned_to_7() {
        // §2.2 documents 7 distinct strings.
        assert_eq!(ALL_SMART_SWITCH_STATES.len(), 7);
    }

    #[test]
    fn pool_state_round_trips_through_serde() {
        for s in ALL_POOL_STATES.iter().copied() {
            let json = serde_json::to_string(&s).unwrap();
            let back: LuxosPoolState = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn pool_state_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosPoolState::Idle).unwrap(),
            "\"idle\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosPoolState::Connecting).unwrap(),
            "\"connecting\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosPoolState::Subscribed).unwrap(),
            "\"subscribed\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosPoolState::Alive).unwrap(),
            "\"alive\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosPoolState::Dead).unwrap(),
            "\"dead\""
        );
    }

    #[test]
    fn failover_trigger_round_trips_through_serde() {
        for t in ALL_FAILOVER_TRIGGERS.iter().copied() {
            let json = serde_json::to_string(&t).unwrap();
            let back: LuxosFailoverTrigger = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn smart_switch_state_round_trips_through_serde() {
        for s in ALL_SMART_SWITCH_STATES.iter().copied() {
            let json = serde_json::to_string(&s).unwrap();
            let back: LuxosSmartSwitchState = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn config_round_trips_through_serde() {
        let original = LuxosPoolFailoverConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosPoolFailoverConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn pool_state_groupings_partition_correctly() {
        // Mining-ready + handshake + neither must partition the
        // 5 states without overlap. Idle and Dead are "neither".
        let mining_ready: Vec<LuxosPoolState> = ALL_POOL_STATES
            .iter()
            .copied()
            .filter(|s| s.is_mining_ready())
            .collect();
        let handshake: Vec<LuxosPoolState> = ALL_POOL_STATES
            .iter()
            .copied()
            .filter(|s| s.is_in_handshake())
            .collect();
        assert_eq!(mining_ready, vec![LuxosPoolState::Alive]);
        assert_eq!(
            handshake,
            vec![LuxosPoolState::Connecting, LuxosPoolState::Subscribed]
        );
        // Idle + Dead are in neither bucket.
        assert!(!LuxosPoolState::Idle.is_mining_ready());
        assert!(!LuxosPoolState::Idle.is_in_handshake());
        assert!(!LuxosPoolState::Dead.is_mining_ready());
        assert!(!LuxosPoolState::Dead.is_in_handshake());
    }
}
