//! TABS Phase 1: Parallel binary search over PLL frequency table.
//!
//! The key insight for fast frequency characterization is that all chips are
//! tested simultaneously. Each chip gets its own binary search
//! state. After setting all chips to their test frequencies, we wait one
//! measurement window and use per-chip nonce attribution (BM1387 nonce
//! bits [7:2]) to compute each chip's error rate independently.
//!
//! Binary search over 33 PLL entries converges in ~5 iterations.
//! At 3 seconds per iteration: 5 * 3 = 15 seconds total.
//! Full power and thermal convergence still depends on the higher-level
//! autotuner control loop and measured power telemetry.

use crate::chip_stats::ChipStatsSnapshot;
use crate::config::AutoTunerConfig;
use crate::profile::{ChipGrade, ChipProfile};

// PLL frequency table is now passed at construction (chip-specific),
// not hardcoded to bm1387::pll_frequencies().

/// Resolve a safe inclusive PLL-index search band from operator/config bounds.
///
/// `AutoTunerConfig::validate()` rejects `min_freq_mhz > max_freq_mhz`, but
/// this module is also used directly in unit tests and future callers may
/// construct it from already-mutated runtime state. Keep the point-of-use
/// search space bounded even if validation was bypassed: the operator ceiling
/// wins when the range is inverted, and an impossible below-table ceiling
/// collapses to the lowest lockable PLL entry instead of silently exploring the
/// top of the table.
fn safe_pll_search_bounds(
    min_freq_mhz: u16,
    max_freq_mhz: u16,
    pll_table: &'static [u16],
) -> (usize, usize) {
    debug_assert!(
        !pll_table.is_empty(),
        "ASIC PLL tables must contain at least one lockable frequency"
    );
    if pll_table.is_empty() {
        return (0, 0);
    }

    let ceiling_mhz = max_freq_mhz.max(1);
    let floor_mhz = min_freq_mhz.min(ceiling_mhz);

    let hi = pll_table
        .iter()
        .rposition(|&freq| freq <= ceiling_mhz)
        .unwrap_or(0);
    let lo = pll_table
        .iter()
        .position(|&freq| freq >= floor_mhz)
        .unwrap_or(hi)
        .min(hi);

    (lo, hi)
}

/// Per-chip binary search state.
#[derive(Debug, Clone)]
pub struct ChipSearchState {
    /// Chip index (0-62).
    chip_index: u8,
    /// Lower bound index into the PLL frequency table.
    lo: usize,
    /// Upper bound index into the PLL frequency table.
    hi: usize,
    /// Current test point index into the PLL frequency table.
    mid: usize,
    /// Highest frequency that passed (stable, error rate < threshold).
    best_stable_idx: Option<usize>,
    /// Whether this chip's search is complete.
    pub done: bool,
    /// Total nonces counted during search.
    total_nonces: u64,
    /// Last measured error rate.
    last_error_rate: f64,
    /// Recorded observations: (freq_mhz, error_rate_fraction) for sigmoid fitting.
    observations: Vec<(u16, f64)>,
    /// Reference to the chip-specific PLL frequency table.
    pll_table: &'static [u16],
}

impl ChipSearchState {
    fn new(chip_index: u8, min_freq: u16, max_freq: u16, pll_table: &'static [u16]) -> Self {
        let (lo, hi) = safe_pll_search_bounds(min_freq, max_freq, pll_table);
        let mid = (lo + hi) / 2;

        Self {
            chip_index,
            lo,
            hi,
            mid,
            best_stable_idx: None,
            done: false,
            total_nonces: 0,
            last_error_rate: 0.0,
            observations: Vec::new(),
            pll_table,
        }
    }

    /// Get the current test frequency.
    pub fn test_freq(&self) -> u16 {
        self.pll_table[self.mid]
    }

    /// Advance the binary search based on whether the current frequency was stable.
    fn advance(&mut self, stable: bool) {
        if stable {
            // This frequency is stable — record it and try higher
            self.best_stable_idx = Some(self.mid);
            if self.mid >= self.hi {
                self.done = true;
                return;
            }
            self.lo = self.mid + 1;
        } else {
            // This frequency is unstable — try lower
            if self.mid == 0 || self.mid <= self.lo {
                self.done = true;
                return;
            }
            self.hi = self.mid - 1;
        }

        if self.lo > self.hi {
            self.done = true;
            return;
        }

        self.mid = (self.lo + self.hi) / 2;
    }

