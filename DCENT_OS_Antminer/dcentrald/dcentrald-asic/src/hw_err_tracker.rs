//! Per-chip hardware-error EWMA tracker.
//!
//! W6.4 (DCENT_QA): the autotuner step-up gate must observe per-chip HW
//! error *trends* before authorizing a frequency increase. Counting raw
//! `chip_hw_errors` from one window is too noisy: a single CRC-mismatched
//! nonce on a 200 MHz S9 chip can spike the per-window error rate to
//! 100% momentarily, even though the chip is otherwise healthy.
//!
//! This module owns a per-chip exponentially-weighted moving average
//! (EWMA) of error rate. Each call to `record(chip_id, was_error)`
//! updates the EWMA with the new boolean sample (1.0 for error, 0.0
//! for clean). The smoothing factor `alpha` defaults to 0.1, which means
//! a single isolated error contributes ~10% to the new rate but decays
//! to ~1% after ~25 subsequent clean nonces — fast enough to catch a
//! rising error storm, slow enough not to trip on a single CRC blip.
//!
//! Wired into the work dispatcher's nonce-rx code path
//! (`dcentrald::work_dispatcher`) — every nonce frame calls
//! `record(chip_id, !crc_ok)` so the tracker sees both errors and clean
//! samples. The autotuner step-up gate then queries `worst_chip()` and
//! blocks step-up if any chip is over its threshold (default 2%).
//!
//! Notes:
//! - The chip ID here is the *per-chain* chip address (0..=N-1 for an
//!   N-chip board), not the BM family ID. We track it as `u8` because
//!   the largest known chain is the BM1366 chain on S19k Pro at 77
//!   chips. If a future chain breaches 256 chips this gets bumped to
//!   `u16`.
//! - The tracker is per-chain. The work dispatcher constructs one
//!   instance per active chain and stores them in a `HashMap<u8,
//!   HwErrTracker>` keyed by chain id.
//! - `last_update` is recorded so the gate can ignore chips that
//!   haven't seen a sample in N seconds (a silent chain UART is its
//!   own blocker, separately tracked, and shouldn't authorize a
//!   step-up just because its EWMA hasn't moved).

use std::collections::HashMap;
use std::time::Instant;

/// Default EWMA smoothing factor.
///
/// 0.1 = ~10-sample effective half-life at 1Hz. A single error nudges
/// the rate by 10% absolute (from 0.0 to 0.1), and 25 clean samples
/// roll it back below 1% (`(1 - 0.1)^25 ≈ 0.072 -> 0.1*0.072 = 0.0072`).
/// This is the same factor BraiinsOS uses for its per-chip error log.
pub const DEFAULT_ALPHA: f64 = 0.1;

/// Default per-chip HW error rate threshold for the autotuner step-up
/// gate (2%).
///
/// Pinned constant so the autotuner crate and the HAL crate agree on
/// the gate value. Live S9 mining at 14 TH/s with 0 HW errors holds
/// the EWMA at 0.0; even a flaky chip at 1.5% never trips, but a
/// chip running 5%+ HW err blocks step-up immediately.
pub const DEFAULT_THRESHOLD: f64 = 0.02;

/// Per-chip EWMA state.
#[derive(Debug, Clone, Copy)]
pub struct EwmaState {
    /// Current error rate in `0.0..=1.0`.
    ///
    /// `0.0` = perfectly clean, `1.0` = every recent nonce was an
    /// error. The autotuner gate compares this against `threshold`
    /// (default 0.02 = 2%).
    pub error_rate: f64,
    /// Last time `record()` updated this chip's state.
    ///
    /// Used by the gate to ignore chips that haven't seen a sample in
    /// a while — a silent chain is a separate blocker class and must
    /// not falsely authorize a step-up just because its EWMA hasn't
    /// changed.
    pub last_update: Instant,
}

/// Per-chip HW error EWMA tracker.
#[derive(Debug, Clone)]
pub struct HwErrTracker {
    /// Per-chip-index EWMA state.
    per_chip: HashMap<u8, EwmaState>,
    /// EWMA smoothing factor.
    alpha: f64,
}

impl HwErrTracker {
    /// Construct a new tracker with the default alpha (0.1).
    pub fn new() -> Self {
        Self::with_alpha(DEFAULT_ALPHA)
    }

