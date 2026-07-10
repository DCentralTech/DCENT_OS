//!  tune-G — VNish 5-phase autotuner state machine (HAL-free).
//!
//! Source RE evidence:
//!
//! ALGO 2 (lines 76-152). State and constant names extracted from VNish
//! 1.2.7 `hwscan` Rust binary symbol table.
//!
//! Five-phase pipeline:
//! 1. **InitPresets** — apply preset's `(freq, voltage, target_hashrate)`
//!    and settle for `state_time` seconds.
//! 2. **FindMinVolt** — step DOWN voltage in `dec_volt` increments while
//!    stable; once unstable, snap UP and lock in
//!    `last_stable + higher_volt_offset`.
//! 3. **AdjustChipFreqs** — raise each chip's freq while
//!    `chip_HW_errors == 0`. Strong chips end higher.
//! 4. **TuneChipFreqs** — fine-tune per-chip freqs based on observed
//!    error rate. Up to `max_rounds` (default 15) of micro-adjustments.
//!    Exit when no chip changed across 2 consecutive rounds.
//! 5. **FinalVolt** — trim voltage UP by `end_change_volt` for safety
//!    margin; run for `end_inc_volt` window to verify.
//!
//! HAL-free pure state machine. The runtime adapter feeds observations
//! and reads back the next phase.
//!
//! Distinct from  GDTUNER (BraiinsOS) and  atm_stepper
//! (LuxOS). This is the VNish flavour.

use serde::{Deserialize, Serialize};

/// Discrete autotuner phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutotunePhase {
    /// Pre-tune state — autotuner not yet started.
    Idle,
    /// Phase 1 — apply preset; settle.
    InitPresets,
    /// Phase 2 — step voltage DOWN until unstable, then back up.
    FindMinVolt,
    /// Phase 3 — raise per-chip freqs while error-free.
    AdjustChipFreqs,
    /// Phase 4 — fine-tune per-chip freqs based on error rate.
    TuneChipFreqs,
    /// Phase 5 — trim voltage UP for safety margin; verify.
    FinalVolt,
    /// Tuning complete; persist results.
    Done,
    /// Hard fault — operator intervention required.
    Failed,
}

/// Per-tick observation fed by the runtime adapter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutotuneObservation {
    /// Operator triggered the autotune run.
    pub operator_started: bool,
    /// Current chain voltage in mV*10 (matches VNish `modded_max_volt`
    /// unit: 1640 = 16.40 V).
    pub voltage_mv10: u32,
    /// Sum of HW errors across all chips on the chain since last tick.
    pub hw_errors_sum: u32,
    /// Current total chain hashrate as a fraction of preset target
    /// (e.g. 0.95 = 95 %).
    pub hashrate_ratio: f32,
    /// True when Phase 4 has converged (no chip changed across 2
    /// consecutive rounds OR max_rounds reached).
    pub phase4_converged: bool,
    /// Whether the runtime adapter completed the timed-wait portion of
    /// the current phase (caller threads time; module is clock-free).
    pub timed_wait_done: bool,
    /// Hard-fault signal (e.g. thermal panic, voltage out-of-range).
    pub hard_fault: bool,
}

impl AutotuneObservation {
    pub const fn empty() -> Self {
        Self {
            operator_started: false,
            voltage_mv10: 0,
            hw_errors_sum: 0,
            hashrate_ratio: 0.0,
            phase4_converged: false,
            timed_wait_done: false,
            hard_fault: false,
        }
    }
}

// ---------------------------------------------------------------------------
// VnishPhaseAdapter observe-only shadow input helpers (HAL-free, host-tested)
// ---------------------------------------------------------------------------

