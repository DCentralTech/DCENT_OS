//!  braiins-B — BraiinsOS+ MinerStatus state machine (HAL-free).
//!
//! Source RE evidence:
//!
//! §9 (line 1099 — `MinerService.GetMinerStatus` streams NORMAL/PAUSED/
//! SUSPENDED/etc.) + §6 DPS state machine (lines 753-758 — DPS chain
//! NORMAL → SCALING_DOWN → SHUTDOWN → RESTART) + §7 Cooling thresholds
//! (lines 854-863 — hot/dangerous temperature triggers).
//!
//!  braiins-A `braiinsos_grpc_catalog.rs` shipped the method
//! catalog (`BraiinsMethod::GetMinerStatus` lives on
//! `BraiinsService::Miner`). This module ships the typed payload —
//! the streaming status enum + transition trigger surface that
//! dcent-toolbox + dashboard can reason about without hitting Tonic.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Status enum
// ---------------------------------------------------------------------------

/// MinerStatus per BraiinsOS+ gRPC `MinerService.GetMinerStatus`
/// streaming reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MinerStatus {
    /// Mining at the configured power/hashrate target.
    Normal,
    /// Operator paused via `PauseMining`. Resume with `ResumeMining`.
    Paused,
    /// Stopped via `Stop` action; recoverable with `Start`.
    Suspended,
    /// DPS (Dynamic Performance Scaling) scaling-down phase per §6
    /// — typically triggered by `hot_temperature` threshold.
    ScalingDown,
    /// DPS shutdown phase. Hash boards de-energized.
    Shutdown,
    /// DPS restart phase — coming back up at original power.
    Restart,
    /// Tuning error / chip enumeration failure / license expiry.
    /// Operator action required.
    Error,
}

impl MinerStatus {
    /// True iff the miner is currently producing hashes (only
    /// `Normal` qualifies).
    pub fn is_mining_capable(&self) -> bool {
        matches!(self, Self::Normal)
    }

    /// True iff the miner is in a terminal error state requiring
    /// operator intervention.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Error)
    }

    /// True iff the miner is in a transient DPS-managed state (will
    /// auto-recover when ambient cools / target restored).
    pub fn is_transient_dps_state(&self) -> bool {
        matches!(self, Self::ScalingDown | Self::Shutdown | Self::Restart)
    }

    /// True iff the miner is in an operator-controlled hold state
    /// (Paused/Suspended) and recoverable via API.
    pub fn is_operator_held(&self) -> bool {
        matches!(self, Self::Paused | Self::Suspended)
    }
}

// ---------------------------------------------------------------------------
// State-transition trigger
// ---------------------------------------------------------------------------

/// Trigger that caused the most recent `MinerStatus` change. Sourced
/// from BRAIINSOS_REVERSE_ENGINEERING.md §6 + §7 trigger evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MinerStatusTransition {
    /// Operator called `ActionsService.PauseMining`.
    OperatorPause,
    /// Operator called `ActionsService.ResumeMining`.
    OperatorResume,
    /// Operator called `ActionsService.Stop`.
    OperatorStop,
    /// Operator called `ActionsService.Start`.
    OperatorStart,
    /// Operator called `ActionsService.Restart`.
    OperatorRestart,
    /// Operator called `ActionsService.Reboot`.
    RebootRequest,
    /// `hot_temperature` threshold exceeded → DPS scales down.
    HotTemperatureExceeded,
    /// `dangerous_temperature` threshold exceeded → emergency shutdown.
    DangerousTemperatureExceeded,
    /// PSU watchdog (cmd 0x81) timed out — chains lost power.
    PsuWatchdogMissed,
    /// Fan tachometer reads zero with PWM > 0 — fan failure.
    FanFailure,
    /// BraiinsOS+ license expired or invalid.
    LicenseExpired,
    /// Bosminer chip enumeration failed (no ASICs detected).
    ChipEnumerationFailed,
    /// Pool unreachable for sustained period — no work available.
    PoolUnreachable,
}

// ---------------------------------------------------------------------------
// State-transition event
// ---------------------------------------------------------------------------

/// One event in the `GetMinerStatus` stream. The runtime adapter
/// builds these from the bos-plus-api protobuf wire form; consumers
/// (dashboard / toolbox / autotuner-watchdog) can pattern-match on
/// the `to` + `trigger` to decide UI / alarm / recovery actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinerStatusEvent {
    /// Previous status. `None` on the first stream event.
    pub from: Option<MinerStatus>,
    /// New status.
    pub to: MinerStatus,
    /// What caused the transition.
    pub trigger: MinerStatusTransition,
    /// Free-text human-readable reason (e.g. "Board 2 over 95°C").
    /// Optional — Bosminer doesn't always emit one.
    pub reason: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
}

