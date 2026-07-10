//!  thm-B — bad-sensor outlier detector (HAL-free).
//!
//! Source RE evidence:
//!
//! §1 (4-corner sensor model) + §3 (`bad_average_threshold = 2°C`,
//! `max_bad_readings = 10`, `min_per_board = 1`).
//!
//! Hashboard temperature sensors fail in two modes:
//! 1. **Stuck** — the sensor reports the same wrong value indefinitely
//!    (TMP451/NCT218/ADT7461 all do this on I²C bus contention).
//! 2. **Drift** — the sensor reads consistently higher or lower than its
//!    peers by more than `bad_average_threshold`.
//!
//! In both cases LuxOS drops the bad sensor from the per-board aggregate
//! (max-of-corners reduction), but keeps mining as long as `min_per_board`
//! sensors remain valid. We mirror this here.
//!
//! The detector is **per-board** — caller provides 4 readings (one per
//! corner of a single hashboard) and the detector decides which corners
//! are valid for that board's `max_temp_c` reduction.
//!
//! Method: Median Absolute Deviation (MAD). For each board's 4-corner
//! reading set, compute the median and the median absolute deviation. A
//! corner is flagged as an outlier when `|x - median| > bad_average_threshold`.
//! Each consecutive flagged tick increments a per-corner counter; once it
//! crosses `max_bad_readings`, the corner is dropped from the aggregate.
//! A clean tick resets the counter.
//!
//! `min_per_board` invariant: if dropping a corner would leave fewer than
//! `min_per_board` valid sensors, the drop is rejected. The miner keeps
//! mining with the LEAST-bad available corners (the runtime caller can
//! emit a WARN; bad-sensor escalation is in `failure-mode-matrix.md`).

use serde::{Deserialize, Serialize};

/// Per-corner state in the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorVerdict {
    /// Reading is within tolerance — use in aggregate.
    Healthy,
    /// Reading deviates but hasn't crossed the streak threshold yet.
    Suspect,
    /// Reading has been bad for `max_bad_readings` ticks consecutively;
    /// drop from aggregate (subject to `min_per_board` invariant).
    Dropped,
}

/// Configuration for the outlier detector.
#[derive(Debug, Clone, Copy)]
pub struct OutlierConfig {
    /// Maximum allowed deviation from median in °C before a corner is
    /// considered Suspect for this tick (LuxOS default 2.0).
    pub bad_average_threshold_c: f32,
    /// Consecutive Suspect readings before a corner is Dropped (LuxOS
    /// default 10).
    pub max_bad_readings: u32,
    /// Minimum sensors that must remain Healthy on each board (LuxOS
    /// default 1). Prevents dropping the last good sensor.
    pub min_per_board: u8,
}

impl Default for OutlierConfig {
    fn default() -> Self {
        Self {
            bad_average_threshold_c: 2.0,
            max_bad_readings: 10,
            min_per_board: 1,
        }
    }
}

/// Per-board fixed-corner detector. `N` is the corner count (typically 4
/// for S19/S19j Pro; 5 or 6 for S19 XP / Hydro variants).
#[derive(Debug, Clone)]
pub struct SensorRing<const N: usize> {
    config: OutlierConfig,
    /// Consecutive-bad-tick counter per corner.
    bad_streak: [u32; N],
    /// Latest verdict per corner.
    verdicts: [SensorVerdict; N],
}

impl<const N: usize> SensorRing<N> {
    pub fn new(config: OutlierConfig) -> Self {
        Self {
            config,
            bad_streak: [0; N],
            verdicts: [SensorVerdict::Healthy; N],
        }
    }

    pub fn fresh() -> Self {
        Self::new(OutlierConfig::default())
    }

    /// Per-corner verdicts as of the most recent `feed()`.
    pub fn verdicts(&self) -> &[SensorVerdict; N] {
        &self.verdicts
    }

    /// Consecutive-bad-tick counts.
    pub fn bad_streaks(&self) -> &[u32; N] {
        &self.bad_streak
    }

    /// Number of currently-Healthy corners.
    pub fn healthy_count(&self) -> u8 {
        self.verdicts
            .iter()
            .filter(|v| **v == SensorVerdict::Healthy)
            .count() as u8
    }