/// Stateful cumulative→delta converter for the chain HW-error count.
///
/// [`AutotuneObservation::hw_errors_sum`] is documented as "sum of HW errors
/// across all chips on the chain **since last tick**", but the live
/// `MinerState` per-chain `errors` field is a *cumulative* CRC-error counter.
/// This pure helper bridges the two: feed it the current cumulative total each
/// tick and it returns the delta since the previous tick.
///
/// It lives here in the no-HAL api-types crate (not the daemon) so it is pure
/// and host-testable: it holds NO hardware handle, does NO I/O, and reads NO
/// clock — the caller supplies the cumulative total each observation. A counter
/// that goes *backwards* (chain re-enumerated / counters reset on a fresh init)
/// yields a `0` delta rather than a giant bogus spike, mirroring the same
/// anti-spurious discipline as
/// [`crate::braiinsos_dps_configuration::SustainedBelowHotCounter`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HwErrorDeltaCounter {
    /// Last cumulative total observed, or `None` before the first sample.
    last_cumulative: Option<u32>,
}

impl HwErrorDeltaCounter {
    /// New counter; no prior sample.
    pub fn new() -> Self {
        Self {
            last_cumulative: None,
        }
    }

    /// Feed the current cumulative HW-error total and return the delta since
    /// the previous tick. The first observation always returns `0` (no prior
    /// baseline), and a decrease (counter reset) also returns `0`.
    pub fn observe(&mut self, cumulative: u32) -> u32 {
        let delta = match self.last_cumulative {
            Some(prev) => cumulative.saturating_sub(prev),
            None => 0,
        };
        self.last_cumulative = Some(cumulative);
        delta
    }
}

/// Compute the VNish FSM `hashrate_ratio` (current / preset-target) as a pure
/// fraction, clamped to be non-negative.
///
/// [`AutotuneObservation::hashrate_ratio`] is "current total chain hashrate as
/// a fraction of preset target". An observe-only shadow derives `current_ths`
/// from the live `MinerState.hashrate_ghs` and `target_ths` from the
/// operator-configured target (or a self-referenced baseline when no target is
/// configured). This helper just does the safe division: a non-positive or
/// non-finite target yields `0.0` (treated by the FSM as "below target", the
/// no-spurious-advance reading for a shadow), and a non-finite current also
/// yields `0.0`.
pub fn hashrate_ratio(current_ths: f64, target_ths: f64) -> f32 {
    if !current_ths.is_finite() || !target_ths.is_finite() || target_ths <= 0.0 {
        return 0.0;
    }
    (current_ths / target_ths).max(0.0) as f32
}

/// VNish-canonical autotuner constants per RE doc lines 126-145.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutotuneConfig {
    /// Per-state settle time in seconds (typ 30-60).
    pub state_time_s: u32,
    /// Maximum HW errors tolerated per round (0 = strict).
    pub hw_error_limit: u32,
    /// Voltage step DOWN per Phase 2 iteration in mV*10 units.
    pub dec_volt_mv10: u32,
    /// Voltage step UP after Phase 2 hits unstable, in mV*10 units.
    pub inc_volt_mv10: u32,
    /// Final safety margin added on top of `last_stable` after Phase 2,
    /// in mV*10 units.
    pub higher_volt_offset_mv10: u32,
    /// Hashrate-target percent (success threshold).
    pub state_target_percent: f32,
    /// Lower hashrate gate (kick chip to next state).
    pub state_lower_percent: f32,
    /// Phase 4 max rounds.
    pub phase4_max_rounds: u32,
    /// Phase 5 final voltage trim in mV*10 units (typ 10-20 mV).
    pub end_change_volt_mv10: u32,
}

impl Default for AutotuneConfig {
    fn default() -> Self {
        Self {
            state_time_s: 30,
            hw_error_limit: 0,
            dec_volt_mv10: 500, // 50 mV
            inc_volt_mv10: 500,
            higher_volt_offset_mv10: 200, // 20 mV
            state_target_percent: 0.95,
            state_lower_percent: 0.70,
            phase4_max_rounds: 15,
            end_change_volt_mv10: 150, // 15 mV
        }
    }
}

