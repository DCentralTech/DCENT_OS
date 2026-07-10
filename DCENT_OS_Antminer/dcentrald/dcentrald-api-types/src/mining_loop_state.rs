//!  mln-A — Top-level mining-loop state machine (HAL-free).
//!
//! Source RE evidence:
//!
//! and `mining-loop-pseudocode.md`.
//!
//! Captures the high-level mining-orchestration states, distinct from
//! the cold-boot states in `power_state.rs`. The mining loop transitions:
//!
//! ```text
//!   Idle  ─enable→  Initializing  ─chains_ready→  Connecting
//!                                                      │
//!                                                      ▼
//!                                                   Running
//!                                                      │ no_notify_120s / chains_no_nonces_60s
//!                                                      ▼
//!                                                   Stalled  ─reset_attempted→  Recovering
//!                                                                                    │
//!                                              ┌─────────────────────────────────────┘
//!                                              ▼
//!                                           Running (on success) / Faulted (on giving up)
//! ```
//!
//! Plus terminal `Stopping` / `Stopped` for graceful shutdown and
//! `Faulted` for operator-intervention-required end state.
//!
//! HAL-free pure state machine. The runtime adapter consumes
//! observations (notify-age, chain-nonce-age, pool-state, operator
//! commands) and reads back the next state. Mirrors the
//!  `power_state` shape.

use serde::{Deserialize, Serialize};

/// Discrete mining-loop states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiningLoopState {
    /// Daemon up, mining not yet started.
    Idle,
    /// Chains coming up via `power_cold_boot` (Phase A→READY).
    Initializing,
    /// Stratum subscribe + authorize + first-notify in flight.
    Connecting,
    /// Receiving jobs, dispatching work, accepting nonces.
    Running,
    /// No `mining.notify` for >120 s, or chains stopped producing
    /// nonces for >60 s. Runtime decides whether to recover.
    Stalled,
    /// Runtime adapter is attempting a recovery (chain reset, pool
    /// reconnect, etc.). Returns to `Running` on success, `Faulted`
    /// on giving up.
    Recovering,
    /// Terminal — operator intervention required. Cleared only by
    /// `reset()`.
    Faulted,
    /// Graceful shutdown in progress (SIGTERM received).
    Stopping,
    /// Terminal — daemon stopped after Stopping. Cleared only by
    /// `reset()`.
    Stopped,
}

/// Per-tick observation for the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiningObservation {
    /// Operator has explicitly enabled mining (Idle → Initializing).
    pub operator_enabled: bool,
    /// Operator has requested graceful shutdown (any → Stopping).
    pub operator_stop_requested: bool,
    /// All chains have reached `READY` per `power_state.rs`.
    pub chains_ready: bool,
    /// Stratum client is `subscribed AND authorized AND has-first-notify`.
    pub stratum_connected: bool,
    /// Age in seconds since last `mining.notify` from the active pool.
    /// `None` if no notify has ever arrived.
    pub last_notify_age_s: Option<u64>,
    /// Age in seconds since the most recent nonce from any chain.
    /// `None` if no nonce has ever arrived.
    pub last_nonce_age_s: Option<u64>,
    /// Runtime adapter reports a recovery attempt has succeeded
    /// (Recovering → Running).
    pub recovery_succeeded: bool,
    /// Runtime adapter has given up on recovery (Recovering → Faulted).
    pub recovery_exhausted: bool,
}

impl MiningObservation {
    /// All-default observation — useful for tests.
    pub const fn idle() -> Self {
        Self {
            operator_enabled: false,
            operator_stop_requested: false,
            chains_ready: false,
            stratum_connected: false,
            last_notify_age_s: None,
            last_nonce_age_s: None,
            recovery_succeeded: false,
            recovery_exhausted: false,
        }
    }
}

/// Configuration thresholds. Defaults match operator-empirical values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiningLoopConfig {
    /// Stalled trigger: notify age above this → Stalled.
    pub stall_notify_age_s: u64,
    /// Stalled trigger: nonce age above this → Stalled.
    pub stall_nonce_age_s: u64,
}