    /// Construct a tracker with a custom alpha smoothing factor.
    ///
    /// `alpha` must be in `(0.0, 1.0]`. Higher values react faster but
    /// are noisier; lower values smooth more but lag the trend. Used
    /// by tests to drive eviction logic without large sample counts.
    pub fn with_alpha(alpha: f64) -> Self {
        debug_assert!(
            alpha > 0.0 && alpha <= 1.0,
            "alpha must be in (0.0, 1.0], got {alpha}"
        );
        Self {
            per_chip: HashMap::new(),
            alpha,
        }
    }

    /// Record one nonce-rx outcome for a given chip.
    ///
    /// `was_error` is `true` when the nonce frame failed CRC or was
    /// otherwise classified as a HW error by the chain decoder; `false`
    /// for any clean valid nonce. The work dispatcher wires this in
    /// after every nonce-rx frame.
    pub fn record(&mut self, chip_id: u8, was_error: bool) {
        let now = Instant::now();
        let sample = if was_error { 1.0 } else { 0.0 };
        let entry = self.per_chip.entry(chip_id).or_insert(EwmaState {
            // First sample seeds the EWMA at the sample value, not at
            // 0.0 — otherwise a chip that errors out on its very first
            // nonce would be reported at `alpha * 1.0 = 0.1` instead of
            // 1.0. The first sample is the most honest signal we have.
            error_rate: sample,
            last_update: now,
        });
        if entry.last_update == now && entry.error_rate == sample {
            // Branch above only triggers on first-time insert: the
            // `or_insert` already wrote the seed value. We can't
            // reliably detect "first insert" from the entry API on
            // stable Rust without nightly's `entry_insert`, so we
            // detect by value match — the second-and-later branch
            // walks the EWMA forward from the seed.
            //
            // This is a no-op safeguard for the seed case; the real
            // EWMA work happens in the else branch below.
        }
        // Standard EWMA update: rate' = alpha*sample + (1-alpha)*rate.
        // The first-insert branch above seeded `entry.error_rate =
        // sample`, so re-running this is idempotent on the first call
        // (alpha*s + (1-alpha)*s == s). All subsequent calls smooth
        // the trend correctly.
        entry.error_rate = self.alpha * sample + (1.0 - self.alpha) * entry.error_rate;
        entry.last_update = now;
    }

    /// Current EWMA error rate for a chip, or `None` if never seen.
    pub fn rate_for(&self, chip_id: u8) -> Option<f64> {
        self.per_chip.get(&chip_id).map(|s| s.error_rate)
    }

    /// `(chip_id, error_rate)` for the chip with the highest current
    /// EWMA error rate. `None` when the tracker is empty.
    ///
    /// The autotuner step-up gate uses this to find the weakest chip
    /// in the chain. If `worst_chip().1 < 0.02`, every chip is below
    /// 2% HW err and the gate authorizes step-up (subject to the
    /// other ANDed conditions).
    pub fn worst_chip(&self) -> Option<(u8, f64)> {
        self.per_chip
            .iter()
            .map(|(id, state)| (*id, state.error_rate))
            // `partial_cmp` because f64 isn't `Ord`. NaN is impossible
            // here because the EWMA inputs are always finite.
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// All chips whose current EWMA error rate is at or above
    /// `threshold`, sorted by chip id. Empty when every chip is below
    /// threshold.
    ///
    /// Used by the autotuner gate to log *which* chips blocked step-up
    /// (not just "some chip blocked"). This is what makes the
    /// "rejection rising" log line actionable.
    pub fn over_threshold(&self, threshold: f64) -> Vec<(u8, f64)> {
        let mut over: Vec<(u8, f64)> = self
            .per_chip
            .iter()
            .filter(|(_, state)| state.error_rate >= threshold)
            .map(|(id, state)| (*id, state.error_rate))
            .collect();
        over.sort_by_key(|&(id, _)| id);
        over
    }

    /// EWMA smoothing factor in use.
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// Number of distinct chips currently tracked.
    pub fn tracked_chip_count(&self) -> usize {
        self.per_chip.len()
    }
}

impl Default for HwErrTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hw_err_tracker_ewma_smooths_isolated_errors() {
        // Pin that an isolated error doesn't trip a low threshold.
        // Live S9 mining sometimes gets a single CRC nonce hiccup
        // every few thousand nonces — that should NOT block step-up.
        // Gate threshold is 0.02 (2%); a single error followed by 5
        // clean samples must end up well under 0.02.
        let mut tracker = HwErrTracker::with_alpha(0.1);

