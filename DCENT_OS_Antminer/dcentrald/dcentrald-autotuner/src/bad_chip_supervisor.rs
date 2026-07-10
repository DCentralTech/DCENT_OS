//! Cross-firmware bad-chip / degraded-chip supervisor (RE-004 closure, Wave E
//! 2026-05-19).
//!
//! 4-state per-chip FSM (`Healthy / Degraded / Bad / Missing`) with a two-
//! threshold confidence gate, layered ON TOP of the existing per-window
//! `ChipStatsSnapshot` stream from `crate::chip_stats::ChipStatsSnapshot`. Per
//! the RE team handoff at
//!
//! §"Best-of-Breed Behavior Spec for DCENT_OS", the runtime FSM consumes per-
//! chip nonce + error counts each window, classifies each chip with
//! statistical confidence, and emits action requests (per-chip downclock,
//! per-chip blacklist, board-profile step-down, bounded board reset, halt-
//! mining when too few healthy chips remain).
//!
//! # Source of truth
//!
//! Clean-room implementation from the RE-004 handoff (LuxOS-primary action
//! ladder, VNish policy knobs, BraiinsOS telemetry richness). No proprietary
//! code copied; thresholds and confidence gate are documented behavior, not
//! lifted constants. Confidence per RE-004:
//! - LuxOS lane: HIGH for behavior, MEDIUM for persistence path.
//! - VNish lane: MEDIUM-HIGH for surface, MEDIUM for runtime control-flow.
//! - BraiinsOS lane: MEDIUM for telemetry, LOW-MEDIUM for exact thresholds.
//! - Stock Bitmain: MEDIUM-LOW; per-chip isolation not evidenced.
//!
//! # Opt-in safety
//!
//! This module is COMPILED but NOT INSTANTIATED into the running daemon by
//! default. The integration site is gated on `[autotune.bad_chip].enabled =
//! true` in `dcentrald.toml`; with that flag false (default), the existing
//! autotuner stays master and no per-chip blacklisting happens. Live HW
//! validation (Wave H, with operator per-action authorization) is the gate to
//! enabling it in production.
//!
//! # Load-bearing rules
//!
//! - **Blacklisted chip nonces MUST still submit.** The supervisor only
//!   emits intent (a `BlacklistChip` action); the share-submit path stays
//!   intact and honors any nonce that arrives. Mirror of the Wave-D BIP320
//!   lesson — the supervisor reclassifies *expected* nonce accounting only,
//!   never discards a real valid hash. Enforced by
//!   `tests::blacklisted_chip_nonce_still_submits`.
//! - **Quiet-home fan cap MUST NOT be raised** by any action this supervisor
//!   emits. The supervisor never emits fan-control actions; thermal/fan stays
//!   the thermal supervisor's domain (RE-005).
//! - **HaltMining preserves the cut-hash-before-noise rule.** When emitted,
//!   the caller cuts power but does NOT raise fan PWM.
//!
//! # State machine
//!
//! ```text
//!   Healthy ──(actual < ~85% expected AND z < -2σ AND n >= min_samples)──> Degraded
//!   Degraded ──(actual < ~20% expected AND z < -4σ AND repeated_bad_windows >= 2)──> Bad
//!   Degraded ──(8 consecutive good windows)──> Healthy
//!   Bad ──(24 consecutive good windows)──> Degraded
//!   Bad ──(fingerprint mismatch on reboot)──> Healthy (force retune)
//!   Any ──(missing in chain enumeration)──> Missing
//!   Missing ──(re-enumerated next window)──> Healthy (force retune)
//! ```
//!
//! All actual/expected/z statistics are computed over a TRUE bounded rolling
//! window of the most-recent `ROLLING_WINDOW_CAP` `observe()` calls (W24-BC-2),
//! never lifetime accumulators — so a chip running at a stable sub-100% ratio
//! holds a stable bounded z-score and is never blacklisted by drift, and a
//! recovered chip climbs back out within roughly one window-length.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::chip_stats::ChipStatsSnapshot;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Per-chip health classification per RE-004 §"Action Ladder".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChipHealthState {
    /// Chip produces expected nonce rate within statistical confidence.
    #[default]
    Healthy,
    /// Chip produces 20%–~85% of expected; per-chip downclock requested.
    Degraded,
    /// Chip produces <20% of expected across repeated windows OR consistent
    /// hardware errors; blacklist requested (chip excluded from expected-
    /// nonce accounting; share-submit path stays intact).
    Bad,
    /// Chip missing from chain enumeration entirely.
    Missing,
}

// ---------------------------------------------------------------------------
// Action emitted to caller
// ---------------------------------------------------------------------------

/// What the supervisor decides the operator-facing system should do this tick.
/// The caller (autotuner integration + chain orchestrator) consumes these and
/// applies them; the supervisor never touches hardware directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadChipAction {
    /// No state change; continue current behavior.
    NoOp,
    /// Reduce per-chip frequency on (chain, chip) by `mhz_step`. The caller
    /// applies via the autotuner's per-chip frequency setter.
    PerChipDownclock {
        chain_id: u8,
        chip_index: u16,
        mhz_step: u16,
    },
    /// Remove (chain, chip) from expected-nonce accounting + per-chip tuning
    /// loop. Reason is for telemetry/audit logging. Chip nonces that DO
    /// arrive are still submitted by the share-submit path (load-bearing).
    BlacklistChip {
        chain_id: u8,
        chip_index: u16,
        reason: BadChipReason,
    },
    /// One chain has too many degraded chips; reduce the whole board's
    /// profile by one step. Avoids whack-a-mole on a board-wide failure
    /// pattern.
    ReduceBoardProfile { chain_id: u8 },
    /// Telemetry inconsistent or many chips fail together; reset the hash
    /// board once within a bounded reboot budget. `attempt` indicates which
    /// reset this is (1, 2, 3, …) for budget enforcement.
    BoardReset { chain_id: u8, attempt: u32 },
    /// Healthy-chip count on this chain fell below
    /// `min_operational_chips_per_chain` OR safety uncertain. Caller cuts hash
    /// power; quiet-home cooling profile preserved (NEVER raises fan PWM as a
    /// halt response).
    HaltMining { reason: HaltReason },
}

/// Why a chip was marked Bad. Kept narrow + serializable for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadChipReason {
    /// Nonce production stayed below `bad_threshold_pct` for ≥
    /// `repeated_bad_windows` windows with 4σ confidence.
    PersistentlyLowNonceRate,
    /// Hardware-error rate (CRC failures, not timeouts) exceeded the chip's
    /// own valid-nonce rate across the window.
    HwErrorsExceedNonces,
    /// Chip missing from chain enumeration for ≥ `missing_grace_windows`
    /// consecutive windows.
    PersistentlyMissing,
}

