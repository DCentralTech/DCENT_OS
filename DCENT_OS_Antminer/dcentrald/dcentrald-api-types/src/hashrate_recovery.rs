//! PH-3 — bounded hashrate auto-recovery ladder (pure FSM, HAL-free, host-tested).
//!
//! This is the operator-gated follow-up to the **detection-only**
//! `degraded_hashrate_alert_floor_ghs` alert: when a sustained low-hashrate
//! episode is confirmed, an *enabled* ladder may request a bounded number of
//! daemon recovery attempts before giving up and latching a persistent
//! "recovery exhausted" alert. The daemon currently refuses process replacement
//! for every platform until a typed hardware-disposition receipt can authorize
//! a new owner. The pure FSM remains useful for detection and durable budgeting;
//! its outcome is a request, never restart authority.
//!
//! ## Why this module is a pure FSM in a HAL-free crate
//!
//! It was designed against an adversarial safety panel () that REJECTED
//! a first design. The panel's blocking findings are baked in here as hard
//! invariants, and because the FSM is pure (inputs are plain values, the only
//! clock is an injected `now_s`, no I/O) every invariant is unit-tested on the
//! host. The daemon merely supplies the inputs and performs the side effect
//! (`schedule_daemon_restart`) when the FSM returns [`LadderOutcome::ScheduleRestart`].
//!
//! ## Load-bearing safety invariants (each has a test)
//!
//! 1. **Default OFF.** [`HashrateRecoveryLadderConfig::default`] is `enabled:
//!    false`; a disabled ladder is fully inert and never returns an action.
//! 2. **Platform allowlist, not blocklist.** The ladder only arms where a
//!    daemon restart is *proven* to re-establish mining in software. On every
//!    other platform a restart is a no-op-to-harmful warm reboot (AM2-Zynq XIL /
//!    Amlogic / BB leave the chip un-enumerable — "needs a fresh AC cycle"), so
//!    the caller passes `platform_recovery_allowed = false` and the ladder is
//!    inert. Today only `am1-s9` qualifies (see [`platform_recovery_allowed`]).
//! 3. **Per-episode, monotonic budget — never self-refilling.** Attempts are
//!    counted per degradation *episode* and are NEVER refunded by a brief or
//!    partial recovery. An episode only clears (and re-arms a fresh budget)
//!    after the hashrate stays at/above a *recovered margin* (`floor * (1 +
//!    recovered_margin_pct/100)`) for `recovered_hold_s`.
//! 4. **Sticky give-up + backoff.** Once the budget is exhausted the ladder
//!    latches `GaveUp`, takes no further action, and will not re-arm until both
//!    the episode clears AND `give_up_backoff_s` has elapsed since give-up.
//! 5. **Curtailment / standby / off-grid skip + post-wake grace.** Never act
//!    while curtailed (re-energizing a deliberately-curtailed rail is a
//!    power-safety issue), and suppress action for `post_wake_grace_s` after a
//!    curtailment→mining wake so the wake ramp through the sub-floor band can't
//!    confirm a false recovery action.
//! 6. **Startup grace.** No action until `startup_grace_s` after daemon start
//!    (cold-boot ramp protection; also bounds restart-respawn loops).
//! 7. **Degraded-hardware skip.** Never act when the unit can't safely re-init
//!    (dsPIC fw=0x86 / untrusted EEPROM) — alert-only.
//! 8. **No box reboot in v1.** The only action is a daemon-restart request,
//!    currently refused at the shared persistent-session policy boundary.
//!    A box-reboot rung (heavier; transits a loud hardware-default fan state
//!    during the OS-down window) is a deliberate, deferred, separately-gated
//!    future increment — NOT in this module.
//! 9. **Never raise fans, never invent hardware control.** The outcome is a
//!    restart *request* only. This module emits no fan/voltage/I2C/PSU action of
//!    any kind and cannot clear the persistent hardware-session latch.

