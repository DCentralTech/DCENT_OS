//! LuxOS-shape pool-failover FSM runtime (RE-006 closure, Wave D 2026-05-19).
//!
//! The catalog at `dcentrald_api_types::luxos_pool_failover` provides the
//! pure types (`LuxosPoolState`, `LuxosFailoverTrigger`,
//! `LuxosPoolFailoverConfig`, `LuxosSmartSwitchState`, `linear_backoff_seconds`).
//! This module adds the **stateful runtime FSM** on top of those types —
//! per-pool error counter, state transitions, SmartSwitch probe timer,
//! deterministic round-robin failback decision.
//!
//! # Source of truth
//!
//! Clean-room implementation grounded in the RE team handoff at
//!
//! §RE-006 (Pool-Failover FSM). Behavioral spec extracted from the LuxOS
//! `a lab unit` live capture + `analysis/G-stratum-pool.md`. No proprietary code
//! copied — fields and constants come from the documented LuxOS TOML / API
//! surface plus this module's clean-room state machine.
//!
//! # Opt-in safety + wiring status (matrix §7 #2 / §6 SmartSwitch row)
//!
//! This module is COMPILED and, since SW-01 (2026-06-02), it CAN drive live
//! pool selection — but only when explicitly armed by BOTH gates described
//! below. Its shipped default is observe-only (byte-identical to the
//! pre-toggle daemon).
//!
//! The `[stratum].smart_failover_enabled` toggle (a.k.a.
//! `[pool].smart_failover_enabled` at the daemon `Config` layer) IS plumbed
//! end-to-end as of the §7-#2 wiring pass: `dcentrald.toml` →
//! `config::PoolConfig::smart_failover_enabled` → every
//! `StratumConfig::smart_failover_enabled` construction site →
//! `StratumV1Client` (readable via
//! `StratumV1Client::smart_failover_enabled()`), and surfaced truthfully in
//! `PoolFailoverStatus.smart_failover_enabled`. With the flag false (the
//! shipped default) the existing pool-failover-robustness logic in
//! `v1/client.rs` is the SOLE driver of pool selection and runtime behavior
//! is byte-identical to the pre-toggle daemon.
//!
//! Drive vs observe (SW-01): `StratumV1Client::shadow_observe_failover()`
//! feeds the FSM the trigger the existing failover logic is about to act on.
//! When `smart_failover_enabled` is ON it LOGS what the FSM *would* decide
//! (shadow). When — and ONLY when — `smart_failover_enabled` AND a drive arm
//! are BOTH set, the FSM's recommended active pool index is applied to
//! `current_pool_index` (the FSM drives selection). The drive arm is either
//! the `DCENT_POOL_FAILOVER_FSM_DRIVE` env gate (see [`ENV_FSM_DRIVE`]) or the
//! `[stratum].smart_failover_drive` config field — both default OFF. So the
//! shipped daemon runs observe-only and the legacy failover logic remains the
//! sole driver until an operator explicitly arms drive. Promoting drive to a
//! *default* (every miner) is a behavioral change gated on an operator soak
//! per-action authorization, NOT on host tests alone.
//!
//! PSF-1 (2026-06-20) — DRIVE-ARM ADVANCEMENT LIMITATION (read before relying on
//! drive to switch pools): in production `shadow_observe_failover()` is only ever
//! called with SAME-POOL triggers — `PoolInactivity` (no-notify) and
//! `TooManyRejections` (reject-rate). Both classify as
//! [`LuxosFailoverTrigger::reconnects_same_pool`], so the FSM keeps its active
//! index and the drive arm CANNOT advance the pool from those triggers even when
//! armed — it reconnects the SAME pool. The advancing triggers
//! (`TcpConnectTimeout`/`IoError`/`AuthError`/`TlsError`) are produced by the
//! connect/handshake-failure arm, which today drives real failover through the
//! legacy backoff loop in `v1/client.rs` and does NOT route through the FSM. So
//! the drive arm is currently observe-equivalent for pool *advancement*; wiring an
//! advancing trigger into the FSM is the (soak-gated) follow-up that would make
//! drive change pools. Pinned by the `v1/client.rs` test
//! `fov6_production_triggers_do_not_advance_under_drive`.
//!
//! # Public API
//!
//! ```ignore
//! let cfg = LuxosPoolFailoverConfig::default();
//! let mut fsm = PoolFailoverFsm::new(cfg, /* pool_count */ 3);
//! fsm.set_active(0);
//! fsm.mark_alive(0);
//!
//! // V1 client observes events as they happen:
//! let action = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
//! // action might be Reconnect { backoff_seconds: 1 } or NextPool { ... }
//!
//! // SmartSwitch is driven by tick():
//! let smart_switch_action = fsm.tick(60); // 60s elapsed since last tick
//! ```