impl Default for MiningLoopConfig {
    fn default() -> Self {
        Self {
            // Per `mining-core-bible.md`: 120 s no-notify is treated as
            // a stall.
            stall_notify_age_s: 120,
            // Per `mining-core-bible.md` + watchdog policy: 60 s without
            // any nonce on a chain that's supposed to be running.
            stall_nonce_age_s: 60,
        }
    }
}

/// State machine. One per miner.
#[derive(Debug, Clone)]
pub struct MiningLoopFsm {
    state: MiningLoopState,
    config: MiningLoopConfig,
    samples_total: u64,
}

impl MiningLoopFsm {
    pub fn new(config: MiningLoopConfig) -> Self {
        Self {
            state: MiningLoopState::Idle,
            config,
            samples_total: 0,
        }
    }

    pub fn fresh() -> Self {
        Self::new(MiningLoopConfig::default())
    }

    pub fn state(&self) -> MiningLoopState {
        self.state
    }

    pub fn samples_total(&self) -> u64 {
        self.samples_total
    }

    pub fn config(&self) -> &MiningLoopConfig {
        &self.config
    }

    /// Reset back to Idle. Caller must `enable` again.
    pub fn reset(&mut self) {
        self.state = MiningLoopState::Idle;
    }

    /// Force-fault the loop (e.g. on a critical sensor reading).
    /// Operator must `reset()` to clear.
    pub fn mark_fault(&mut self) {
        self.state = MiningLoopState::Faulted;
    }

    /// Feed one observation. Returns the new state.
    pub fn feed(&mut self, obs: MiningObservation) -> MiningLoopState {
        self.samples_total += 1;

        // Operator-stop overrides every state except Stopped/Faulted/Stopping.
        if obs.operator_stop_requested {
            self.state = match self.state {
                MiningLoopState::Stopped | MiningLoopState::Faulted => self.state,
                MiningLoopState::Stopping => MiningLoopState::Stopped,
                _ => MiningLoopState::Stopping,
            };
            return self.state;
        }

        match self.state {
            MiningLoopState::Idle => {
                if obs.operator_enabled {
                    self.state = MiningLoopState::Initializing;
                }
            }
            MiningLoopState::Initializing => {
                if obs.chains_ready {
                    self.state = MiningLoopState::Connecting;
                }
            }
            MiningLoopState::Connecting => {
                if obs.stratum_connected && obs.chains_ready {
                    self.state = MiningLoopState::Running;
                }
            }
            MiningLoopState::Running => {
                let notify_stall = obs
                    .last_notify_age_s
                    .map(|a| a >= self.config.stall_notify_age_s)
                    .unwrap_or(false);
                let nonce_stall = obs
                    .last_nonce_age_s
                    .map(|a| a >= self.config.stall_nonce_age_s)
                    .unwrap_or(false);
                if notify_stall || nonce_stall {
                    self.state = MiningLoopState::Stalled;
                }
            }
            MiningLoopState::Stalled => {
                // Runtime kicks off recovery. We can't observe that
                // directly; the recovery_succeeded / recovery_exhausted
                // signals are how the runtime hands control back.
                self.state = MiningLoopState::Recovering;
            }
            MiningLoopState::Recovering => {
                if obs.recovery_exhausted {
                    self.state = MiningLoopState::Faulted;
                } else if obs.recovery_succeeded {
                    self.state = MiningLoopState::Running;
                }
            }
            MiningLoopState::Stopping => {
                // Once chains_ready is false (chains shut down) we
                // transition to Stopped.
                if !obs.chains_ready {
                    self.state = MiningLoopState::Stopped;
                }
            }
            MiningLoopState::Faulted | MiningLoopState::Stopped => {
                // Terminal until reset().
            }
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enable() -> MiningObservation {
        MiningObservation {
            operator_enabled: true,
            ..MiningObservation::idle()
        }
    }

    fn ready() -> MiningObservation {
        MiningObservation {
            operator_enabled: true,
            chains_ready: true,
            ..MiningObservation::idle()
        }
    }

    fn online() -> MiningObservation {
        MiningObservation {
            operator_enabled: true,
            chains_ready: true,
            stratum_connected: true,
            last_notify_age_s: Some(2),
            last_nonce_age_s: Some(5),
            ..MiningObservation::idle()
        }
    }

    #[test]
    fn fresh_starts_idle() {
        let m = MiningLoopFsm::fresh();
        assert_eq!(m.state(), MiningLoopState::Idle);
    }

    #[test]
    fn enable_drives_idle_to_initializing() {
        let mut m = MiningLoopFsm::fresh();
        m.feed(enable());
        assert_eq!(m.state(), MiningLoopState::Initializing);
    }

    #[test]
    fn chains_ready_drives_initializing_to_connecting() {
        let mut m = MiningLoopFsm::fresh();
        m.feed(enable());
        m.feed(ready());
        assert_eq!(m.state(), MiningLoopState::Connecting);
    }

    #[test]
    fn stratum_online_drives_connecting_to_running() {
        let mut m = MiningLoopFsm::fresh();
        m.feed(enable());
        m.feed(ready());
        m.feed(online());
        assert_eq!(m.state(), MiningLoopState::Running);
    }

    #[test]
    fn no_notify_stall_drives_running_to_stalled() {
        let mut m = MiningLoopFsm::fresh();
        m.feed(enable());
        m.feed(ready());
        m.feed(online());
        let stalled = MiningObservation {
            last_notify_age_s: Some(150),
            ..online()
        };
        m.feed(stalled);
        assert_eq!(m.state(), MiningLoopState::Stalled);
    }

    #[test]
    fn no_nonce_stall_drives_running_to_stalled() {
        let mut m = MiningLoopFsm::fresh();
        m.feed(enable());
        m.feed(ready());
        m.feed(online());
        let stalled = MiningObservation {
            last_nonce_age_s: Some(90),
            ..online()
        };
        m.feed(stalled);
        assert_eq!(m.state(), MiningLoopState::Stalled);
    }

    #[test]
    fn stalled_advances_to_recovering_on_next_tick() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Stalled;
        m.feed(online());
        assert_eq!(m.state(), MiningLoopState::Recovering);
    }

    #[test]
    fn recovery_success_returns_to_running() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Recovering;
        let ok = MiningObservation {
            recovery_succeeded: true,
            ..online()
        };
        m.feed(ok);
        assert_eq!(m.state(), MiningLoopState::Running);
    }