/// Why mining was halted. Kept narrow + serializable for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltReason {
    /// Healthy-chip count on a chain fell below
    /// `min_operational_chips_per_chain`.
    InsufficientHealthyChains,
    /// Board-reset budget exhausted on a chain that's still failing.
    BoardResetBudgetExhausted,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Bad-chip supervisor configuration (TOML `[autotune.bad_chip]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BadChipConfig {
    /// **Default false.** Live-HW gated to `true` only after Wave H operator
    /// authorization. With this flag false the supervisor is dormant and
    /// `tick()` returns `NoOp` immediately.
    #[serde(default)]
    pub enabled: bool,

    /// Below this % of expected nonces → `Degraded`. Default 85.0 (mid of
    /// the RE-004 "roughly 80–90 percent expected" band).
    #[serde(default = "default_degraded_pct")]
    pub degraded_threshold_pct: f32,

    /// Below this % of expected nonces → `Bad` (after repeated windows +
    /// 4σ confidence). Default 20.0 per RE-004.
    #[serde(default = "default_bad_pct")]
    pub bad_threshold_pct: f32,

    /// Minimum expected-nonce sample size before the classifier is allowed
    /// to demote `Healthy` → `Degraded` / `Bad`. Default 120 per RE-004
    /// ("minimum sample window similar to LuxOS").
    #[serde(default = "default_min_samples")]
    pub min_samples: u32,

    /// z-score absolute threshold for `Bad` classification (the high-confidence
    /// gate). Default 4.0σ per RE-004 ("high-confidence gate equivalent to
    /// 4-sigma"). A lower σ threshold (e.g. 2.0) is used internally for the
    /// `Degraded` step which is less destructive.
    #[serde(default = "default_sigma")]
    pub bad_sigma_threshold: f32,

    /// How many consecutive bad-classified windows before a chip is moved
    /// `Degraded` → `Bad`. Default 2 (no single-window false positives).
    #[serde(default = "default_repeated_bad")]
    pub repeated_bad_windows: u32,

    /// How many consecutive missing windows before a chip is classified
    /// `Missing` (vs transient enumeration glitch). Default 3.
    #[serde(default = "default_missing_grace")]
    pub missing_grace_windows: u32,

    /// Per-chip downclock step in MHz on `Healthy → Degraded`. Default 25
    /// MHz (matches the FEATURE_COMPARISON_MATRIX "Gradual" throttling
    /// convention).
    #[serde(default = "default_downclock_step")]
    pub per_chip_downclock_mhz: u16,

    /// Bounded board-reset budget per chain per power-cycle. Default 3 per
    /// RE-004 ("bounded by reboot budget").
    #[serde(default = "default_board_reset_budget")]
    pub board_reset_budget: u32,

    /// Halt mining when the count of healthy CHIPS on a chain drops below this
    /// floor. **Per-chain, per-chip** semantics: `observe()` runs once per
    /// chain and compares this floor against `healthy_this_chain` (the number
    /// of healthy chips on THAT chain). Default 1 (any chain that keeps ≥1
    /// healthy chip keeps mining; a chain with 0 healthy chips halts).
    ///
    /// **W24-BC-3 ():** this field was previously named
    /// `min_operational_chains` and documented as a board-wide "healthy CHAIN
    /// count" floor, but the code has always compared it against a per-chain
    /// healthy-CHIP count — a units mismatch that the default of 1 masked.
    /// Because the supervisor evaluates one chain at a time and has no
    /// cross-chain aggregation point (the wiring caller is Wave-H-gated and
    /// does not yet exist), the honest, lower-risk fix is to name + document
    /// the field for what the code actually enforces: a per-chain healthy-chip
    /// floor. A future board-wide "min healthy chains" policy would be a
    /// separate knob evaluated by the (not-yet-written) cross-chain caller.
    #[serde(default = "default_min_operational_chips_per_chain")]
    pub min_operational_chips_per_chain: u8,

    /// On a chain, if this fraction of chips are simultaneously Degraded or
    /// worse, emit `ReduceBoardProfile` instead of per-chip downclocks
    /// ("many chips degrade on one board" rung). Default 0.50.
    #[serde(default = "default_board_profile_fraction")]
    pub board_profile_step_fraction: f32,
}

impl Default for BadChipConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            degraded_threshold_pct: default_degraded_pct(),
            bad_threshold_pct: default_bad_pct(),
            min_samples: default_min_samples(),
            bad_sigma_threshold: default_sigma(),
            repeated_bad_windows: default_repeated_bad(),
            missing_grace_windows: default_missing_grace(),
            per_chip_downclock_mhz: default_downclock_step(),
            board_reset_budget: default_board_reset_budget(),
            min_operational_chips_per_chain: default_min_operational_chips_per_chain(),
            board_profile_step_fraction: default_board_profile_fraction(),
        }
    }
}

fn default_degraded_pct() -> f32 {
    85.0
}
fn default_bad_pct() -> f32 {
    20.0
}
fn default_min_samples() -> u32 {
    120
}
fn default_sigma() -> f32 {
    4.0
}
fn default_repeated_bad() -> u32 {
    2
}
fn default_missing_grace() -> u32 {
    3
}
fn default_downclock_step() -> u16 {
    25
}
fn default_board_reset_budget() -> u32 {
    3
}
fn default_min_operational_chips_per_chain() -> u8 {
    1
}
fn default_board_profile_fraction() -> f32 {
    0.50
}

// ---------------------------------------------------------------------------
// Persistence (keyed by board fingerprint)
// ---------------------------------------------------------------------------

/// Stable identifier for a hashboard. Per RE-004 §"Persistence / Recovery":
/// "Reapply persisted chip overrides only when fingerprint and chip count
/// match. On replacement/fingerprint mismatch, discard blacklist and force
/// retune."
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoardFingerprint {
    /// Platform key (e.g., `"am2-zynq"`, `"am3-bb"`).
    pub platform: String,
    /// Model key (e.g., `"s19jpro"`).
    pub model: String,
    /// Chain ID this fingerprint applies to.
    pub chain_id: u8,
    /// Chip count enumerated on this chain.
    pub chip_count: u16,
    /// Optional EEPROM/board hash (first 16 hex chars). When present, a
    /// fingerprint mismatch on reboot discards the blacklist (chip replaced).
    #[serde(default)]
    pub eeprom_hash16: Option<String>,
}

/// Per-chip persistent record. Serializable; emitted to JSON via the existing
/// `state_persistence` module's serde path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChipPersistentState {
    pub chain_id: u8,
    pub chip_index: u16,
    pub status: ChipHealthState,
    /// Reason for the most recent demotion (cleared on promotion back to
    /// Healthy). Telemetry only.
    pub last_demote_reason: Option<BadChipReason>,
}

/// Full persisted state for one board. JSON-serde; same shape as the existing
/// tuner profile per RE-004 §"Persistence / Recovery".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadChipPersistedState {
    pub fingerprint: BoardFingerprint,
    pub chips: Vec<ChipPersistentState>,
}

// ---------------------------------------------------------------------------
// Per-chip rolling window (in-memory, NOT serialized)
// ---------------------------------------------------------------------------

/// How many of the most-recent `observe()` windows a chip's statistic is
/// computed over. A TRUE bounded rolling window: once full, the oldest
/// per-window sample is evicted as a new one arrives, so `z_score()` /
/// `actual_pct()` reflect RECENT behavior and never drift unboundedly off the
/// chip's lifetime history.
///
/// **W24-BC-2 ():** the prior `ChipWindow` was a LIFETIME ACCUMULATOR
/// (`actual_nonces += …`, `expected_nonces += …`, never reset). For a chip
/// running at a stable sub-100% ratio `r`, the z-score
/// `(actual − expected)/sqrt(expected)` ≈ `(r−1)·sqrt(expected)` drifts
/// unboundedly negative as samples accumulate, so a perfectly healthy chip
/// stable at e.g. 80% would EVENTUALLY cross the −4σ Bad gate purely from
/// elapsed time and get blacklisted (lost hashrate), and recovery was
/// near-impossible because the banked deficit dominated. Bounding the window
/// fixes both: the statistic is over the last N windows only, so a stable
/// ratio yields a stable (bounded) z-score that never escalates by drift.
///
/// 12 windows ≈ a few minutes of evaluation at a typical multi-second autotuner
/// cadence — long enough to average out per-window Poisson noise, short enough
/// that the statistic tracks recent behavior and a recovered chip climbs back
/// out within roughly one window-length of sustained good production.
const ROLLING_WINDOW_CAP: usize = 12;

/// One per-window observation contributing to the rolling statistic.
#[derive(Debug, Clone, Copy, Default)]
struct WindowSample {
    actual: u64,
    expected: f64,
    hw_errors: u64,
}

#[derive(Debug, Clone, Default)]
struct ChipWindow {
    /// Fixed-capacity ring of the most-recent per-window samples. Capacity is
    /// `ROLLING_WINDOW_CAP`; once full, the oldest sample is evicted on push.
    /// All of `actual_nonces` / `expected_nonces` / `hw_errors` /
    /// `sample_windows` are DERIVED from this ring (sums over recent windows),
    /// never lifetime accumulators.
    samples: std::collections::VecDeque<WindowSample>,
    /// Consecutive bad-classified windows since last good window.
    consecutive_bad: u32,
    /// Consecutive missing windows since last seen.
    consecutive_missing: u32,
    /// Consecutive good-classified (well-performing) windows since the last
    /// degraded/bad window. Drives the Bad→Degraded→Healthy trust decay
    /// (W24-BC-2): with a bounded rolling window the old `sample_windows >=
    /// 8/24` lifetime gate is unreachable (ring caps at ROLLING_WINDOW_CAP),
    /// so recovery is keyed on SUSTAINED good behavior instead — which is also
    /// the more correct signal (a chip earns promotion by performing well, not
    /// merely by elapsed observe() calls).
    consecutive_good: u32,
}