use dcentrald_api_types::luxos_pool_failover::{
    linear_backoff_seconds, LuxosFailoverTrigger, LuxosPoolFailoverConfig, LuxosPoolState,
    LuxosSmartSwitchState,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Action emitted by the FSM
// ---------------------------------------------------------------------------

/// What the FSM decides after observing an event or a tick. The V1 client
/// integration consumes this and executes the actual TCP / Stratum work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailoverAction {
    /// No state change; continue current behavior.
    NoOp,
    /// Reconnect the same pool after `backoff_seconds`.
    Reconnect {
        pool_index: usize,
        backoff_seconds: u32,
    },
    /// Advance to the next pool in priority order.
    NextPool { from_index: usize },
    /// Drop this pool from the list (operator-removed).
    DropFromList { pool_index: usize },
    /// SmartSwitch detected a higher-priority pool came back online —
    /// switch back to it.
    FailbackToHigherPriority { target_index: usize },
    /// All pools are dead; halt mining (or hash-on-disconnect if enabled
    /// — that's an integration concern, not the FSM's job).
    AllPoolsDead,
}

impl FailoverAction {
    /// SW-01: the pool index this action wants the V1 client to make active,
    /// if any (pure → host-tested). This is the single decode point the
    /// (default-OFF, soak-gated) FSM-drives-selection path consults to map an
    /// FSM decision onto `current_pool_index`.
    ///
    /// - `NextPool { from_index }` and `DropFromList` do NOT carry the *target*
    ///   index (the FSM advanced internally), so they return `None` — the
    ///   caller reads `PoolFailoverFsm::active_pool()` for the resolved target.
    /// - `Reconnect` stays on the same pool → `Some(pool_index)`.
    /// - `FailbackToHigherPriority` → `Some(target_index)`.
    /// - `NoOp` / `AllPoolsDead` → `None` (no selection change to apply here).
    pub fn recommended_active_index(&self) -> Option<usize> {
        match *self {
            FailoverAction::Reconnect { pool_index, .. } => Some(pool_index),
            FailoverAction::FailbackToHigherPriority { target_index } => Some(target_index),
            FailoverAction::NoOp
            | FailoverAction::NextPool { .. }
            | FailoverAction::DropFromList { .. }
            | FailoverAction::AllPoolsDead => None,
        }
    }
}

/// SW-01: env gate that ARMS the FSM-drives-pool-selection path. Default-OFF.
///
/// The `PoolFailoverFsm` has, until now, only ever run in observe/shadow mode
/// (it logs what it *would* decide; the legacy failover logic actually selects
/// the pool). Promoting the FSM to *drive* selection is an active behavioral
/// change that must survive an operator soak first, so it is gated by BOTH the
/// existing `[stratum].smart_failover_enabled` config toggle AND this env var.
/// With either unset, behavior is byte-identical to the shipped daemon.
pub const ENV_FSM_DRIVE: &str = "DCENT_POOL_FAILOVER_FSM_DRIVE";

/// `true` only when the operator explicitly armed the FSM-drives-selection path
/// via `DCENT_POOL_FAILOVER_FSM_DRIVE` (truthy: `1`/`true`/`yes`/`on`). Unset →
/// `false` → the FSM stays observe-only. See [`ENV_FSM_DRIVE`].
pub fn fsm_drive_enabled() -> bool {
    std::env::var(ENV_FSM_DRIVE)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Per-pool entry
// ---------------------------------------------------------------------------

/// Per-pool runtime state — one row per configured pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolStateEntry {
    /// Current FSM state for this pool.
    pub state: LuxosPoolState,
    /// Consecutive errors since last successful connect (resets on Alive).
    pub consecutive_errors: u32,
    /// 1-indexed reconnect attempt counter (drives backoff).
    pub reconnect_attempt: u32,
    /// True if operator explicitly disabled this pool (not in active rotation).
    pub disabled: bool,
    /// True if pool was removed from list (state == Dead permanently).
    pub removed: bool,
}

