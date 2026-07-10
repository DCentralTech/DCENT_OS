//! Long-term chip degradation tracking.
//!
//! Monitors per-chip error rate trends using an exponential moving average (EMA)
//! over many monitoring windows. When a chip's EMA error rate stays above a
//! threshold for a sustained number of consecutive windows, it is flagged for
//! re-characterization — its max stable frequency has likely decreased due to
//! aging, electromigration, or thermal cycling.
//!
//! This is distinct from the short-term back-off in `background_monitor()` which
//! handles transient error spikes. The aging tracker looks at slow, persistent
//! degradation trends over hours/days.
//!
//! Phase 5 MVP: flags chips and logs warnings. Full re-characterization of
//! individual chips (re-running binary search for just the affected chip) is
//! planned for a future iteration.

use std::collections::HashMap;

use crate::chip_stats::ChipStatsSnapshot;

/// Tracks per-chip performance degradation over time.
///
/// Uses an exponential moving average (EMA) of error rates to detect slow
/// degradation. The EMA smoothing factor is intentionally small (default 0.01)
/// so the average adapts slowly — it takes ~100 windows to converge, which
/// means it tracks trends over hours, not seconds.
pub struct AgingTracker {
    /// Per-chip rolling average error rate (EMA).
    /// Key: (chain_id, chip_index), Value: EMA error rate (fraction, 0.0-1.0).
    chip_ema_error_rates: HashMap<(u8, u8), f64>,

    /// EMA smoothing factor. Small = slow-moving average.
    /// 0.01 means each new sample contributes only 1% to the average.
    ema_alpha: f64,

    /// Threshold for flagging re-characterization (fraction, not percent).
    /// When EMA error rate exceeds this, the chip is considered degraded.
    rechar_threshold: f64,

    /// Number of consecutive high-EMA windows before triggering re-characterization.
    rechar_trigger_count: u32,

    /// Per-chip consecutive high-EMA counter.
    chip_high_count: HashMap<(u8, u8), u32>,

    /// Chips currently flagged for re-characterization.
    /// Cleared when the chip is re-tuned.
    pub needs_rechar: Vec<(u8, u8)>,
}

impl AgingTracker {
    /// Create a new aging tracker with default parameters.
    ///
    /// Defaults:
    ///   - ema_alpha: 0.01 (adapts over ~100 windows = ~100 minutes at 60s intervals)
    ///   - rechar_threshold: 0.3% error rate (matches 0.3% which is below the
    ///     0.5% back-off threshold but indicates trending upward)
    ///   - rechar_trigger_count: 10 consecutive high windows before flagging
    pub fn new() -> Self {
        Self {
            chip_ema_error_rates: HashMap::new(),
            ema_alpha: 0.01,
            rechar_threshold: 0.003, // 0.3% error rate
            rechar_trigger_count: 10,
            chip_high_count: HashMap::new(),
            needs_rechar: Vec::new(),
        }
    }

    /// Create an aging tracker with custom parameters.
    pub fn with_params(ema_alpha: f64, rechar_threshold: f64, trigger_count: u32) -> Self {
        Self {
            chip_ema_error_rates: HashMap::new(),
            ema_alpha,
            rechar_threshold,
            rechar_trigger_count: trigger_count,
            chip_high_count: HashMap::new(),
            needs_rechar: Vec::new(),
        }
    }

    /// Update with a new stats snapshot.
    ///
    /// Computes per-chip error rates from the snapshot, updates each chip's
    /// EMA, and checks if any chips have crossed the re-characterization
    /// threshold for a sustained period.
    ///
    /// Returns a list of (chain_id, chip_index) pairs that need re-characterization.
    /// This list only contains newly-flagged chips (not previously flagged ones).
    pub fn update(&mut self, snapshot: &ChipStatsSnapshot) -> Vec<(u8, u8)> {
        let mut newly_flagged = Vec::new();

        for i in 0..snapshot.chip_nonces.len() {
            let chip_idx = i as u8;
            let key = (snapshot.chain_id, chip_idx);

            let nonces = snapshot.chip_nonces[i];
            let errors = snapshot.chip_errors[i];
            let total = nonces + errors;

            // Skip chips with no data this window
            if total == 0 {
                continue;
            }

            let error_rate = errors as f64 / total as f64;

            // Update EMA
            let ema = self.chip_ema_error_rates.entry(key).or_insert(error_rate);
            *ema = self.ema_alpha * error_rate + (1.0 - self.ema_alpha) * *ema;

            let current_ema = *ema;

            // Check if EMA exceeds threshold
            if current_ema > self.rechar_threshold {
                let count = self.chip_high_count.entry(key).or_insert(0);
                *count += 1;

                if *count >= self.rechar_trigger_count {
                    // Check if already flagged
                    if !self.needs_rechar.contains(&key) {
                        self.needs_rechar.push(key);
                        newly_flagged.push(key);
                    }
                }
            } else {
                // Reset counter when EMA drops below threshold
                self.chip_high_count.insert(key, 0);
            }
        }

        newly_flagged
    }

    /// Clear re-characterization flag for a chip (after re-tuning).
    pub fn clear_rechar(&mut self, chain_id: u8, chip_index: u8) {
        let key = (chain_id, chip_index);
        self.needs_rechar.retain(|k| *k != key);
        self.chip_high_count.insert(key, 0);
    }

    /// Get the current EMA error rate for a chip.
    ///
    /// Returns None if no data has been recorded for this chip yet.
    pub fn chip_ema(&self, chain_id: u8, chip_index: u8) -> Option<f64> {
        self.chip_ema_error_rates
            .get(&(chain_id, chip_index))
            .copied()
    }

    /// Get the number of chips currently flagged for re-characterization.
    pub fn flagged_count(&self) -> usize {
        self.needs_rechar.len()
    }
}

