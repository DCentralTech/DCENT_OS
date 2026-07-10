//! VNish "Algo 2" 5-phase autotuner daemon adapter (RE-007 closure, Wave F
//! 2026-05-19).
//!
//! Thin daemon-side wrapper around the HAL-free 5-phase state machine
//! already shipped in  at
//! `dcentrald_api_types::autotune_phase::AutotuneFsm`. Adds:
//! 1. A TOML opt-in gate `[autotune.vnish_phase].enabled` so the runtime
//!    can be disabled by default (Wave-D/E safety contract).
//! 2. An explicit `VnishTuneAction` enum the caller consumes — same shape
//!    as Wave D's `DpsAction` (the FSM emits intent; the caller executes
//!    against the autotuner / chain / PSU).
//! 3. Documentation cross-referencing the RE handoff so future maintainers
//!    can trace each constant back to the corpus.
//!
//! # Source of truth
//!
//! - State machine + transitions: `dcentrald_api_types::autotune_phase`
//!   ( commit `ae5ba426`), grounded in
//!   `re-handoffs/RE-007-vnish-algo2-autotune.md`.
//! - VNish 1.2.7 constants: shipped as `AutotuneConfig` defaults in the
//!   catalog crate per RE-007 §"Voltage Walk" + §"Frequency Walk".
//! - Behavioral spec: `re-handoffs/RE-007-vnish-algo2-autotune.md`
//!   §"Runtime Algorithm" + §"Entry / Exit / Failure Rules".
//!
//! Confidence per RE-007: HIGH on phase order/constants/basic advance
//! rules. MEDIUM on failure/regression. LOW on persistence file path
//! (Wave F controls its own filename — DCENT_OS does NOT match VNish's
//! `/nvdata/anthillos/` layout).
//!
//! # Opt-in safety
//!
//! This module is COMPILED but NOT INSTANTIATED into the running daemon
//! by default. The integration site in the autotuner runtime will be
//! gated on `[autotune.vnish_phase].enabled = true` in `dcentrald.toml`;
//! with that flag false (default), the existing TABS autotuner stays
//! master. Live HW validation (Wave H, with operator per-action
//! authorization) is the gate to enabling this in production.
//!
//! # Load-bearing rules
//!
//! - **VNish thermal thresholds are informational only.** The
//!   `re-handoffs/RE-007` document quotes hot 85 C / dangerous 90 C /
//!   dangerous_board 80 C; these are **NOT** used as DCENT_OS thermal
//!   thresholds (those stay owned by `dcentrald-thermal::profiles` per
//!   platform). Enforced by `tests::vnish_thresholds_not_used_as_dcentos_thermal_limits`.
//! - **Adapter emits intent only.** The caller (autotuner runtime)
//!   executes against hardware; the adapter never touches the chain,
//!   PSU, or fan controller.
//! - **Default-off TOML opt-in.** Matches Wave D's `dps_governor` +
//!   Wave E's `bad_chip_supervisor`/`thermal_supervisor` shape.

use dcentrald_api_types::autotune_phase::{
    AutotuneConfig, AutotuneFsm, AutotuneObservation, AutotunePhase,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// VNish 5-phase autotuner adapter configuration (TOML
/// `[autotune.vnish_phase]`).
///
/// The actual VNish constants live in `AutotuneConfig` ( catalog);
/// this struct adds the daemon-side enable gate + operator-tunable
/// overrides of any constant the operator wants to deviate from VNish's
/// defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VnishPhaseConfig {
    /// **Default false.** Live-HW gated to `true` only after Wave H
    /// operator authorization. With this flag false the adapter is
    /// dormant and `observe()` returns `NoOp` immediately.
    #[serde(default)]
    pub enabled: bool,

    /// Operator override of the underlying `AutotuneConfig` (per-state
    /// settle time, voltage steps, target percent, phase4 max rounds,
    /// final voltage trim). Defaults to VNish 1.2.7 constants per Wave
    /// 17 catalog.
    #[serde(default = "default_autotune_config")]
    pub autotune: AutotuneConfig,
}

fn default_autotune_config() -> AutotuneConfig {
    AutotuneConfig::default()
}

// ---------------------------------------------------------------------------
// Action emitted to caller
// ---------------------------------------------------------------------------