impl PoolStateEntry {
    /// Fresh pool entry — Idle, no errors, not disabled.
    pub fn new() -> Self {
        Self {
            state: LuxosPoolState::Idle,
            consecutive_errors: 0,
            reconnect_attempt: 0,
            disabled: false,
            removed: false,
        }
    }

    /// True iff this pool is eligible to be tried (not disabled, not removed).
    pub fn is_eligible(&self) -> bool {
        !self.disabled && !self.removed
    }
}

impl Default for PoolStateEntry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// FSM
// ---------------------------------------------------------------------------

/// Runtime LuxOS-shape pool-failover state machine.
///
/// Pools are referenced by `pool_index` (0..pool_count); priority is implied
/// by index order (0 = highest priority). The active pool is the one
/// currently being mined against; SmartSwitch probes higher-priority pools
/// every `config.smart_switch_secs` and emits
/// `FailbackToHigherPriority` when one returns Alive.
#[derive(Debug, Clone)]
pub struct PoolFailoverFsm {
    pub config: LuxosPoolFailoverConfig,
    pools: Vec<PoolStateEntry>,
    active_index: Option<usize>,
    smart_switch_state: LuxosSmartSwitchState,
    smart_switch_elapsed: u32,
    /// Round-robin cursor for SmartSwitch probing of higher-priority pools.
    /// Resets to 0 whenever the active pool changes.
    smart_switch_probe_cursor: usize,
}

impl PoolFailoverFsm {
    /// Construct an FSM over `pool_count` pools, all starting in Idle.
    /// The first eligible pool will be made active via `set_active(0)` —
    /// the caller is expected to do this once configuration is loaded.
    pub fn new(config: LuxosPoolFailoverConfig, pool_count: usize) -> Self {
        Self {
            config,
            pools: vec![PoolStateEntry::new(); pool_count],
            active_index: None,
            smart_switch_state: LuxosSmartSwitchState::CheckerStarted,
            smart_switch_elapsed: 0,
            smart_switch_probe_cursor: 0,
        }
    }

    /// Number of configured pools.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Current active pool index (None if all pools are dead).
    pub fn active_pool(&self) -> Option<usize> {
        self.active_index
    }

    /// Get per-pool state entry.
    pub fn entry(&self, pool_index: usize) -> Option<&PoolStateEntry> {
        self.pools.get(pool_index)
    }

    /// Get the FSM state for a pool (Idle if index out of range).
    pub fn state_of(&self, pool_index: usize) -> LuxosPoolState {
        self.pools
            .get(pool_index)
            .map(|e| e.state)
            .unwrap_or(LuxosPoolState::Idle)
    }

    /// Current SmartSwitch state.
    pub fn smart_switch_state(&self) -> LuxosSmartSwitchState {
        self.smart_switch_state
    }

    /// Explicitly set the active pool. Transitions Idle → Connecting.
    /// No-op if pool_index is out of range or pool is ineligible.
    pub fn set_active(&mut self, pool_index: usize) {
        if let Some(entry) = self.pools.get_mut(pool_index) {
            if entry.is_eligible() {
                entry.state = LuxosPoolState::Connecting;
                entry.consecutive_errors = 0;
                entry.reconnect_attempt = 0;
                self.active_index = Some(pool_index);
                self.smart_switch_probe_cursor = 0;
            }
        }
    }

    /// Mark a pool's state as Subscribed (mid-handshake).
    pub fn mark_subscribed(&mut self, pool_index: usize) {
        if let Some(entry) = self.pools.get_mut(pool_index) {
            entry.state = LuxosPoolState::Subscribed;
        }
    }

    /// Mark a pool's state as Alive (mining-ready). Resets error counters.
    pub fn mark_alive(&mut self, pool_index: usize) {
        if let Some(entry) = self.pools.get_mut(pool_index) {
            entry.state = LuxosPoolState::Alive;
            entry.consecutive_errors = 0;
            entry.reconnect_attempt = 0;
        }
    }