    /// Get the final result. Returns (max_stable_mhz, grade, error_rate, total_nonces).
    fn result(&self, nominal_mhz: u16) -> ChipProfile {
        let max_stable = self
            .best_stable_idx
            .map(|idx| self.pll_table[idx])
            .unwrap_or(0);

        let grade = grade_chip(max_stable, nominal_mhz);

        ChipProfile {
            chip_index: self.chip_index,
            max_stable_mhz: max_stable,
            operating_mhz: max_stable, // safety margin applied later
            grade,
            error_rate: self.last_error_rate,
            nonces_counted: self.total_nonces,
            thermal_max_stable_mhz: None,
            vf_curve: None,
        }
    }
}

/// Determine chip grade based on max stable frequency vs nominal.
fn grade_chip(max_stable_mhz: u16, nominal_mhz: u16) -> ChipGrade {
    if max_stable_mhz == 0 {
        return ChipGrade::D;
    }
    let diff = max_stable_mhz as i32 - nominal_mhz as i32;
    if diff >= 50 {
        ChipGrade::A // Excellent: at or above nominal + 50 MHz
    } else if diff >= -25 {
        ChipGrade::B // Good: within nominal +/- 25 MHz
    } else if diff >= -100 {
        ChipGrade::C // Below average: nominal - 25 to nominal - 100
    } else {
        ChipGrade::D // Weak/damaged: more than 100 MHz below nominal
    }
}

/// Parallel binary search tuner for all chips on a chain.
pub struct BinarySearchTuner {
    config: AutoTunerConfig,
    /// Nominal frequency from config (used for grading).
    nominal_mhz: u16,
    /// Chip-specific PLL frequency table for binary search bounds.
    pll_table: &'static [u16],
    /// ASIC chip ID (e.g., 0x1387 for BM1387) for nonce rate calculations.
    chip_id: u16,
}

impl BinarySearchTuner {
    pub fn new(config: AutoTunerConfig, nominal_mhz: u16) -> Self {
        Self::new_for_chip(config, nominal_mhz, 0x1387)
    }

    /// Create a tuner for a specific chip type with its PLL frequency table.
    pub fn new_for_chip(config: AutoTunerConfig, nominal_mhz: u16, chip_id: u16) -> Self {
        let pll_table = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(chip_id);
        Self {
            config,
            nominal_mhz,
            pll_table,
            chip_id,
        }
    }

    /// Compute frequency-dependent minimum samples for a chip.
    ///
    /// At low frequencies (~200 MHz), a chip produces ~5.4 nonces per 6s window.
    /// The threshold must be REACHABLE in a single measurement window.
    /// Previous bug: `expected_per_window * 2.0` required 2x more nonces than
    /// a single window could produce, causing ALL chips to hit the "too few
    /// samples" branch and never advance the binary search (all graded D).
    ///
    /// Fix: use `expected_per_window * 0.3` with floor of 4. This means we need
    /// at least 30% of the expected nonces to make a pass/fail decision — enough
    /// to detect a dead chip but tolerant of natural variance.
    fn adaptive_min_samples(&self, freq_mhz: u16, difficulty: u32, window_s: f64) -> u64 {
        let expected_nps =
            crate::chip_geometry::expected_nps_for_chip(self.chip_id, freq_mhz, difficulty);
        let expected_per_window = expected_nps * window_s;
        (expected_per_window * 0.3).max(4.0) as u64
    }

    /// Initialize search states for all chips on a chain.
    pub fn init_search(&self, chip_count: u8) -> Vec<ChipSearchState> {
        (0..chip_count)
            .map(|i| {
                ChipSearchState::new(
                    i,
                    self.config.min_freq_mhz,
                    self.config.max_freq_mhz,
                    self.pll_table,
                )
            })
            .collect()
    }

    /// Initialize search states for specific chips only (re-characterization).
    ///
    /// Used when the aging tracker flags individual chips for re-tuning.
    /// Only the specified chip indices get new search states.
    pub fn init_search_for_chips(&self, chip_indices: &[u8]) -> Vec<ChipSearchState> {
        chip_indices
            .iter()
            .map(|&i| {
                ChipSearchState::new(
                    i,
                    self.config.min_freq_mhz,
                    self.config.max_freq_mhz,
                    self.pll_table,
                )
            })
            .collect()
    }

    fn safe_search_floor_mhz(&self) -> u16 {
        let (lo, _) = safe_pll_search_bounds(
            self.config.min_freq_mhz,
            self.config.max_freq_mhz,
            self.pll_table,
        );
        self.pll_table.get(lo).copied().unwrap_or(0)
    }

    /// Get the per-chip frequencies to set for the current search iteration.
    ///
    /// Returns a Vec of (chip_index, freq_mhz) pairs.
    /// Chips that are done searching get their best stable frequency.
    pub fn current_frequencies(&self, states: &[ChipSearchState]) -> Vec<(u8, u16)> {
        states
            .iter()
            .map(|s| {
                if s.done {
                    let freq = s
                        .best_stable_idx
                        .map(|idx| self.pll_table[idx])
                        .unwrap_or_else(|| self.safe_search_floor_mhz());
                    (s.chip_index, freq)
                } else {
                    (s.chip_index, s.test_freq())
                }
            })
            .collect()
    }