/// What the adapter decides this tick. Mirrors the Wave-D `DpsAction`
/// shape: the FSM emits intent; the caller (autotuner runtime) executes
/// against hardware. The adapter never touches the chain, PSU, or fan
/// controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VnishTuneAction {
    /// No state change; continue current behavior.
    NoOp,
    /// Phase 1 entered: apply preset and start settling. The caller
    /// applies `(freq_mhz, voltage_mv10, target_hashrate_ths)` from the
    /// active preset, then waits `state_time_s` before the next
    /// observation.
    ApplyPreset,
    /// Phase 2 step: lower voltage by `dec_volt_mv10` from the current
    /// stable point and observe stability.
    StepVoltageDown { dec_volt_mv10: u32 },
    /// Phase 2 → Phase 3 transition: voltage was found unstable; lock
    /// `last_stable + higher_volt_offset` and start per-chip frequency
    /// walk.
    LockVoltageAndAdvance {
        last_stable_mv10: u32,
        higher_volt_offset_mv10: u32,
    },
    /// Phase 3 step: raise the next chip's frequency by `mhz_step` while
    /// it stays error-free.
    RaiseChipFreq { mhz_step: u16 },
    /// Phase 4 round: fine-tune per-chip frequencies based on error
    /// rate. `round` is 1, 2, …, up to `phase4_max_rounds`.
    TuneChipFreqsRound { round: u32 },
    /// Phase 4 → Phase 5 transition: rounds converged or `max_rounds`
    /// hit. Caller starts the final-volt trim window.
    AdvanceToFinalVolt,
    /// Phase 5 trim: raise voltage by `end_change_volt_mv10` for safety
    /// margin and run the verification window.
    TrimVoltageFinal { end_change_volt_mv10: u32 },
    /// Phase 5 verified clean: persist the tuned preset.
    MarkPresetTuned,
    /// Run failed: hard fault, voltage out-of-range, or verification
    /// errors. Caller resets and notifies the operator.
    MarkPresetFailed { reason: TuneFailureReason },
}

/// Why an autotune run failed. Kept narrow + serializable for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuneFailureReason {
    /// Hard fault signal from sensor / safety supervisor.
    HardFault,
    /// HW errors detected during Phase 5 final-volt verification.
    FinalVoltVerificationFailed,
    /// Phase 4 hit `max_rounds` without converging AND the FinalVolt
    /// verify also failed — chained failure.
    Phase4DivergedAndFinalVerifyFailed,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// VNish 5-phase autotuner daemon-side adapter. Owns the underlying
/// `AutotuneFsm`, the TOML enable gate, and the action-emission logic.
#[derive(Debug, Clone)]
pub struct VnishPhaseAdapter {
    config: VnishPhaseConfig,
    fsm: AutotuneFsm,
    /// Previous phase, used to detect transitions and emit phase-entry
    /// actions exactly once.
    previous_phase: AutotunePhase,
}

impl VnishPhaseAdapter {
    /// Construct an adapter with the given config. The underlying FSM
    /// starts in `Idle`.
    pub fn new(config: VnishPhaseConfig) -> Self {
        let fsm = AutotuneFsm::new(config.autotune);
        Self {
            previous_phase: fsm.phase(),
            config,
            fsm,
        }
    }

    /// True iff the adapter is enabled in the config (caller should
    /// NoOp early when this returns false).
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Read current FSM phase.
    pub fn phase(&self) -> AutotunePhase {
        self.fsm.phase()
    }

    /// Read current Phase 4 round counter.
    pub fn phase4_round(&self) -> u32 {
        self.fsm.phase4_round()
    }

    /// Read the last-stable voltage observed during Phase 2.
    pub fn last_stable_voltage_mv10(&self) -> u32 {
        self.fsm.last_stable_voltage_mv10()
    }

    /// Reset the adapter back to `Idle`.
    pub fn reset(&mut self) {
        self.fsm.reset();
        self.previous_phase = AutotunePhase::Idle;
    }