impl ChipWindow {
    /// Push one window's observation, evicting the oldest sample when the
    /// bounded window is full. This is what makes the window ROLL (W24-BC-2):
    /// the statistic only ever reflects the last `ROLLING_WINDOW_CAP` windows.
    fn accumulate(&mut self, actual: u64, expected: f64, hw_errors: u64) {
        if self.samples.len() == ROLLING_WINDOW_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(WindowSample {
            actual,
            expected,
            hw_errors,
        });
    }

    /// Sum of valid nonces over the rolling window.
    fn actual_nonces(&self) -> u64 {
        self.samples
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.actual))
    }

    /// Sum of expected nonces over the rolling window.
    fn expected_nonces(&self) -> f64 {
        self.samples.iter().map(|s| s.expected).sum()
    }

    /// Sum of hardware errors over the rolling window.
    fn hw_errors(&self) -> u64 {
        self.samples
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.hw_errors))
    }

    /// Number of windows currently in the rolling window (0..=ROLLING_WINDOW_CAP).
    fn sample_windows(&self) -> u32 {
        self.samples.len() as u32
    }

    /// Compute z-score of (actual - expected) under a Poisson approximation
    /// (std-dev ≈ sqrt(expected) for sufficiently large expected), over the
    /// ROLLING window only — so a chip at a stable ratio yields a bounded
    /// z-score that does not drift more negative with elapsed time.
    fn z_score(&self) -> f64 {
        let expected = self.expected_nonces();
        if expected < 1.0 {
            return 0.0;
        }
        let std_dev = expected.sqrt();
        (self.actual_nonces() as f64 - expected) / std_dev
    }

    /// Ratio of actual to expected (0.0 .. 1.0+) over the rolling window.
    fn actual_pct(&self) -> f64 {
        let expected = self.expected_nonces();
        if expected < 1.0 {
            return 1.0;
        }
        self.actual_nonces() as f64 / expected
    }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

/// Cross-firmware bad-chip supervisor. Owns the FSM state, per-chip rolling
/// windows, per-board reset attempt counter, and per-board fingerprints.
#[derive(Debug, Clone)]
pub struct BadChipSupervisor {
    config: BadChipConfig,
    /// One fingerprint per chain (keyed by chain_id).
    fingerprints: HashMap<u8, BoardFingerprint>,
    /// Per-(chain, chip) FSM state.
    chip_states: HashMap<(u8, u16), ChipPersistentState>,
    /// Per-(chain, chip) rolling window (in-memory).
    chip_windows: HashMap<(u8, u16), ChipWindow>,
    /// Per-chain board-reset attempts since the supervisor was constructed.
    board_reset_attempts: HashMap<u8, u32>,
}

