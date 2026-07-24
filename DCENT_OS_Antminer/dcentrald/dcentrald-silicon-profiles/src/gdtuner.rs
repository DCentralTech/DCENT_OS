//!  tune-A — BraiinsOS GDTUNER state machine port (HAL-free).
//!
//! Source RE evidence: .
//!
//! GDTUNER is bosminer's per-chain frequency tuner. It operates in five
//! sequential stages, each with a specific goal and exit criterion. The
//! state machine sits underneath dcentrald-autotuner's higher-level scheduler
//! and operates on **a single chain at a time**:
//!
//! ```text
//!   Bootstrap → Stage2 → Stage3 → Stage8 → Stage10 → Stage13 → Done
//!     |          |        |        |         |          |
//!   discover    nominal  fast     slow      thermal    final
//!   silicon     freq     ramp     ramp      bias       commit
//! ```
//!
//! This module is **pure logic, no HAL**: it owns transition rules and
//! exit-criteria predicates. The runtime adapter inside `dcentrald-autotuner`
//! drives the state machine by feeding samples (hashrate, error rate, temp)
//! and reading back the next target frequency.
//!
//! State semantics:
//! - **Bootstrap**: emit a low-watts probing frequency, observe nonces
//!   to confirm the chain is alive; transition to Stage2 once the first
//!   `min_alive_samples` produce shares.
//! - **Stage2**: ramp to nominal target frequency in coarse +25 MHz hops,
//!   transition out when chain hashrate within 5 % of expected nameplate.
//! - **Stage3**: stress test at nominal, observe error rate, transition
//!   out when error rate < 5 % over `error_window_samples` samples.
//! - **Stage8**: slow per-chip frequency search; transition out when
//!   each chip has converged within ±1 step of its individual sweet spot.
//! - **Stage10**: apply thermal bias to the per-chip targets based on
//!   chip temperature; transition out when all chips are within thermal
//!   tolerance.
//! - **Stage13**: emit the final per-chip frequency vector, persist it,
//!   transition to Done.
//!
//! `Done` is terminal. The autotuner scheduler triggers a re-run by
//! resetting to Bootstrap (e.g. after a chain reset or thermal event).

use serde::{Deserialize, Serialize};

/// Discrete GDTUNER stage labels. Order matters: each stage transitions
/// only to the next stage in this list (or `Failed` on hard error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GdtunerStage {
    /// Initial chain probe at low watts.
    Bootstrap,
    /// Coarse frequency ramp toward nominal.
    Stage2,
    /// Stress test at nominal — measure error rate.
    Stage3,
    /// Per-chip slow frequency search.
    Stage8,
    /// Thermal bias application.
    Stage10,
    /// Final per-chip frequency commit.
    Stage13,
    /// Tuning complete; emit nothing further until reset.
    Done,
    /// Hard error — chain or chip irrecoverable. Caller decides recovery
    /// (retry from Bootstrap, mark chain dead, etc.).
    Failed,
}

/// Per-tick sample fed into the state machine. All fields are nominal
/// observables; the state machine never needs HAL access.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GdtunerSample {
    /// Current hashrate as a fraction of nameplate target (0.0..=1.0+).
    /// Above 1.0 is allowed — it means the chip is over-clocking the target.
    pub hashrate_ratio: f32,
    /// Hardware error rate (0.0..=1.0).
    pub error_rate: f32,
    /// Highest chip temperature on the chain in °C.
    pub max_chip_temp_c: f32,
    /// Whether ANY share was accepted by the pool since the last sample.
    pub any_share_accepted: bool,
}

/// Tuning thresholds. All values copied from BOSMINER_AUTOTUNER_RE.md unless
/// noted. Override via `GdtunerConfig::with_*` for tests or per-platform
/// adjustments.
#[derive(Debug, Clone, Copy)]
pub struct GdtunerConfig {
    /// Bootstrap → Stage2: minimum consecutive samples reporting an
    /// accepted share before promoting.
    pub bootstrap_min_alive_samples: u32,
    /// Stage2 → Stage3: hashrate must reach at least this fraction of
    /// nameplate before promoting (e.g. 0.95 = 95 %).
    pub stage2_hashrate_ratio_target: f32,
    /// Stage3 → Stage8: error rate must stay below this for
    /// `error_window_samples` samples.
    pub stage3_max_error_rate: f32,
    /// Stage3 → Stage8: window length in samples.
    pub error_window_samples: u32,
    /// Stage8 → Stage10: per-chip step convergence tolerance.
    pub stage8_chip_step_tolerance: i32,
    /// Stage10 → Stage13: thermal tolerance in °C.
    pub stage10_thermal_tolerance_c: f32,
    /// Hard fail: any chip exceeding this temp at any time aborts tuning.
    pub hard_thermal_limit_c: f32,
}