    /// Observe a failover trigger on a specific pool. Returns the action the
    /// V1 client should take next. The FSM internally updates per-pool state
    /// + error counters + reconnect-attempt counters.
    pub fn observe(&mut self, pool_index: usize, trigger: LuxosFailoverTrigger) -> FailoverAction {
        // Out-of-range: NoOp.
        let entry = match self.pools.get_mut(pool_index) {
            Some(e) => e,
            None => return FailoverAction::NoOp,
        };

        // PoolRemovedByOperator: permanently drop. PoolDisabledByOperator:
        // mark disabled (can be re-enabled).
        match trigger {
            LuxosFailoverTrigger::PoolRemovedByOperator => {
                entry.state = LuxosPoolState::Dead;
                entry.removed = true;
                if self.active_index == Some(pool_index) {
                    return self
                        .advance_to_next(pool_index, FailoverAction::DropFromList { pool_index });
                }
                return FailoverAction::DropFromList { pool_index };
            }
            LuxosFailoverTrigger::PoolDisabledByOperator => {
                entry.state = LuxosPoolState::Dead;
                entry.disabled = true;
                if self.active_index == Some(pool_index) {
                    return self.advance_to_next(
                        pool_index,
                        FailoverAction::NextPool {
                            from_index: pool_index,
                        },
                    );
                }
                return FailoverAction::NoOp;
            }
            _ => {}
        }

        // Error-incrementing triggers: bump counter; if threshold reached,
        // mark Dead and advance to next pool. Otherwise reconnect same pool
        // with linear backoff.
        if trigger.increments_error_counter() {
            entry.consecutive_errors = entry.consecutive_errors.saturating_add(1);
            entry.reconnect_attempt = entry.reconnect_attempt.saturating_add(1);

            if entry.consecutive_errors >= self.config.max_errors {
                entry.state = LuxosPoolState::Dead;
                if self.active_index == Some(pool_index) {
                    return self.advance_to_next(
                        pool_index,
                        FailoverAction::NextPool {
                            from_index: pool_index,
                        },
                    );
                }
                return FailoverAction::NextPool {
                    from_index: pool_index,
                };
            }

            // Still under threshold: reconnect same pool with backoff.
            entry.state = LuxosPoolState::Connecting;
            let backoff = if self.config.backoff_on_error {
                linear_backoff_seconds(entry.reconnect_attempt)
            } else {
                0
            };
            return FailoverAction::Reconnect {
                pool_index,
                backoff_seconds: backoff,
            };
        }

        // Reconnect-same-pool triggers (StratumReadFailure, TooManyRejections,
        // PoolInactivity): close + reconnect WITHOUT incrementing the error
        // counter (the catalog's increments_error_counter() returns false for
        // these). Per the RE handoff: "Stratum read failure / Disconnected →
        // reconnect same pool".
        if trigger.reconnects_same_pool() {
            entry.state = LuxosPoolState::Connecting;
            entry.reconnect_attempt = entry.reconnect_attempt.saturating_add(1);
            let backoff = if self.config.backoff_on_error {
                linear_backoff_seconds(entry.reconnect_attempt)
            } else {
                0
            };
            return FailoverAction::Reconnect {
                pool_index,
                backoff_seconds: backoff,
            };
        }

        FailoverAction::NoOp
    }

    /// Advance the active pool to the next eligible pool in priority order.
    /// If none are eligible, returns AllPoolsDead. Otherwise returns the
    /// caller-provided action (typically NextPool or DropFromList) and
    /// updates self.active_index.
    fn advance_to_next(
        &mut self,
        from_index: usize,
        default_action: FailoverAction,
    ) -> FailoverAction {
        // Search wrapping from from_index+1 through all pools, skipping
        // ineligible ones (disabled, removed, or already Dead).
        let n = self.pools.len();
        for offset in 1..=n {
            let candidate = (from_index + offset) % n;
            if candidate == from_index {
                // Wrapped all the way around without finding an eligible pool.
                self.active_index = None;
                return FailoverAction::AllPoolsDead;
            }
            if let Some(entry) = self.pools.get(candidate) {
                if entry.is_eligible() && entry.state != LuxosPoolState::Dead {
                    self.active_index = Some(candidate);
                    if let Some(e) = self.pools.get_mut(candidate) {
                        e.state = LuxosPoolState::Connecting;
                        e.consecutive_errors = 0;
                        e.reconnect_attempt = 0;
                    }
                    self.smart_switch_probe_cursor = 0;
                    return default_action;
                }
            }
        }
        self.active_index = None;
        FailoverAction::AllPoolsDead
    }