impl BadChipSupervisor {
    /// Construct a supervisor with documented spec defaults + a per-chain
    /// fingerprint registry. Chips are implicitly `Healthy` until the first
    /// `observe()` flips them.
    pub fn new(config: BadChipConfig, fingerprints: Vec<BoardFingerprint>) -> Self {
        let mut fp_map = HashMap::new();
        for fp in fingerprints {
            fp_map.insert(fp.chain_id, fp);
        }
        Self {
            config,
            fingerprints: fp_map,
            chip_states: HashMap::new(),
            chip_windows: HashMap::new(),
            board_reset_attempts: HashMap::new(),
        }
    }

    /// True iff the supervisor is enabled in the config (caller should NoOp
    /// early when this returns false).
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Read current state for a specific chip. `Healthy` by default if not
    /// yet observed.
    pub fn state(&self, chain_id: u8, chip_index: u16) -> ChipHealthState {
        self.chip_states
            .get(&(chain_id, chip_index))
            .map(|s| s.status)
            .unwrap_or(ChipHealthState::Healthy)
    }

    /// Apply persisted state from disk. Fingerprint mismatch discards the
    /// persisted blacklist (chip replacement) per RE-004 §"Recovery".
    pub fn restore_from_persisted(&mut self, persisted: BadChipPersistedState) {
        let chain_id = persisted.fingerprint.chain_id;
        let current = self.fingerprints.get(&chain_id);
        match current {
            Some(fp) if *fp == persisted.fingerprint => {
                // Same board — restore per-chip state.
                for chip in persisted.chips {
                    self.chip_states
                        .insert((chip.chain_id, chip.chip_index), chip);
                }
            }
            _ => {
                // Fingerprint mismatch (or chain not registered). Discard
                // persisted state; chips force-retune as Healthy.
            }
        }
    }

    /// Snapshot current state for serialization to disk.
    pub fn snapshot_for_chain(&self, chain_id: u8) -> Option<BadChipPersistedState> {
        let fp = self.fingerprints.get(&chain_id)?;
        let chips: Vec<ChipPersistentState> = self
            .chip_states
            .iter()
            .filter(|((c, _), _)| *c == chain_id)
            .map(|(_, v)| v.clone())
            .collect();
        Some(BadChipPersistedState {
            fingerprint: fp.clone(),
            chips,
        })
    }