/// State machine. One per autotune run.
#[derive(Debug, Clone)]
pub struct AutotuneFsm {
    phase: AutotunePhase,
    config: AutotuneConfig,
    /// Last-known stable voltage observed during Phase 2.
    last_stable_voltage_mv10: u32,
    /// Number of Phase 4 rounds completed.
    phase4_round: u32,
}

impl AutotuneFsm {
    pub fn new(config: AutotuneConfig) -> Self {
        Self {
            phase: AutotunePhase::Idle,
            config,
            last_stable_voltage_mv10: 0,
            phase4_round: 0,
        }
    }

    pub fn fresh() -> Self {
        Self::new(AutotuneConfig::default())
    }

    pub fn phase(&self) -> AutotunePhase {
        self.phase
    }

    pub fn config(&self) -> &AutotuneConfig {
        &self.config
    }

    pub fn last_stable_voltage_mv10(&self) -> u32 {
        self.last_stable_voltage_mv10
    }

    pub fn phase4_round(&self) -> u32 {
        self.phase4_round
    }

    /// Reset back to Idle.
    pub fn reset(&mut self) {
        self.phase = AutotunePhase::Idle;
        self.last_stable_voltage_mv10 = 0;
        self.phase4_round = 0;
    }

    /// Mark fault (operator must reset to clear).
    pub fn mark_fault(&mut self) {
        self.phase = AutotunePhase::Failed;
    }

    /// Feed one observation. Returns the new phase.
    pub fn feed(&mut self, obs: AutotuneObservation) -> AutotunePhase {
        if obs.hard_fault {
            self.phase = AutotunePhase::Failed;
            return self.phase;
        }
        match self.phase {
            AutotunePhase::Idle => {
                if obs.operator_started {
                    self.phase = AutotunePhase::InitPresets;
                }
            }
            AutotunePhase::InitPresets => {
                if obs.timed_wait_done {
                    // Capture initial voltage as our first "stable" baseline.
                    self.last_stable_voltage_mv10 = obs.voltage_mv10;
                    self.phase = AutotunePhase::FindMinVolt;
                }
            }
            AutotunePhase::FindMinVolt => {
                if !obs.timed_wait_done {
                    return self.phase;
                }
                let hashrate_ok = obs.hashrate_ratio >= self.config.state_target_percent;
                let errors_ok = obs.hw_errors_sum <= self.config.hw_error_limit;
                if hashrate_ok && errors_ok {
                    // Stable at this voltage; record and prepare to step
                    // down further. Runtime adapter handles the actual
                    // voltage-set; we just track state.
                    self.last_stable_voltage_mv10 = obs.voltage_mv10;
                } else {
                    // Unstable — snap back to last_stable + offset and
                    // advance to Phase 3.
                    self.phase = AutotunePhase::AdjustChipFreqs;
                }
            }
            AutotunePhase::AdjustChipFreqs => {
                if !obs.timed_wait_done {
                    return self.phase;
                }
                // Phase 3 raises per-chip freq while error-free. The
                // runtime adapter does the per-chip iteration; we
                // transition out when it reports back.
                if obs.hw_errors_sum > self.config.hw_error_limit
                    || obs.hashrate_ratio >= self.config.state_target_percent
                {
                    self.phase = AutotunePhase::TuneChipFreqs;
                    self.phase4_round = 0;
                }
            }
            AutotunePhase::TuneChipFreqs => {
                if obs.phase4_converged || self.phase4_round >= self.config.phase4_max_rounds {
                    self.phase = AutotunePhase::FinalVolt;
                } else if obs.timed_wait_done {
                    self.phase4_round = self.phase4_round.saturating_add(1);
                }
            }
            AutotunePhase::FinalVolt => {
                if obs.timed_wait_done && obs.hw_errors_sum <= self.config.hw_error_limit {
                    self.phase = AutotunePhase::Done;
                } else if obs.timed_wait_done && obs.hw_errors_sum > self.config.hw_error_limit {
                    // Errors during final-volt verify → run failed.
                    self.phase = AutotunePhase::Failed;
                }
            }
            AutotunePhase::Done | AutotunePhase::Failed => {
                // Terminal until reset().
            }
        }
        self.phase
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fresh_starts_idle() {
        let fsm = AutotuneFsm::fresh();
        assert_eq!(fsm.phase(), AutotunePhase::Idle);
    }

    #[test]
    fn operator_start_drives_idle_to_init_presets() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.feed(started());
        assert_eq!(fsm.phase(), AutotunePhase::InitPresets);
    }

