//! Wattage measurement helper for the WattageDescent autotuner phase.
//!
//! Maintains a rolling-window average of power readings to provide stable
//! input to the binary-search descent. Sampling is driven by the existing
//! Telemetry heartbeat (5-15s post-phase-Q polling); the window holds the
//! last 30+ seconds of samples.
//!
//!: no `format!`, no
//! `String::new()`, no `Vec::new()` allocation in the hot path. The window
//! is a fixed-capacity ring buffer.
//!
//!: any serializer for this
//! type uses `#[derive(Serialize)]`, never the `serde_json::json!{}` macro.
//!
//! Reference: `plans/wave4-dcentaxe-wattage-autotune.md` §A (Phase 1);
//! TPS546 telemetry source `dcentaxe-hal/src/power.rs:153-202`;
//! `Telemetry::power_w` lifted via `dcentaxe/src/shared.rs:199-228`.
//!
//! Phase 1 (W5-F) lays down the API surface; Phase 2 (W5-G) wires
//! `std_dev_power_w`, `count`, `is_full`, `recent` into the bounded
//! binary-search descent and the `/api/autotuner/history` endpoint. The
//! `#[allow(dead_code)]` markers on those methods are intentional — they
//! get exercised by W5-G's tests and runtime path.

#![allow(dead_code)]

use serde::Serialize;

/// Number of slots in the rolling window. With 5-15s sample interval this
/// covers ~30-90 seconds of history. Pick 8 slots for stable 30s+ window
/// at the 5s polling rate (8 × ~5s ≈ 40s minimum, 8 × 15s = 120s ceiling).
pub const WINDOW_SIZE: usize = 8;

/// One sample point. Fields mirror the `PowerTelemetry` shape in
/// `dcentaxe-hal/src/power.rs:153-202` so a sample can be assembled
/// directly from a heartbeat snapshot.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct PowerSample {
    pub timestamp_ms: u64,
    pub power_w: f32,
    pub voltage_mv: f32,
    pub current_ma: f32,
}

/// Fixed-capacity ring buffer of samples. Index is `next_slot` (write
/// position); `count` tracks how many slots are populated.
///
/// Storage is `[PowerSample; WINDOW_SIZE]` — a flat 8-slot inline array
/// that never escapes to the heap. Total footprint ~192 B.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PowerWindow {
    samples: [PowerSample; WINDOW_SIZE],
    next_slot: usize,
    count: usize,
}

impl Default for PowerWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerWindow {
    pub const fn new() -> Self {
        Self {
            samples: [PowerSample {
                timestamp_ms: 0,
                power_w: 0.0,
                voltage_mv: 0.0,
                current_ma: 0.0,
            }; WINDOW_SIZE],
            next_slot: 0,
            count: 0,
        }
    }

    /// Push a sample. Overwrites oldest when full.
    pub fn push(&mut self, sample: PowerSample) {
        self.samples[self.next_slot] = sample;
        self.next_slot = (self.next_slot + 1) % WINDOW_SIZE;
        if self.count < WINDOW_SIZE {
            self.count += 1;
        }
    }

    /// Mean wattage across populated slots. Returns 0.0 on empty window.
    pub fn mean_power_w(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        let mut sum = 0.0_f32;
        // Iterate only populated slots — when the buffer is partial, slots
        // 0..count are valid; when full, every slot is valid.
        let active = if self.count == WINDOW_SIZE {
            WINDOW_SIZE
        } else {
            self.count
        };
        for i in 0..active {
            sum += self.samples[i].power_w;
        }
        sum / self.count as f32
    }

    /// Standard deviation (sample stddev, n-1 denominator). Returns 0.0
    /// on count < 2. Used by the convergence test in
    /// `autotuner::wattage_converged()`.
    pub fn std_dev_power_w(&self) -> f32 {
        if self.count < 2 {
            return 0.0;
        }
        let mean = self.mean_power_w();
        let mut sum_sq = 0.0_f32;
        let active = if self.count == WINDOW_SIZE {
            WINDOW_SIZE
        } else {
            self.count
        };
        for i in 0..active {
            let diff = self.samples[i].power_w - mean;
            sum_sq += diff * diff;
        }
        (sum_sq / (self.count - 1) as f32).sqrt()
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is_full(&self) -> bool {
        self.count == WINDOW_SIZE
    }

    pub fn clear(&mut self) {
        self.next_slot = 0;
        self.count = 0;
    }

    /// Returns the most recent N samples in chronological order, up to
    /// `count`. Useful for the descent history endpoint and tests.
    pub fn recent(&self, n: usize) -> impl Iterator<Item = &PowerSample> + '_ {
        let take = n.min(self.count);
        // When the buffer has wrapped, the chronological start is
        // (next_slot - take) mod WINDOW_SIZE; when partial, slot 0 is the
        // oldest and slot (count - 1) is the newest.
        let start = if self.count == WINDOW_SIZE {
            (self.next_slot + WINDOW_SIZE - take) % WINDOW_SIZE
        } else {
            self.count - take
        };
        (0..take).map(move |i| &self.samples[(start + i) % WINDOW_SIZE])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: u64, power: f32) -> PowerSample {
        PowerSample {
            timestamp_ms: ts,
            power_w: power,
            voltage_mv: 5000.0,
            current_ma: 1000.0,
        }
    }