    /// Feed one tick of all-corner readings. Returns the per-corner
    /// verdict array (also accessible via `verdicts()`).
    pub fn feed(&mut self, readings: [f32; N]) -> [SensorVerdict; N] {
        if N == 0 {
            return self.verdicts;
        }

        let median = median_f32(&readings);
        // First pass: classify per-corner Suspect vs Healthy.
        let mut new_streaks = self.bad_streak;
        let mut suspect_now = [false; N];
        for i in 0..N {
            let dev = (readings[i] - median).abs();
            if dev > self.config.bad_average_threshold_c {
                suspect_now[i] = true;
                new_streaks[i] = new_streaks[i].saturating_add(1);
            } else {
                new_streaks[i] = 0;
            }
        }
        self.bad_streak = new_streaks;

        // Second pass: assign verdicts under the min_per_board invariant.
        // Count how many would-be Dropped corners exist; reject the drops
        // that would push healthy count below min_per_board.
        let would_drop_count = self
            .bad_streak
            .iter()
            .filter(|s| **s >= self.config.max_bad_readings)
            .count();
        let healthy_after_drops = N as u32 - would_drop_count as u32;
        let drops_allowed = if healthy_after_drops >= self.config.min_per_board as u32 {
            would_drop_count
        } else {
            (N as u32).saturating_sub(self.config.min_per_board as u32) as usize
        };

        // Apply verdicts. Greedily pick the `drops_allowed` worst (longest
        // streak) corners to actually drop; the rest become Suspect.
        let mut by_streak: [(usize, u32); N] = [(0, 0); N];
        for (i, (slot, streak)) in by_streak.iter_mut().zip(self.bad_streak.iter()).enumerate() {
            *slot = (i, *streak);
        }
        by_streak.sort_by(|a, b| b.1.cmp(&a.1));
        let mut dropped_so_far = 0usize;
        let mut new_verdicts = [SensorVerdict::Healthy; N];
        for (corner_idx, streak) in by_streak.iter() {
            if *streak >= self.config.max_bad_readings && dropped_so_far < drops_allowed {
                new_verdicts[*corner_idx] = SensorVerdict::Dropped;
                dropped_so_far += 1;
            } else if suspect_now[*corner_idx] {
                new_verdicts[*corner_idx] = SensorVerdict::Suspect;
            } else {
                new_verdicts[*corner_idx] = SensorVerdict::Healthy;
            }
        }
        self.verdicts = new_verdicts;
        self.verdicts
    }

    /// Effective max-temp reduction across Healthy + Suspect corners
    /// (i.e. all NOT-Dropped). Dropped corners are excluded entirely.
    /// Returns `None` if every corner is Dropped (caller treats this as
    /// "no readable sensor — escalate per failure-mode-matrix").
    pub fn effective_max_temp_c(&self, readings: &[f32; N]) -> Option<f32> {
        let mut max: Option<f32> = None;
        for (verdict, r) in self.verdicts.iter().zip(readings.iter()) {
            if *verdict != SensorVerdict::Dropped {
                max = Some(match max {
                    Some(m) if m >= *r => m,
                    _ => *r,
                });
            }
        }
        max
    }
}