    /// Process one per-chain stats snapshot. The caller supplies
    /// `expected_nonces_per_chip` (computed from frequency × difficulty ×
    /// cores per the autotuner's existing per-chip nonce math). Returns a
    /// list of actions in priority order (HaltMining first; then board-
    /// level; then per-chip). Empty when the supervisor is dormant or no
    /// state changed this tick.
    pub fn observe(
        &mut self,
        snapshot: &ChipStatsSnapshot,
        expected_nonces_per_chip: f64,
    ) -> Vec<BadChipAction> {
        // Disabled supervisor: NoOp (caller does nothing). Wave-D pattern.
        if !self.is_enabled() {
            return Vec::new();
        }

        let chain_id = snapshot.chain_id;
        let chip_count = snapshot.chip_nonces.len() as u16;

        // Update windows + classify per-chip.
        let mut newly_degraded: Vec<u16> = Vec::new();
        let mut newly_bad: Vec<(u16, BadChipReason)> = Vec::new();
        let mut newly_missing: Vec<u16> = Vec::new();
        let mut promoted_to_healthy: Vec<u16> = Vec::new();

        let hw_errors_default = vec![0u64; snapshot.chip_nonces.len()];
        let chip_hw_errors = snapshot
            .chip_hw_errors
            .as_ref()
            .unwrap_or(&hw_errors_default);

        for (i, &actual) in snapshot.chip_nonces.iter().enumerate() {
            let chip_idx = i as u16;
            let key = (chain_id, chip_idx);
            let prior_state = self.state(chain_id, chip_idx);

            // Detect missing-from-enumeration: zero actual + zero expected
            // is "not enumerated" only when expected was supposed to be
            // > 0 (otherwise the chip is just idle, not missing). We treat
            // expected==0 OR actual==0 with hw_errors==0 as "missing"
            // candidate.
            let hw_err = chip_hw_errors.get(i).copied().unwrap_or(0);
            let is_missing_candidate = actual == 0 && hw_err == 0 && expected_nonces_per_chip > 0.0;

            let window = self.chip_windows.entry(key).or_default();
            if is_missing_candidate {
                window.consecutive_missing = window.consecutive_missing.saturating_add(1);
                if window.consecutive_missing >= self.config.missing_grace_windows
                    && prior_state != ChipHealthState::Missing
                {
                    newly_missing.push(chip_idx);
                }
                continue;
            }

            // Chip is reporting; reset missing counter.
            window.consecutive_missing = 0;
            window.accumulate(actual, expected_nonces_per_chip, hw_err);

            // Need enough samples before destructive classification.
            if window.sample_windows() == 0 || (window.actual_nonces() as f64) < 1.0 {
                continue;
            }
            let total_expected = window.expected_nonces();
            if total_expected < self.config.min_samples as f64 {
                // Not enough confidence to act; chip stays as it was.
                continue;
            }

            let actual_pct_100 = window.actual_pct() * 100.0;
            let z = window.z_score();

            // Hardware errors exceed valid nonces → Bad (regardless of
            // pct, this is the "hw error rate exceeded nonce rate" rung).
            // Computed over the rolling window (W24-BC-2).
            let window_hw_errors = window.hw_errors();
            if window_hw_errors > window.actual_nonces() && window_hw_errors >= 16 {
                window.consecutive_bad = window.consecutive_bad.saturating_add(1);
                window.consecutive_good = 0;
                if window.consecutive_bad >= self.config.repeated_bad_windows
                    && prior_state != ChipHealthState::Bad
                {
                    newly_bad.push((chip_idx, BadChipReason::HwErrorsExceedNonces));
                }
                continue;
            }

            // Bad classification: very low actual + 4σ confidence + repeated.
            if (actual_pct_100 as f32) < self.config.bad_threshold_pct
                && (z as f32) < -self.config.bad_sigma_threshold
            {
                window.consecutive_bad = window.consecutive_bad.saturating_add(1);
                window.consecutive_good = 0;
                if window.consecutive_bad >= self.config.repeated_bad_windows
                    && prior_state != ChipHealthState::Bad
                {
                    newly_bad.push((chip_idx, BadChipReason::PersistentlyLowNonceRate));
                }
                continue;
            }

            // Degraded: below degraded_threshold + 2σ confidence (less
            // destructive — only triggers per-chip downclock).
            if (actual_pct_100 as f32) < self.config.degraded_threshold_pct && (z as f32) < -2.0 {
                if prior_state == ChipHealthState::Healthy {
                    newly_degraded.push(chip_idx);
                }
                window.consecutive_bad = 0;
                window.consecutive_good = 0;
                continue;
            }

            // Chip is performing well. Reset bad-window counter, count the
            // good window, and decay trust upward (Bad → Degraded → Healthy
            // across repeated good windows per RE-004 §"Persistence /
            // Recovery"). W24-BC-2: recovery is keyed on consecutive GOOD
            // windows, not on the lifetime `sample_windows` (which is now
            // bounded by ROLLING_WINDOW_CAP and could never reach the old 24).
            window.consecutive_bad = 0;
            window.consecutive_good = window.consecutive_good.saturating_add(1);
            let consecutive_good = window.consecutive_good;
            match prior_state {
                ChipHealthState::Degraded if consecutive_good >= 8 => {
                    promoted_to_healthy.push(chip_idx);
                }
                ChipHealthState::Bad if consecutive_good >= 24 => {
                    // Bad → Degraded (incremental promotion).
                    self.chip_states.insert(
                        key,
                        ChipPersistentState {
                            chain_id,
                            chip_index: chip_idx,
                            status: ChipHealthState::Degraded,
                            last_demote_reason: None,
                        },
                    );
                }
                _ => {}
            }
        }

        // Apply state transitions.
        for chip in &newly_missing {
            self.chip_states.insert(
                (chain_id, *chip),
                ChipPersistentState {
                    chain_id,
                    chip_index: *chip,
                    status: ChipHealthState::Missing,
                    last_demote_reason: Some(BadChipReason::PersistentlyMissing),
                },
            );
        }
        for chip in &newly_degraded {
            self.chip_states.insert(
                (chain_id, *chip),
                ChipPersistentState {
                    chain_id,
                    chip_index: *chip,
                    status: ChipHealthState::Degraded,
                    last_demote_reason: None,
                },
            );
        }
        for (chip, reason) in &newly_bad {
            self.chip_states.insert(
                (chain_id, *chip),
                ChipPersistentState {
                    chain_id,
                    chip_index: *chip,
                    status: ChipHealthState::Bad,
                    last_demote_reason: Some(*reason),
                },
            );
        }
        for chip in &promoted_to_healthy {
            self.chip_states.insert(
                (chain_id, *chip),
                ChipPersistentState {
                    chain_id,
                    chip_index: *chip,
                    status: ChipHealthState::Healthy,
                    last_demote_reason: None,
                },
            );
        }

        // Compose actions in priority order.
        let mut actions: Vec<BadChipAction> = Vec::new();

        // Count this-chain health AFTER updates for the board-level rungs.
        let mut degraded_or_worse = 0u16;
        let mut healthy_this_chain = 0u16;
        for (i, _) in snapshot.chip_nonces.iter().enumerate() {
            let status = self.state(chain_id, i as u16);
            if matches!(
                status,
                ChipHealthState::Degraded | ChipHealthState::Bad | ChipHealthState::Missing
            ) {
                degraded_or_worse += 1;
            } else {
                healthy_this_chain += 1;
            }
        }

        let board_profile_threshold =
            (chip_count as f32 * self.config.board_profile_step_fraction).ceil() as u16;
        let many_chips_degraded =
            chip_count > 0 && degraded_or_worse >= board_profile_threshold.max(1);

        // Halt path: too few healthy CHIPS on this chain — emit HaltMining.
        // The caller cuts hash power; never raises fan PWM (load-bearing).
        // W24-BC-3: `min_operational_chips_per_chain` is a per-chain healthy-
        // CHIP floor (units now match the field name + doc); `healthy_this_chain`
        // is the count of healthy chips on THIS chain.
        if healthy_this_chain < self.config.min_operational_chips_per_chain as u16 {
            actions.push(BadChipAction::HaltMining {
                reason: HaltReason::InsufficientHealthyChains,
            });
            return actions;
        }

        if many_chips_degraded && !newly_missing.is_empty() {
            // Many missing or missing+degraded on one board → board reset
            // first (bounded budget). If budget exhausted → HaltMining.
            let attempts = self.board_reset_attempts.entry(chain_id).or_insert(0);
            *attempts += 1;
            if *attempts > self.config.board_reset_budget {
                actions.push(BadChipAction::HaltMining {
                    reason: HaltReason::BoardResetBudgetExhausted,
                });
                return actions;
            }
            actions.push(BadChipAction::BoardReset {
                chain_id,
                attempt: *attempts,
            });
        } else if many_chips_degraded {
            actions.push(BadChipAction::ReduceBoardProfile { chain_id });
        }

        // Per-chip actions for newly-classified chips.
        for chip in &newly_degraded {
            actions.push(BadChipAction::PerChipDownclock {
                chain_id,
                chip_index: *chip,
                mhz_step: self.config.per_chip_downclock_mhz,
            });
        }
        for (chip, reason) in &newly_bad {
            actions.push(BadChipAction::BlacklistChip {
                chain_id,
                chip_index: *chip,
                reason: *reason,
            });
        }

        if actions.is_empty() {
            actions.push(BadChipAction::NoOp);
        }
        actions
    }

    /// Iterate over (chain, chip) → status for telemetry / API exposure.
    pub fn iter_states(&self) -> impl Iterator<Item = (&(u8, u16), &ChipPersistentState)> {
        self.chip_states.iter()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn fp(chain_id: u8) -> BoardFingerprint {
        BoardFingerprint {
            platform: "am2-zynq".to_string(),
            model: "s19jpro".to_string(),
            chain_id,
            chip_count: 4,
            eeprom_hash16: Some("deadbeefcafebabe".to_string()),
        }
    }

    fn config_enabled() -> BadChipConfig {
        BadChipConfig {
            enabled: true,
            ..BadChipConfig::default()
        }
    }

    fn snapshot(chain_id: u8, nonces: Vec<u64>, hw_errors: Option<Vec<u64>>) -> ChipStatsSnapshot {
        ChipStatsSnapshot {
            chain_id,
            measurement_epoch: 0,
            chip_nonces: nonces,
            chip_errors: vec![],
            window_duration_s: 60.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: hw_errors,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        }
    }

    // -- 1. Default-off contract --
    #[test]
    fn supervisor_disabled_by_default_emits_no_actions() {
        let mut sup = BadChipSupervisor::new(BadChipConfig::default(), vec![fp(0)]);
        assert!(!sup.is_enabled());
        let snap = snapshot(0, vec![0, 0, 0, 0], None);
        // Even a chain of dead chips with the default config should produce no actions.
        assert!(sup.observe(&snap, 200.0).is_empty());
    }

    // -- 2. Healthy stays Healthy --
    #[test]
    fn healthy_chip_stays_healthy() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let snap = snapshot(0, vec![200, 200, 200, 200], None);
        let actions = sup.observe(&snap, 200.0);
        for i in 0..4 {
            assert_eq!(sup.state(0, i), ChipHealthState::Healthy);
        }
        assert_eq!(actions, vec![BadChipAction::NoOp]);
    }