    /// Tick the SmartSwitch sub-FSM. `elapsed_secs` is wall-time since the
    /// last call. Returns Some(FailbackToHigherPriority) when a higher-
    /// priority pool is detected alive; None otherwise.
    ///
    /// SmartSwitch probes pools in deterministic round-robin order (clean-
    /// room choice; LuxOS scoring/stickiness in binary not corpus-derivable).
    pub fn tick(&mut self, elapsed_secs: u32) -> Option<FailoverAction> {
        self.smart_switch_elapsed = self.smart_switch_elapsed.saturating_add(elapsed_secs);

        if self.smart_switch_elapsed < self.config.smart_switch_secs {
            return None;
        }
        // Time to probe.
        self.smart_switch_elapsed = 0;

        let active = match self.active_index {
            Some(idx) => idx,
            None => {
                self.smart_switch_state = LuxosSmartSwitchState::CheckerFinished;
                return None;
            }
        };

        // Active pool is index 0 = already highest priority. Nothing to probe.
        if active == 0 {
            self.smart_switch_state = LuxosSmartSwitchState::AllHigherDisabled;
            return None;
        }

        // Round-robin probe among pools with priority HIGHER than active
        // (i.e., index < active). Cursor wraps within [0..active).
        let probe_range = active;
        if probe_range == 0 {
            return None;
        }
        let probe_index = self.smart_switch_probe_cursor % probe_range;
        self.smart_switch_probe_cursor =
            self.smart_switch_probe_cursor.wrapping_add(1) % probe_range;

        self.smart_switch_state = LuxosSmartSwitchState::ProbeChecking;

        if let Some(entry) = self.pools.get(probe_index) {
            if entry.is_eligible() && entry.state == LuxosPoolState::Alive {
                self.smart_switch_state = LuxosSmartSwitchState::ProbeOnline;
                self.active_index = Some(probe_index);
                // bug-hunt LOW #7 (2026-05-28): reset the round-robin probe cursor
                // on failback, mirroring the other two active-change sites
                // (set_active :213, advance_to_next :356). Without this the cursor
                // (advanced at :401) carries a stale offset into the next probe
                // cycle, so the next round-robin starts mid-cycle (probe-order
                // unfairness — not a panic, but wrong fairness after every failback).
                self.smart_switch_probe_cursor = 0;
                return Some(FailoverAction::FailbackToHigherPriority {
                    target_index: probe_index,
                });
            }
        }

        self.smart_switch_state = LuxosSmartSwitchState::ProbeOffline;
        None
    }