    #[test]
    fn init_presets_advances_to_find_min_volt_after_settle() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.feed(started());
        fsm.feed(settled());
        assert_eq!(fsm.phase(), AutotunePhase::FindMinVolt);
        assert_eq!(fsm.last_stable_voltage_mv10(), 1500);
    }

    #[test]
    fn find_min_volt_records_stable_voltage_each_tick() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.feed(started());
        fsm.feed(settled());
        // Now in FindMinVolt. Step voltage down → still stable.
        fsm.feed(AutotuneObservation {
            voltage_mv10: 1450,
            ..settled()
        });
        assert_eq!(fsm.phase(), AutotunePhase::FindMinVolt);
        assert_eq!(fsm.last_stable_voltage_mv10(), 1450);
    }

    #[test]
    fn find_min_volt_advances_when_unstable() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.feed(started());
        fsm.feed(settled());
        fsm.feed(unstable());
        assert_eq!(fsm.phase(), AutotunePhase::AdjustChipFreqs);
    }

    #[test]
    fn adjust_chip_freqs_advances_to_tune_chip_freqs_on_settle() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::AdjustChipFreqs;
        // Hashrate at target → advance.
        fsm.feed(settled());
        assert_eq!(fsm.phase(), AutotunePhase::TuneChipFreqs);
        assert_eq!(fsm.phase4_round(), 0);
    }

    #[test]
    fn tune_chip_freqs_increments_round_each_settle_tick() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::TuneChipFreqs;
        fsm.feed(settled());
        assert_eq!(fsm.phase4_round(), 1);
        fsm.feed(settled());
        assert_eq!(fsm.phase4_round(), 2);
    }

    #[test]
    fn tune_chip_freqs_advances_when_converged() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::TuneChipFreqs;
        let conv = AutotuneObservation {
            phase4_converged: true,
            ..settled()
        };
        fsm.feed(conv);
        assert_eq!(fsm.phase(), AutotunePhase::FinalVolt);
    }

    #[test]
    fn tune_chip_freqs_advances_when_max_rounds_reached() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::TuneChipFreqs;
        fsm.phase4_round = AutotuneConfig::default().phase4_max_rounds;
        fsm.feed(settled());
        assert_eq!(fsm.phase(), AutotunePhase::FinalVolt);
    }

    #[test]
    fn final_volt_clean_run_advances_to_done() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::FinalVolt;
        fsm.feed(settled());
        assert_eq!(fsm.phase(), AutotunePhase::Done);
    }

    #[test]
    fn final_volt_with_errors_drives_to_failed() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::FinalVolt;
        fsm.feed(unstable());
        assert_eq!(fsm.phase(), AutotunePhase::Failed);
    }

    #[test]
    fn hard_fault_drives_any_state_to_failed() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::TuneChipFreqs;
        let bad = AutotuneObservation {
            hard_fault: true,
            ..settled()
        };
        fsm.feed(bad);
        assert_eq!(fsm.phase(), AutotunePhase::Failed);
    }

    #[test]
    fn done_is_terminal_until_reset() {
        let mut fsm = AutotuneFsm::fresh();
        fsm.phase = AutotunePhase::Done;
        for _ in 0..5 {
            fsm.feed(settled());
        }
        assert_eq!(fsm.phase(), AutotunePhase::Done);
        fsm.reset();
        assert_eq!(fsm.phase(), AutotunePhase::Idle);
    }

    #[test]
    fn config_default_locks_in_vnish_constants() {
        // Per RE doc lines 126-145.
        let cfg = AutotuneConfig::default();
        assert_eq!(cfg.state_time_s, 30);
        assert_eq!(cfg.dec_volt_mv10, 500); // 50 mV
        assert_eq!(cfg.higher_volt_offset_mv10, 200); // 20 mV
        assert!((cfg.state_target_percent - 0.95).abs() < 1e-6);
        assert!((cfg.state_lower_percent - 0.70).abs() < 1e-6);
        assert_eq!(cfg.phase4_max_rounds, 15);
    }

    // -----------------------------------------------------------------
    // VnishPhaseAdapter observe-only shadow input helpers
    // -----------------------------------------------------------------

    #[test]
    fn hw_error_delta_first_sample_is_zero_then_deltas() {
        let mut c = HwErrorDeltaCounter::new();
        // First observation has no prior baseline → 0 (don't treat the whole
        // cumulative count as one tick's errors).
        assert_eq!(c.observe(100), 0);
        // Subsequent observations return the delta since the previous tick.
        assert_eq!(c.observe(105), 5);
        assert_eq!(c.observe(105), 0);
        assert_eq!(c.observe(110), 5);
    }

    #[test]
    fn hw_error_delta_counter_reset_yields_zero_not_spike() {
        // Chain re-enumeration / counter reset (cumulative goes backwards)
        // must NOT produce a giant bogus delta — it returns 0 (saturating_sub).
        let mut c = HwErrorDeltaCounter::new();
        assert_eq!(c.observe(1_000), 0);
        assert_eq!(c.observe(10), 0); // reset, not a +negative spike
                                      // re-accrues from the new baseline.
        assert_eq!(c.observe(13), 3);
    }

    #[test]
    fn hashrate_ratio_basic_and_edge_cases() {
        // Normal: 95 of 100 TH/s → 0.95.
        assert!((hashrate_ratio(95.0, 100.0) - 0.95).abs() < 1e-6);
        // Over target is allowed (>1.0) — used by FindMinVolt "stable" check.
        assert!((hashrate_ratio(110.0, 100.0) - 1.10).abs() < 1e-6);
        // Zero / non-positive target → 0.0 (treated as below target — the
        // no-spurious-advance reading for an observe-only shadow).
        assert_eq!(hashrate_ratio(95.0, 0.0), 0.0);
        assert_eq!(hashrate_ratio(95.0, -1.0), 0.0);
        // Non-finite inputs → 0.0.
        assert_eq!(hashrate_ratio(f64::NAN, 100.0), 0.0);
        assert_eq!(hashrate_ratio(95.0, f64::INFINITY), 0.0);
        // Negative current clamps to 0.0 (never negative ratio).
        assert_eq!(hashrate_ratio(-5.0, 100.0), 0.0);
    }

    #[test]
    fn hashrate_ratio_feeds_fsm_target_percent_gate() {
        // Wire the helper to the FSM's default 95% target gate: a 96%-of-target
        // observation must satisfy `hashrate_ratio >= state_target_percent`.
        let cfg = AutotuneConfig::default();
        let ratio = hashrate_ratio(96.0, 100.0);
        assert!(ratio >= cfg.state_target_percent);
        // A 70%-of-target observation must NOT satisfy the gate.
        let low = hashrate_ratio(70.0, 100.0);
        assert!(low < cfg.state_target_percent);
    }

    #[test]
    fn phase_round_trips_through_serde() {
        for p in [
            AutotunePhase::Idle,
            AutotunePhase::InitPresets,
            AutotunePhase::FindMinVolt,
            AutotunePhase::AdjustChipFreqs,
            AutotunePhase::TuneChipFreqs,
            AutotunePhase::FinalVolt,
            AutotunePhase::Done,
            AutotunePhase::Failed,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: AutotunePhase = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }
}
