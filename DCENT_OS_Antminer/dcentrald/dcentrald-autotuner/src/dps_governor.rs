//! BraiinsOS-shape DPS runtime governor (RE-002 closure, Wave D 2026-05-19).
//!
//! The DPS (Dynamic Performance Scaling) state machine layered ON TOP of
//! the existing per-step walker at `crate::dps::DpsWalkerConfig`. Per the
//! RE team handoff at
//!
//! §RE-002, the runtime FSM transitions through 4 states (Normal /
//! ScalingDown / Shutdown / ScalingUp) driven by board/chip temperature
//! samples, fan speed, tuner-stable duration, and current vs configured
//! power_target.
//!
//! # Source of truth
//!
//! Clean-room implementation from the RE handoff doc + the catalog at
//! `dcentrald_api_types::braiinsos_dps_configuration` (which already
//! implements the documented 4-AND scale-up gate at
//! `DpsScaleUpConditions::met()`). No proprietary code copied; behavior
//! grounded in documented BraiinsOS API/proto fields + observed runtime
//! values (S19j default target 3068 W, min 943 W, power_step 300 W,
//! shutdown_duration 10800 s, etc. from the live `a lab unit` capture in
//! ).
//!
//! # Opt-in safety
//!
//! This module is COMPILED but NOT INSTANTIATED into the running daemon
//! by default. The integration site in the thermal supervisor is gated on
//! `[thermal].dps_enabled = true` in `dcentrald.toml`; with that flag
//! false (default), the existing thermal-controller stays master and DPS
//! is disabled. Live HW validation (Wave H, with operator per-action
//! authorization) is the gate to enabling it in production.
//!
//! # State machine
//!
//! ```text
//!   Normal ──(board_temp >= hot)──> ScalingDown
//!   ScalingDown ──(scale-up gate met)──> ScalingUp
//!   ScalingDown ──(target <= min AND shutdown_enabled)──> Shutdown
//!   ScalingDown ──(target <= min AND !shutdown_enabled)──> ScalingDown (hold)
//!   Shutdown ──(shutdown_duration elapsed)──> ScalingUp (Restart)
//!   ScalingUp ──(target reached)──> Normal
//!   Any ──(board_temp >= dangerous)──> emergency override (caller handles)
//! ```

use std::path::Path;

use dcentrald_api_types::braiinsos_dps_configuration::{
    DpsConfiguration, DpsScaleUpConditions, DpsThermalProfile,
};
use serde::{Deserialize, Serialize};

use crate::dps::DpsWalkerConfig;

/// Persisted-record schema version. Bump on any incompatible field change.
pub const DPS_GOVERNOR_STATE_VERSION: u32 = 1;
const MAX_DPS_GOVERNOR_STATE_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// DPS FSM state per BRAIINSOS_RE.md §6 (DPS Algorithm State Machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpsState {
    /// Normal operation; mining at configured target.
    Normal,
    /// Scaling power_target down in response to hot temps.
    ScalingDown,
    /// Mining halted; waiting `shutdown_duration_hours` before restart.
    Shutdown,
    /// Scaling power_target back up toward configured target after thermal
    /// recovery (entered via Shutdown → Restart OR via scale-up gate).
    ScalingUp,
}

// ---------------------------------------------------------------------------
// Persisted scale-down record (D5)
// ---------------------------------------------------------------------------

/// D5 (RE-002, 2026-05-20): a small persisted record of a thermally-throttled
/// DPS position, written when the governor scales down / into Shutdown and
/// CLEARED on thermal recovery (return to Normal).
///
/// # Why
///
/// Before D5, `DpsGovernor` held its FSM state ONLY in memory. A daemon
/// restart while throttled would lose a scaled-down position and silently
/// re-boost a home unit to full power — exactly the wrong failure mode for a
/// quiet/space-heater unit that was hot enough to throttle. This record lets a
/// fresh governor restore the throttled state on init instead of starting at
/// Normal.
///
/// # Fail-open-safe
///
/// Persistence is best-effort and fail-open to in-memory: a missing, corrupt,
/// or unreadable record means the governor behaves EXACTLY as before D5
/// (starts at Normal). Persistence never panics and a write error never aborts
/// a tick. The shared crash-durable state-file replacement/deletion framework
/// is reused for publication and thermal-recovery clearing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpsGovernorPersistedState {
    /// Schema version (see [`DPS_GOVERNOR_STATE_VERSION`]).
    pub version: u32,
    /// Wall-clock unix seconds the record was written (diagnostics only).
    pub saved_at_unix_s: u64,
    /// The throttled FSM state to restore. Only the throttled states
    /// (`ScalingDown` / `Shutdown` / `ScalingUp`) are ever persisted; `Normal`
    /// is represented by the ABSENCE of a record (the file is cleared).
    pub state: DpsState,
    /// The current scaled power target in watts at the moment of persist, so
    /// a restored governor knows where the throttle left the power target.
    pub scaled_power_target_watts: u32,
    /// Seconds already elapsed in Shutdown when persisted (so a restart does
    /// not reset the shutdown dwell clock). 0 for non-Shutdown states.
    #[serde(default)]
    pub shutdown_elapsed_secs: u32,
}