    /// Compute the recommended measurement window duration based on the
    /// lowest active chip frequency. Lower frequencies produce fewer nonces,
    /// so they need longer windows for statistical significance.
    ///
    /// `difficulty` is the current ASIC difficulty (default 256 for BM1387).
    ///
    /// Returns the window in seconds, clamped to [1.0, 10.0].
    pub fn recommended_window_s(&self, states: &[ChipSearchState], difficulty: u32) -> f64 {
        let min_active_freq = states
            .iter()
            .filter(|s| !s.done)
            .map(|s| s.test_freq())
            .min()
            .unwrap_or(self.nominal_mhz);

        let expected_nps =
            crate::chip_geometry::expected_nps_for_chip(self.chip_id, min_active_freq, difficulty);
        if expected_nps <= 0.0 {
            return 10.0;
        }

        // Live characterization should collect enough data to clear the fixed
        // noise floor used by the decision path. This makes the recommended
        // window meaningfully longer for low-frequency / high-difficulty chips.
        let needed_s = Self::MIN_NONCE_GUARD as f64 / expected_nps;
        needed_s.clamp(1.0, 10.0)
    }

    /// Process a stats snapshot and advance binary search for all chips.
    ///
    /// `difficulty` is the current ASIC difficulty (default 256 for BM1387).
    /// Used for frequency-dependent adaptive minimum sample calculation.
    ///
    /// Returns true if all chips are done searching.
    pub fn process_snapshot(
        &self,
        states: &mut [ChipSearchState],
        snapshot: &ChipStatsSnapshot,
    ) -> bool {
        self.process_snapshot_with_difficulty(states, snapshot, snapshot.current_difficulty)
    }

    /// Minimum nonce count for a measurement to be considered valid.
    /// Below this threshold, the result is treated as "insufficient data"
    /// rather than "chip is dead" — prevents false grade-D from sparse windows.
    const MIN_NONCE_GUARD: u64 = 10;