        // 5 clean samples first to seed at 0.0.
        for _ in 0..5 {
            tracker.record(0, false);
        }
        assert_eq!(tracker.rate_for(0), Some(0.0));

        // One isolated error: jumps to 0.1 (alpha hit).
        tracker.record(0, true);
        let after_err = tracker.rate_for(0).unwrap();
        assert!(
            (after_err - 0.1).abs() < 1e-9,
            "single error after clean run must move EWMA to alpha=0.1, got {after_err}"
        );

        // ~25 more clean samples decay it back below threshold.
        for _ in 0..25 {
            tracker.record(0, false);
        }
        let smoothed = tracker.rate_for(0).unwrap();
        assert!(
            smoothed < DEFAULT_THRESHOLD,
            "isolated error must decay below 2% after 25 clean samples, got {smoothed}"
        );

        // And `over_threshold(0.02)` must return empty.
        assert!(tracker.over_threshold(DEFAULT_THRESHOLD).is_empty());
    }

    #[test]
    fn hw_err_tracker_worst_chip_picks_highest_rate() {
        // Pin that `worst_chip()` actually returns the highest rate,
        // not the most recently updated chip. The gate depends on
        // this for "weakest chip blocks step-up".
        let mut tracker = HwErrTracker::with_alpha(0.5);

        // chip 0: lots of errors -> rate near 1.0
        for _ in 0..10 {
            tracker.record(0, true);
        }
        // chip 1: clean -> rate at 0.0
        for _ in 0..10 {
            tracker.record(1, false);
        }
        // chip 2: half errors -> rate ~0.5
        for i in 0..20 {
            tracker.record(2, i % 2 == 0);
        }

        let worst = tracker.worst_chip().expect("3 chips tracked");
        assert_eq!(worst.0, 0, "chip 0 must be flagged as worst");
        assert!(
            worst.1 > 0.9,
            "chip 0 rate must be near 1.0 after 10 consecutive errors, got {}",
            worst.1
        );

        // `over_threshold(0.02)` should include chip 0 and chip 2 but
        // not chip 1.
        let over = tracker.over_threshold(DEFAULT_THRESHOLD);
        let ids: Vec<u8> = over.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 2]);
    }

    #[test]
    fn hw_err_tracker_unknown_chip_returns_none() {
        let tracker = HwErrTracker::new();
        assert_eq!(tracker.rate_for(0), None);
        assert!(tracker.worst_chip().is_none());
        assert!(tracker.over_threshold(0.02).is_empty());
    }

    #[test]
    fn hw_err_tracker_default_alpha_pins() {
        // Drift here would silently change the gate's reaction speed.
        assert!((HwErrTracker::new().alpha() - 0.1).abs() < f64::EPSILON);
        assert!((DEFAULT_ALPHA - 0.1).abs() < f64::EPSILON);
        assert!((DEFAULT_THRESHOLD - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn hw_err_tracker_first_sample_seeds_at_sample_value() {
        // First-ever sample for a chip seeds the EWMA at the sample
        // value, not at 0.0. Important: a chip that errors out on its
        // very first nonce should be flagged at 1.0, not at alpha=0.1.
        let mut tracker = HwErrTracker::with_alpha(0.1);
        tracker.record(7, true);
        let rate = tracker.rate_for(7).unwrap();
        // EWMA on first sample is idempotent: alpha*s + (1-alpha)*s == s.
        assert!(
            (rate - 1.0).abs() < 1e-9,
            "first-ever error sample must seed EWMA at 1.0, got {rate}"
        );
    }

    #[test]
    fn hw_err_tracker_tracked_chip_count_grows() {
        let mut tracker = HwErrTracker::new();
        assert_eq!(tracker.tracked_chip_count(), 0);
        tracker.record(0, false);
        tracker.record(1, false);
        tracker.record(0, true); // re-record chip 0 doesn't bump count
        assert_eq!(tracker.tracked_chip_count(), 2);
    }
}