    /// Operator re-enables a previously disabled pool. Returns to Idle.
    pub fn enable_pool(&mut self, pool_index: usize) {
        if let Some(entry) = self.pools.get_mut(pool_index) {
            if !entry.removed {
                entry.disabled = false;
                entry.state = LuxosPoolState::Idle;
                entry.consecutive_errors = 0;
                entry.reconnect_attempt = 0;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fsm_3_pools() -> PoolFailoverFsm {
        PoolFailoverFsm::new(LuxosPoolFailoverConfig::default(), 3)
    }

    #[test]
    fn new_fsm_all_idle_no_active() {
        let fsm = fsm_3_pools();
        assert_eq!(fsm.pool_count(), 3);
        assert_eq!(fsm.active_pool(), None);
        for i in 0..3 {
            assert_eq!(fsm.state_of(i), LuxosPoolState::Idle);
        }
    }

    #[test]
    fn set_active_transitions_idle_to_connecting() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        assert_eq!(fsm.active_pool(), Some(0));
        assert_eq!(fsm.state_of(0), LuxosPoolState::Connecting);
    }

    // SW-01: pure decode the FSM-drives-selection path consults to map an FSM
    // decision onto the V1 client's `current_pool_index`.
    #[test]
    fn recommended_active_index_decodes_each_action() {
        assert_eq!(FailoverAction::NoOp.recommended_active_index(), None);
        assert_eq!(
            FailoverAction::Reconnect {
                pool_index: 2,
                backoff_seconds: 5
            }
            .recommended_active_index(),
            Some(2)
        );
        // NextPool/DropFromList advance INSIDE the FSM; the action carries the
        // *from* index, not the target → None (caller reads active_pool()).
        assert_eq!(
            FailoverAction::NextPool { from_index: 0 }.recommended_active_index(),
            None
        );
        assert_eq!(
            FailoverAction::DropFromList { pool_index: 1 }.recommended_active_index(),
            None
        );
        assert_eq!(
            FailoverAction::FailbackToHigherPriority { target_index: 0 }.recommended_active_index(),
            Some(0)
        );
        assert_eq!(
            FailoverAction::AllPoolsDead.recommended_active_index(),
            None
        );
    }

    #[test]
    fn fsm_drive_gate_defaults_off() {
        // Default-OFF: unless the operator explicitly arms it, the FSM stays
        // observe-only. We don't set the env var here (and must not — process
        // env is global), so it must read false in a clean test process.
        // (Truthy parsing is exercised indirectly by the auth.rs env_truthy
        // pattern this mirrors; here we pin the safe default.)
        if std::env::var(ENV_FSM_DRIVE).is_err() {
            assert!(!fsm_drive_enabled());
        }
    }

    #[test]
    fn mark_alive_resets_error_counters() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        let _ = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
        let _ = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
        assert_eq!(fsm.entry(0).unwrap().consecutive_errors, 2);
        fsm.mark_alive(0);
        assert_eq!(fsm.state_of(0), LuxosPoolState::Alive);
        assert_eq!(fsm.entry(0).unwrap().consecutive_errors, 0);
    }

    #[test]
    fn error_under_threshold_reconnects_same_pool_with_backoff() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        let action = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
        match action {
            FailoverAction::Reconnect {
                pool_index,
                backoff_seconds,
            } => {
                assert_eq!(pool_index, 0);
                assert_eq!(backoff_seconds, 1); // attempt=1 → min(1,4)=1
            }
            _ => panic!("expected Reconnect; got {:?}", action),
        }
        assert_eq!(fsm.active_pool(), Some(0));
        assert_eq!(fsm.state_of(0), LuxosPoolState::Connecting);
    }