    /// Process a stats snapshot with explicit difficulty parameter.
    fn process_snapshot_with_difficulty(
        &self,
        states: &mut [ChipSearchState],
        snapshot: &ChipStatsSnapshot,
        difficulty: u32,
    ) -> bool {
        let error_threshold = self.config.error_threshold_pct;
        let recommended_window_s = self.recommended_window_s(states, difficulty);

        for state in states.iter_mut() {
            if state.done {
                continue;
            }

            let idx = state.chip_index as usize;
            if idx >= snapshot.chip_nonces.len() {
                continue;
            }

            let nonces = snapshot.chip_nonces[idx];
            let errors = snapshot.stability_error_count(idx);
            let comm_issues = snapshot.communication_issue_count(idx);
            state.total_nonces += nonces;

            // Calculate error rate
            let total = nonces + errors;
            let error_rate = if total > 0 {
                errors as f64 / total as f64 * 100.0
            } else {
                // No data in this window. If the window was long enough, this chip
                // produced zero nonces — but only treat as unstable if we've seen
                // this consistently. A single empty window can happen due to timing.
                if snapshot.window_duration_s >= recommended_window_s {
                    tracing::warn!(
                        chip = state.chip_index,
                        freq_mhz = state.test_freq(),
                        window_s = format_args!("{:.1}", snapshot.window_duration_s),
                        recommended_window_s = format_args!("{:.1}", recommended_window_s),
                        "AUTOTUNE_DIAG: chip {} got 0 nonces in {:.1}s window at {} MHz — \
                         treating as insufficient data (not marking dead)",
                        state.chip_index,
                        snapshot.window_duration_s,
                        state.test_freq(),
                    );
                    // Don't treat as 100% error — skip this iteration.
                    // If the chip is truly dead, it will get 0 nonces across
                    // multiple iterations and eventually time out.
                    continue;
                } else {
                    continue; // Window too short, wait
                }
            };
            state.last_error_rate = error_rate;

            // Minimum nonce guard: if a chip has fewer than MIN_NONCE_GUARD nonces,
            // the measurement is too noisy to make a reliable pass/fail decision.
            // Keep current frequency and wait for more data.
            if total < Self::MIN_NONCE_GUARD {
                tracing::info!(
                    chip = state.chip_index,
                    freq_mhz = state.test_freq(),
                    nonces,
                    errors,
                    total,
                    min_guard = Self::MIN_NONCE_GUARD,
                    "AUTOTUNE_DIAG: chip {} has only {} samples (need {}) at {} MHz — \
                     insufficient data, keeping current frequency",
                    state.chip_index,
                    total,
                    Self::MIN_NONCE_GUARD,
                    state.test_freq(),
                );
                continue;
            }

            // Adaptive minimum samples: scale with expected nonce rate at current frequency.
            // At low frequencies (~200 MHz), a chip may only produce ~5 nonces per
            // 6s window. This threshold must be reachable in a single window.
            let min_samples = self.adaptive_min_samples(
                state.test_freq(),
                difficulty,
                snapshot.window_duration_s.max(1.0),
            );
            if total < min_samples {
                tracing::debug!(
                    chip = state.chip_index,
                    freq_mhz = state.test_freq(),
                    samples = total,
                    min_samples,
                    "Too few samples ({}/{}) to make stable/unstable decision — waiting for next window",
                    total, min_samples,
                );
                continue;
            }

            // Record observation for sigmoid model fitting
            let error_fraction = if total > 0 {
                errors as f64 / total as f64
            } else {
                1.0
            };
            state.observations.push((state.test_freq(), error_fraction));

            let stable = error_rate < error_threshold;

            tracing::info!(
                chip = state.chip_index,
                freq_mhz = state.test_freq(),
                nonces,
                errors,
                communication_issues = comm_issues,
                error_rate = format_args!("{:.2}%", error_rate),
                stable,
                observations = state.observations.len(),
                "AUTOTUNE_DIAG: binary search step — chip {} at {} MHz: {} nonces, {} stability errors, {} comm issues, {:.2}% err, stable={}",
                state.chip_index, state.test_freq(), nonces, errors, comm_issues, error_rate, stable,
            );

            state.advance(stable);

            // Attempt sigmoid acceleration: if we have 3+ observations, try to
            // choose the next test point near the predicted cliff. The measured
            // point above has already updated the bounds; prediction only
            // changes the next `mid`, never stable/unstable truth.
            if !state.done && state.observations.len() >= 3 {
                if let Some(model) = crate::error_model::SigmoidModel::fit(&state.observations) {
                    let predicted_max = model.frequency_at_threshold(error_threshold);
                    let current_lo = state.pll_table[state.lo];
                    let current_hi = state.pll_table[state.hi];

                    // Only use prediction if it falls within the current search range
                    if predicted_max >= current_lo && predicted_max <= current_hi {
                        // Find the PLL index closest to the prediction
                        if let Some(predicted_idx) =
                            state.pll_table.iter().rposition(|&f| f <= predicted_max)
                        {
                            if predicted_idx >= state.lo && predicted_idx <= state.hi {
                                tracing::info!(
                                    chip = state.chip_index,
                                    predicted_max_mhz = predicted_max,
                                    r_squared = format_args!("{:.3}", model.r_squared),
                                    cliff_mhz = format_args!("{:.0}", model.f_cliff_mhz),
                                    "Sigmoid acceleration: predicted max stable {} MHz (R²={:.3})",
                                    predicted_max,
                                    model.r_squared,
                                );
                                state.mid = predicted_idx;
                            }
                        }
                    }
                }
            }
        }

        states.iter().all(|s| s.done)
    }

    /// Apply safety margin and produce final chip profiles.
    pub fn finalize(&self, states: &[ChipSearchState]) -> Vec<ChipProfile> {
        // Defense-in-depth for a priority-1 voltage/frequency safety value:
        // config validation already pins safety_margin_pct to (0.0, 50.0), but
        // finalize() must NOT trust that. Clamp the factor to [0.0, 1.0] at the
        // point of use so a safety margin can only ever REDUCE the operating
        // frequency toward min_freq, never push `target` above max_stable_mhz
        // (an over-clock past the stability-tested point) — regardless of whether
        // the config was validated, a future validation change, or a NaN margin
        // (NaN clamps through and the `as u16` cast saturates it to 0 => min_freq).
        let safety_factor = (1.0 - self.config.safety_margin_pct / 100.0).clamp(0.0, 1.0);

        states
            .iter()
            .map(|state| {
                let mut profile = state.result(self.nominal_mhz);

                // Apply safety margin: find the nearest PLL freq below the margin
                if profile.max_stable_mhz > 0 {
                    let target = (profile.max_stable_mhz as f64 * safety_factor) as u16;
                    // Find nearest PLL entry <= target
                    profile.operating_mhz = self
                        .pll_table
                        .iter()
                        .rev()
                        .find(|&&f| f <= target)
                        .copied()
                        .unwrap_or_else(|| self.safe_search_floor_mhz());
                } else {
                    profile.operating_mhz = self.safe_search_floor_mhz();
                    profile.grade = ChipGrade::D;
                }

                profile
            })
            .collect()
    }

    /// Check if all search states are complete.
    pub fn all_done(states: &[ChipSearchState]) -> bool {
        states.iter().all(|s| s.done)
    }