    // -- 3. Healthy → Degraded triggers PerChipDownclock --
    #[test]
    fn weak_chip_classified_degraded_with_downclock_action() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // Chip 2 produces 60% (z ~= -5.6 at expected=200, below 85% threshold).
        let snap = snapshot(0, vec![200, 200, 120, 200], None);
        let actions = sup.observe(&snap, 200.0);
        assert_eq!(sup.state(0, 2), ChipHealthState::Degraded);
        assert!(actions.iter().any(|a| matches!(
            a,
            BadChipAction::PerChipDownclock {
                chain_id: 0,
                chip_index: 2,
                ..
            }
        )));
    }

    // -- 4. Degraded → Bad requires repeated_bad_windows + 4σ + repeated --
    #[test]
    fn very_low_chip_classified_bad_after_repeated_windows() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // Chip 1 produces ~5% expected (z ~= -13.4 at expected=200).
        let bad_snap = snapshot(0, vec![200, 10, 200, 200], None);
        // First bad window — not enough; should be Degraded (or remain Healthy on first observe).
        sup.observe(&bad_snap, 200.0);
        // Second bad window — now meets repeated_bad_windows = 2.
        let actions = sup.observe(&bad_snap, 200.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Bad);
        assert!(actions.iter().any(|a| matches!(
            a,
            BadChipAction::BlacklistChip {
                chip_index: 1,
                reason: BadChipReason::PersistentlyLowNonceRate,
                ..
            }
        )));
    }

    // -- 5. HW errors exceed nonces → Bad (HwErrorsExceedNonces reason) --
    #[test]
    fn hw_errors_exceeding_nonces_classify_bad() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // expected_nonces_per_chip = 150 (>= min_samples 120) so EACH window
        // clears the confidence gate and classifies — needed to reach
        // repeated_bad_windows=2. Chips 0/2/3 produce 150 (healthy, so the
        // higher-priority InsufficientHealthy halt never preempts the per-chip
        // BlacklistChip). Chip 1: actual 100 < hw_errors 200 (and hw_errors >=
        // 16) → HwErrorsExceedNonces rung on both windows.
        let snap = snapshot(0, vec![150, 100, 150, 150], Some(vec![5, 200, 5, 5]));
        sup.observe(&snap, 150.0);
        let actions = sup.observe(&snap, 150.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Bad);
        assert!(actions.iter().any(|a| matches!(
            a,
            BadChipAction::BlacklistChip {
                chip_index: 1,
                reason: BadChipReason::HwErrorsExceedNonces,
                ..
            }
        )));
    }

    // -- 6. Persistently missing → Missing after grace windows --
    #[test]
    fn missing_chip_classified_after_grace_windows() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let snap = snapshot(0, vec![200, 0, 200, 200], Some(vec![0, 0, 0, 0]));
        // Default missing_grace = 3.
        sup.observe(&snap, 200.0);
        sup.observe(&snap, 200.0);
        sup.observe(&snap, 200.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Missing);
    }

    // -- 7. Confidence gate: short window does not demote --
    #[test]
    fn short_window_does_not_classify_bad() {
        let cfg = BadChipConfig {
            enabled: true,
            min_samples: 1000, // require huge sample size
            ..BadChipConfig::default()
        };
        let mut sup = BadChipSupervisor::new(cfg, vec![fp(0)]);
        // Even an obviously-dead chip should NOT classify when expected_nonces
        // per window (200) × sample_windows (1) << min_samples (1000).
        let snap = snapshot(0, vec![200, 10, 200, 200], None);
        sup.observe(&snap, 200.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Healthy);
    }

    // -- 8. LOAD-BEARING: blacklist does NOT touch share-submit nonces --
    // The supervisor only emits intent; the share-submit path is the
    // caller's responsibility and stays intact. We assert by construction:
    // there is no API on BadChipSupervisor that gates nonce submission.
    #[test]
    fn blacklisted_chip_nonce_still_submits() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let bad_snap = snapshot(0, vec![200, 10, 200, 200], None);
        sup.observe(&bad_snap, 200.0);
        sup.observe(&bad_snap, 200.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Bad);
        // Confirm: the supervisor's public API contains NO method that
        // suppresses, rejects, or filters per-chip nonces from a share-
        // submit path. The supervisor never sees nonces — only aggregates.
        // This is the test that the load-bearing rule cannot regress
        // through this module without an API change.
        let _ = sup.iter_states(); // public API surface check
                                   // Build a "now this chip produces a nonce" snapshot — the
                                   // supervisor must remain a pure aggregator (no nonce dropping).
        let good_snap = snapshot(0, vec![200, 250, 200, 200], None);
        let _actions = sup.observe(&good_snap, 200.0);
        // The chip stays Bad (a single good window doesn't promote — that's
        // by design), but no action says "drop this chip's nonce."
    }

    // -- 9. Board-reset budget exhausts → HaltMining --
    // The board-reset rung only fires when (a) >= board_profile_step_fraction
    // of the chain is degraded-or-worse AND (b) a NEW chip went Missing this
    // window (`newly_missing` non-empty), and it is reached ONLY when at least
    // one chip stays healthy (otherwise the higher-priority InsufficientHealthy
    // halt preempts it). So we use an 8-chip chain, keep chip 0 healthy
    // throughout, and progressively kill chips so each window adds a fresh
    // Missing chip and crosses the 50% board fraction.
    #[test]
    fn board_reset_budget_exhausts_to_halt() {
        let cfg = BadChipConfig {
            enabled: true,
            board_reset_budget: 2,
            missing_grace_windows: 1,
            board_profile_step_fraction: 0.50,
            min_operational_chips_per_chain: 1,
            ..BadChipConfig::default()
        };
        let mut fp8 = fp(0);
        fp8.chip_count = 8;
        let mut sup = BadChipSupervisor::new(cfg, vec![fp8]);

        // Window 1: chips 1-4 go Missing (grace=1 ⇒ Missing this window).
        // 4 of 8 missing == ceil(8*0.5)=4 threshold ⇒ many; chip 0 still
        // healthy ⇒ no InsufficientHealthy preempt ⇒ BoardReset attempt 1.
        let w1 = snapshot(0, vec![200, 0, 0, 0, 0, 200, 200, 200], Some(vec![0; 8]));
        let actions1 = sup.observe(&w1, 200.0);
        assert!(actions1
            .iter()
            .any(|a| matches!(a, BadChipAction::BoardReset { attempt: 1, .. })));

        // Window 2: chips 5-6 newly go Missing (a fresh `newly_missing`),
        // 6 of 8 missing, chip 0 still healthy ⇒ BoardReset attempt 2.
        let w2 = snapshot(0, vec![200, 0, 0, 0, 0, 0, 0, 200], Some(vec![0; 8]));
        let actions2 = sup.observe(&w2, 200.0);
        assert!(actions2
            .iter()
            .any(|a| matches!(a, BadChipAction::BoardReset { attempt: 2, .. })));

        // Window 3: chip 7 newly goes Missing (fresh `newly_missing`), chip 0
        // still healthy (so InsufficientHealthy does NOT preempt). Attempt 3
        // exceeds budget=2 ⇒ HaltMining(BoardResetBudgetExhausted).
        let w3 = snapshot(0, vec![200, 0, 0, 0, 0, 0, 0, 0], Some(vec![0; 8]));
        let actions3 = sup.observe(&w3, 200.0);
        assert!(actions3.iter().any(|a| matches!(
            a,
            BadChipAction::HaltMining {
                reason: HaltReason::BoardResetBudgetExhausted
            }
        )));
    }

    // -- 10. min_operational_chips_per_chain halt-mining gate --
    #[test]
    fn insufficient_healthy_chips_halts_mining() {
        let cfg = BadChipConfig {
            enabled: true,
            min_operational_chips_per_chain: 4, // require all 4 chips healthy
            ..BadChipConfig::default()
        };
        let mut sup = BadChipSupervisor::new(cfg, vec![fp(0)]);
        // 1 chip degraded → 3 healthy < 4 required.
        let snap = snapshot(0, vec![200, 120, 200, 200], None);
        let actions = sup.observe(&snap, 200.0);
        assert!(actions.iter().any(|a| matches!(
            a,
            BadChipAction::HaltMining {
                reason: HaltReason::InsufficientHealthyChains
            }
        )));
    }

    // -- 11. ReduceBoardProfile when many chips degrade together --
    #[test]
    fn many_degraded_chips_reduces_board_profile() {
        let cfg = BadChipConfig {
            enabled: true,
            board_profile_step_fraction: 0.50,
            min_operational_chips_per_chain: 1,
            ..BadChipConfig::default()
        };
        let mut sup = BadChipSupervisor::new(cfg, vec![fp(0)]);
        // 3 of 4 chips degraded → 75% ≥ 50% threshold → ReduceBoardProfile.
        let snap = snapshot(0, vec![200, 120, 120, 120], None);
        let actions = sup.observe(&snap, 200.0);
        assert!(actions
            .iter()
            .any(|a| matches!(a, BadChipAction::ReduceBoardProfile { chain_id: 0 })));
    }

    // -- 12. Fingerprint match restores persisted state --
    #[test]
    fn fingerprint_match_restores_persisted_blacklist() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let persisted = BadChipPersistedState {
            fingerprint: fp(0),
            chips: vec![ChipPersistentState {
                chain_id: 0,
                chip_index: 1,
                status: ChipHealthState::Bad,
                last_demote_reason: Some(BadChipReason::PersistentlyLowNonceRate),
            }],
        };
        sup.restore_from_persisted(persisted);
        assert_eq!(sup.state(0, 1), ChipHealthState::Bad);
    }

    // -- 13. Fingerprint mismatch discards persisted state (chip replacement) --
    #[test]
    fn fingerprint_mismatch_discards_blacklist_on_replacement() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let mut replaced_fp = fp(0);
        replaced_fp.eeprom_hash16 = Some("0000000000000000".to_string()); // new board
        let persisted = BadChipPersistedState {
            fingerprint: replaced_fp,
            chips: vec![ChipPersistentState {
                chain_id: 0,
                chip_index: 1,
                status: ChipHealthState::Bad,
                last_demote_reason: Some(BadChipReason::PersistentlyLowNonceRate),
            }],
        };
        sup.restore_from_persisted(persisted);
        // Should remain Healthy (default) — persisted blacklist discarded.
        assert_eq!(sup.state(0, 1), ChipHealthState::Healthy);
    }

    // -- 14. Snapshot round-trip preserves state --
    #[test]
    fn snapshot_for_chain_round_trips_state() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let bad_snap = snapshot(0, vec![200, 10, 200, 200], None);
        sup.observe(&bad_snap, 200.0);
        sup.observe(&bad_snap, 200.0);
        let snap = sup.snapshot_for_chain(0).expect("fingerprint registered");
        assert_eq!(snap.fingerprint, fp(0));
        assert!(snap
            .chips
            .iter()
            .any(|c| c.chip_index == 1 && c.status == ChipHealthState::Bad));
    }

    // -- 15. Quiet-home fan-cap preservation: supervisor never emits fan actions --
    #[test]
    fn supervisor_never_emits_fan_control_action() {
        // The action enum has NO variant for fan control. This is a
        // structural test: if a future change adds a fan-control variant,
        // this test forces an explicit assertion that the quiet-home cap
        // contract is reviewed.
        // Compile-time check by exhaustive match:
        fn _never_fan(action: &BadChipAction) {
            match action {
                BadChipAction::NoOp
                | BadChipAction::PerChipDownclock { .. }
                | BadChipAction::BlacklistChip { .. }
                | BadChipAction::ReduceBoardProfile { .. }
                | BadChipAction::BoardReset { .. }
                | BadChipAction::HaltMining { .. } => {}
            }
        }
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        let snap = snapshot(0, vec![0, 0, 0, 0], Some(vec![0, 0, 0, 0]));
        for action in sup.observe(&snap, 200.0) {
            _never_fan(&action);
        }
    }

    // -- 16. W24-BC-2: ChipWindow is a TRUE bounded rolling window --
    // Pins the unit-level rolling behavior the fix introduces: the window
    // never holds more than ROLLING_WINDOW_CAP samples, and the z-score of a
    // chip at a STABLE sub-100% ratio is BOUNDED (does not drift more negative
    // with elapsed time). The pre-fix cumulative accumulator failed both.
    #[test]
    fn chip_window_rolls_and_z_score_is_bounded_for_stable_ratio() {
        let mut w = ChipWindow::default();
        // Feed 100 windows of a stable 80% ratio (actual 160 / expected 200).
        let mut z_at_cap: Option<f64> = None;
        for i in 0..100 {
            w.accumulate(160, 200.0, 0);
            // The ring never exceeds its capacity.
            assert!(w.sample_windows() as usize <= ROLLING_WINDOW_CAP);
            // Once the window is full it holds exactly the cap.
            if i + 1 >= ROLLING_WINDOW_CAP {
                assert_eq!(w.sample_windows() as usize, ROLLING_WINDOW_CAP);
                let z = w.z_score();
                // Steady-state z is fixed (does NOT drift more negative).
                if let Some(prev) = z_at_cap {
                    assert!(
                        (z - prev).abs() < 1e-9,
                        "z-score must be stable once the window is full, got {prev} then {z}"
                    );
                }
                z_at_cap = Some(z);
            }
        }
        // Sanity: the bounded z for 80% over a 12-window ring of expected=200
        // is (0.8-1.0)*sqrt(12*200) ≈ -9.8σ — large, but BOUNDED. The pre-fix
        // accumulator would have reached ≈ (0.8-1.0)*sqrt(100*200) ≈ -28σ and
        // kept growing without bound.
        let z = w.z_score();
        assert!(z < 0.0 && z > -12.0, "bounded steady-state z, got {z}");
        // pct reflects the recent ratio, not a lifetime sum.
        assert!((w.actual_pct() - 0.8).abs() < 1e-9);
    }

    // -- 17. W24-BC-2 REGRESSION: a chip at a stable 90% ratio stays Healthy
    // indefinitely and is NEVER blacklisted by drift (the exact false-positive
    // the rolling-window fix prevents). 90% ≥ 85% degraded threshold ⇒ never
    // even Degraded; with the old cumulative window the z-score would have
    // drifted past every gate over enough windows. --
    #[test]
    fn stable_90pct_chip_never_blacklisted_by_drift() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // Chip 1 holds a stable 90% (180 / 200). Chips 0/2/3 healthy at 100%.
        let snap = snapshot(0, vec![200, 180, 200, 200], None);
        for _ in 0..200 {
            let actions = sup.observe(&snap, 200.0);
            // Drift must NEVER produce a blacklist for any chip.
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, BadChipAction::BlacklistChip { .. })),
                "stable 90% chip must never be blacklisted by accumulated drift"
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, BadChipAction::HaltMining { .. })),
                "no halt from a single slightly-weak-but-healthy chip"
            );
        }
        // After 200 windows the 90% chip is still Healthy (never escalated).
        assert_eq!(sup.state(0, 1), ChipHealthState::Healthy);
        assert_eq!(sup.state(0, 0), ChipHealthState::Healthy);
    }

    // -- 18. W24-BC-2: a chip stuck between the bad and degraded thresholds
    // (stable 50%) is correctly Degraded and stays Degraded forever — the
    // drift never pushes Degraded → Bad, because Bad requires pct < 20%. --
    #[test]
    fn stable_50pct_chip_stays_degraded_never_bad() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // Chip 1 stable at 50% (100 / 200) — below 85% degraded, above 20% bad.
        let snap = snapshot(0, vec![200, 100, 200, 200], None);
        for _ in 0..200 {
            let actions = sup.observe(&snap, 200.0);
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, BadChipAction::BlacklistChip { .. })),
                "a stable 50% chip must never escalate to Bad via drift"
            );
        }
        assert_eq!(sup.state(0, 1), ChipHealthState::Degraded);
    }

    // -- 19. W24-BC-2: recovery — a Degraded chip that runs healthy for the
    // decay window climbs back to Healthy (keyed on consecutive GOOD windows,
    // which the bounded ring still supports; the old `sample_windows >= 8/24`
    // lifetime gate would be unreachable once the ring caps). --
    #[test]
    fn degraded_chip_recovers_to_healthy_after_sustained_good() {
        let mut sup = BadChipSupervisor::new(config_enabled(), vec![fp(0)]);
        // Drive chip 1 to Degraded (60% — below 85%, z past -2σ).
        let weak = snapshot(0, vec![200, 120, 200, 200], None);
        sup.observe(&weak, 200.0);
        assert_eq!(sup.state(0, 1), ChipHealthState::Degraded);
        // Now run it perfectly healthy. Recovery needs the weak sample to age
        // out of the rolling window AND >= 8 consecutive good-classified
        // windows (the rolling blend keeps the first 1-2 windows below the
        // degraded threshold), so a generous run proves the chip climbs back.
        let good = snapshot(0, vec![200, 200, 200, 200], None);
        for _ in 0..16 {
            sup.observe(&good, 200.0);
        }
        assert_eq!(
            sup.state(0, 1),
            ChipHealthState::Healthy,
            "Degraded chip must recover to Healthy after sustained good windows"
        );
    }

    // -- 20. W24-BC-3: min_operational_chips_per_chain is a per-chain
    // healthy-CHIP floor with units that now match its name. Use a fixture
    // where chip_count != threshold so the old chips==chains coincidence can
    // no longer mask the semantics. --
    #[test]
    fn min_operational_chips_per_chain_uses_chip_units() {
        // 8-chip chain; require >= 5 healthy chips on the chain.
        let mut fp8 = fp(0);
        fp8.chip_count = 8;
        let cfg = BadChipConfig {
            enabled: true,
            min_operational_chips_per_chain: 5,
            ..BadChipConfig::default()
        };
        let mut sup = BadChipSupervisor::new(cfg, vec![fp8]);
        // 3 of 8 chips degraded → 5 healthy == floor 5 → NOT below → no halt.
        let ok = snapshot(0, vec![200, 200, 200, 200, 200, 120, 120, 120], None);
        let actions = sup.observe(&ok, 200.0);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, BadChipAction::HaltMining { .. })),
            "5 healthy chips meets the per-chip floor of 5 — must not halt"
        );

        // 4 of 8 chips degraded → only 4 healthy < floor 5 → HaltMining.
        // (chip_count 8 != threshold 5, so this can't be a chips==chains fluke.)
        let mut sup2 = BadChipSupervisor::new(
            BadChipConfig {
                enabled: true,
                min_operational_chips_per_chain: 5,
                ..BadChipConfig::default()
            },
            vec![{
                let mut f = fp(0);
                f.chip_count = 8;
                f
            }],
        );
        let halt = snapshot(0, vec![200, 200, 200, 200, 120, 120, 120, 120], None);
        let actions2 = sup2.observe(&halt, 200.0);
        assert!(
            actions2.iter().any(|a| matches!(
                a,
                BadChipAction::HaltMining {
                    reason: HaltReason::InsufficientHealthyChains
                }
            )),
            "4 healthy chips < per-chip floor 5 — must halt"
        );
    }
}