use serde::{Deserialize, Serialize};

/// Operator-gated configuration for the recovery ladder. DEFAULT-OFF.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HashrateRecoveryLadderConfig {
    /// Master enable. `false` (default) ⇒ the ladder is fully inert and the
    /// detection-only alert behavior is unchanged. Enabling the detection alert
    /// does NOT enable this — they are independent toggles.
    pub enabled: bool,
    /// Max graceful daemon-restart attempts PER EPISODE before giving up.
    pub max_attempts: u32,
    /// Minimum seconds between restart attempts.
    pub cooldown_s: u64,
    /// Seconds after daemon start before the first action may fire (cold-boot
    /// ramp protection).
    pub startup_grace_s: u64,
    /// Seconds after a curtailment→mining wake before an action may fire.
    pub post_wake_grace_s: u64,
    /// An episode is only considered RECOVERED (re-arming a fresh budget) once
    /// hashrate is at/above `floor * (1 + recovered_margin_pct/100)`. Prevents a
    /// unit hovering at exactly the floor from flapping and accumulating attempts.
    pub recovered_margin_pct: u8,
    /// Hashrate must hold at/above the recovered margin for this long to clear
    /// the episode.
    pub recovered_hold_s: u64,
    /// After give-up, the episode cannot clear (and re-arm) for at least this
    /// long — bounds rapid re-exhaustion cycles on a flapping unit.
    pub give_up_backoff_s: u64,
}

impl Default for HashrateRecoveryLadderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: 3,
            cooldown_s: 600,
            startup_grace_s: 900,
            post_wake_grace_s: 120,
            recovered_margin_pct: 5,
            recovered_hold_s: 60,
            give_up_backoff_s: 3600,
        }
    }
}

impl HashrateRecoveryLadderConfig {
    /// Validate ONLY when enabled — a disabled/default ladder always passes so
    /// it never blocks boot (zero-cost when off).
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.max_attempts < 1 {
            return Err("recovery_ladder.max_attempts must be >= 1 when enabled".to_string());
        }
        if self.cooldown_s < 1 {
            return Err("recovery_ladder.cooldown_s must be >= 1 when enabled".to_string());
        }
        if self.startup_grace_s < 1 {
            return Err("recovery_ladder.startup_grace_s must be >= 1 when enabled".to_string());
        }
        if self.recovered_hold_s < 1 {
            return Err("recovery_ladder.recovered_hold_s must be >= 1 when enabled".to_string());
        }
        Ok(())
    }

    fn recovered_threshold(&self, floor_ghs: f64) -> f64 {
        floor_ghs * (1.0 + (self.recovered_margin_pct as f64) / 100.0)
    }
}

/// Platform allowlist gate. A daemon restart only RECOVERS mining on platforms
/// where cold-boot-from-restart is proven in software. On AM2-Zynq XIL /
/// Amlogic / BB a restart leaves the chip un-enumerable (documented
/// "not warm-recoverable / needs a fresh AC cycle"), so those return `false` and
/// the ladder degrades to alert-only. Today only `am1-s9` qualifies.
///
/// `platform_key` is the canonical key (e.g. `am1-s9`, `am2-s19jpro-zynq`,
/// `am3-aml`, `am3-bb`).
pub fn platform_recovery_allowed(platform_key: &str) -> bool {
    matches!(platform_key.trim().to_ascii_lowercase().as_str(), "am1-s9")
}

