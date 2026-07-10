//! Tuner-stability clock (RE-001 closure gap-fill, Wave F 2026-05-19).
//!
//! The BraiinsOS-shape per-chip autotuner FSM is **already implemented** by
//! `crate::tuner::AutoTuner` (`TunerState` Idle → Characterizing → Verifying
//! → ThermalRefinement → Tuned / PartiallyTuned / Failed → BackgroundAdjust),
//! with hardware-fingerprint-keyed persistence in `crate::state_persistence`
//! and per-target profile caching in `AutoTuner::profile_cache` keyed by
//! `(chain_id, target_watts)`.
//! re-handoffs/RE-001-003-braiinsos-autotune-dps-power.md` and the RE-001
//! mapping in `RE-TEAM-ASKS.md`.
//!
//! The ONE genuine coupling gap RE-001 surfaced: the Wave-D DPS scale-up gate
//! (`crate::dps_governor::DpsTick::tuner_stable_minutes`,
//! `dps_governor.rs:89`) requires "tuner status has been stable for >= 30
//! minutes" per RE-002 §"Scale-Up Gate", but `AutoTuner` does not itself
//! expose a stable-since clock — the integration layer had to hand-roll it.
//!
//! This module is that clock, as a standalone helper the DPS integration
//! layer drives off the already-public `AutoTuner::state()` accessor. It is
//! deliberately a SEPARATE module (zero edits to the 7000-line
//! safety-critical `tuner.rs`) so the 5-platform accepted-share roster
//! (am1-s9 / am2-s17·S19Pro / am3-aml / am3-bb / am2-XIL) cannot regress.
//!
//! # Usage
//!
//! ```ignore
//! let mut clock = TunerStabilityClock::new();
//! // each DPS tick:
//! let stable_minutes = clock.observe(autotuner.state());
//! let dps_tick = DpsTick { tuner_stable_minutes: stable_minutes, .. };
//! ```
//!
//! # Stability definition
//!
//! Only `TunerState::Tuned` counts as "stable" for the scale-up gate.
//! `PartiallyTuned` (a chain fell back) and `BackgroundAdjust` (monitor
//! detected issues, backing off) deliberately do NOT — scaling power UP
//! while a chain is degraded is exactly the risk the gate guards against.

use std::time::Instant;

use crate::tuner::TunerState;

/// Tracks how long the autotuner has been continuously in the steady
/// `Tuned` state. Driven by the DPS integration layer once per tick off
/// `AutoTuner::state()`.
#[derive(Debug, Clone, Default)]
pub struct TunerStabilityClock {
    /// When the tuner most recently entered `Tuned`. `None` when not
    /// currently `Tuned`.
    stable_since: Option<Instant>,
}

impl TunerStabilityClock {
    /// New clock; not yet stable.
    pub fn new() -> Self {
        Self { stable_since: None }
    }

    /// Observe the current tuner state and return the number of whole
    /// minutes the tuner has been continuously `Tuned`. Returns 0 when
    /// the tuner is in any non-`Tuned` state (the clock resets on every
    /// departure from `Tuned`).
    pub fn observe(&mut self, state: TunerState) -> u32 {
        self.observe_at(state, Instant::now())
    }

    /// Test-seam variant of [`observe`](Self::observe) that takes an
    /// explicit `now` so unit tests don't have to sleep.
    pub fn observe_at(&mut self, state: TunerState, now: Instant) -> u32 {
        let is_stable = matches!(state, TunerState::Tuned);
        match (is_stable, self.stable_since) {
            (true, None) => self.stable_since = Some(now),
            (false, Some(_)) => self.stable_since = None,
            _ => {}
        }
        self.minutes_at(now)
    }

    /// Whole minutes since the tuner entered `Tuned`, or 0 when not
    /// currently stable. Pure read (no state change).
    pub fn minutes(&self) -> u32 {
        self.minutes_at(Instant::now())
    }

    fn minutes_at(&self, now: Instant) -> u32 {
        match self.stable_since {
            Some(since) => (now.saturating_duration_since(since).as_secs() / 60) as u32,
            None => 0,
        }
    }

    /// True iff the tuner is currently being tracked as stable.
    pub fn is_stable(&self) -> bool {
        self.stable_since.is_some()
    }
}