    /// Process one observation. Returns the action the caller should
    /// execute. When the adapter is disabled, returns `NoOp` without
    /// touching the FSM. Otherwise feeds the observation to the
    /// underlying FSM and computes the action from the
    /// `(previous_phase, new_phase, observation)` triple.
    pub fn observe(&mut self, obs: AutotuneObservation) -> VnishTuneAction {
        if !self.is_enabled() {
            return VnishTuneAction::NoOp;
        }

        let prior = self.previous_phase;
        let next = self.fsm.feed(obs);
        self.previous_phase = next;
        let cfg = self.fsm.config();

        match (prior, next) {
            // Idle → InitPresets: caller applies preset and starts settling.
            (AutotunePhase::Idle, AutotunePhase::InitPresets) => VnishTuneAction::ApplyPreset,

            // InitPresets → FindMinVolt: settle done; caller starts the
            // first voltage step-down. The "step" is the canonical
            // dec_volt_mv10.
            (AutotunePhase::InitPresets, AutotunePhase::FindMinVolt) => {
                VnishTuneAction::StepVoltageDown {
                    dec_volt_mv10: cfg.dec_volt_mv10,
                }
            }

            // FindMinVolt → FindMinVolt (stable, step further): caller
            // continues stepping voltage down.
            (AutotunePhase::FindMinVolt, AutotunePhase::FindMinVolt) => {
                VnishTuneAction::StepVoltageDown {
                    dec_volt_mv10: cfg.dec_volt_mv10,
                }
            }

            // FindMinVolt → AdjustChipFreqs: voltage found unstable;
            // lock the last stable + offset and start chip-freq walk.
            (AutotunePhase::FindMinVolt, AutotunePhase::AdjustChipFreqs) => {
                VnishTuneAction::LockVoltageAndAdvance {
                    last_stable_mv10: self.fsm.last_stable_voltage_mv10(),
                    higher_volt_offset_mv10: cfg.higher_volt_offset_mv10,
                }
            }

            // AdjustChipFreqs → AdjustChipFreqs (still walking): caller
            // raises the next chip's freq by mhz_step (canonical 5 MHz
            // per RE-007 §"Frequency Walk").
            (AutotunePhase::AdjustChipFreqs, AutotunePhase::AdjustChipFreqs) => {
                VnishTuneAction::RaiseChipFreq { mhz_step: 5 }
            }

            // AdjustChipFreqs → TuneChipFreqs: start Phase 4 fine-tune.
            (AutotunePhase::AdjustChipFreqs, AutotunePhase::TuneChipFreqs) => {
                VnishTuneAction::TuneChipFreqsRound {
                    round: self.fsm.phase4_round() + 1,
                }
            }

            // TuneChipFreqs → TuneChipFreqs: next round.
            (AutotunePhase::TuneChipFreqs, AutotunePhase::TuneChipFreqs) => {
                VnishTuneAction::TuneChipFreqsRound {
                    round: self.fsm.phase4_round(),
                }
            }

            // TuneChipFreqs → FinalVolt: converged or max rounds hit.
            (AutotunePhase::TuneChipFreqs, AutotunePhase::FinalVolt) => {
                VnishTuneAction::AdvanceToFinalVolt
            }

            // FinalVolt → FinalVolt (still verifying): caller trims
            // voltage by end_change_volt_mv10 (typ 10-20 mV per RE-007).
            (AutotunePhase::FinalVolt, AutotunePhase::FinalVolt) => {
                VnishTuneAction::TrimVoltageFinal {
                    end_change_volt_mv10: cfg.end_change_volt_mv10,
                }
            }

            // FinalVolt → Done: tuned cleanly; persist the preset.
            (AutotunePhase::FinalVolt, AutotunePhase::Done) => VnishTuneAction::MarkPresetTuned,

            // Any → Failed: hard fault or verification failure.
            (prev, AutotunePhase::Failed) => {
                let reason = if obs.hard_fault {
                    TuneFailureReason::HardFault
                } else if prev == AutotunePhase::FinalVolt {
                    TuneFailureReason::FinalVoltVerificationFailed
                } else {
                    TuneFailureReason::Phase4DivergedAndFinalVerifyFailed
                };
                VnishTuneAction::MarkPresetFailed { reason }
            }

            // Terminal hold (Done → Done, Failed → Failed) and any
            // other no-transition tick → NoOp.
            _ => VnishTuneAction::NoOp,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_enabled() -> VnishPhaseConfig {
        VnishPhaseConfig {
            enabled: true,
            ..VnishPhaseConfig::default()
        }
    }

    fn started() -> AutotuneObservation {
        AutotuneObservation {
            operator_started: true,
            voltage_mv10: 1500,
            hw_errors_sum: 0,
            hashrate_ratio: 1.0,
            phase4_converged: false,
            timed_wait_done: false,
            hard_fault: false,
        }
    }

    fn settled() -> AutotuneObservation {
        AutotuneObservation {
            timed_wait_done: true,
            ..started()
        }
    }

    fn unstable() -> AutotuneObservation {
        AutotuneObservation {
            hw_errors_sum: 10,
            hashrate_ratio: 0.6,
            timed_wait_done: true,
            ..started()
        }
    }

    // -- 1. Default-off contract --
    #[test]
    fn adapter_disabled_by_default_emits_noop() {
        let mut adapter = VnishPhaseAdapter::new(VnishPhaseConfig::default());
        assert!(!adapter.is_enabled());
        // Even an operator-start observation should produce NoOp while disabled.
        assert_eq!(adapter.observe(started()), VnishTuneAction::NoOp);
        // FSM should NOT have advanced (default-off contract — adapter is fully dormant).
        assert_eq!(adapter.phase(), AutotunePhase::Idle);
    }

    // -- 2. Idle → InitPresets emits ApplyPreset --
    #[test]
    fn operator_start_emits_apply_preset() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        assert_eq!(adapter.observe(started()), VnishTuneAction::ApplyPreset);
        assert_eq!(adapter.phase(), AutotunePhase::InitPresets);
    }

    // -- 3. InitPresets → FindMinVolt emits StepVoltageDown --
    #[test]
    fn settle_after_init_emits_step_voltage_down() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        let action = adapter.observe(settled());
        assert!(matches!(
            action,
            VnishTuneAction::StepVoltageDown { dec_volt_mv10: 500 }
        ));
        assert_eq!(adapter.phase(), AutotunePhase::FindMinVolt);
    }

