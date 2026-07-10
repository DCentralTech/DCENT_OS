//!  thm-A — LuxOS ATM (Advanced Thermal Management) state machine port.
//!
//! Source RE evidence:
//!
//! §3 (lines 181-285).
//!
//! ATM is LuxOS's automatic profile-stepper. At a fixed evaluation cadence
//! (typically the same 5-10 s thermal tick as the fan PID), it inspects
//! `(max_board_temp, max_chip_temp)` and decides whether to step the active
//! profile up (more hashrate, more heat) or down (less hashrate, less heat).
//!
//! The state machine is HAL-free pure logic. The runtime adapter inside
//! `dcentrald-thermal` (HAL-bound) feeds samples and acts on the resulting
//! `AtmDecision`.
//!
//! State diagram (per H-thermal-fan.md §3.2):
//! ```text
//!   Disabled  →  Off (boards down / autotuner not ready)
//!                ↓ "Health data is now available"
//!                Locked (startup_minutes grace, default 15 min)
//!                ↓ "ATM is now unlocked and can act normally"
//!                Active ⇄ Ramping (post_ramp_minutes grace per change)
//! ```
//!
//! Step rules (per §3.3):
//! - Step **DOWN** when `Tb > H OR Tc > Hc`, not in post-ramp lockout,
//!   not at min_profile.
//! - Step **UP** when `Tb < H - Wb AND Tc < Hc - Wc` (hysteresis margin),
//!   startup grace expired, not in post-ramp lockout, not at max_profile.
//!
//! Hard thermal abort: any sample with `max_chip_temp_c >= hard_chip_panic`
//! transitions immediately to `Failed` regardless of state. The runtime
//! caller is expected to escalate to the `dangerous_temp_c` board-shutdown
//! path; ATM itself stops emitting step requests.
//!
//! The state machine deliberately mirrors the shape of the
//! `dcentrald-silicon-profiles::gdtuner` module — same `feed(sample)`
//! contract, same Done/Failed terminals, same explicit `advance_to`
//! adapter hook for cases the state machine itself can't decide.

use serde::{Deserialize, Serialize};

/// Discrete ATM stages. `Disabled` is the operator-off state; every other
/// stage represents an active control path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtmStage {
    /// Operator disabled (`enabled = false` in `[atm]`).
    Disabled,
    /// Boards down or autotuner not yet ready ("Health data" pending).
    Off,
    /// startup_minutes grace lockout — temps settling, autotuner converging.
    Locked,
    /// Steady-state evaluation loop running.
    Active,
    /// Post-step ramp lockout — board thermalizing at new freq/voltage.
    Ramping,
    /// Hard thermal abort. Cleared only by `reset()`.
    Failed,
}

/// Direction of the most recent ATM decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtmDecision {
    /// Hold current profile (within hysteresis band, in lockout, etc.).
    Hold,
    /// Step to the next-higher profile (more hashrate).
    StepUp,
    /// Step to the next-lower profile (less hashrate).
    StepDown,
    /// Hard thermal abort — caller must shut hashboards down.
    AbortShutdown,
}

/// Per-tick sample fed into the state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtmSample {
    /// Highest board PCB temperature in °C (across all 4-corner sensors).
    pub max_board_temp_c: f32,
    /// Highest chip die temperature in °C (across all chips × all chains).
    pub max_chip_temp_c: f32,
    /// Whether the runtime adapter reports the autotuner is ready and
    /// boards are alive enough for ATM to start considering steps.
    pub health_ready: bool,
    /// Whether the active profile is at the bottom of the table.
    pub at_min_profile: bool,
    /// Whether the active profile is at the top of the table.
    pub at_max_profile: bool,
    /// Monotonic tick counter (caller-provided; we don't read the clock).
    pub tick: u64,
}