impl Default for GdtunerConfig {
    fn default() -> Self {
        Self {
            bootstrap_min_alive_samples: 3,
            stage2_hashrate_ratio_target: 0.95,
            stage3_max_error_rate: 0.05,
            error_window_samples: 30,
            stage8_chip_step_tolerance: 1,
            stage10_thermal_tolerance_c: 75.0,
            hard_thermal_limit_c: 88.0,
        }
    }
}

/// GDTUNER state machine. One instance per chain.
#[derive(Debug, Clone)]
pub struct Gdtuner {
    stage: GdtunerStage,
    config: GdtunerConfig,
    /// Counters that drive transitions. The state machine is one-shot —
    /// each stage records its own progress in these counters and resets
    /// them on transition.
    bootstrap_alive_count: u32,
    stage3_clean_count: u32,
    /// Samples fed since last reset; used for diagnostics only.
    samples_total: u64,
}

impl Gdtuner {
    pub fn new(config: GdtunerConfig) -> Self {
        Self {
            stage: GdtunerStage::Bootstrap,
            config,
            bootstrap_alive_count: 0,
            stage3_clean_count: 0,
            samples_total: 0,
        }
    }

    /// Default config, Bootstrap stage.
    pub fn fresh() -> Self {
        Self::new(GdtunerConfig::default())
    }

    pub fn stage(&self) -> GdtunerStage {
        self.stage
    }

    pub fn samples_total(&self) -> u64 {
        self.samples_total
    }

    /// Reset to Bootstrap, e.g. after a chain reset or thermal event.
    pub fn reset(&mut self) {
        self.stage = GdtunerStage::Bootstrap;
        self.bootstrap_alive_count = 0;
        self.stage3_clean_count = 0;
        // Don't reset samples_total — it's a lifetime counter.
    }

    /// Mark tuning failed. Once Failed, only `reset()` clears it.
    pub fn mark_failed(&mut self) {
        self.stage = GdtunerStage::Failed;
    }

    /// Feed one sample, possibly transitioning to the next stage.
    /// Returns the new stage (caller can compare to previous to detect
    /// transitions).
    pub fn feed(&mut self, sample: GdtunerSample) -> GdtunerStage {
        self.samples_total += 1;

        // Hard thermal limit: any stage aborts tuning at this temp. Written as
        // `!(x < limit)` (not `x >= limit`) so a NaN max_chip_temp_c — a garbled /
        // failed sensor read — trips the abort fail-closed instead of slipping
        // through (`NaN >= limit` and `NaN < limit` are both false).
        if !(sample.max_chip_temp_c < self.config.hard_thermal_limit_c) {
            self.stage = GdtunerStage::Failed;
            return self.stage;
        }

        match self.stage {
            GdtunerStage::Bootstrap => {
                if sample.any_share_accepted {
                    self.bootstrap_alive_count += 1;
                } else {
                    // Resets on a "no share" sample — we want a streak.
                    self.bootstrap_alive_count = 0;
                }
                if self.bootstrap_alive_count >= self.config.bootstrap_min_alive_samples {
                    self.stage = GdtunerStage::Stage2;
                    self.bootstrap_alive_count = 0;
                }
            }
            GdtunerStage::Stage2 => {
                if sample.hashrate_ratio >= self.config.stage2_hashrate_ratio_target {
                    self.stage = GdtunerStage::Stage3;
                    self.stage3_clean_count = 0;
                }
            }
            GdtunerStage::Stage3 => {
                if sample.error_rate <= self.config.stage3_max_error_rate {
                    self.stage3_clean_count += 1;
                } else {
                    self.stage3_clean_count = 0;
                }
                if self.stage3_clean_count >= self.config.error_window_samples {
                    self.stage = GdtunerStage::Stage8;
                    self.stage3_clean_count = 0;
                }
            }
            GdtunerStage::Stage8 => {
                // Stage8 convergence is per-chip and lives in the runtime
                // adapter; the state machine itself just waits for the
                // adapter to call `advance_to(Stage10)` when chip-step
                // convergence is reached. We stay in Stage8 here.
            }
            GdtunerStage::Stage10 => {
                if sample.max_chip_temp_c <= self.config.stage10_thermal_tolerance_c {
                    self.stage = GdtunerStage::Stage13;
                }
            }
            GdtunerStage::Stage13 => {
                self.stage = GdtunerStage::Done;
            }
            GdtunerStage::Done | GdtunerStage::Failed => {
                // Terminal. No further transitions until reset().
            }
        }
        self.stage
    }