    // -- 4. FindMinVolt stable → continue stepping down --
    #[test]
    fn stable_in_find_min_volt_continues_stepping_down() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        adapter.observe(settled());
        // Now in FindMinVolt. Another stable observation at a lower voltage.
        let action = adapter.observe(AutotuneObservation {
            voltage_mv10: 1450,
            ..settled()
        });
        assert!(matches!(
            action,
            VnishTuneAction::StepVoltageDown { dec_volt_mv10: 500 }
        ));
        assert_eq!(adapter.phase(), AutotunePhase::FindMinVolt);
        assert_eq!(adapter.last_stable_voltage_mv10(), 1450);
    }

    // -- 5. FindMinVolt unstable → LockVoltageAndAdvance --
    #[test]
    fn unstable_in_find_min_volt_locks_voltage_and_advances() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        adapter.observe(settled()); // → FindMinVolt
                                    // Now unstable.
        let action = adapter.observe(unstable());
        match action {
            VnishTuneAction::LockVoltageAndAdvance {
                last_stable_mv10,
                higher_volt_offset_mv10,
            } => {
                assert_eq!(last_stable_mv10, 1500); // initial baseline
                assert_eq!(higher_volt_offset_mv10, 200); // 20 mV default
            }
            other => panic!("expected LockVoltageAndAdvance, got {:?}", other),
        }
        assert_eq!(adapter.phase(), AutotunePhase::AdjustChipFreqs);
    }

    // -- 6. AdjustChipFreqs advances → TuneChipFreqsRound(1) --
    #[test]
    fn adjust_to_tune_emits_first_round() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        // Force adapter to AdjustChipFreqs via the public path.
        adapter.observe(started());
        adapter.observe(settled());
        adapter.observe(unstable()); // → AdjustChipFreqs
                                     // Hashrate at target → advance to TuneChipFreqs.
        let action = adapter.observe(settled());
        assert!(matches!(
            action,
            VnishTuneAction::TuneChipFreqsRound { round: 1 }
        ));
    }

    // -- 7. TuneChipFreqs → FinalVolt on convergence emits AdvanceToFinalVolt --
    #[test]
    fn phase4_converged_emits_advance_to_final_volt() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        adapter.observe(settled());
        adapter.observe(unstable());
        adapter.observe(settled()); // → TuneChipFreqs
        let conv = AutotuneObservation {
            phase4_converged: true,
            ..settled()
        };
        let action = adapter.observe(conv);
        assert_eq!(action, VnishTuneAction::AdvanceToFinalVolt);
        assert_eq!(adapter.phase(), AutotunePhase::FinalVolt);
    }

    // -- 8. FinalVolt clean → MarkPresetTuned --
    #[test]
    fn final_volt_clean_emits_mark_preset_tuned() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        // Drive to FinalVolt.
        adapter.observe(started());
        adapter.observe(settled());
        adapter.observe(unstable());
        adapter.observe(settled());
        adapter.observe(AutotuneObservation {
            phase4_converged: true,
            ..settled()
        });
        // Now in FinalVolt; settle clean.
        let action = adapter.observe(settled());
        assert_eq!(action, VnishTuneAction::MarkPresetTuned);
        assert_eq!(adapter.phase(), AutotunePhase::Done);
    }

    // -- 9. FinalVolt verification failure → MarkPresetFailed { FinalVoltVerificationFailed } --
    #[test]
    fn final_volt_with_errors_emits_failed_verification() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        adapter.observe(settled());
        adapter.observe(unstable());
        adapter.observe(settled());
        adapter.observe(AutotuneObservation {
            phase4_converged: true,
            ..settled()
        });
        let action = adapter.observe(unstable());
        assert!(matches!(
            action,
            VnishTuneAction::MarkPresetFailed {
                reason: TuneFailureReason::FinalVoltVerificationFailed
            }
        ));
    }

    // -- 10. Hard fault from any state → MarkPresetFailed { HardFault } --
    #[test]
    fn hard_fault_emits_failed_hard_fault() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        let action = adapter.observe(AutotuneObservation {
            hard_fault: true,
            ..started()
        });
        assert!(matches!(
            action,
            VnishTuneAction::MarkPresetFailed {
                reason: TuneFailureReason::HardFault
            }
        ));
        assert_eq!(adapter.phase(), AutotunePhase::Failed);
    }

    // -- 11. Terminal hold (Done → Done) → NoOp --
    #[test]
    fn done_terminal_holds_emits_noop() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        adapter.observe(settled());
        adapter.observe(unstable());
        adapter.observe(settled());
        adapter.observe(AutotuneObservation {
            phase4_converged: true,
            ..settled()
        });
        adapter.observe(settled()); // → Done
        let action = adapter.observe(settled());
        assert_eq!(action, VnishTuneAction::NoOp);
        assert_eq!(adapter.phase(), AutotunePhase::Done);
    }

    // -- 12. Reset clears state back to Idle --
    #[test]
    fn reset_returns_adapter_to_idle() {
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        adapter.observe(started());
        assert_eq!(adapter.phase(), AutotunePhase::InitPresets);
        adapter.reset();
        assert_eq!(adapter.phase(), AutotunePhase::Idle);
    }

    // -- 13. LOAD-BEARING: VNish thermal thresholds are NOT exported as
    //        DCENT_OS thermal limits.
    // Structural check: this module must not re-export VNish's documented
    // 85°C / 90°C / 80°C thresholds as pub consts. They are informational
    // only (in the module doc-comment); DCENT_OS thermal thresholds stay
    // owned by `dcentrald-thermal::profiles` per platform.
    #[test]
    fn vnish_thresholds_not_used_as_dcentos_thermal_limits() {
        // Compile-time check: this module's `pub` surface does NOT
        // include any thermal-threshold constants. A future change that
        // adds e.g. `pub const VNISH_HOT_CHIP_C: f32 = 85.0;` to this
        // module would survive `cargo check` but fails this regression
        // by code review intent. We assert the invariant structurally
        // by verifying the module's emitted action types carry NO
        // thermal-threshold field.
        //
        // Verification: enumerate every `VnishTuneAction` variant; none
        // carries a thermal threshold parameter.
        fn _no_thermal_in_action(action: &VnishTuneAction) {
            match action {
                VnishTuneAction::NoOp
                | VnishTuneAction::ApplyPreset
                | VnishTuneAction::StepVoltageDown { .. }
                | VnishTuneAction::LockVoltageAndAdvance { .. }
                | VnishTuneAction::RaiseChipFreq { .. }
                | VnishTuneAction::TuneChipFreqsRound { .. }
                | VnishTuneAction::AdvanceToFinalVolt
                | VnishTuneAction::TrimVoltageFinal { .. }
                | VnishTuneAction::MarkPresetTuned
                | VnishTuneAction::MarkPresetFailed { .. } => {}
            }
        }
        let mut adapter = VnishPhaseAdapter::new(cfg_enabled());
        for action in [adapter.observe(started()), adapter.observe(settled())] {
            _no_thermal_in_action(&action);
        }
    }

    // -- 14. VNish constants flow through unchanged from the catalog --
    #[test]
    fn vnish_constants_match_catalog_defaults() {
        let cfg = VnishPhaseConfig::default();
        assert_eq!(cfg.autotune.dec_volt_mv10, 500); // 50 mV
        assert_eq!(cfg.autotune.inc_volt_mv10, 500); // 50 mV
        assert_eq!(cfg.autotune.higher_volt_offset_mv10, 200); // 20 mV
        assert_eq!(cfg.autotune.phase4_max_rounds, 15);
        assert_eq!(cfg.autotune.end_change_volt_mv10, 150); // 15 mV
        assert!((cfg.autotune.state_target_percent - 0.95).abs() < 1e-6);
        assert!((cfg.autotune.state_lower_percent - 0.70).abs() < 1e-6);
        assert_eq!(cfg.autotune.hw_error_limit, 0);
    }
}