impl Default for AgingTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_snapshot(chain_id: u8, nonces: Vec<u64>, errors: Vec<u64>) -> ChipStatsSnapshot {
        ChipStatsSnapshot {
            chain_id,
            measurement_epoch: 0,
            chip_nonces: nonces,
            chip_errors: errors,
            window_duration_s: 60.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        }
    }

    #[test]
    fn test_ema_converges_to_zero() {
        let mut tracker = AgingTracker::with_params(0.1, 0.003, 3);

        // Feed many windows with zero errors
        for _ in 0..100 {
            let snapshot = make_snapshot(6, vec![100], vec![0]);
            tracker.update(&snapshot);
        }

        let ema = tracker.chip_ema(6, 0).unwrap();
        assert!(ema < 0.001, "EMA should converge near zero, got {}", ema);
    }

    #[test]
    fn test_ema_converges_to_error_rate() {
        // Use a faster alpha so convergence happens within 100 iterations
        let mut tracker = AgingTracker::with_params(0.1, 0.003, 3);

        // Feed windows with 1% error rate
        for _ in 0..200 {
            let snapshot = make_snapshot(6, vec![99], vec![1]);
            tracker.update(&snapshot);
        }

        let ema = tracker.chip_ema(6, 0).unwrap();
        let expected = 0.01; // 1/100 = 1%
        assert!(
            (ema - expected).abs() < 0.002,
            "EMA should converge to ~{}, got {}",
            expected,
            ema,
        );
    }

    #[test]
    fn test_rechar_trigger_after_sustained_high() {
        let mut tracker = AgingTracker::with_params(0.5, 0.003, 3);

        // Feed high error rate windows — EMA should converge quickly with alpha=0.5
        for i in 0..10 {
            let snapshot = make_snapshot(6, vec![90], vec![10]); // 10% error rate
            let flagged = tracker.update(&snapshot);

            if i >= 2 {
                // After 3 consecutive high windows, should trigger
                if !flagged.is_empty() {
                    assert_eq!(flagged[0], (6, 0));
                    return; // Test passed
                }
            }
        }

        // Should have been flagged by now
        assert!(
            tracker.needs_rechar.contains(&(6, 0)),
            "Chip should be flagged for re-characterization"
        );
    }

    #[test]
    fn test_rechar_not_triggered_below_threshold() {
        let mut tracker = AgingTracker::with_params(0.5, 0.003, 3);

        // Feed windows with very low error rate (0.1%)
        for _ in 0..20 {
            let snapshot = make_snapshot(6, vec![999], vec![1]); // 0.1% error rate
            let flagged = tracker.update(&snapshot);
            assert!(
                flagged.is_empty(),
                "Should not flag chip with low error rate"
            );
        }

        assert!(tracker.needs_rechar.is_empty());
    }

    #[test]
    fn test_clear_rechar() {
        let mut tracker = AgingTracker::with_params(0.9, 0.003, 1);

        // Immediately flag a chip
        let snapshot = make_snapshot(6, vec![90], vec![10]);
        tracker.update(&snapshot);

        assert!(tracker.needs_rechar.contains(&(6, 0)));

        // Clear it
        tracker.clear_rechar(6, 0);
        assert!(!tracker.needs_rechar.contains(&(6, 0)));
        assert_eq!(tracker.flagged_count(), 0);
    }

    #[test]
    fn test_counter_resets_when_error_drops() {
        let mut tracker = AgingTracker::with_params(0.5, 0.003, 5);

        // Feed 3 high windows
        for _ in 0..3 {
            let snapshot = make_snapshot(6, vec![90], vec![10]);
            tracker.update(&snapshot);
        }

        // Now feed low error rate — counter should reset
        for _ in 0..5 {
            let snapshot = make_snapshot(6, vec![1000], vec![0]);
            tracker.update(&snapshot);
        }

        // Feed 3 more high windows — should not trigger yet (counter was reset)
        for _ in 0..3 {
            let snapshot = make_snapshot(6, vec![90], vec![10]);
            let flagged = tracker.update(&snapshot);
            assert!(flagged.is_empty(), "Should not flag after counter reset");
        }
    }

    #[test]
    fn test_multiple_chips() {
        let mut tracker = AgingTracker::with_params(0.9, 0.003, 1);

        // Chip 0 has high errors, chip 1 is fine
        let snapshot = make_snapshot(6, vec![90, 1000], vec![10, 0]);
        let flagged = tracker.update(&snapshot);

        assert!(flagged.contains(&(6, 0)), "Chip 0 should be flagged");
        assert!(!flagged.contains(&(6, 1)), "Chip 1 should not be flagged");
    }

    #[test]
    fn test_skip_zero_data_chips() {
        let mut tracker = AgingTracker::new();

        // Chip with no data should not be tracked
        let snapshot = make_snapshot(6, vec![0], vec![0]);
        tracker.update(&snapshot);

        assert!(tracker.chip_ema(6, 0).is_none());
    }

    #[test]
    fn test_chip_ema_returns_none_for_unknown() {
        let tracker = AgingTracker::new();
        assert!(tracker.chip_ema(99, 99).is_none());
    }

    #[test]
    fn test_no_duplicate_flags() {
        let mut tracker = AgingTracker::with_params(0.9, 0.003, 1);

        // Flag the same chip multiple times
        for _ in 0..5 {
            let snapshot = make_snapshot(6, vec![90], vec![10]);
            tracker.update(&snapshot);
        }

        // Should only appear once in needs_rechar
        let count = tracker
            .needs_rechar
            .iter()
            .filter(|&&k| k == (6, 0))
            .count();
        assert_eq!(count, 1, "Chip should only be flagged once");
    }
}