    /// Adapter-driven explicit transition for Stage8 (per-chip convergence
    /// is decided outside the state machine). Returns false if the current
    /// stage is not Stage8 or the target is not Stage10.
    pub fn advance_to(&mut self, target: GdtunerStage) -> bool {
        match (self.stage, target) {
            (GdtunerStage::Stage8, GdtunerStage::Stage10) => {
                self.stage = GdtunerStage::Stage10;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive_sample() -> GdtunerSample {
        GdtunerSample {
            hashrate_ratio: 0.5,
            error_rate: 0.0,
            max_chip_temp_c: 60.0,
            any_share_accepted: true,
        }
    }

    fn dead_sample() -> GdtunerSample {
        GdtunerSample {
            hashrate_ratio: 0.0,
            error_rate: 0.0,
            max_chip_temp_c: 50.0,
            any_share_accepted: false,
        }
    }

    #[test]
    fn fresh_starts_at_bootstrap() {
        let g = Gdtuner::fresh();
        assert_eq!(g.stage(), GdtunerStage::Bootstrap);
        assert_eq!(g.samples_total(), 0);
    }

    #[test]
    fn bootstrap_to_stage2_after_min_alive_samples() {
        let mut g = Gdtuner::fresh();
        // 2 samples — not enough.
        g.feed(alive_sample());
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Bootstrap);
        // 3rd sample crosses threshold.
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Stage2);
    }

    #[test]
    fn bootstrap_streak_resets_on_dead_sample() {
        let mut g = Gdtuner::fresh();
        g.feed(alive_sample());
        g.feed(alive_sample());
        g.feed(dead_sample()); // resets streak
        g.feed(alive_sample());
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Bootstrap);
        g.feed(alive_sample()); // 3rd in a row → promote
        assert_eq!(g.stage(), GdtunerStage::Stage2);
    }