    /// Get the maximum number of binary search iterations needed.
    /// log2(table_size) rounded up.
    pub fn max_iterations(&self) -> u32 {
        let (lo, hi) = safe_pll_search_bounds(
            self.config.min_freq_mhz,
            self.config.max_freq_mhz,
            self.pll_table,
        );
        let table_size = hi.saturating_sub(lo).saturating_add(1);
        (table_size as f64).log2().ceil() as u32 + 1
    }
}

/// Post-search verification state for confirming stability at discovered frequencies.
///
/// After TABS binary search completes, each chip runs at its discovered max_stable
/// frequency for a 30-second verification window. If the error rate exceeds 0.5%
/// during verification, the chip steps down one PLL level.
///
/// This catches edge-case instability that a single measurement window might miss
/// (e.g., thermal drift during search, marginal silicon that passes 3s but fails 30s).
#[derive(Debug, Clone)]
pub struct VerificationState {
    /// Per-chip verification tracking.
    chips: Vec<ChipVerification>,
    /// Verification window duration in seconds.
    verification_window_s: f64,
    /// Error rate threshold for verification pass (fraction, not percentage).
    error_threshold: f64,
    /// PLL frequency table for step-down calculations.
    pll_table: &'static [u16],
}

#[derive(Debug, Clone)]
struct ChipVerification {
    chip_index: u8,
    /// Frequency being verified (from binary search result).
    freq_mhz: u16,
    /// Accumulated nonces during verification.
    total_nonces: u64,
    /// Accumulated errors during verification.
    total_errors: u64,
    /// Total elapsed seconds of verification data collected.
    elapsed_s: f64,
    /// Whether this chip has passed or failed verification.
    result: Option<VerificationResult>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VerificationResult {
    /// Chip passed verification at this frequency.
    Passed,
    /// Chip failed — needs to step down one PLL level.
    StepDown,
}

impl VerificationState {
    /// Create a new verification state from finalized chip profiles.
    ///
    /// `verification_window_s`: how long to verify (default 30s).
    /// `error_threshold`: maximum acceptable error rate as a fraction (default 0.005 = 0.5%).
    pub fn new(profiles: &[ChipProfile], verification_window_s: f64, error_threshold: f64) -> Self {
        Self::new_with_pll(
            profiles,
            verification_window_s,
            error_threshold,
            dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(0x1387),
        )
    }

    /// Create with a chip-specific PLL table for step-down calculations.
    pub fn new_with_pll(
        profiles: &[ChipProfile],
        verification_window_s: f64,
        error_threshold: f64,
        pll_table: &'static [u16],
    ) -> Self {
        let chips = profiles
            .iter()
            .map(|p| ChipVerification {
                chip_index: p.chip_index,
                freq_mhz: p.operating_mhz,
                total_nonces: 0,
                total_errors: 0,
                elapsed_s: 0.0,
                result: None,
            })
            .collect();

        Self {
            chips,
            verification_window_s,
            error_threshold,
            pll_table,
        }
    }

    /// Process a stats snapshot during the verification window.
    ///
    /// Returns true when all chips have accumulated enough data to decide.
    pub fn process_snapshot(&mut self, snapshot: &ChipStatsSnapshot) -> bool {
        for chip in &mut self.chips {
            if chip.result.is_some() {
                continue; // Already decided
            }

            let idx = chip.chip_index as usize;
            if idx >= snapshot.chip_nonces.len() {
                continue;
            }

            chip.total_nonces += snapshot.chip_nonces[idx];
            chip.total_errors += snapshot.stability_error_count(idx);
            chip.elapsed_s += snapshot.window_duration_s;

            // Check if we have enough verification data
            if chip.elapsed_s >= self.verification_window_s {
                let total = chip.total_nonces + chip.total_errors;
                if total == 0 {
                    // No data at all — treat as step-down (chip might be dead)
                    tracing::warn!(
                        chip = chip.chip_index,
                        freq_mhz = chip.freq_mhz,
                        elapsed_s = format_args!("{:.1}", chip.elapsed_s),
                        "Verification: chip {} got 0 nonces in {:.1}s — stepping down",
                        chip.chip_index,
                        chip.elapsed_s,
                    );
                    chip.result = Some(VerificationResult::StepDown);
                } else {
                    let error_rate = chip.total_errors as f64 / total as f64;
                    if error_rate > self.error_threshold {
                        tracing::info!(
                            chip = chip.chip_index,
                            freq_mhz = chip.freq_mhz,
                            error_rate = format_args!("{:.3}%", error_rate * 100.0),
                            nonces = chip.total_nonces,
                            errors = chip.total_errors,
                            "Verification FAILED: chip {} at {} MHz has {:.3}% error rate (threshold {:.3}%) — stepping down",
                            chip.chip_index, chip.freq_mhz, error_rate * 100.0, self.error_threshold * 100.0,
                        );
                        chip.result = Some(VerificationResult::StepDown);
                    } else {
                        tracing::info!(
                            chip = chip.chip_index,
                            freq_mhz = chip.freq_mhz,
                            error_rate = format_args!("{:.3}%", error_rate * 100.0),
                            nonces = chip.total_nonces,
                            "Verification PASSED: chip {} at {} MHz — {:.3}% error rate, stable",
                            chip.chip_index,
                            chip.freq_mhz,
                            error_rate * 100.0,
                        );
                        chip.result = Some(VerificationResult::Passed);
                    }
                }
            }
        }

        self.chips.iter().all(|c| c.result.is_some())
    }