impl DpsGovernorPersistedState {
    /// Read-only load. Returns `Ok(None)` if the file does not exist; returns
    /// `Ok(None)` (NOT an error) on a corrupt/unparseable record so callers
    /// fail-open to Normal. A genuine non-NotFound I/O error is propagated.
    pub fn load(path: impl AsRef<Path>) -> crate::Result<Option<Self>> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(toml) => match toml::from_str::<Self>(&toml) {
                Ok(state) if state.version == DPS_GOVERNOR_STATE_VERSION => Ok(Some(state)),
                // Wrong version OR parse failure => treat as no record
                // (fail-open). A daemon must never refuse to start because a
                // throttle-resume file is corrupt.
                Ok(_) | Err(_) => Ok(None),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Atomic write (temp file + fsync + rename), mirroring
    /// [`crate::state_persistence::AutotunerResumeState::save_atomic`].
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> crate::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            // Parent/ancestor creation durability is outside atomic_write's
            // single-file contract; production images should pre-create it.
            std::fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self).map_err(|err| {
            crate::AutoTunerError::Config(format!("dps governor state serialize error: {err}"))
        })?;
        dcentrald_common::atomic_file::atomic_write(
            path,
            toml.as_bytes(),
            dcentrald_common::atomic_file::AtomicWriteOptions::state_file(
                MAX_DPS_GOVERNOR_STATE_BYTES,
            ),
        )
        .map_err(dcentrald_common::atomic_file::AtomicWriteError::into_io_error)?;
        Ok(())
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Input / output
// ---------------------------------------------------------------------------

/// Per-tick thermal + autotuner sample the governor consumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpsTick {
    /// Max hashboard temperature in °C (sensor fusion / XADC die-temp
    /// fallback handled by the thermal supervisor before passing here).
    pub board_temp_c: f32,
    /// Max chip-die temperature in °C (optional; pass 0.0 if unavailable).
    pub chip_temp_c: f32,
    /// Current fan speed as percentage (0..100).
    pub fan_speed_pct: u8,
    /// How many consecutive minutes board_temp has been below `hot_temp`.
    /// Used by the scale-up gate's "sustained ≥ 30 min" condition.
    pub sustained_below_hot_minutes: u32,
    /// How many consecutive minutes the autotuner has been STABLE.
    /// Used by the scale-up gate's "tuner stable ≥ 30 min" condition.
    pub tuner_stable_minutes: u32,
    /// Current effective power_target in watts (after any previous step).
    pub current_power_target_watts: u32,
    /// Configured (operator-set) power_target in watts. ScalingUp targets this.
    pub configured_power_target_watts: u32,
}

/// What the governor decides for this tick. The caller (thermal supervisor)
/// forwards `StepDown` / `StepUp` to the autotuner's power-target setter,
/// and `Shutdown` to the safety-supervisor's power-cut path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpsAction {
    /// Hold current target; no change.
    NoOp,
    /// Reduce power_target to `new_watts` (one walker step toward min_power).
    StepDownPowerTarget { new_watts: u32 },
    /// Increase power_target to `new_watts` (one walker step toward configured).
    StepUpPowerTarget { new_watts: u32 },
    /// Halt mining; wait `shutdown_duration_hours` before re-entering ScalingUp.
    Shutdown,
    /// Resume from Shutdown (typically followed by a sequence of StepUp).
    Restart,
    /// `board_temp >= dangerous_temp` — caller must override DPS and let the
    /// safety supervisor handle the emergency. The governor does NOT emit
    /// power-target changes in this case; the caller's existing
    /// EmergencyShutdown path takes precedence.
    DangerousTempOverride,
}

// ---------------------------------------------------------------------------
// Governor
// ---------------------------------------------------------------------------

/// DPS runtime governor. Owns the FSM state + scale-up gate evaluator +
/// shutdown dwell counter. The walker (`DpsWalkerConfig`) is held by
/// reference / value depending on caller preference.
#[derive(Debug, Clone)]
pub struct DpsGovernor {
    /// Catalog config (operator-tunable via REST `/api/dps`).
    pub config: DpsConfiguration,
    /// Per-family thermal thresholds (target/hot/dangerous).
    pub thermal_profile: DpsThermalProfile,
    /// Step-walker config (existing crate::dps::DpsWalkerConfig).
    pub walker: DpsWalkerConfig,
    /// Documented 4-AND scale-up gate (default per spec).
    pub scale_up_conditions: DpsScaleUpConditions,
    /// Current FSM state.
    state: DpsState,
    /// Seconds elapsed in Shutdown state (resets on entry).
    shutdown_elapsed_secs: u32,
    /// D5: optional on-disk path for the throttle-resume record. `None`
    /// disables persistence entirely — the governor then behaves exactly as
    /// the pre-D5 in-memory-only governor. Set via
    /// [`DpsGovernor::with_persistence`].
    persist_path: Option<std::path::PathBuf>,
    /// D5: scaled power target watts tracked across ticks so a persist can
    /// record where the throttle currently sits. Updated whenever the
    /// governor emits a StepDown / StepUp action.
    last_scaled_power_target_watts: u32,
}