/// ATM thresholds. Defaults match LuxOS's `[temp_control]` defaults.
#[derive(Debug, Clone, Copy)]
pub struct AtmConfig {
    /// Board PCB hot threshold in °C. Step-DOWN trigger.
    pub board_hot_temp_c: f32,
    /// Board PCB hysteresis window in °C (`Wb` per RE doc, default 5).
    pub board_temp_window_c: f32,
    /// Chip die hot threshold in °C. Step-DOWN trigger.
    pub chip_hot_temp_c: f32,
    /// Chip die hysteresis window in °C (`Wc` per RE doc, default 8).
    pub chip_temp_window_c: f32,
    /// Hard chip panic — abort any stage at this temp.
    pub hard_chip_panic_c: f32,
    /// Startup-grace ticks (default 15 min × 60s/tick assumption).
    pub startup_grace_ticks: u32,
    /// Post-ramp lockout ticks.
    pub post_ramp_grace_ticks: u32,
}

impl Default for AtmConfig {
    fn default() -> Self {
        Self {
            board_hot_temp_c: 65.0,
            board_temp_window_c: 5.0,
            chip_hot_temp_c: 93.0,
            chip_temp_window_c: 8.0,
            hard_chip_panic_c: 100.0,
            // Live LuxOS default is 15 min. Tick rate is caller-defined; we
            // express this as 90 ticks (assuming a ~10 s evaluation cadence).
            // The runtime adapter can override via `with_grace_ticks`.
            startup_grace_ticks: 90,
            post_ramp_grace_ticks: 90,
        }
    }
}

/// ATM state machine. One instance per miner.
#[derive(Debug, Clone)]
pub struct AtmStepper {
    stage: AtmStage,
    config: AtmConfig,
    /// Tick at which Locked→Active transition becomes eligible.
    locked_until_tick: u64,
    /// Tick at which Ramping→Active transition becomes eligible.
    ramping_until_tick: u64,
    /// Last seen tick (for grace-window comparison).
    last_tick: u64,
    /// Lifetime sample counter.
    samples_total: u64,
}

impl AtmStepper {
    pub fn new(config: AtmConfig) -> Self {
        Self {
            stage: AtmStage::Disabled,
            config,
            locked_until_tick: 0,
            ramping_until_tick: 0,
            last_tick: 0,
            samples_total: 0,
        }
    }

    /// Default config; starts in `Disabled` state.
    pub fn fresh() -> Self {
        Self::new(AtmConfig::default())
    }

    /// Override grace windows (e.g. for shorter home-mining cadence).
    pub fn with_grace_ticks(mut self, startup: u32, post_ramp: u32) -> Self {
        self.config.startup_grace_ticks = startup;
        self.config.post_ramp_grace_ticks = post_ramp;
        self
    }

    pub fn stage(&self) -> AtmStage {
        self.stage
    }

    pub fn samples_total(&self) -> u64 {
        self.samples_total
    }

    /// Operator enables ATM. Disabled → Off.
    pub fn enable(&mut self) {
        if self.stage == AtmStage::Disabled {
            self.stage = AtmStage::Off;
        }
    }

    /// Operator disables ATM. Any state → Disabled.
    pub fn disable(&mut self) {
        self.stage = AtmStage::Disabled;
    }

    /// Reset Failed state back to Disabled. Caller must `enable()` again.
    pub fn reset(&mut self) {
        self.stage = AtmStage::Disabled;
        self.locked_until_tick = 0;
        self.ramping_until_tick = 0;
    }

    /// Acknowledge that a profile change was applied (started ramp).
    /// Caller invokes this AFTER physically stepping the profile so we know
    /// the post-ramp lockout has started.
    pub fn note_ramp_started(&mut self, current_tick: u64) {
        self.stage = AtmStage::Ramping;
        self.ramping_until_tick =
            current_tick.saturating_add(self.config.post_ramp_grace_ticks as u64);
    }