    /// Apply verification results to chip profiles.
    ///
    /// Chips that failed verification get stepped down one PLL level.
    /// Returns the adjusted profiles.
    pub fn apply_results(&self, profiles: &mut [ChipProfile]) {
        for chip in &self.chips {
            if chip.result == Some(VerificationResult::StepDown) {
                let idx = chip.chip_index as usize;
                if idx < profiles.len() {
                    let old_freq = profiles[idx].operating_mhz;
                    // Step down one PLL level
                    let new_freq = self
                        .pll_table
                        .iter()
                        .rev()
                        .find(|&&f| f < old_freq)
                        .copied()
                        .unwrap_or(old_freq);

                    if new_freq < old_freq {
                        tracing::info!(
                            chip = chip.chip_index,
                            old_freq,
                            new_freq,
                            "Verification step-down: chip {} {} → {} MHz",
                            chip.chip_index,
                            old_freq,
                            new_freq,
                        );
                        profiles[idx].operating_mhz = new_freq;
                    }
                }
            }
        }
    }

    /// Check if all chips have completed verification.
    pub fn all_done(&self) -> bool {
        self.chips.iter().all(|c| c.result.is_some())
    }

    /// Get the number of chips that need to step down.
    pub fn step_down_count(&self) -> usize {
        self.chips
            .iter()
            .filter(|c| c.result == Some(VerificationResult::StepDown))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chip_grade() {
        assert_eq!(grade_chip(750, 650), ChipGrade::A); // +100 >= +50
        assert_eq!(grade_chip(700, 650), ChipGrade::A); // +50 >= +50
        assert_eq!(grade_chip(675, 650), ChipGrade::B); // +25, within -25..+49
        assert_eq!(grade_chip(650, 650), ChipGrade::B); // 0, within -25..+49
        assert_eq!(grade_chip(625, 650), ChipGrade::B); // -25, >= -25
        assert_eq!(grade_chip(600, 650), ChipGrade::C); // -50, >= -100
        assert_eq!(grade_chip(500, 650), ChipGrade::D); // -150, < -100
        assert_eq!(grade_chip(0, 650), ChipGrade::D); // dead
    }

    #[test]
    fn test_binary_search_converges() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            error_threshold_pct: 0.5,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let mut states = tuner.init_search(1);

        // Simulate: chip is stable up to 700 MHz, unstable at 725+
        let stable_max = 700u16;
        let mut iterations = 0;

        while !states[0].done && iterations < 20 {
            let freq = states[0].test_freq();
            let stable = freq <= stable_max;
            states[0].advance(stable);
            iterations += 1;
        }

        assert!(states[0].done);
        assert!(
            iterations <= 7,
            "Should converge in <= 7 iterations, took {}",
            iterations
        );

        let profile = states[0].result(650);
        assert_eq!(profile.max_stable_mhz, stable_max);
        assert_eq!(profile.grade, ChipGrade::A); // 700 - 650 = 50, >=50 is A
    }

    #[test]
    fn test_safety_margin() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            safety_margin_pct: 5.0,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);

        // Create a state that found max stable at 700 MHz
        let pll = tuner.pll_table;
        let mut state = ChipSearchState::new(0, 200, 800, pll);
        state.best_stable_idx = Some(pll.iter().position(|&f| f == 700).unwrap());
        state.done = true;