/// One tick of input to the ladder. All plain values — no HAL, no hidden clock.
#[derive(Debug, Clone, Copy)]
pub struct LadderTick {
    /// Monotonic-ish wall seconds (the caller's tick clock).
    pub now_s: u64,
    /// The HashrateDegraded predicate is currently confirmed (the existing
    /// 30-tick debounce already applied upstream).
    pub degraded_confirmed: bool,
    /// Live total hashrate (GH/s).
    pub observed_ghs: f64,
    /// The resolved degraded floor (GH/s) — same value the alert uses.
    pub floor_ghs: f64,
    /// Mining is actually enabled (management-only / held units pass `false`).
    pub mining_enabled: bool,
    /// Curtailment / standby / off-grid sleep is active right now.
    pub curtailed: bool,
    /// Seconds since the most recent curtailment→mining wake (None = never woke
    /// from curtailment this run).
    pub since_last_wake_s: Option<u64>,
    /// Seconds since daemon start.
    pub daemon_uptime_s: u64,
    /// Platform allowlist result (see [`platform_recovery_allowed`]).
    pub platform_recovery_allowed: bool,
    /// The unit can't safely re-init (dsPIC fw=0x86 / untrusted EEPROM).
    pub degraded_hardware: bool,
}

/// Why the ladder took no action this tick (for logging / telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleReason {
    Disabled,
    PlatformNotAllowed,
    MiningDisabled,
    Curtailed,
    PostWakeGrace,
    StartupGrace,
    DegradedHardware,
    NotDegraded,
    Stabilizing,
    Cooldown,
    GaveUp,
}

/// The ladder's decision for a tick.
#[derive(Debug, Clone, PartialEq)]
pub enum LadderOutcome {
    /// No recovery action (with the reason, for logging).
    Idle(IdleReason),
    /// Request ONE graceful daemon-process restart. The caller MUST persist the
    /// updated state (see [`HashrateRecoveryLadder::persisted_state`]) BEFORE
    /// scheduling, so the per-episode budget survives the respawn.
    ScheduleRestart { reason: &'static str, attempt: u32 },
    /// Budget exhausted for this episode — emit a HashrateRecoveryExhausted
    /// alert exactly once; no further action until the episode clears + backoff.
    GiveUp { attempts: u32 },
}

/// Serializable slice of ladder state that must survive a daemon respawn so the
/// per-episode budget is not reset by the very restart it scheduled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedLadderState {
    pub episode_active: bool,
    pub attempts: u32,
    pub last_action_at_s: Option<u64>,
    pub gave_up_at_s: Option<u64>,
}

/// The pure recovery-ladder FSM.
#[derive(Debug, Clone)]
pub struct HashrateRecoveryLadder {
    config: HashrateRecoveryLadderConfig,
    episode_active: bool,
    attempts: u32,
    last_action_at_s: Option<u64>,
    gave_up_at_s: Option<u64>,
    /// When the hashrate first crossed the recovered margin in the current
    /// not-degraded window (for the recovered-hold timer). Reset whenever it
    /// drops back below the margin.
    recovered_since_s: Option<u64>,
}

impl HashrateRecoveryLadder {
    pub fn new(config: HashrateRecoveryLadderConfig) -> Self {
        Self {
            config,
            episode_active: false,
            attempts: 0,
            last_action_at_s: None,
            gave_up_at_s: None,
            recovered_since_s: None,
        }
    }

    /// Reconstruct after a respawn from persisted state. The caller is
    /// responsible for the FAIL-CLOSED read policy: if the state file is
    /// present-but-corrupt (or a crash marker indicates a prior ladder action
    /// this episode but the file is missing), pass a state with
    /// `attempts >= max_attempts` so the ladder starts in give-up rather than
    /// fail-open into an unbounded loop.
    pub fn from_persisted(
        config: HashrateRecoveryLadderConfig,
        state: PersistedLadderState,
    ) -> Self {
        Self {
            config,
            episode_active: state.episode_active,
            attempts: state.attempts,
            last_action_at_s: state.last_action_at_s,
            gave_up_at_s: state.gave_up_at_s,
            recovered_since_s: None,
        }
    }