    #[test]
    fn recovery_exhausted_drives_to_faulted() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Recovering;
        let exhausted = MiningObservation {
            recovery_exhausted: true,
            ..online()
        };
        m.feed(exhausted);
        assert_eq!(m.state(), MiningLoopState::Faulted);
    }

    #[test]
    fn operator_stop_drives_running_to_stopping() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Running;
        let stop = MiningObservation {
            operator_stop_requested: true,
            ..online()
        };
        m.feed(stop);
        assert_eq!(m.state(), MiningLoopState::Stopping);
    }

    #[test]
    fn stopping_advances_to_stopped_when_chains_down() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Stopping;
        let chains_down = MiningObservation {
            chains_ready: false,
            ..MiningObservation::idle()
        };
        m.feed(chains_down);
        assert_eq!(m.state(), MiningLoopState::Stopped);
    }

    #[test]
    fn faulted_is_terminal_until_reset() {
        let mut m = MiningLoopFsm::fresh();
        m.mark_fault();
        for _ in 0..10 {
            m.feed(online());
        }
        assert_eq!(m.state(), MiningLoopState::Faulted);
        m.reset();
        assert_eq!(m.state(), MiningLoopState::Idle);
    }

    #[test]
    fn stopped_is_terminal_until_reset() {
        let mut m = MiningLoopFsm::fresh();
        m.state = MiningLoopState::Stopped;
        for _ in 0..10 {
            m.feed(online());
        }
        assert_eq!(m.state(), MiningLoopState::Stopped);
    }

    #[test]
    fn config_default_locks_in_canonical_thresholds() {
        let cfg = MiningLoopConfig::default();
        assert_eq!(cfg.stall_notify_age_s, 120);
        assert_eq!(cfg.stall_nonce_age_s, 60);
    }

    #[test]
    fn state_round_trips_through_serde() {
        for s in [
            MiningLoopState::Idle,
            MiningLoopState::Initializing,
            MiningLoopState::Connecting,
            MiningLoopState::Running,
            MiningLoopState::Stalled,
            MiningLoopState::Recovering,
            MiningLoopState::Faulted,
            MiningLoopState::Stopping,
            MiningLoopState::Stopped,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: MiningLoopState = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }
}