        let profiles = tuner.finalize(&[state]);
        // 700 * 0.95 = 665, nearest PLL entry <= 665 is 650
        assert_eq!(profiles[0].operating_mhz, 650);
    }

    #[test]
    fn finalize_safety_margin_never_overclocks_past_max_stable() {
        // The safety margin must ONLY reduce the operating frequency toward
        // min_freq — never push it above the stability-tested max_stable_mhz.
        // Config validation pins safety_margin_pct to (0.0, 50.0), but finalize()
        // clamps the factor itself so an UNVALIDATED config (direct construction,
        // a future validation change, or a NaN) can't over-clock a chip past its
        // tested-stable point. Pin that for adversarial margins.
        let max_stable = 700u16;
        for margin in [-50.0_f64, -1.0, 0.0, 5.0, 49.0, 100.0, 500.0, f64::NAN] {
            let config = AutoTunerConfig {
                min_freq_mhz: 200,
                max_freq_mhz: 800,
                safety_margin_pct: margin,
                ..Default::default()
            };
            let tuner = BinarySearchTuner::new(config, 650);
            let pll = tuner.pll_table;
            let mut state = ChipSearchState::new(0, 200, 800, pll);
            state.best_stable_idx = Some(pll.iter().position(|&f| f == max_stable).unwrap());
            state.done = true;
            let op = tuner.finalize(&[state])[0].operating_mhz;
            assert!(
                op <= max_stable,
                "safety_margin_pct={margin} over-clocked: operating {op} > max_stable {max_stable}"
            );
            assert!(
                op >= 200,
                "safety_margin_pct={margin} under-ran below min_freq: operating {op} < 200"
            );
        }
    }

    #[test]
    fn init_search_collapses_inverted_band_to_operator_ceiling() {
        // Defense-in-depth for W0-7/N-19: validate() rejects min > max at
        // config load, but the binary-search constructor must also be safe if a
        // future caller bypasses validation. The operator ceiling wins; the
        // search space collapses to the highest lockable PLL entry <= max.
        let config = AutoTunerConfig {
            min_freq_mhz: 800,
            max_freq_mhz: 200,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let expected_ceiling = tuner
            .pll_table
            .iter()
            .copied()
            .rev()
            .find(|&freq| freq <= 200)
            .unwrap_or(tuner.pll_table[0]);

        let mut states = tuner.init_search(1);
        assert_eq!(
            states[0].test_freq(),
            expected_ceiling,
            "inverted search band must not probe above the operator ceiling"
        );
        assert_eq!(
            tuner.max_iterations(),
            1,
            "collapsed single-point search should report one bounded iteration"
        );

        // Pin no-stable-result fallbacks too: they must use the resolved search
        // floor, not the raw unvalidated min_freq_mhz=800.
        states[0].done = true;
        let current = tuner.current_frequencies(&states);
        assert_eq!(current, vec![(0, expected_ceiling)]);
        let profiles = tuner.finalize(&states);
        assert_eq!(profiles[0].operating_mhz, expected_ceiling);
    }

    #[test]
    fn below_table_ceiling_uses_lowest_lockable_pll_not_table_top() {
        // If an unvalidated caller supplies a ceiling below the first PLL entry,
        // there is no frequency inside the requested band. The safest usable
        // fallback is the lowest lockable point. The old code used
        // `rposition(...).unwrap_or(pll_table.len() - 1)`, which explored the
        // table top instead.
        let config = AutoTunerConfig {
            min_freq_mhz: 900,
            max_freq_mhz: 1,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let lowest_lockable = tuner.pll_table[0];

        let mut states = tuner.init_search(1);
        assert_eq!(states[0].test_freq(), lowest_lockable);
        assert_eq!(tuner.max_iterations(), 1);

        states[0].done = true;
        assert_eq!(
            tuner.current_frequencies(&states),
            vec![(0, lowest_lockable)]
        );
        assert_eq!(tuner.finalize(&states)[0].operating_mhz, lowest_lockable);
    }

    #[test]
    fn test_pll_freqs_sorted() {
        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(0x1387);
        for i in 1..pll.len() {
            assert!(
                pll[i] > pll[i - 1],
                "pll_freqs() not sorted at index {}: {} <= {}",
                i,
                pll[i],
                pll[i - 1]
            );
        }
    }

    #[test]
    fn test_recommended_window_adaptive() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(0x1387);

        // Low frequency chip needs longer window
        let mut low_state = ChipSearchState::new(0, 200, 200, pll);
        low_state.done = false;
        let low_window = tuner.recommended_window_s(&[low_state], 16);

        // High frequency chip needs shorter window
        let mut high_state = ChipSearchState::new(0, 700, 700, pll);
        high_state.done = false;
        let high_window = tuner.recommended_window_s(&[high_state], 16);

        assert!(
            low_window > high_window,
            "Low freq ({:.1}s) should need longer window than high freq ({:.1}s)",
            low_window,
            high_window
        );
    }

    #[test]
    fn test_adaptive_min_samples() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            measurement_window_s: 6,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);

        // At difficulty 1, 200 MHz produces enough nonces in a 6s window for
        // the 30% adaptive threshold to be reachable.
        // CRITICAL: min_samples must be REACHABLE in a single window (less than expected_per_window)
        let low_min = tuner.adaptive_min_samples(200, 1, 6.0);
        assert!(
            low_min >= 4,
            "Low freq min_samples should be >= 4, got {}",
            low_min
        );
        let expected_200 = crate::chip_geometry::bm1387_expected_nps(200, 1) * 6.0;
        assert!(
            (low_min as f64) < expected_200,
            "min_samples ({}) must be reachable in a single window (expected {:.0} nonces)",
            low_min,
            expected_200,
        );

        let high_min = tuner.adaptive_min_samples(700, 1, 6.0);
        assert!(
            high_min > low_min,
            "High freq ({}) should need more samples than low freq ({})",
            high_min,
            low_min
        );

        // Higher difficulty = fewer nonces = lower min_samples
        let high_diff = tuner.adaptive_min_samples(650, 4, 6.0);
        let low_diff = tuner.adaptive_min_samples(650, 1, 6.0);
        assert!(
            high_diff < low_diff,
            "Higher diff ({}) should need fewer samples than lower diff ({})",
            high_diff,
            low_diff
        );
    }

    #[test]
    fn test_recommended_window_difficulty_aware() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(0x1387);

        let mut state = ChipSearchState::new(0, 400, 400, pll);
        state.done = false;

        // Higher difficulty should require longer windows
        let window_256 = tuner.recommended_window_s(&[state.clone()], 256);
        let window_1024 = tuner.recommended_window_s(&[state], 1024);

        assert!(
            window_1024 >= window_256,
            "Higher diff window ({:.1}s) should be >= lower diff window ({:.1}s)",
            window_1024,
            window_256
        );
    }

    #[test]
    fn test_observations_recorded() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            error_threshold_pct: 0.5,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let mut states = tuner.init_search(1);

        // Create snapshot with enough samples
        let snapshot = crate::chip_stats::ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 0,
            chip_nonces: vec![100],
            chip_errors: vec![0],
            window_duration_s: 3.0,
            timestamp: std::time::Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        tuner.process_snapshot(&mut states, &snapshot);
        assert!(
            !states[0].observations.is_empty(),
            "Should record observations for sigmoid fitting"
        );
    }

    #[test]
    fn test_process_snapshot_prefers_hw_errors_for_stability() {
        let config = AutoTunerConfig {
            min_freq_mhz: 200,
            max_freq_mhz: 800,
            measurement_window_s: 3,
            error_threshold_pct: 0.5,
            ..Default::default()
        };
        let tuner = BinarySearchTuner::new(config, 650);
        let mut states = tuner.init_search(1);
        let original_mid = states[0].mid;

        let snapshot = crate::chip_stats::ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 0,
            chip_nonces: vec![100],
            chip_errors: vec![20],
            window_duration_s: 3.0,
            timestamp: std::time::Instant::now(),
            board_temp_c: None,
            chip_hw_errors: Some(vec![0]),
            chip_timeouts: Some(vec![10]),
            chip_duplicates: Some(vec![10]),
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        tuner.process_snapshot(&mut states, &snapshot);

        assert_eq!(states[0].last_error_rate, 0.0);
        assert!(states[0].best_stable_idx == Some(original_mid));
    }

    #[test]
    fn test_verification_pass() {
        let profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 700,
            operating_mhz: 650,
            grade: ChipGrade::A,
            error_rate: 0.0,
            nonces_counted: 100,
            thermal_max_stable_mhz: None,
            vf_curve: None,
        }];

        let mut verify = VerificationState::new(&profiles, 5.0, 0.005);

        // Send 2 snapshots totaling 6s of data with 0 errors
        let snap = crate::chip_stats::ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 0,
            chip_nonces: vec![500],
            chip_errors: vec![0],
            window_duration_s: 3.0,
            timestamp: std::time::Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        assert!(!verify.process_snapshot(&snap));
        assert!(verify.process_snapshot(&snap)); // 6s > 5s threshold
        assert!(verify.all_done());
        assert_eq!(verify.step_down_count(), 0);
    }

    #[test]
    fn test_verification_fail_steps_down() {
        let mut profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 700,
            operating_mhz: 650,
            grade: ChipGrade::A,
            error_rate: 0.0,
            nonces_counted: 100,
            thermal_max_stable_mhz: None,
            vf_curve: None,
        }];

        let mut verify = VerificationState::new(&profiles, 5.0, 0.005);

        // Send snapshot with >0.5% error rate
        let snap = crate::chip_stats::ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 0,
            chip_nonces: vec![100],
            chip_errors: vec![5], // 5% error rate
            window_duration_s: 6.0,
            timestamp: std::time::Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        assert!(verify.process_snapshot(&snap));
        assert_eq!(verify.step_down_count(), 1);

        // Apply step-down
        verify.apply_results(&mut profiles);
        assert!(
            profiles[0].operating_mhz < 650,
            "Should have stepped down from 650, got {}",
            profiles[0].operating_mhz
        );
    }
}