    #[test]
    fn stage2_to_stage3_when_hashrate_target_met() {
        let mut g = Gdtuner::new(GdtunerConfig {
            bootstrap_min_alive_samples: 1,
            ..Default::default()
        });
        g.feed(alive_sample()); // Bootstrap → Stage2
        assert_eq!(g.stage(), GdtunerStage::Stage2);

        // 0.94 < 0.95 default threshold, no promotion.
        g.feed(GdtunerSample {
            hashrate_ratio: 0.94,
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage2);

        // Hit the target.
        g.feed(GdtunerSample {
            hashrate_ratio: 0.96,
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage3);
    }

    #[test]
    fn stage3_to_stage8_after_error_window_clean() {
        let mut g = Gdtuner::new(GdtunerConfig {
            bootstrap_min_alive_samples: 1,
            error_window_samples: 5,
            ..Default::default()
        });
        // Drive to Stage3.
        g.feed(alive_sample());
        g.feed(GdtunerSample {
            hashrate_ratio: 1.0,
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage3);

        // 4 clean samples — not enough.
        for _ in 0..4 {
            g.feed(alive_sample());
        }
        assert_eq!(g.stage(), GdtunerStage::Stage3);

        // 5th promotes.
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Stage8);
    }

    #[test]
    fn stage3_resets_on_dirty_sample() {
        let mut g = Gdtuner::new(GdtunerConfig {
            bootstrap_min_alive_samples: 1,
            error_window_samples: 3,
            ..Default::default()
        });
        g.feed(alive_sample());
        g.feed(GdtunerSample {
            hashrate_ratio: 1.0,
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage3);

        g.feed(alive_sample()); // 1
        g.feed(alive_sample()); // 2
        g.feed(GdtunerSample {
            error_rate: 0.10, // dirty — resets streak
            ..alive_sample()
        });
        g.feed(alive_sample()); // 1 of 3 again
        g.feed(alive_sample()); // 2
        assert_eq!(g.stage(), GdtunerStage::Stage3);
        g.feed(alive_sample()); // 3 → promote
        assert_eq!(g.stage(), GdtunerStage::Stage8);
    }

    #[test]
    fn stage8_only_advances_via_explicit_call() {
        let mut g = Gdtuner::new(GdtunerConfig {
            bootstrap_min_alive_samples: 1,
            error_window_samples: 1,
            ..Default::default()
        });
        // Drive to Stage8.
        g.feed(alive_sample());
        g.feed(GdtunerSample {
            hashrate_ratio: 1.0,
            ..alive_sample()
        });
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Stage8);

        // Feeding alone doesn't advance — adapter must call advance_to.
        for _ in 0..10 {
            g.feed(alive_sample());
        }
        assert_eq!(g.stage(), GdtunerStage::Stage8);

        assert!(g.advance_to(GdtunerStage::Stage10));
        assert_eq!(g.stage(), GdtunerStage::Stage10);

        // Wrong target → returns false, no transition.
        let mut g2 = g.clone();
        g2.stage = GdtunerStage::Stage8;
        assert!(!g2.advance_to(GdtunerStage::Done));
        assert_eq!(g2.stage(), GdtunerStage::Stage8);
    }

    #[test]
    fn stage10_to_stage13_when_thermals_within_tolerance() {
        let mut g = Gdtuner::fresh();
        g.stage = GdtunerStage::Stage10;
        g.feed(GdtunerSample {
            max_chip_temp_c: 76.0, // above default 75 tolerance
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage10);

        g.feed(GdtunerSample {
            max_chip_temp_c: 70.0,
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Stage13);
    }

    #[test]
    fn stage13_promotes_to_done_on_next_sample() {
        let mut g = Gdtuner::fresh();
        g.stage = GdtunerStage::Stage13;
        g.feed(alive_sample());
        assert_eq!(g.stage(), GdtunerStage::Done);
    }

    #[test]
    fn done_is_terminal() {
        let mut g = Gdtuner::fresh();
        g.stage = GdtunerStage::Done;
        for _ in 0..10 {
            g.feed(alive_sample());
        }
        assert_eq!(g.stage(), GdtunerStage::Done);
    }

    #[test]
    fn hard_thermal_limit_aborts_any_stage() {
        let mut g = Gdtuner::fresh();
        g.feed(GdtunerSample {
            max_chip_temp_c: 90.0, // above default 88 hard limit
            ..alive_sample()
        });
        assert_eq!(g.stage(), GdtunerStage::Failed);

        // Even from Stage8/10/13, hard-limit aborts.
        let mut g2 = Gdtuner::fresh();
        g2.stage = GdtunerStage::Stage10;
        g2.feed(GdtunerSample {
            max_chip_temp_c: 95.0,
            ..alive_sample()
        });
        assert_eq!(g2.stage(), GdtunerStage::Failed);
    }

    #[test]
    fn failed_only_clears_on_reset() {
        let mut g = Gdtuner::fresh();
        g.mark_failed();
        for _ in 0..10 {
            g.feed(alive_sample());
        }
        assert_eq!(g.stage(), GdtunerStage::Failed);
        g.reset();
        assert_eq!(g.stage(), GdtunerStage::Bootstrap);
    }

    #[test]
    fn samples_total_is_lifetime_counter() {
        let mut g = Gdtuner::fresh();
        for _ in 0..5 {
            g.feed(alive_sample());
        }
        assert_eq!(g.samples_total(), 5);
        g.reset();
        // reset() does NOT zero samples_total.
        assert_eq!(g.samples_total(), 5);
        g.feed(alive_sample());
        assert_eq!(g.samples_total(), 6);
    }
}