    pub fn persisted_state(&self) -> PersistedLadderState {
        PersistedLadderState {
            episode_active: self.episode_active,
            attempts: self.attempts,
            last_action_at_s: self.last_action_at_s,
            gave_up_at_s: self.gave_up_at_s,
        }
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Clear the episode and re-arm a fresh budget.
    fn clear_episode(&mut self) {
        self.episode_active = false;
        self.attempts = 0;
        self.last_action_at_s = None;
        self.gave_up_at_s = None;
        self.recovered_since_s = None;
    }

    /// Advance the FSM one tick and return the decision. Pure: the only effect
    /// is on `self`'s internal counters; no I/O.
    pub fn step(&mut self, t: LadderTick) -> LadderOutcome {
        let c = &self.config;

        // --- Hard gates, evaluated BEFORE any episode/action logic. ---
        if !c.enabled {
            return LadderOutcome::Idle(IdleReason::Disabled);
        }
        if !t.platform_recovery_allowed {
            return LadderOutcome::Idle(IdleReason::PlatformNotAllowed);
        }
        if !t.mining_enabled {
            // A stopped miner is not "degraded" — drop any in-flight episode.
            self.clear_episode();
            return LadderOutcome::Idle(IdleReason::MiningDisabled);
        }
        if t.curtailed {
            // Pause the episode's recovery tracking; do NOT clear the budget
            // (the episode may resume after wake), but never act while curtailed.
            self.recovered_since_s = None;
            return LadderOutcome::Idle(IdleReason::Curtailed);
        }
        if t.degraded_hardware {
            return LadderOutcome::Idle(IdleReason::DegradedHardware);
        }
        if t.daemon_uptime_s < c.startup_grace_s {
            return LadderOutcome::Idle(IdleReason::StartupGrace);
        }
        if let Some(since_wake) = t.since_last_wake_s {
            if since_wake < c.post_wake_grace_s {
                self.recovered_since_s = None;
                return LadderOutcome::Idle(IdleReason::PostWakeGrace);
            }
        }

        // --- Episode / recovery logic. ---
        if !t.degraded_confirmed {
            // Not degraded right now. Track recovery only above the MARGIN.
            let threshold = c.recovered_threshold(t.floor_ghs);
            if self.episode_active && t.observed_ghs >= threshold {
                let since = *self.recovered_since_s.get_or_insert(t.now_s);
                let held_long_enough = t.now_s.saturating_sub(since) >= c.recovered_hold_s;
                // After give-up, also honor the mandatory backoff before clearing.
                let backoff_ok = match self.gave_up_at_s {
                    Some(g) => t.now_s.saturating_sub(g) >= c.give_up_backoff_s,
                    None => true,
                };
                if held_long_enough && backoff_ok {
                    self.clear_episode();
                    return LadderOutcome::Idle(IdleReason::NotDegraded);
                }
                return LadderOutcome::Idle(IdleReason::Stabilizing);
            }
            // Below the margin but not degraded-confirmed: in the dead-band —
            // do NOT count this as recovery (anti-flap).
            self.recovered_since_s = None;
            return LadderOutcome::Idle(if self.episode_active {
                IdleReason::Stabilizing
            } else {
                IdleReason::NotDegraded
            });
        }

        // Degraded confirmed ⇒ an episode is active.
        self.episode_active = true;
        self.recovered_since_s = None;

        // Sticky give-up: once latched, no action until the episode clears.
        if self.gave_up_at_s.is_some() {
            return LadderOutcome::Idle(IdleReason::GaveUp);
        }

        // Budget exhausted ⇒ latch give-up exactly once.
        if self.attempts >= c.max_attempts {
            self.gave_up_at_s = Some(t.now_s);
            return LadderOutcome::GiveUp {
                attempts: self.attempts,
            };
        }

        // Cooldown between attempts.
        if let Some(last) = self.last_action_at_s {
            if t.now_s.saturating_sub(last) < c.cooldown_s {
                return LadderOutcome::Idle(IdleReason::Cooldown);
            }
        }

        // Take one bounded restart attempt.
        self.attempts += 1;
        self.last_action_at_s = Some(t.now_s);
        LadderOutcome::ScheduleRestart {
            reason: "hashrate_recovery_restart",
            attempt: self.attempts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_enabled() -> HashrateRecoveryLadderConfig {
        HashrateRecoveryLadderConfig {
            enabled: true,
            ..Default::default()
        }
    }

    /// A degraded tick on the allowed platform, well past startup grace, mining
    /// on, not curtailed, healthy hardware.
    fn degraded_tick(now_s: u64) -> LadderTick {
        LadderTick {
            now_s,
            degraded_confirmed: true,
            observed_ghs: 1.0,
            floor_ghs: 10.0,
            mining_enabled: true,
            curtailed: false,
            since_last_wake_s: None,
            daemon_uptime_s: 100_000,
            platform_recovery_allowed: true,
            degraded_hardware: false,
        }
    }

    #[test]
    fn default_is_off_and_inert() {
        let mut l = HashrateRecoveryLadder::new(HashrateRecoveryLadderConfig::default());
        for i in 0..50 {
            let out = l.step(degraded_tick(i * 1000));
            assert_eq!(out, LadderOutcome::Idle(IdleReason::Disabled));
        }
    }

    #[test]
    fn disabled_config_validates_default_does_not_block_boot() {
        assert!(HashrateRecoveryLadderConfig::default().validate().is_ok());
        let bad = HashrateRecoveryLadderConfig {
            enabled: false,
            max_attempts: 0,
            ..Default::default()
        };
        assert!(
            bad.validate().is_ok(),
            "disabled ladder must never block boot"
        );
    }

    #[test]
    fn enabled_config_rejects_zero_bounds() {
        assert!(HashrateRecoveryLadderConfig {
            enabled: true,
            max_attempts: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(HashrateRecoveryLadderConfig {
            enabled: true,
            cooldown_s: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(HashrateRecoveryLadderConfig {
            enabled: true,
            startup_grace_s: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(cfg_enabled().validate().is_ok());
    }

    #[test]
    fn platform_allowlist_is_s9_only() {
        assert!(platform_recovery_allowed("am1-s9"));
        assert!(platform_recovery_allowed("AM1-S9"));
        for p in [
            "am2-s19jpro-zynq",
            "am3-aml",
            "am3-bb",
            "am2-s17",
            "unknown",
            "",
        ] {
            assert!(
                !platform_recovery_allowed(p),
                "{p} must NOT be auto-recovery-allowed"
            );
        }
    }

    #[test]
    fn not_allowed_platform_never_acts() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut t = degraded_tick(100_000);
        t.platform_recovery_allowed = false;
        assert_eq!(
            l.step(t),
            LadderOutcome::Idle(IdleReason::PlatformNotAllowed)
        );
        assert_eq!(l.attempts(), 0);
    }

    #[test]
    fn startup_grace_blocks_first_action() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut t = degraded_tick(100);
        t.daemon_uptime_s = 10; // < 900 default
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::StartupGrace));
    }

    #[test]
    fn first_restart_fires_after_grace() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        match l.step(degraded_tick(100_000)) {
            LadderOutcome::ScheduleRestart { attempt, .. } => assert_eq!(attempt, 1),
            o => panic!("expected restart, got {o:?}"),
        }
        assert_eq!(l.attempts(), 1);
    }

    #[test]
    fn cooldown_blocks_second_restart() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        assert!(matches!(
            l.step(degraded_tick(100_000)),
            LadderOutcome::ScheduleRestart { .. }
        ));
        // 1s later, still degraded, within 600s cooldown.
        assert_eq!(
            l.step(degraded_tick(100_001)),
            LadderOutcome::Idle(IdleReason::Cooldown)
        );
    }

    #[test]
    fn bounded_attempts_then_sticky_give_up() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled()); // max_attempts 3, cooldown 600
        let mut now = 100_000;
        // 3 restarts spaced past cooldown.
        for expect in 1..=3 {
            match l.step(degraded_tick(now)) {
                LadderOutcome::ScheduleRestart { attempt, .. } => assert_eq!(attempt, expect),
                o => panic!("expected restart {expect}, got {o:?}"),
            }
            now += 700;
        }
        // 4th would-be action: GiveUp emitted exactly once.
        assert_eq!(
            l.step(degraded_tick(now)),
            LadderOutcome::GiveUp { attempts: 3 }
        );
        now += 700;
        // Subsequent ticks stay GaveUp, no action.
        for _ in 0..5 {
            assert_eq!(
                l.step(degraded_tick(now)),
                LadderOutcome::Idle(IdleReason::GaveUp)
            );
            now += 700;
        }
        assert_eq!(l.attempts(), 3);
    }

    #[test]
    fn brief_recovery_below_margin_does_not_refund_budget() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut now = 100_000;
        assert!(matches!(
            l.step(degraded_tick(now)),
            LadderOutcome::ScheduleRestart { .. }
        ));
        now += 700;
        // Hashrate recovers to EXACTLY the floor (10.0) but not the 5% margin
        // (10.5) — must NOT clear the episode/refund the budget.
        let mut t = degraded_tick(now);
        t.degraded_confirmed = false;
        t.observed_ghs = 10.0;
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::Stabilizing));
        now += 700;
        // Degraded again: budget still consumed (attempt 2, not refunded to 1).
        match l.step(degraded_tick(now)) {
            LadderOutcome::ScheduleRestart { attempt, .. } => assert_eq!(attempt, 2),
            o => panic!("expected attempt 2 (no refund), got {o:?}"),
        }
    }

    #[test]
    fn sustained_recovery_above_margin_clears_episode() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled()); // recovered_hold 60s
        let mut now = 100_000;
        assert!(matches!(
            l.step(degraded_tick(now)),
            LadderOutcome::ScheduleRestart { .. }
        ));
        // Recover above margin (11.0 >= 10.5) and hold for >= 60s.
        let recovered = |n: u64| LadderTick {
            degraded_confirmed: false,
            observed_ghs: 11.0,
            ..degraded_tick(n)
        };
        now += 700;
        assert_eq!(
            l.step(recovered(now)),
            LadderOutcome::Idle(IdleReason::Stabilizing)
        ); // recovered_since set
        now += 61; // held >= 60s
        assert_eq!(
            l.step(recovered(now)),
            LadderOutcome::Idle(IdleReason::NotDegraded)
        ); // episode cleared
        assert_eq!(l.attempts(), 0, "fresh budget after sustained recovery");
        // A new episode can act again.
        now += 700;
        match l.step(degraded_tick(now)) {
            LadderOutcome::ScheduleRestart { attempt, .. } => assert_eq!(attempt, 1),
            o => panic!("expected fresh-budget restart, got {o:?}"),
        }
    }

    #[test]
    fn curtailed_never_acts() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut t = degraded_tick(100_000);
        t.curtailed = true;
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::Curtailed));
        assert_eq!(l.attempts(), 0);
    }

    #[test]
    fn post_wake_grace_blocks_action() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut t = degraded_tick(100_000);
        t.since_last_wake_s = Some(30); // < 120 default post-wake grace
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::PostWakeGrace));
        // Past the grace, it can act.
        t.since_last_wake_s = Some(200);
        assert!(matches!(l.step(t), LadderOutcome::ScheduleRestart { .. }));
    }

    #[test]
    fn mining_disabled_never_acts_and_clears_episode() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        assert!(matches!(
            l.step(degraded_tick(100_000)),
            LadderOutcome::ScheduleRestart { .. }
        ));
        let mut t = degraded_tick(100_700);
        t.mining_enabled = false;
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::MiningDisabled));
        assert_eq!(l.attempts(), 0, "stopped miner clears the episode");
    }

    #[test]
    fn degraded_hardware_never_acts() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        let mut t = degraded_tick(100_000);
        t.degraded_hardware = true;
        assert_eq!(l.step(t), LadderOutcome::Idle(IdleReason::DegradedHardware));
        assert_eq!(l.attempts(), 0);
    }

    #[test]
    fn persisted_budget_survives_respawn() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled());
        assert!(matches!(
            l.step(degraded_tick(100_000)),
            LadderOutcome::ScheduleRestart { attempt: 1, .. }
        ));
        // Daemon respawns: reconstruct from persisted state.
        let state = l.persisted_state();
        assert_eq!(state.attempts, 1);
        let mut l2 = HashrateRecoveryLadder::from_persisted(cfg_enabled(), state);
        // Next attempt is 2, not 1 — the budget was not reset by the respawn.
        match l2.step(degraded_tick(100_700)) {
            LadderOutcome::ScheduleRestart { attempt, .. } => assert_eq!(attempt, 2),
            o => panic!("expected attempt 2 after respawn, got {o:?}"),
        }
    }

    #[test]
    fn fail_closed_state_starts_in_give_up() {
        // Caller's fail-closed policy: a corrupt state file ⇒ attempts at max.
        let state = PersistedLadderState {
            episode_active: true,
            attempts: 3,
            last_action_at_s: Some(0),
            gave_up_at_s: None,
        };
        let mut l = HashrateRecoveryLadder::from_persisted(cfg_enabled(), state);
        assert_eq!(
            l.step(degraded_tick(100_000)),
            LadderOutcome::GiveUp { attempts: 3 }
        );
    }

    #[test]
    fn give_up_backoff_prevents_immediate_re_arm() {
        let mut l = HashrateRecoveryLadder::new(cfg_enabled()); // give_up_backoff 3600, hold 60
        let mut now = 100_000;
        for _ in 0..3 {
            assert!(matches!(
                l.step(degraded_tick(now)),
                LadderOutcome::ScheduleRestart { .. }
            ));
            now += 700;
        }
        assert_eq!(
            l.step(degraded_tick(now)),
            LadderOutcome::GiveUp { attempts: 3 }
        );
        let gave_up_at = now;
        // Recover above margin and hold 60s, but still within the 3600s backoff:
        // the episode must NOT clear yet.
        let recovered = |n: u64| LadderTick {
            degraded_confirmed: false,
            observed_ghs: 11.0,
            ..degraded_tick(n)
        };
        now = gave_up_at + 100;
        assert_eq!(
            l.step(recovered(now)),
            LadderOutcome::Idle(IdleReason::Stabilizing)
        );
        now += 100; // held > 60s but < 3600s backoff
        assert_eq!(
            l.step(recovered(now)),
            LadderOutcome::Idle(IdleReason::Stabilizing)
        );
        assert_eq!(
            l.attempts(),
            3,
            "backoff keeps the episode (and its spent budget) latched"
        );
        // After the full backoff (>=3600s) the episode finally clears — the
        // recovered-hold was already satisfied by the earlier in-window ticks,
        // so the backoff was the only thing holding it latched.
        now = gave_up_at + 3601;
        assert_eq!(
            l.step(recovered(now)),
            LadderOutcome::Idle(IdleReason::NotDegraded)
        );
        assert_eq!(l.attempts(), 0);
    }

    #[test]
    fn config_serde_round_trip_defaults_off() {
        let json = "{}";
        let c: HashrateRecoveryLadderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c, HashrateRecoveryLadderConfig::default());
        assert!(!c.enabled);
    }

    #[test]
    fn config_rejects_unknown_keys() {
        let json = r#"{"enabled":true,"bogus_key":1}"#;
        assert!(serde_json::from_str::<HashrateRecoveryLadderConfig>(json).is_err());
    }
}