/// Map an [`crate::tuner::AutotunerRuntimeStatus::state`] string back to a
/// [`TunerState`]. The runtime status publishes the FSM state as the `Display`
/// string of [`TunerState`] (see `tuner.rs::build_runtime_status`), so an
/// integration layer that only has the watch-published status string (and not
/// a direct `&AutoTuner`) can still drive a [`TunerStabilityClock`].
///
/// Returns `None` for the synthetic non-FSM strings the status can carry
/// (`"disabled"`, `"runtime_unavailable"`, etc.). A caller observing the
/// stability clock should treat `None` as "not stable" (e.g. feed
/// `TunerState::Idle`, which resets the clock). This is a pure, host-testable
/// string→enum map with NO side effects and NO hardware dependency — useful for
/// observe-only shadow integrations that only have the published status string.
pub fn tuner_state_from_status_str(state: &str) -> Option<TunerState> {
    Some(match state {
        "Idle" => TunerState::Idle,
        "Characterizing" => TunerState::Characterizing,
        "Verifying" => TunerState::Verifying,
        "ThermalRefinement" => TunerState::ThermalRefinement,
        "Tuned" => TunerState::Tuned,
        "PartiallyTuned" => TunerState::PartiallyTuned,
        "Failed" => TunerState::Failed,
        "BackgroundAdjust" => TunerState::BackgroundAdjust,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // -- 1. Fresh clock reports zero --
    #[test]
    fn fresh_clock_is_zero_and_unstable() {
        let clock = TunerStabilityClock::new();
        assert_eq!(clock.minutes(), 0);
        assert!(!clock.is_stable());
    }

    // -- 2. Non-Tuned states never start the clock --
    #[test]
    fn non_tuned_states_keep_clock_zero() {
        let mut clock = TunerStabilityClock::new();
        for state in [
            TunerState::Idle,
            TunerState::Characterizing,
            TunerState::Verifying,
            TunerState::ThermalRefinement,
            TunerState::PartiallyTuned,
            TunerState::Failed,
            TunerState::BackgroundAdjust,
        ] {
            assert_eq!(
                clock.observe(state),
                0,
                "state {:?} must not start clock",
                state
            );
            assert!(!clock.is_stable());
        }
    }

    // -- 3. Tuned starts the clock; minutes accrue --
    #[test]
    fn tuned_accrues_minutes() {
        let mut clock = TunerStabilityClock::new();
        let t0 = Instant::now();
        assert_eq!(clock.observe_at(TunerState::Tuned, t0), 0);
        assert!(clock.is_stable());
        // 30 minutes later, still Tuned.
        let t30 = t0 + Duration::from_secs(30 * 60);
        assert_eq!(clock.observe_at(TunerState::Tuned, t30), 30);
        // 45 minutes later.
        let t45 = t0 + Duration::from_secs(45 * 60);
        assert_eq!(clock.observe_at(TunerState::Tuned, t45), 45);
    }

    // -- 4. LOAD-BEARING: 30-minute DPS scale-up gate threshold --
    #[test]
    fn thirty_minute_threshold_matches_dps_gate() {
        let mut clock = TunerStabilityClock::new();
        let t0 = Instant::now();
        clock.observe_at(TunerState::Tuned, t0);
        // 29m59s — gate NOT satisfied.
        let t29 = t0 + Duration::from_secs(29 * 60 + 59);
        assert!(clock.observe_at(TunerState::Tuned, t29) < 30);
        // 30m00s — gate satisfied (DpsScaleUpConditions wants >= 30).
        let t30 = t0 + Duration::from_secs(30 * 60);
        assert!(clock.observe_at(TunerState::Tuned, t30) >= 30);
    }

    // -- 5. Leaving Tuned resets the clock (anti-flap) --
    #[test]
    fn leaving_tuned_resets_clock() {
        let mut clock = TunerStabilityClock::new();
        let t0 = Instant::now();
        clock.observe_at(TunerState::Tuned, t0);
        let t30 = t0 + Duration::from_secs(30 * 60);
        assert_eq!(clock.observe_at(TunerState::Tuned, t30), 30);
        // Background monitor backs off → clock resets.
        let t31 = t0 + Duration::from_secs(31 * 60);
        assert_eq!(clock.observe_at(TunerState::BackgroundAdjust, t31), 0);
        assert!(!clock.is_stable());
        // Re-enters Tuned → clock restarts from this point, NOT the old one.
        let t32 = t0 + Duration::from_secs(32 * 60);
        assert_eq!(clock.observe_at(TunerState::Tuned, t32), 0);
        let t40 = t0 + Duration::from_secs(40 * 60);
        assert_eq!(clock.observe_at(TunerState::Tuned, t40), 8); // 40 - 32
    }

    // -- 6. PartiallyTuned does NOT count as stable (scale-up safety) --
    #[test]
    fn partially_tuned_does_not_satisfy_gate() {
        let mut clock = TunerStabilityClock::new();
        let t0 = Instant::now();
        // A long PartiallyTuned soak must never satisfy the scale-up gate.
        let t60 = t0 + Duration::from_secs(60 * 60);
        assert_eq!(clock.observe_at(TunerState::PartiallyTuned, t60), 0);
        assert!(!clock.is_stable());
    }

    // -- 7. tuner_state_from_status_str maps every Display string + None --
    #[test]
    fn status_str_round_trips_every_tuner_state() {
        // Every FSM state must round-trip through its Display string so a
        // shadow observer that only has the published status string can
        // reconstruct the exact TunerState.
        for state in [
            TunerState::Idle,
            TunerState::Characterizing,
            TunerState::Verifying,
            TunerState::ThermalRefinement,
            TunerState::Tuned,
            TunerState::PartiallyTuned,
            TunerState::Failed,
            TunerState::BackgroundAdjust,
        ] {
            let s = state.to_string();
            assert_eq!(
                tuner_state_from_status_str(&s),
                Some(state),
                "Display string {:?} must map back to {:?}",
                s,
                state
            );
        }
    }

    #[test]
    fn status_str_synthetic_strings_are_none() {
        // The default/runtime-unavailable status strings are NOT FSM states.
        assert_eq!(tuner_state_from_status_str("disabled"), None);
        assert_eq!(tuner_state_from_status_str("runtime_unavailable"), None);
        assert_eq!(tuner_state_from_status_str(""), None);
        assert_eq!(tuner_state_from_status_str("garbage"), None);
    }
}