/// Median of a fixed-size slice. For N=4 we sort a stack copy.
fn median_f32(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_ring_starts_all_healthy() {
        let r: SensorRing<4> = SensorRing::fresh();
        assert_eq!(r.healthy_count(), 4);
        assert!(r.verdicts().iter().all(|v| *v == SensorVerdict::Healthy));
    }

    #[test]
    fn clean_4corner_cluster_stays_healthy() {
        let mut r: SensorRing<4> = SensorRing::fresh();
        let v = r.feed([55.0, 55.5, 54.8, 55.2]);
        assert_eq!(r.healthy_count(), 4);
        assert!(v.iter().all(|x| *x == SensorVerdict::Healthy));
    }

    #[test]
    fn one_drifting_corner_becomes_suspect_first() {
        // Median of {55, 55, 55, 65} = 55. Corner 3 deviates by 10 — Suspect.
        let mut r: SensorRing<4> = SensorRing::fresh();
        let v = r.feed([55.0, 55.0, 55.0, 65.0]);
        assert_eq!(v[0], SensorVerdict::Healthy);
        assert_eq!(v[1], SensorVerdict::Healthy);
        assert_eq!(v[2], SensorVerdict::Healthy);
        assert_eq!(v[3], SensorVerdict::Suspect);
    }

    #[test]
    fn drift_for_max_bad_readings_drops_corner() {
        let mut r: SensorRing<4> = SensorRing::fresh();
        for _ in 0..10 {
            r.feed([55.0, 55.0, 55.0, 65.0]);
        }
        assert_eq!(r.verdicts()[3], SensorVerdict::Dropped);
        // Healthy count reduced by exactly one.
        assert_eq!(r.healthy_count(), 3);
    }

    #[test]
    fn clean_tick_resets_streak() {
        let mut r: SensorRing<4> = SensorRing::fresh();
        for _ in 0..9 {
            r.feed([55.0, 55.0, 55.0, 65.0]);
        }
        // One clean reading resets corner 3's streak.
        r.feed([55.0, 55.0, 55.0, 55.0]);
        assert_eq!(r.bad_streaks()[3], 0);
        assert_eq!(r.verdicts()[3], SensorVerdict::Healthy);
    }

    #[test]
    fn min_per_board_invariant_blocks_dropping_last_corner() {
        let mut r: SensorRing<4> = SensorRing::fresh();
        // Drive ALL 4 corners into outlier territory: 2-vs-2 split with
        // median at 52.5; every corner deviates by 2.5 (> threshold 2).
        for _ in 0..15 {
            r.feed([50.0, 55.0, 50.0, 55.0]);
        }
        // The invariant is min_per_board=1 NOT-Dropped sensors must
        // remain. Those sensors may be Suspect (still failing this tick)
        // — `effective_max_temp_c` will still include them in the
        // aggregate, which is the safety contract: never lose ALL
        // sensors.
        let non_dropped = r
            .verdicts()
            .iter()
            .filter(|v| **v != SensorVerdict::Dropped)
            .count();
        assert!(
            non_dropped >= 1,
            "must keep >= min_per_board (1) sensors valid; got verdicts={:?}",
            r.verdicts()
        );
        // And `effective_max_temp_c` must produce a finite result (not None).
        assert!(r.effective_max_temp_c(&[50.0, 55.0, 50.0, 55.0]).is_some());
    }

    #[test]
    fn effective_max_temp_excludes_dropped_corners() {
        let mut r: SensorRing<4> = SensorRing::fresh();
        // Drop corner 3 (the 65°C one).
        for _ in 0..10 {
            r.feed([55.0, 55.0, 55.0, 65.0]);
        }
        let m = r.effective_max_temp_c(&[55.0, 55.0, 55.0, 65.0]).unwrap();
        // Without corner 3, max is 55.
        assert_eq!(m, 55.0);
    }

    #[test]
    fn effective_max_temp_returns_none_when_all_dropped() {
        // Synthetic: manually mark all corners as Dropped and verify the
        // helper returns None. Needs a custom config that allows dropping
        // all (min_per_board = 0).
        let mut r: SensorRing<2> = SensorRing::new(OutlierConfig {
            bad_average_threshold_c: 2.0,
            max_bad_readings: 1,
            min_per_board: 0,
        });
        // Pair {50, 55}: median = 52.5; both corners 2.5 from median.
        r.feed([50.0, 55.0]);
        // Streak threshold 1, so both should drop.
        assert!(r.verdicts().iter().all(|v| *v == SensorVerdict::Dropped));
        assert!(r.effective_max_temp_c(&[50.0, 55.0]).is_none());
    }

    #[test]
    fn two_corner_tied_drift_drops_both_under_min_per_board_0() {
        // With min_per_board=0 we allow dropping all corners. Verify the
        // sort-by-streak picks both equal-streak corners as Dropped.
        let mut r: SensorRing<2> = SensorRing::new(OutlierConfig {
            bad_average_threshold_c: 2.0,
            max_bad_readings: 5,
            min_per_board: 0,
        });
        for _ in 0..5 {
            r.feed([50.0, 55.0]);
        }
        assert!(r.verdicts().iter().all(|v| *v == SensorVerdict::Dropped));
    }
}