    #[test]
    fn linear_backoff_progression_caps_at_4() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        for expected in 1..=4u32 {
            let action = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
            if let FailoverAction::Reconnect {
                backoff_seconds, ..
            } = action
            {
                assert_eq!(backoff_seconds, expected.min(4));
            }
        }
        // 5th error trips threshold (max_errors=5) → NextPool, not Reconnect.
        let action = fsm.observe(0, LuxosFailoverTrigger::TcpConnectTimeout);
        match action {
            FailoverAction::NextPool { from_index } => assert_eq!(from_index, 0),
            _ => panic!("expected NextPool after 5 errors; got {:?}", action),
        }
    }

    #[test]
    fn error_at_threshold_advances_to_next_pool() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        // Hit max_errors (5) consecutively.
        for _ in 0..5 {
            let _ = fsm.observe(0, LuxosFailoverTrigger::IoError);
        }
        assert_eq!(fsm.state_of(0), LuxosPoolState::Dead);
        assert_eq!(fsm.active_pool(), Some(1));
        assert_eq!(fsm.state_of(1), LuxosPoolState::Connecting);
    }

    #[test]
    fn reconnect_same_pool_triggers_dont_increment_error_counter() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        // StratumReadFailure reconnects same pool but doesn't increment errors.
        for _ in 0..10 {
            let _ = fsm.observe(0, LuxosFailoverTrigger::StratumReadFailure);
        }
        assert_eq!(fsm.entry(0).unwrap().consecutive_errors, 0);
        assert_eq!(fsm.active_pool(), Some(0));
    }

    #[test]
    fn pool_removed_by_operator_drops_and_advances() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        let action = fsm.observe(0, LuxosFailoverTrigger::PoolRemovedByOperator);
        match action {
            FailoverAction::DropFromList { pool_index } => assert_eq!(pool_index, 0),
            _ => panic!("expected DropFromList; got {:?}", action),
        }
        assert!(fsm.entry(0).unwrap().removed);
        assert_eq!(fsm.active_pool(), Some(1));
    }

    #[test]
    fn pool_disabled_by_operator_marks_disabled() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        let _ = fsm.observe(0, LuxosFailoverTrigger::PoolDisabledByOperator);
        assert!(fsm.entry(0).unwrap().disabled);
        assert_eq!(fsm.active_pool(), Some(1));
        // Re-enable returns to Idle.
        fsm.enable_pool(0);
        assert!(!fsm.entry(0).unwrap().disabled);
        assert_eq!(fsm.state_of(0), LuxosPoolState::Idle);
    }

    #[test]
    fn all_pools_dead_returns_all_pools_dead() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        // Kill all 3 pools in turn.
        for _expected_pool in [0usize, 1, 2] {
            for _ in 0..5 {
                let _ = fsm.observe(fsm.active_pool().unwrap(), LuxosFailoverTrigger::IoError);
            }
        }
        // After the last pool dies, active should be None.
        assert_eq!(fsm.active_pool(), None);
        for i in 0..3 {
            assert_eq!(fsm.state_of(i), LuxosPoolState::Dead);
        }
    }

    #[test]
    fn smart_switch_no_op_before_interval() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(1); // active = secondary pool
        fsm.mark_alive(0); // higher-priority pool is alive
                           // tick with elapsed less than smart_switch_secs (60).
        assert_eq!(fsm.tick(30), None);
        assert_eq!(fsm.active_pool(), Some(1));
    }

    #[test]
    fn smart_switch_fails_back_when_higher_priority_alive() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(1); // active = secondary
        fsm.mark_alive(0); // higher-priority is alive again
        let action = fsm.tick(60);
        match action {
            Some(FailoverAction::FailbackToHigherPriority { target_index }) => {
                assert_eq!(target_index, 0);
            }
            _ => panic!("expected FailbackToHigherPriority; got {:?}", action),
        }
        assert_eq!(fsm.active_pool(), Some(0));
    }

    #[test]
    fn smart_switch_finds_offline_when_higher_priority_not_alive() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(1);
        // pool 0 is Idle (not Alive); smart switch should NOT fail back.
        let action = fsm.tick(60);
        assert_eq!(action, None);
        assert_eq!(
            fsm.smart_switch_state(),
            LuxosSmartSwitchState::ProbeOffline
        );
    }

    #[test]
    fn smart_switch_with_active_at_index_0_is_all_higher_disabled() {
        let mut fsm = fsm_3_pools();
        fsm.set_active(0);
        let action = fsm.tick(60);
        assert_eq!(action, None);
        assert_eq!(
            fsm.smart_switch_state(),
            LuxosSmartSwitchState::AllHigherDisabled
        );
    }

    #[test]
    fn smart_switch_round_robin_cursor_advances() {
        // Create FSM with 5 pools, active at index 4. SmartSwitch should
        // cycle through pools 0, 1, 2, 3 in round-robin order on each tick.
        let mut fsm = PoolFailoverFsm::new(LuxosPoolFailoverConfig::default(), 5);
        fsm.set_active(4);
        // None of the higher-priority pools are Alive — every tick returns None
        // but advances the cursor.
        for _ in 0..10 {
            assert_eq!(fsm.tick(60), None);
        }
        // Active pool unchanged (no failback target).
        assert_eq!(fsm.active_pool(), Some(4));
    }

    #[test]
    fn observe_unknown_pool_is_noop() {
        let mut fsm = fsm_3_pools();
        let action = fsm.observe(99, LuxosFailoverTrigger::IoError);
        assert_eq!(action, FailoverAction::NoOp);
    }

    #[test]
    fn config_default_matches_luxos_capture() {
        let cfg = LuxosPoolFailoverConfig::default();
        assert_eq!(cfg.timeout_secs, 5);
        assert_eq!(cfg.max_errors, 5);
        assert_eq!(cfg.smart_switch_secs, 60);
        assert!(cfg.backoff_on_error);
    }
}