    #[test]
    fn empty_window_mean_is_zero() {
        let w = PowerWindow::new();
        assert_eq!(w.count(), 0);
        assert_eq!(w.mean_power_w(), 0.0);
        assert_eq!(w.std_dev_power_w(), 0.0);
        assert!(!w.is_full());
    }

    #[test]
    fn push_fills_slots_in_order() {
        let mut w = PowerWindow::new();
        w.push(sample(1000, 10.0));
        w.push(sample(2000, 11.0));
        w.push(sample(3000, 12.0));
        assert_eq!(w.count(), 3);
        assert!(!w.is_full());
        // (10 + 11 + 12) / 3 = 11.0
        assert!((w.mean_power_w() - 11.0).abs() < 1e-5);
    }

    #[test]
    fn push_wraps_at_capacity() {
        let mut w = PowerWindow::new();
        // Push WINDOW_SIZE + 2 samples; oldest two get overwritten.
        for i in 0..(WINDOW_SIZE as u64 + 2) {
            w.push(sample(i * 1000, i as f32));
        }
        assert_eq!(w.count(), WINDOW_SIZE);
        assert!(w.is_full());
        // Slots now hold powers [8.0, 9.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
        // (slots 0,1 were overwritten by the wrap). Mean = (8+9+2+3+4+5+6+7)/8 = 44/8 = 5.5
        assert!((w.mean_power_w() - 5.5).abs() < 1e-5);
    }

    #[test]
    fn mean_computes_over_filled_slots_only() {
        // Partial fill: only 4 of 8 slots have data; mean uses those 4.
        let mut w = PowerWindow::new();
        w.push(sample(1, 20.0));
        w.push(sample(2, 20.0));
        w.push(sample(3, 20.0));
        w.push(sample(4, 20.0));
        // Confirm uninitialized slots (5..8) do NOT bleed 0.0 into the mean.
        assert!((w.mean_power_w() - 20.0).abs() < 1e-5);
        assert_eq!(w.count(), 4);
    }

    #[test]
    fn std_dev_zero_for_constant_series() {
        let mut w = PowerWindow::new();
        for _ in 0..6 {
            w.push(sample(0, 15.0));
        }
        // All identical → std dev = 0.
        assert!(w.std_dev_power_w().abs() < 1e-5);
    }

    #[test]
    fn std_dev_nonzero_for_varying_series() {
        // Series: 10, 12, 14, 16. Mean = 13. Sample stddev = sqrt((9+1+1+9)/3) = sqrt(20/3) ≈ 2.582
        let mut w = PowerWindow::new();
        w.push(sample(0, 10.0));
        w.push(sample(0, 12.0));
        w.push(sample(0, 14.0));
        w.push(sample(0, 16.0));
        let sd = w.std_dev_power_w();
        assert!((sd - 2.5819888).abs() < 1e-3, "got std_dev {}", sd);
    }

    #[test]
    fn recent_returns_chronological_order() {
        let mut w = PowerWindow::new();
        for i in 0..(WINDOW_SIZE as u64 + 3) {
            w.push(sample(i, i as f32));
        }
        // Buffer wrapped 3 times into slots 0,1,2.
        // Most-recent-3 chronologically = powers 8.0, 9.0, 10.0
        let last_three: Vec<f32> = w.recent(3).map(|s| s.power_w).collect();
        assert_eq!(last_three, vec![8.0, 9.0, 10.0]);
    }

    #[test]
    fn recent_clamps_to_count_when_partial() {
        let mut w = PowerWindow::new();
        w.push(sample(0, 1.0));
        w.push(sample(0, 2.0));
        // Asking for 5 should return only the 2 we have.
        let got: Vec<f32> = w.recent(5).map(|s| s.power_w).collect();
        assert_eq!(got, vec![1.0, 2.0]);
    }

    #[test]
    fn clear_resets_state() {
        let mut w = PowerWindow::new();
        for i in 0..5 {
            w.push(sample(i, 5.0));
        }
        w.clear();
        assert_eq!(w.count(), 0);
        assert_eq!(w.mean_power_w(), 0.0);
    }
}