impl DpsGovernor {
    /// Construct a governor with documented spec defaults.
    pub fn new(
        config: DpsConfiguration,
        thermal_profile: DpsThermalProfile,
        walker: DpsWalkerConfig,
    ) -> Self {
        Self {
            config,
            thermal_profile,
            walker,
            scale_up_conditions: DpsScaleUpConditions::default(),
            state: DpsState::Normal,
            shutdown_elapsed_secs: 0,
            persist_path: None,
            last_scaled_power_target_watts: 0,
        }
    }

    /// D5: enable throttle-state persistence at `path` and, if a
    /// scaled-down record already exists there (e.g. after a daemon restart
    /// while throttled), RESTORE that state instead of starting at Normal.
    ///
    /// Fail-open-safe: a missing/corrupt record (or any persistence I/O
    /// error) leaves the governor at its current in-memory state (Normal for
    /// a freshly-`new()`'d governor) — identical to the pre-D5 behavior. A
    /// genuine unexpected I/O error during the initial read is swallowed (the
    /// daemon must never refuse to start because of the resume file); it is
    /// surfaced via the returned bool only for caller logging.
    ///
    /// Returns `self` plus whether a throttled state was restored.
    pub fn with_persistence(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        if let Ok(Some(record)) = DpsGovernorPersistedState::load(&path) {
            // Restore a previously-throttled position. Normal is never
            // persisted (it's the cleared/absent state), so any present
            // record means "resume throttled".
            self.state = record.state;
            self.shutdown_elapsed_secs = record.shutdown_elapsed_secs;
            self.last_scaled_power_target_watts = record.scaled_power_target_watts;
        }
        // No record, corrupt record, or read error => stay at current
        // in-memory state (Normal). Fail-open.
        self.persist_path = Some(path);
        self
    }

    /// Read current FSM state.
    pub fn state(&self) -> DpsState {
        self.state
    }

    /// D5: the scaled power target watts the governor last acted on. Exposed
    /// so a caller restoring after a restart can re-apply the throttled
    /// power target it left off at.
    pub fn last_scaled_power_target_watts(&self) -> u32 {
        self.last_scaled_power_target_watts
    }