    /// Feed one sample. Returns `(new_stage, decision)`.
    pub fn feed(&mut self, sample: AtmSample) -> (AtmStage, AtmDecision) {
        self.samples_total += 1;
        self.last_tick = sample.tick;

        // Hard thermal abort: any stage transitions to Failed.
        if sample.max_chip_temp_c >= self.config.hard_chip_panic_c {
            self.stage = AtmStage::Failed;
            return (self.stage, AtmDecision::AbortShutdown);
        }

        match self.stage {
            AtmStage::Disabled | AtmStage::Failed => (self.stage, AtmDecision::Hold),

            AtmStage::Off => {
                if sample.health_ready {
                    self.stage = AtmStage::Locked;
                    self.locked_until_tick = sample
                        .tick
                        .saturating_add(self.config.startup_grace_ticks as u64);
                }
                (self.stage, AtmDecision::Hold)
            }

            AtmStage::Locked => {
                // Early break-out for over-temp anticipation:
                // if temps already exceed hot-band before grace expires,
                // we still don't step (matches `applying a closer profile` /
                // "Changing ATM state early to avoid overtemp" only triggers
                // from Active, not Locked). Just hold.
                if sample.tick >= self.locked_until_tick {
                    self.stage = AtmStage::Active;
                }
                (self.stage, AtmDecision::Hold)
            }

            AtmStage::Ramping => {
                // Early break-out: if a board is heading toward panic during
                // post-ramp grace, allow stepping DOWN immediately.
                let hot_emergency = sample.max_board_temp_c
                    > self.config.board_hot_temp_c + self.config.board_temp_window_c
                    || sample.max_chip_temp_c
                        > self.config.chip_hot_temp_c + self.config.chip_temp_window_c;
                if hot_emergency && !sample.at_min_profile {
                    self.stage = AtmStage::Active;
                    return (self.stage, AtmDecision::StepDown);
                }
                if sample.tick >= self.ramping_until_tick {
                    self.stage = AtmStage::Active;
                }
                (self.stage, AtmDecision::Hold)
            }

            AtmStage::Active => {
                // Step DOWN when hot.
                let hot = sample.max_board_temp_c > self.config.board_hot_temp_c
                    || sample.max_chip_temp_c > self.config.chip_hot_temp_c;
                if hot && !sample.at_min_profile {
                    return (self.stage, AtmDecision::StepDown);
                }
                // Step UP when cool with hysteresis margin on BOTH dims.
                let cool = sample.max_board_temp_c
                    < self.config.board_hot_temp_c - self.config.board_temp_window_c
                    && sample.max_chip_temp_c
                        < self.config.chip_hot_temp_c - self.config.chip_temp_window_c;
                if cool && !sample.at_max_profile {
                    return (self.stage, AtmDecision::StepUp);
                }
                (self.stage, AtmDecision::Hold)
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  atm-B extension: autotuner-interlock + api-gate
// ---------------------------------------------------------------------------
//
// Source RE evidence:
// `luxos/79-live-2026-04-29/analysis/H-thermal-fan.md` §3.5 (ATM ↔
// autotuner interlock, lines 263-271) + §3.7 (ATM API gate, lines
// 282-284).
//
// Two binary-string-pinned LuxOS behaviors that complement the
//  ATM stepper:
//
// 1. **Autotuner interlock**: ATM picks a profile; the autotuner
//    refines per-silicon voltage within that profile. When the
//    autotuner is enabled, ATM uses refined voltages from the per-
//    chip table. When disabled, ATM falls back to canned profile
//    voltages (which can sag the chain).
// 2. **API gate**: while ATM is running, several destructive
//    commands (profileset / frequencyset / voltageset / tunerswitch)
//    are blocked unless the operator passes `update_atm=true`. This
//    keeps users from fighting the controller.

/// State of the autotuner ↔ ATM interlock per §3.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AtmInterlock {
    /// True iff the autotuner is currently running and producing
    /// per-silicon voltage refinements. Per binary string:
    /// "Autotuner enabled; ATM profile changes will guard against
    /// non-tuned voltage values" vs "Autotuner disabled; ATM profile
    /// changes will use voltage as-is".
    pub autotuner_active: bool,
    /// True iff ATM is configured to USE refined voltages when
    /// autotuner_active=true. When false, ATM ignores the autotuner
    /// even if it's running (operator opt-out).
    pub use_refined_voltage: bool,
}

impl AtmInterlock {
    /// True iff refined per-silicon voltages are available AND ATM
    /// will use them. When false, ATM falls back to canned profile
    /// voltages.
    pub fn refined_voltage_available(&self) -> bool {
        self.autotuner_active && self.use_refined_voltage
    }
}

/// Destructive-class commands gated by the ATM API gate per §3.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtmGatedCommand {
    /// `profileset` — change the active autotuner profile.
    Profileset,
    /// `frequencyset` — set per-chain frequency.
    Frequencyset,
    /// `voltageset` — set per-chain voltage.
    Voltageset,
    /// `tunerswitch` — toggle autotuner state.
    Tunerswitch,
}

/// All 4 gated commands in stable iteration order.
pub const ATM_GATED_COMMANDS: &[AtmGatedCommand] = &[
    AtmGatedCommand::Profileset,
    AtmGatedCommand::Frequencyset,
    AtmGatedCommand::Voltageset,
    AtmGatedCommand::Tunerswitch,
];

/// Returns `true` iff the gated command may proceed at this ATM
/// stage. Per §3.7: "Cannot run this command while ATM is running".
/// The gate is closed when ATM is in any non-Disabled stage UNLESS
/// the operator explicitly passes `update_atm=true`.
pub fn api_gate_open(stage: AtmStage, _command: AtmGatedCommand, update_atm_flag: bool) -> bool {
    match stage {
        // ATM is off — the gate is always open; fight away.
        AtmStage::Disabled => true,
        // ATM is in any active stage — gate closed unless override.
        AtmStage::Off
        | AtmStage::Locked
        | AtmStage::Active
        | AtmStage::Ramping
        | AtmStage::Failed => update_atm_flag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cool_sample(tick: u64) -> AtmSample {
        AtmSample {
            max_board_temp_c: 50.0,
            max_chip_temp_c: 70.0,
            health_ready: true,
            at_min_profile: false,
            at_max_profile: false,
            tick,
        }
    }

    fn hot_sample(tick: u64) -> AtmSample {
        AtmSample {
            max_board_temp_c: 67.0, // above 65 hot
            max_chip_temp_c: 95.0,  // above 93 chip_hot
            health_ready: true,
            at_min_profile: false,
            at_max_profile: false,
            tick,
        }
    }

    #[test]
    fn fresh_starts_disabled() {
        let s = AtmStepper::fresh();
        assert_eq!(s.stage(), AtmStage::Disabled);
        assert_eq!(s.samples_total(), 0);
    }

    #[test]
    fn enable_transitions_disabled_to_off() {
        let mut s = AtmStepper::fresh();
        s.enable();
        assert_eq!(s.stage(), AtmStage::Off);
    }

    #[test]
    fn off_to_locked_when_health_ready() {
        let mut s = AtmStepper::fresh();
        s.enable();
        // health_ready=false stays Off.
        let (stage, dec) = s.feed(AtmSample {
            health_ready: false,
            ..cool_sample(1)
        });
        assert_eq!(stage, AtmStage::Off);
        assert_eq!(dec, AtmDecision::Hold);

        let (stage, _) = s.feed(cool_sample(2));
        assert_eq!(stage, AtmStage::Locked);
    }

    #[test]
    fn locked_promotes_to_active_after_startup_grace() {
        let mut s = AtmStepper::fresh().with_grace_ticks(5, 5);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked, locked_until=5

        // Ticks 0..4 stay Locked.
        for t in 1..5 {
            let (stage, dec) = s.feed(cool_sample(t));
            assert_eq!(stage, AtmStage::Locked, "tick {} should still be Locked", t);
            assert_eq!(dec, AtmDecision::Hold);
        }
        // Tick 5 promotes to Active.
        let (stage, _) = s.feed(cool_sample(5));
        assert_eq!(stage, AtmStage::Active);
    }

    #[test]
    fn active_steps_down_when_hot() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked
        s.feed(cool_sample(1)); // Locked -> Active (grace=0)
        let (_, dec) = s.feed(hot_sample(2));
        assert_eq!(dec, AtmDecision::StepDown);
    }

    #[test]
    fn active_steps_up_when_cool_with_hysteresis_margin() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1)); // -> Active
        let (_, dec) = s.feed(cool_sample(2));
        assert_eq!(dec, AtmDecision::StepUp);
    }

    #[test]
    fn active_holds_within_hysteresis_band() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1)); // -> Active
                                // Board 63 < 65 (hot) but 63 > 65-5=60, so within hysteresis on board side.
                                // Chip 88 < 93 (chip_hot) but 88 > 93-8=85, so within hysteresis on chip side.
        let (_, dec) = s.feed(AtmSample {
            max_board_temp_c: 63.0,
            max_chip_temp_c: 88.0,
            ..cool_sample(2)
        });
        assert_eq!(dec, AtmDecision::Hold);
    }

    #[test]
    fn active_holds_at_min_profile_when_hot() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1)); // -> Active
        let (_, dec) = s.feed(AtmSample {
            at_min_profile: true,
            ..hot_sample(2)
        });
        // Cannot go lower; hold (panic path is the runtime adapter's job).
        assert_eq!(dec, AtmDecision::Hold);
    }

    #[test]
    fn active_holds_at_max_profile_when_cool() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1));
        let (_, dec) = s.feed(AtmSample {
            at_max_profile: true,
            ..cool_sample(2)
        });
        assert_eq!(dec, AtmDecision::Hold);
    }

    #[test]
    fn ramping_lockout_holds_for_post_ramp_ticks() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 5);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1)); // -> Active
        s.note_ramp_started(2); // -> Ramping until tick 7

        for t in 3..7 {
            let (stage, dec) = s.feed(cool_sample(t));
            assert_eq!(stage, AtmStage::Ramping);
            assert_eq!(dec, AtmDecision::Hold);
        }
        // Tick 7 unlocks back to Active; that sample itself doesn't step.
        let (stage, _) = s.feed(cool_sample(7));
        assert_eq!(stage, AtmStage::Active);
    }

    #[test]
    fn ramping_early_break_out_on_emergency_hot() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 100);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1));
        s.note_ramp_started(2);
        // Within ramp lockout but board is at 71 (above 65+5=70) — emergency.
        let (stage, dec) = s.feed(AtmSample {
            max_board_temp_c: 71.0,
            ..cool_sample(3)
        });
        assert_eq!(stage, AtmStage::Active);
        assert_eq!(dec, AtmDecision::StepDown);
    }

    #[test]
    fn hard_chip_panic_aborts_any_stage() {
        let mut s = AtmStepper::fresh();
        s.enable();
        let (stage, dec) = s.feed(AtmSample {
            max_chip_temp_c: 105.0,
            ..cool_sample(1)
        });
        assert_eq!(stage, AtmStage::Failed);
        assert_eq!(dec, AtmDecision::AbortShutdown);

        // Failed is terminal until reset.
        for t in 2..10 {
            let (stage, dec) = s.feed(cool_sample(t));
            assert_eq!(stage, AtmStage::Failed);
            assert_eq!(dec, AtmDecision::Hold);
        }
        s.reset();
        assert_eq!(s.stage(), AtmStage::Disabled);
    }

    #[test]
    fn hard_chip_panic_aborts_from_active_and_ramping_mining_stages() {
        // The existing `hard_chip_panic_aborts_any_stage` only exercises the
        // panic guard from the `Off` stage. The stages where the panic guard
        // matters MOST — `Locked`, `Active`, and `Ramping` (chip running hot
        // under load) — were never directly pinned. This is the firmware's
        // last-resort thermal-shutdown decision: if a refactor ever moved or
        // narrowed the top-of-`feed` panic check so it stopped applying to
        // the active mining stages, an at-panic chip would only get a single
        // one-profile `StepDown` instead of a full `AbortShutdown` — a
        // fire-risk regression that today's suite would not catch.
        //
        // Panic temp (>= hard_chip_panic_c, default 100.0) must win over the
        // ordinary `Active`-stage StepDown path even though the sample is also
        // "hot" by the StepDown threshold.

        // --- Locked stage ---
        let mut s = AtmStepper::fresh().with_grace_ticks(100, 0);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked (locked_until far in the future)
        assert_eq!(s.stage(), AtmStage::Locked);
        let (stage, dec) = s.feed(AtmSample {
            max_chip_temp_c: 105.0,
            ..cool_sample(1)
        });
        assert_eq!(stage, AtmStage::Failed, "Locked-stage panic must Fail");
        assert_eq!(
            dec,
            AtmDecision::AbortShutdown,
            "Locked-stage panic must AbortShutdown, never Hold/StepDown"
        );

        // --- Active stage --- panic must override the StepDown-when-hot path.
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked
        s.feed(cool_sample(1)); // Locked -> Active (grace=0)
        assert_eq!(s.stage(), AtmStage::Active);
        let (stage, dec) = s.feed(AtmSample {
            max_board_temp_c: 67.0, // also "hot" by the StepDown threshold
            max_chip_temp_c: 105.0, // ...but at panic -> abort must win
            ..cool_sample(2)
        });
        assert_eq!(stage, AtmStage::Failed, "Active-stage panic must Fail");
        assert_eq!(
            dec,
            AtmDecision::AbortShutdown,
            "Active-stage panic must AbortShutdown, NOT StepDown"
        );

        // --- Ramping stage --- panic must override the emergency-StepDown path.
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 100);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked
        s.feed(cool_sample(1)); // Locked -> Active
        s.note_ramp_started(2); // Active -> Ramping (lockout far in the future)
        assert_eq!(s.stage(), AtmStage::Ramping);
        let (stage, dec) = s.feed(AtmSample {
            max_board_temp_c: 71.0, // would trigger Ramping emergency StepDown
            max_chip_temp_c: 105.0, // ...but at panic -> abort must win
            ..cool_sample(3)
        });
        assert_eq!(stage, AtmStage::Failed, "Ramping-stage panic must Fail");
        assert_eq!(
            dec,
            AtmDecision::AbortShutdown,
            "Ramping-stage panic must AbortShutdown, NOT emergency StepDown"
        );
    }

    #[test]
    fn hard_chip_panic_is_inclusive_at_exact_threshold() {
        // The guard is `>=`, so a chip sitting EXACTLY at hard_chip_panic_c
        // must abort (boundary safety). A refactor to `>` would let the chip
        // dwell at the panic temperature without shutting down.
        let panic_c = AtmConfig::default().hard_chip_panic_c;
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0)); // Off -> Locked
        s.feed(cool_sample(1)); // Locked -> Active
        assert_eq!(s.stage(), AtmStage::Active);
        let (stage, dec) = s.feed(AtmSample {
            max_chip_temp_c: panic_c,
            ..cool_sample(2)
        });
        assert_eq!(stage, AtmStage::Failed);
        assert_eq!(dec, AtmDecision::AbortShutdown);
    }

    #[test]
    fn disable_returns_to_disabled_from_any_stage() {
        let mut s = AtmStepper::fresh().with_grace_ticks(0, 0);
        s.enable();
        s.feed(cool_sample(0));
        s.feed(cool_sample(1)); // Active
        s.disable();
        assert_eq!(s.stage(), AtmStage::Disabled);
        // Subsequent samples don't move out of Disabled until enable().
        let (stage, dec) = s.feed(cool_sample(2));
        assert_eq!(stage, AtmStage::Disabled);
        assert_eq!(dec, AtmDecision::Hold);
    }

    #[test]
    fn samples_total_is_lifetime_counter() {
        let mut s = AtmStepper::fresh();
        s.enable();
        for t in 0..20 {
            s.feed(cool_sample(t));
        }
        assert_eq!(s.samples_total(), 20);
        s.reset();
        // reset does not zero lifetime counter.
        assert_eq!(s.samples_total(), 20);
    }

    // -----------------------------------------------------------------------
    //  atm-B tests: autotuner-interlock + api-gate
    // -----------------------------------------------------------------------

    #[test]
    fn interlock_default_is_no_autotuner_no_refined_voltage() {
        let i = AtmInterlock::default();
        assert!(!i.autotuner_active);
        assert!(!i.use_refined_voltage);
        assert!(!i.refined_voltage_available());
    }

    #[test]
    fn interlock_refined_voltage_requires_both_flags() {
        // Autotuner running but operator disabled use_refined → no.
        let i = AtmInterlock {
            autotuner_active: true,
            use_refined_voltage: false,
        };
        assert!(!i.refined_voltage_available());
        // Operator wants refined but autotuner is off → no.
        let i = AtmInterlock {
            autotuner_active: false,
            use_refined_voltage: true,
        };
        assert!(!i.refined_voltage_available());
        // Both flags true → refined available.
        let i = AtmInterlock {
            autotuner_active: true,
            use_refined_voltage: true,
        };
        assert!(i.refined_voltage_available());
    }

    #[test]
    fn interlock_round_trips_through_serde() {
        for i in [
            AtmInterlock::default(),
            AtmInterlock {
                autotuner_active: true,
                use_refined_voltage: false,
            },
            AtmInterlock {
                autotuner_active: true,
                use_refined_voltage: true,
            },
        ] {
            let json = serde_json::to_string(&i).unwrap();
            let back: AtmInterlock = serde_json::from_str(&json).unwrap();
            assert_eq!(i, back);
        }
    }

    #[test]
    fn api_gate_open_when_atm_disabled() {
        // §3.7: ATM disabled → no controller fight; gate open for ALL
        // commands regardless of update_atm flag.
        for cmd in ATM_GATED_COMMANDS.iter().copied() {
            assert!(
                api_gate_open(AtmStage::Disabled, cmd, false),
                "{:?} should be open in Disabled stage without flag",
                cmd
            );
            assert!(
                api_gate_open(AtmStage::Disabled, cmd, true),
                "{:?} should be open in Disabled stage with flag",
                cmd
            );
        }
    }

    #[test]
    fn api_gate_closed_in_active_stages_without_flag() {
        // §3.7: any non-Disabled stage CLOSES the gate unless the
        // operator passes update_atm=true.
        for stage in [
            AtmStage::Off,
            AtmStage::Locked,
            AtmStage::Active,
            AtmStage::Ramping,
            AtmStage::Failed,
        ] {
            for cmd in ATM_GATED_COMMANDS.iter().copied() {
                assert!(
                    !api_gate_open(stage, cmd, false),
                    "stage {:?} cmd {:?} should be CLOSED without flag",
                    stage,
                    cmd
                );
            }
        }
    }

    #[test]
    fn api_gate_opens_with_update_atm_flag_override() {
        // Operator passes update_atm=true → gate opens even for active ATM.
        for stage in [
            AtmStage::Off,
            AtmStage::Locked,
            AtmStage::Active,
            AtmStage::Ramping,
            AtmStage::Failed,
        ] {
            for cmd in ATM_GATED_COMMANDS.iter().copied() {
                assert!(
                    api_gate_open(stage, cmd, true),
                    "stage {:?} cmd {:?} should OPEN with flag",
                    stage,
                    cmd
                );
            }
        }
    }

    #[test]
    fn api_gate_command_count_pinned_to_4() {
        // §3.7 lists 4 gated commands: profileset, frequencyset,
        // voltageset, tunerswitch. Pin so a refactor cannot silently
        // add or remove one.
        assert_eq!(ATM_GATED_COMMANDS.len(), 4);
    }

    #[test]
    fn gated_command_round_trips_through_serde() {
        for cmd in ATM_GATED_COMMANDS.iter().copied() {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: AtmGatedCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(cmd, back);
        }
    }

    #[test]
    fn gated_command_serializes_in_snake_case() {
        // Wire form matches the LuxOS API command names verbatim.
        for (cmd, expected) in [
            (AtmGatedCommand::Profileset, "\"profileset\""),
            (AtmGatedCommand::Frequencyset, "\"frequencyset\""),
            (AtmGatedCommand::Voltageset, "\"voltageset\""),
            (AtmGatedCommand::Tunerswitch, "\"tunerswitch\""),
        ] {
            assert_eq!(serde_json::to_string(&cmd).unwrap(), expected);
        }
    }

    #[test]
    fn api_gate_open_independent_of_command_choice() {
        // Pin: the gate decision is purely (stage, flag), not command.
        // All 4 commands behave identically per §3.7.
        for stage in [AtmStage::Off, AtmStage::Active, AtmStage::Ramping] {
            let with_flag: Vec<bool> = ATM_GATED_COMMANDS
                .iter()
                .map(|c| api_gate_open(stage, *c, true))
                .collect();
            let without: Vec<bool> = ATM_GATED_COMMANDS
                .iter()
                .map(|c| api_gate_open(stage, *c, false))
                .collect();
            assert!(with_flag.iter().all(|b| *b));
            assert!(without.iter().all(|b| !*b));
        }
    }
}