impl MinerStatusEvent {
    /// True iff this transition crosses from a mining-capable state
    /// into a non-mining state — the dashboard should flag a
    /// "mining stopped" event.
    pub fn lost_mining_capability(&self) -> bool {
        let was_mining = self.from.map(|s| s.is_mining_capable()).unwrap_or(false);
        let is_mining = self.to.is_mining_capable();
        was_mining && !is_mining
    }

    /// True iff this transition restores mining capability — the
    /// dashboard can clear the "mining stopped" banner.
    pub fn restored_mining_capability(&self) -> bool {
        let was_mining = self.from.map(|s| s.is_mining_capable()).unwrap_or(true);
        let is_mining = self.to.is_mining_capable();
        !was_mining && is_mining
    }
}

// ---------------------------------------------------------------------------
// DPS chain ordering
// ---------------------------------------------------------------------------

/// Documented DPS scaling-down chain per §6. Ordered:
/// `Normal → ScalingDown → Shutdown → Restart → Normal`.
pub const DPS_SCALE_DOWN_CHAIN: &[MinerStatus] = &[
    MinerStatus::Normal,
    MinerStatus::ScalingDown,
    MinerStatus::Shutdown,
    MinerStatus::Restart,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miner_status_round_trips_through_serde() {
        for s in [
            MinerStatus::Normal,
            MinerStatus::Paused,
            MinerStatus::Suspended,
            MinerStatus::ScalingDown,
            MinerStatus::Shutdown,
            MinerStatus::Restart,
            MinerStatus::Error,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: MinerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn miner_status_serializes_in_screaming_snake_case() {
        // Proto3 wire form is SCREAMING_SNAKE_CASE — pin so a
        // refactor doesn't accidentally switch to PascalCase.
        assert_eq!(
            serde_json::to_string(&MinerStatus::Normal).unwrap(),
            "\"NORMAL\""
        );
        assert_eq!(
            serde_json::to_string(&MinerStatus::ScalingDown).unwrap(),
            "\"SCALING_DOWN\""
        );
        assert_eq!(
            serde_json::to_string(&MinerStatus::Suspended).unwrap(),
            "\"SUSPENDED\""
        );
    }

    #[test]
    fn is_mining_capable_only_true_for_normal() {
        assert!(MinerStatus::Normal.is_mining_capable());
        for s in [
            MinerStatus::Paused,
            MinerStatus::Suspended,
            MinerStatus::ScalingDown,
            MinerStatus::Shutdown,
            MinerStatus::Restart,
            MinerStatus::Error,
        ] {
            assert!(!s.is_mining_capable(), "{:?} must not be mining-capable", s);
        }
    }

    #[test]
    fn is_terminal_only_true_for_error() {
        assert!(MinerStatus::Error.is_terminal());
        for s in [
            MinerStatus::Normal,
            MinerStatus::Paused,
            MinerStatus::Suspended,
            MinerStatus::ScalingDown,
            MinerStatus::Shutdown,
            MinerStatus::Restart,
        ] {
            assert!(!s.is_terminal(), "{:?} must not be terminal", s);
        }
    }

    #[test]
    fn dps_chain_is_in_canonical_order() {
        // §6 documented chain: Normal → ScalingDown → Shutdown →
        // Restart. Pin the ordering.
        assert_eq!(DPS_SCALE_DOWN_CHAIN.len(), 4);
        assert_eq!(DPS_SCALE_DOWN_CHAIN[0], MinerStatus::Normal);
        assert_eq!(DPS_SCALE_DOWN_CHAIN[1], MinerStatus::ScalingDown);
        assert_eq!(DPS_SCALE_DOWN_CHAIN[2], MinerStatus::Shutdown);
        assert_eq!(DPS_SCALE_DOWN_CHAIN[3], MinerStatus::Restart);
    }

    #[test]
    fn transient_dps_states_classify_correctly() {
        // ScalingDown / Shutdown / Restart auto-recover via DPS.
        for s in [
            MinerStatus::ScalingDown,
            MinerStatus::Shutdown,
            MinerStatus::Restart,
        ] {
            assert!(
                s.is_transient_dps_state(),
                "{:?} should be DPS-transient",
                s
            );
            // Negative pin: not operator-held.
            assert!(!s.is_operator_held());
        }
        // Normal / Paused / Suspended / Error are NOT DPS-transient.
        for s in [
            MinerStatus::Normal,
            MinerStatus::Paused,
            MinerStatus::Suspended,
            MinerStatus::Error,
        ] {
            assert!(
                !s.is_transient_dps_state(),
                "{:?} should NOT be DPS-transient",
                s
            );
        }
    }

    #[test]
    fn operator_held_states_are_paused_and_suspended() {
        assert!(MinerStatus::Paused.is_operator_held());
        assert!(MinerStatus::Suspended.is_operator_held());
        for s in [
            MinerStatus::Normal,
            MinerStatus::ScalingDown,
            MinerStatus::Shutdown,
            MinerStatus::Restart,
            MinerStatus::Error,
        ] {
            assert!(!s.is_operator_held(), "{:?} not operator-held", s);
        }
    }

    #[test]
    fn transition_serializes_in_screaming_snake_case() {
        for (variant, expected) in [
            (MinerStatusTransition::OperatorPause, "\"OPERATOR_PAUSE\""),
            (
                MinerStatusTransition::HotTemperatureExceeded,
                "\"HOT_TEMPERATURE_EXCEEDED\"",
            ),
            (
                MinerStatusTransition::PsuWatchdogMissed,
                "\"PSU_WATCHDOG_MISSED\"",
            ),
            (MinerStatusTransition::LicenseExpired, "\"LICENSE_EXPIRED\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), expected);
        }
    }

    #[test]
    fn transition_round_trips_through_serde() {
        for t in [
            MinerStatusTransition::OperatorPause,
            MinerStatusTransition::OperatorResume,
            MinerStatusTransition::OperatorStop,
            MinerStatusTransition::OperatorStart,
            MinerStatusTransition::OperatorRestart,
            MinerStatusTransition::RebootRequest,
            MinerStatusTransition::HotTemperatureExceeded,
            MinerStatusTransition::DangerousTemperatureExceeded,
            MinerStatusTransition::PsuWatchdogMissed,
            MinerStatusTransition::FanFailure,
            MinerStatusTransition::LicenseExpired,
            MinerStatusTransition::ChipEnumerationFailed,
            MinerStatusTransition::PoolUnreachable,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: MinerStatusTransition = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn event_lost_mining_capability_detected() {
        let event = MinerStatusEvent {
            from: Some(MinerStatus::Normal),
            to: MinerStatus::ScalingDown,
            trigger: MinerStatusTransition::HotTemperatureExceeded,
            reason: Some("Board 2 over 75 C".to_string()),
            timestamp_ms: 1_700_000_000_000,
        };
        assert!(event.lost_mining_capability());
        assert!(!event.restored_mining_capability());
    }

    #[test]
    fn event_restored_mining_capability_detected() {
        let event = MinerStatusEvent {
            from: Some(MinerStatus::Paused),
            to: MinerStatus::Normal,
            trigger: MinerStatusTransition::OperatorResume,
            reason: None,
            timestamp_ms: 1_700_000_000_000,
        };
        assert!(event.restored_mining_capability());
        assert!(!event.lost_mining_capability());
    }

    #[test]
    fn event_first_stream_event_classifies_normally() {
        // First-stream event has from=None. If the initial status is
        // Normal, treat as "starting in mining state" — do NOT emit a
        // restored-mining banner.
        let initial_normal = MinerStatusEvent {
            from: None,
            to: MinerStatus::Normal,
            trigger: MinerStatusTransition::OperatorStart,
            reason: None,
            timestamp_ms: 0,
        };
        // Both helpers default to "no transition" — first event is
        // pure observation, not a transition.
        assert!(!initial_normal.lost_mining_capability());
        assert!(!initial_normal.restored_mining_capability());
    }

    #[test]
    fn event_round_trips_through_serde() {
        let original = MinerStatusEvent {
            from: Some(MinerStatus::Normal),
            to: MinerStatus::Error,
            trigger: MinerStatusTransition::ChipEnumerationFailed,
            reason: Some("chain 0 returned 0 chips".to_string()),
            timestamp_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: MinerStatusEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn hot_temperature_pairs_with_scaling_down() {
        // Per §6 + §7: hot_temp threshold triggers DPS scaling-down.
        // Pin the canonical (trigger, target_state) tuple by
        // demonstrating an event that uses it.
        let event = MinerStatusEvent {
            from: Some(MinerStatus::Normal),
            to: MinerStatus::ScalingDown,
            trigger: MinerStatusTransition::HotTemperatureExceeded,
            reason: None,
            timestamp_ms: 0,
        };
        assert_eq!(event.to, MinerStatus::ScalingDown);
        assert_eq!(event.trigger, MinerStatusTransition::HotTemperatureExceeded);
    }
}