    /// D5: persist the current throttled state. Best-effort, fail-open — a
    /// write error is swallowed (never aborts a tick). No-op when persistence
    /// is disabled (`persist_path` is `None`).
    fn persist_throttled(&self) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };
        let record = DpsGovernorPersistedState {
            version: DPS_GOVERNOR_STATE_VERSION,
            saved_at_unix_s: now_unix_s(),
            state: self.state,
            scaled_power_target_watts: self.last_scaled_power_target_watts,
            shutdown_elapsed_secs: self.shutdown_elapsed_secs,
        };
        // A throttle-resume hint is never worth aborting a control tick, but a
        // durability failure must remain observable to operators.
        if let Err(error) = record.save_atomic(path) {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "Failed to persist DPS throttle-resume state"
            );
        }
    }

    /// D5: clear any persisted throttle record (called on thermal recovery /
    /// return to Normal). Best-effort, fail-open. A NotFound is fine. No-op
    /// when persistence is disabled.
    fn clear_persisted(&self) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };
        match dcentrald_common::atomic_file::remove_file(path) {
            Ok(
                dcentrald_common::atomic_file::AtomicRemoveOutcome::Removed
                | dcentrald_common::atomic_file::AtomicRemoveOutcome::AlreadyAbsent,
            ) => {}
            Err(error) => {
                // Clearing remains best-effort and never aborts a control tick,
                // but an unlink followed by parent-fsync failure is materially
                // different from a pre-unlink refusal: after a crash the stale
                // throttle record may reappear and must not be reported durable.
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    persistence_stage = %error.stage(),
                    target_unlinked = error.target_unlinked(),
                    target_absent_observed = error.target_absent_observed(),
                    deletion_durability_uncertain = error.deletion_durability_uncertain(),
                    "Failed to durably clear DPS throttle-resume state"
                );
            }
        }
    }

    /// True iff DPS is enabled in the config (caller should NoOp early
    /// when this returns false).
    pub fn is_enabled(&self) -> bool {
        self.config.is_enabled()
    }

    /// Process one tick. The caller passes a fresh `DpsTick` sample and
    /// receives the next action.
    ///
    /// `tick_elapsed_secs` is wall-time since the previous call (used by
    /// the Shutdown state to measure `shutdown_duration_hours`).
    pub fn tick(&mut self, sample: &DpsTick, tick_elapsed_secs: u32) -> DpsAction {
        // DPS disabled: NoOp (caller does nothing).
        if !self.is_enabled() {
            return DpsAction::NoOp;
        }

        let (_target_c, hot_c, dangerous_c) = self.thermal_profile.thresholds();

        // Dangerous-temp override beats every state. Caller's existing
        // EmergencyShutdown path takes precedence.
        if sample.board_temp_c >= dangerous_c || sample.chip_temp_c >= dangerous_c {
            return DpsAction::DangerousTempOverride;
        }

        // TODO(RE-002 D4): bosminer also scales down PRE-EMPTIVELY on two
        // additional triggers that we deliberately do NOT implement yet:
        //   1. dead-pool (all configured pools unreachable) — scale down to
        //      conserve power while no work can be submitted; and
        //   2. TemperatureHalfwayToDangerous — a pre-emptive step-down once
        //      board/chip temp crosses the midpoint between `hot` and
        //      `dangerous`, before the hard `hot` threshold.
        // Both interact with the thermal supervisor and the load-bearing
        // cut-hash-before-noise rule (an early step-down could raise fan
        // noise on a quiet home unit before it's thermally necessary), and
        // dead-pool scaling overlaps the Stratum failover/ hash-on-disconnect
        // policy. They need an EE/Thermal design pass (and likely an
        // operator/RE review of the exact bosminer trigger thresholds) before
        // implementation. Tracked in RE-TEAM-ASKS.md / CORPUS_RESOLUTIONS.md
        // §RE-002 D4. Do NOT wire either trigger here without that review.

        match self.state {
            DpsState::Normal => self.tick_normal(sample, hot_c),
            DpsState::ScalingDown => self.tick_scaling_down(sample, hot_c),
            DpsState::Shutdown => self.tick_shutdown(tick_elapsed_secs),
            DpsState::ScalingUp => self.tick_scaling_up(sample, hot_c),
        }
    }

    fn tick_normal(&mut self, sample: &DpsTick, hot_c: f32) -> DpsAction {
        if sample.board_temp_c >= hot_c {
            self.state = DpsState::ScalingDown;
            return self.compute_step_down(sample.current_power_target_watts);
        }
        DpsAction::NoOp
    }

    fn tick_scaling_down(&mut self, sample: &DpsTick, hot_c: f32) -> DpsAction {
        // First: check scale-up gate (we can recover from a temperature spike).
        // The 4-AND gate REQUIRES board_temp ≤ hot - delta + sustained + fan + tuner.
        let gate_met = self.scale_up_conditions.met(
            sample.board_temp_c,
            hot_c,
            sample.sustained_below_hot_minutes,
            sample.fan_speed_pct,
            sample.tuner_stable_minutes,
        );
        if gate_met {
            self.state = DpsState::ScalingUp;
            return self.compute_step_up(
                sample.current_power_target_watts,
                sample.configured_power_target_watts,
            );
        }

        // Still hot: step down further (or shutdown).
        if sample.board_temp_c >= hot_c {
            return self.compute_step_down(sample.current_power_target_watts);
        }

        // Below hot but gate not yet met (need sustained time + stable tuner):
        // hold at current target.
        DpsAction::NoOp
    }

    fn tick_shutdown(&mut self, elapsed_secs: u32) -> DpsAction {
        self.shutdown_elapsed_secs = self.shutdown_elapsed_secs.saturating_add(elapsed_secs);
        let shutdown_secs = self.config.shutdown_duration_hours.saturating_mul(3600);
        if self.shutdown_elapsed_secs >= shutdown_secs {
            self.shutdown_elapsed_secs = 0;
            self.state = DpsState::ScalingUp;
            // D5: still throttled (now recovering) — keep a fresh record so a
            // restart during the post-shutdown ramp resumes ScalingUp, not
            // Normal.
            self.persist_throttled();
            return DpsAction::Restart;
        }
        DpsAction::NoOp
    }

    fn tick_scaling_up(&mut self, sample: &DpsTick, hot_c: f32) -> DpsAction {
        // If we overheated again during scale-up: bail back to ScalingDown.
        if sample.board_temp_c >= hot_c {
            self.state = DpsState::ScalingDown;
            return self.compute_step_down(sample.current_power_target_watts);
        }
        // If we're already at the configured target: back to Normal.
        if sample.current_power_target_watts >= sample.configured_power_target_watts {
            self.state = DpsState::Normal;
            // D5: thermal recovery complete — CLEAR the persisted throttle
            // record so a future restart starts cleanly at Normal.
            self.clear_persisted();
            return DpsAction::NoOp;
        }
        // One more step up.
        self.compute_step_up(
            sample.current_power_target_watts,
            sample.configured_power_target_watts,
        )
    }

    /// Effective scale-down floor in watts.
    ///
    /// D3 (RE-002, 2026-05-20): the floor is `max(min_power_target_watts,
    /// min_psu_power_budget)`. The PSU-budget floor (when present) is a
    /// minimum stable-output limit DISTINCT from — and never lower than —
    /// the tuning-target floor; bosminer carries it separately so the PSU
    /// is never driven below its stable budget even if the target floor is
    /// lower. When `min_psu_power_budget` is `None` (the default) the floor
    /// is exactly `min_power_target_watts` — identical to prior behavior.
    fn effective_floor_w(&self) -> u32 {
        let target_floor = self.config.min_power_target_watts as u32;
        match self.config.min_psu_power_budget {
            Some(psu_floor) => target_floor.max(psu_floor),
            None => target_floor,
        }
    }

    /// Compute one scale-down step using the existing walker. Caps at the
    /// effective floor (`max(min_power_target_watts, min_psu_power_budget)`;
    /// see [`Self::effective_floor_w`]). If we hit the floor AND
    /// shutdown_enabled, transition to Shutdown; otherwise hold.
    fn compute_step_down(&mut self, current_w: u32) -> DpsAction {
        let min_w = self.effective_floor_w();
        if current_w <= min_w {
            // At the floor.
            self.last_scaled_power_target_watts = min_w;
            if self.config.shutdown_enabled.unwrap_or(false) {
                self.state = DpsState::Shutdown;
                self.shutdown_elapsed_secs = 0;
                // D5: persist the Shutdown position (a restart while shut
                // down must not silently re-boost a hot home unit).
                self.persist_throttled();
                return DpsAction::Shutdown;
            }
            // Hold at minimum. D5: persist the throttled (at-floor) position.
            self.persist_throttled();
            return DpsAction::NoOp;
        }
        let step = self.walker.walk_power_target(current_w, min_w);
        self.last_scaled_power_target_watts = step.next;
        // D5: persist the new throttled target.
        self.persist_throttled();
        DpsAction::StepDownPowerTarget {
            new_watts: step.next,
        }
    }

    /// Compute one scale-up step toward configured target. The walker
    /// clamps to its own min/max.
    fn compute_step_up(&mut self, current_w: u32, target_w: u32) -> DpsAction {
        let step = self.walker.walk_power_target(current_w, target_w);
        self.last_scaled_power_target_watts = step.next;
        // D5: still throttled (ScalingUp has not reached Normal yet) — keep
        // the persisted position fresh so a restart resumes mid-recovery.
        self.persist_throttled();
        DpsAction::StepUpPowerTarget {
            new_watts: step.next,
        }
    }

    /// Operator override: force state (for unit testing + lab use).
    /// Production callers should NOT use this; the FSM should be allowed
    /// to evolve from `tick()` only.
    #[cfg(test)]
    pub(crate) fn force_state(&mut self, state: DpsState) {
        self.state = state;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> DpsConfiguration {
        DpsConfiguration {
            enabled: Some(true),
            power_step_watts: 300,
            hashrate_step_ths: 11.0,
            min_power_target_watts: 943,
            min_hashrate_target_ths: 70.7417,
            shutdown_enabled: Some(false),
            shutdown_duration_hours: 3,
            mode: None,
            on_start_target_percent: Some(100),
            min_psu_power_budget: None,
            hashboard_idx: None,
        }
    }

    fn governor() -> DpsGovernor {
        // The DPS scenario these tests model is the S19j family, whose
        // documented default power target is 3068 W (RE handoff §RE-002). The
        // walker's `DpsWalkerConfig::default()` caps `max_power_w` at the
        // residential `ABSOLUTE_MAX_WATTS` (1800 W) — which would clamp every
        // step assertion below to 1800 and make the 300 W-step math
        // unobservable. Use a walker whose envelope spans the S19j target so
        // the step-down / step-up math (300 W steps from 3068 W) is the
        // behavior actually exercised. (Production callers pass the real
        // platform walker; this is a unit-test fixture only.)
        let walker = DpsWalkerConfig {
            power_step_w: 300,
            min_power_w: 200,
            max_power_w: 4000,
            ..DpsWalkerConfig::default()
        };
        DpsGovernor::new(enabled_config(), DpsThermalProfile::S19Family, walker)
    }

    fn sample_normal(current_w: u32, configured_w: u32) -> DpsTick {
        DpsTick {
            board_temp_c: 70.0, // below S19 hot=85
            chip_temp_c: 70.0,
            fan_speed_pct: 60,
            sustained_below_hot_minutes: 0,
            tuner_stable_minutes: 0,
            current_power_target_watts: current_w,
            configured_power_target_watts: configured_w,
        }
    }

    fn sample_hot(current_w: u32, configured_w: u32) -> DpsTick {
        let mut s = sample_normal(current_w, configured_w);
        s.board_temp_c = 90.0; // above S19 hot=85
        s
    }

    fn sample_recovered(current_w: u32, configured_w: u32) -> DpsTick {
        DpsTick {
            board_temp_c: 70.0, // ≥5°C below S19 hot=85 (75)
            chip_temp_c: 70.0,
            fan_speed_pct: 60, // <80%
            sustained_below_hot_minutes: 30,
            tuner_stable_minutes: 30,
            current_power_target_watts: current_w,
            configured_power_target_watts: configured_w,
        }
    }

    #[test]
    fn disabled_dps_returns_noop() {
        let mut g = governor();
        g.config.enabled = Some(false);
        let action = g.tick(&sample_hot(3068, 3068), 1);
        assert_eq!(action, DpsAction::NoOp);
    }

    #[test]
    fn normal_to_scaling_down_on_hot() {
        let mut g = governor();
        let action = g.tick(&sample_hot(3068, 3068), 1);
        assert_eq!(g.state(), DpsState::ScalingDown);
        match action {
            DpsAction::StepDownPowerTarget { new_watts } => {
                // step is 300; current=3068 → new=2768.
                assert_eq!(new_watts, 2768);
            }
            _ => panic!("expected StepDownPowerTarget; got {:?}", action),
        }
    }

    #[test]
    fn normal_stays_normal_when_cool() {
        let mut g = governor();
        let action = g.tick(&sample_normal(3068, 3068), 1);
        assert_eq!(g.state(), DpsState::Normal);
        assert_eq!(action, DpsAction::NoOp);
    }

    #[test]
    fn scaling_down_steps_to_floor_then_holds() {
        let mut g = governor();
        g.force_state(DpsState::ScalingDown);
        // Walk down step-by-step from 3068 → 943.
        let mut current = 3068u32;
        while current > 943 {
            let action = g.tick(&sample_hot(current, 3068), 1);
            match action {
                DpsAction::StepDownPowerTarget { new_watts } => {
                    assert!(new_watts < current);
                    current = new_watts;
                }
                _ => break,
            }
        }
        assert_eq!(current, 943);
        // One more tick at floor → NoOp (shutdown_enabled=false).
        let action = g.tick(&sample_hot(943, 3068), 1);
        assert_eq!(action, DpsAction::NoOp);
        assert_eq!(g.state(), DpsState::ScalingDown);
    }

    #[test]
    fn scaling_down_to_shutdown_when_shutdown_enabled_at_floor() {
        let mut g = governor();
        g.config.shutdown_enabled = Some(true);
        g.force_state(DpsState::ScalingDown);
        let action = g.tick(&sample_hot(943, 3068), 1);
        assert_eq!(action, DpsAction::Shutdown);
        assert_eq!(g.state(), DpsState::Shutdown);
    }

    #[test]
    fn shutdown_waits_for_duration_then_restarts() {
        let mut g = governor();
        g.config.shutdown_enabled = Some(true);
        g.config.shutdown_duration_hours = 1; // 3600 s
        g.force_state(DpsState::Shutdown);
        // Wait 1799 s → still Shutdown.
        let action = g.tick(&sample_normal(0, 3068), 1799);
        assert_eq!(action, DpsAction::NoOp);
        assert_eq!(g.state(), DpsState::Shutdown);
        // Cross the threshold → Restart.
        let action = g.tick(&sample_normal(0, 3068), 1801);
        assert_eq!(action, DpsAction::Restart);
        assert_eq!(g.state(), DpsState::ScalingUp);
    }

    #[test]
    fn scale_up_gate_must_be_4_and_met() {
        let mut g = governor();
        g.force_state(DpsState::ScalingDown);
        // Only 3 of 4 conditions met: tuner stable < 30 min.
        let mut sample = sample_recovered(2000, 3068);
        sample.tuner_stable_minutes = 0;
        let action = g.tick(&sample, 1);
        assert_eq!(action, DpsAction::NoOp);
        assert_eq!(g.state(), DpsState::ScalingDown);
    }

    #[test]
    fn scale_up_gate_met_transitions_to_scaling_up() {
        let mut g = governor();
        g.force_state(DpsState::ScalingDown);
        let action = g.tick(&sample_recovered(2000, 3068), 1);
        assert_eq!(g.state(), DpsState::ScalingUp);
        match action {
            DpsAction::StepUpPowerTarget { new_watts } => {
                // walker step is 300; 2000 → 2300.
                assert_eq!(new_watts, 2300);
            }
            _ => panic!("expected StepUpPowerTarget; got {:?}", action),
        }
    }

    #[test]
    fn scaling_up_to_normal_when_target_reached() {
        let mut g = governor();
        g.force_state(DpsState::ScalingUp);
        let action = g.tick(&sample_recovered(3068, 3068), 1);
        assert_eq!(g.state(), DpsState::Normal);
        assert_eq!(action, DpsAction::NoOp);
    }

    #[test]
    fn scaling_up_bails_back_to_scaling_down_on_temperature_spike() {
        let mut g = governor();
        g.force_state(DpsState::ScalingUp);
        let action = g.tick(&sample_hot(2500, 3068), 1);
        assert_eq!(g.state(), DpsState::ScalingDown);
        match action {
            DpsAction::StepDownPowerTarget { new_watts } => {
                assert_eq!(new_watts, 2200);
            }
            _ => panic!("expected StepDownPowerTarget; got {:?}", action),
        }
    }

    #[test]
    fn dangerous_temp_overrides_every_state() {
        for state in [
            DpsState::Normal,
            DpsState::ScalingDown,
            DpsState::Shutdown,
            DpsState::ScalingUp,
        ] {
            let mut g = governor();
            g.force_state(state);
            let mut sample = sample_normal(3068, 3068);
            sample.board_temp_c = 100.0; // above S19 dangerous=95
            let action = g.tick(&sample, 1);
            assert_eq!(
                action,
                DpsAction::DangerousTempOverride,
                "dangerous-temp must override {:?}",
                state
            );
        }
    }

    #[test]
    fn scale_up_each_condition_failure_blocks_gate() {
        let cases = [
            // (sustained, tuner_stable, fan_pct, board_temp_c, expected_met)
            (30u32, 30u32, 60u8, 70.0f32, true), // all met
            (29, 30, 60, 70.0, false),           // sustained too short
            (30, 29, 60, 70.0, false),           // tuner not stable long enough
            (30, 30, 81, 70.0, false),           // fan too fast (>=80)
            (30, 30, 60, 81.0, false),           // board within 5°C of hot=85
        ];
        for (sus, tuner, fan, temp, expected) in cases {
            let mut g = governor();
            g.force_state(DpsState::ScalingDown);
            let sample = DpsTick {
                board_temp_c: temp,
                chip_temp_c: temp,
                fan_speed_pct: fan,
                sustained_below_hot_minutes: sus,
                tuner_stable_minutes: tuner,
                current_power_target_watts: 2000,
                configured_power_target_watts: 3068,
            };
            let action = g.tick(&sample, 1);
            let transitioned_up = g.state() == DpsState::ScalingUp;
            assert_eq!(
                transitioned_up, expected,
                "scale-up gate eval mismatch for sample {:?}; action was {:?}",
                sample, action
            );
        }
    }

    #[test]
    fn config_default_anchor_values_match_re_handoff() {
        // From RE handoff §RE-002 "Observed S19j runtime values":
        // power_step=300W, min_power=943W, hashrate_step=11 TH/s.
        let cfg = enabled_config();
        assert_eq!(cfg.power_step_watts, 300);
        assert_eq!(cfg.min_power_target_watts, 943);
        assert_eq!(cfg.hashrate_step_ths, 11.0);
    }

    #[test]
    fn scale_up_conditions_default_match_re_handoff() {
        // RE handoff §RE-002 "Scale-Up Gate":
        // 5°C below hot, 30 min sustained, 80% fan, 30 min tuner stable.
        let c = DpsScaleUpConditions::default();
        assert_eq!(c.board_temp_below_hot_by_c, 5.0);
        assert_eq!(c.sustained_below_hot_minutes, 30);
        assert_eq!(c.max_fan_speed_pct, 80);
        assert_eq!(c.tuner_stable_minutes, 30);
    }

    // -----------------------------------------------------------------
    // D3 — PSU-budget floor (RE-002, 2026-05-20)
    // -----------------------------------------------------------------

    #[test]
    fn d3_psu_budget_raises_step_down_floor_above_target_floor() {
        // target floor 943 W, PSU floor 1200 W → effective floor 1200.
        let mut g = governor();
        g.config.min_psu_power_budget = Some(1200);
        g.force_state(DpsState::ScalingDown);
        // Walk down from 3068; it must STOP at 1200, not 943.
        let mut current = 3068u32;
        loop {
            let action = g.tick(&sample_hot(current, 3068), 1);
            match action {
                DpsAction::StepDownPowerTarget { new_watts } => {
                    assert!(new_watts >= 1200, "must never step below PSU floor 1200");
                    assert!(new_watts < current);
                    current = new_watts;
                }
                DpsAction::NoOp => break, // reached floor, shutdown disabled → hold
                other => panic!("unexpected action {:?}", other),
            }
        }
        assert_eq!(
            current, 1200,
            "effective floor must be the PSU budget (1200)"
        );
    }

    #[test]
    fn d3_no_psu_budget_keeps_target_floor() {
        // None (default) → floor stays min_power_target_watts (943).
        let g = governor();
        assert_eq!(g.config.min_psu_power_budget, None);
        assert_eq!(g.effective_floor_w(), 943);
    }

    #[test]
    fn d3_psu_budget_below_target_floor_is_ignored() {
        // PSU floor 500 < target floor 943 → max() keeps 943.
        let mut g = governor();
        g.config.min_psu_power_budget = Some(500);
        assert_eq!(g.effective_floor_w(), 943);
    }

    // -----------------------------------------------------------------
    // D5 — persist / clear the throttled DPS step (RE-002, 2026-05-20)
    // -----------------------------------------------------------------

    fn temp_state_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dcent_dps_governor_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("dps_governor_state.toml")
    }

    #[test]
    fn d5_persist_then_restore_round_trip() {
        let path = temp_state_path("roundtrip");

        // Governor 1: gets hot, scales down once → should persist ScalingDown.
        let mut g1 = governor().with_persistence(&path);
        let action = g1.tick(&sample_hot(3068, 3068), 1);
        assert_eq!(g1.state(), DpsState::ScalingDown);
        let persisted_target = match action {
            DpsAction::StepDownPowerTarget { new_watts } => new_watts,
            other => panic!("expected StepDownPowerTarget; got {:?}", other),
        };
        assert!(path.exists(), "scale-down must write the resume record");

        // Governor 2: fresh construct from the SAME path simulates a daemon
        // restart — must RESTORE ScalingDown, NOT start at Normal.
        let g2 = governor().with_persistence(&path);
        assert_eq!(
            g2.state(),
            DpsState::ScalingDown,
            "a restart while throttled must resume throttled, not re-boost"
        );
        assert_eq!(g2.last_scaled_power_target_watts(), persisted_target);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn d5_clear_on_recovery_returns_to_normal_on_restart() {
        let path = temp_state_path("clear_on_recovery");

        // Throttle down, then recover all the way to Normal.
        let mut g = governor().with_persistence(&path);
        g.tick(&sample_hot(3068, 3068), 1); // → ScalingDown, persisted
        assert!(path.exists());
        g.force_state(DpsState::ScalingUp);
        // current >= configured → ScalingUp returns to Normal + clears record.
        let action = g.tick(&sample_recovered(3068, 3068), 1);
        assert_eq!(action, DpsAction::NoOp);
        assert_eq!(g.state(), DpsState::Normal);
        assert!(
            !path.exists(),
            "thermal recovery to Normal must CLEAR the persisted record"
        );

        // A fresh governor from the cleared path starts at Normal.
        let g2 = governor().with_persistence(&path);
        assert_eq!(g2.state(), DpsState::Normal);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn d5_clear_refuses_symlink_without_aborting_thermal_recovery() {
        use std::os::unix::fs::symlink;

        let path = temp_state_path("clear_symlink_refusal");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outside = path.parent().unwrap().join("outside.toml");
        std::fs::write(&outside, b"must remain").unwrap();
        symlink(&outside, &path).unwrap();

        let mut g = governor().with_persistence(&path);
        g.force_state(DpsState::ScalingUp);
        let action = g.tick(&sample_recovered(3068, 3068), 1);
        assert_eq!(action, DpsAction::NoOp);
        assert_eq!(g.state(), DpsState::Normal);
        assert_eq!(std::fs::read(&outside).unwrap(), b"must remain");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn d5_clear_path_uses_shared_durable_remove_and_logs_uncertainty() {
        let source = include_str!("dps_governor.rs");
        let start = source.find("fn clear_persisted(&self)").unwrap();
        let end = source[start..]
            .find("pub fn is_enabled")
            .map(|offset| start + offset)
            .unwrap();
        let body = &source[start..end];
        assert!(body.contains("dcentrald_common::atomic_file::remove_file(path)"));
        assert!(!body.contains("std::fs::remove_file"));
        assert!(body.contains("target_unlinked = error.target_unlinked()"));
        assert!(body.contains("deletion_durability_uncertain"));
    }

    #[test]
    fn d5_missing_record_starts_normal_no_panic() {
        let path = temp_state_path("missing");
        assert!(!path.exists());
        // No record present → fail-open to Normal, no panic.
        let g = governor().with_persistence(&path);
        assert_eq!(g.state(), DpsState::Normal);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn d5_corrupt_record_starts_normal_no_panic() {
        let path = temp_state_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Garbage that is neither valid TOML nor a valid record.
        std::fs::write(&path, b"\x00\xff not even toml {{{ ][").unwrap();
        // Must fail-open to Normal (NOT panic, NOT refuse to construct).
        let g = governor().with_persistence(&path);
        assert_eq!(g.state(), DpsState::Normal);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn d5_wrong_version_record_starts_normal() {
        let path = temp_state_path("wrong_version");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale = DpsGovernorPersistedState {
            version: DPS_GOVERNOR_STATE_VERSION + 99,
            saved_at_unix_s: 0,
            state: DpsState::ScalingDown,
            scaled_power_target_watts: 2000,
            shutdown_elapsed_secs: 0,
        };
        // Write it manually (save_atomic stamps the right version, so build
        // the TOML directly with the stale version baked in).
        let toml = toml::to_string_pretty(&stale).unwrap();
        std::fs::write(&path, toml).unwrap();

        let g = governor().with_persistence(&path);
        assert_eq!(
            g.state(),
            DpsState::Normal,
            "a future-version record must be ignored (fail-open), not restored"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn d5_persistence_disabled_behaves_as_before() {
        // No with_persistence() → no file written, behaves exactly as pre-D5.
        let mut g = governor();
        let action = g.tick(&sample_hot(3068, 3068), 1);
        assert_eq!(g.state(), DpsState::ScalingDown);
        assert!(matches!(action, DpsAction::StepDownPowerTarget { .. }));
        // No persist_path → last_scaled is still tracked in-memory but nothing
        // is on disk to restore.
        assert_eq!(g.last_scaled_power_target_watts(), 2768);
    }

    #[test]
    fn d5_shutdown_state_persists_and_restores() {
        let path = temp_state_path("shutdown");

        // Drive to Shutdown (shutdown_enabled, at floor).
        let mut g = governor().with_persistence(&path);
        g.config.shutdown_enabled = Some(true);
        g.force_state(DpsState::ScalingDown);
        let action = g.tick(&sample_hot(943, 3068), 1);
        assert_eq!(action, DpsAction::Shutdown);
        assert_eq!(g.state(), DpsState::Shutdown);
        assert!(path.exists());

        // Restart restores Shutdown (not Normal — must keep waiting out the
        // shutdown dwell rather than re-boosting a hot unit).
        let g2 = governor().with_persistence(&path);
        assert_eq!(g2.state(), DpsState::Shutdown);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
