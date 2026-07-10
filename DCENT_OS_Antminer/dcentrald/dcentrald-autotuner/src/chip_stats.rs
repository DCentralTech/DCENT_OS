//! Per-chip nonce and error statistics.
//!
//! The work dispatcher sends `ChipStatsSnapshot` to the autotuner via an mpsc
//! channel. Each snapshot contains per-chip nonce and error counts for one
//! measurement window on one chain.

use std::time::Instant;

pub const BOARD_TEMP_STALE_TIMEOUT_S: u64 = 30;

/// A snapshot of per-chip statistics for one chain over one measurement window.
#[derive(Debug, Clone)]
pub struct ChipStatsSnapshot {
    /// Which chain this snapshot is from.
    pub chain_id: u8,
    /// Per-chain measurement epoch.
    pub measurement_epoch: u64,
    /// Per-chip valid nonce counts during this window.
    pub chip_nonces: Vec<u64>,
    /// Per-chip hardware error counts during this window.
    pub chip_errors: Vec<u64>,
    /// Duration of the measurement window in seconds.
    pub window_duration_s: f64,
    /// Timestamp when this snapshot was taken.
    pub timestamp: Instant,
    /// Optional board temperature reading (degrees C), if available.
    ///
    /// Read from BM1387 register 0x20 (I2C passthrough) via TMP451/ADT7461/NCT218
    /// temperature sensors on the hash board. None if sensor read failed or
    /// sensor type is not detected.
    pub board_temp_c: Option<f32>,
    /// Per-chip hardware error counts (CRC failures — frequency too high).
    /// Distinct from timeouts and duplicates for smarter backoff decisions.
    #[allow(dead_code)]
    pub chip_hw_errors: Option<Vec<u64>>,
    /// Per-chip timeout counts (no response — communication issue, NOT frequency).
    #[allow(dead_code)]
    pub chip_timeouts: Option<Vec<u64>>,
    /// Per-chip duplicate nonce counts (stale work_id — WORK_TIME misconfigured).
    #[allow(dead_code)]
    pub chip_duplicates: Option<Vec<u64>>,
    /// Current ASIC difficulty setting for nonce rate calculations.
    ///
    /// Default is 256 for BM1387 initial setup. All nonce rate calculations
    /// in binary_search, chip_health, and background monitor must divide by
    /// actual difficulty instead of hardcoding 256. Prevents false backoffs
    /// when pool changes difficulty.
    #[allow(dead_code)]
    pub current_difficulty: u32,
    /// Per-chip temperature readings (degrees C) from BM1387 on-die thermal diode.
    ///
    /// Accessed via ASIC register 0x20 I2C passthrough to TMP451/ADT7461/NCT218.
    /// Enables per-chip thermal derating instead of board-level approximation.
    /// None if per-chip temp sensing is not available or not yet implemented.
    pub chip_temps_c: Option<Vec<f32>>,
    /// Actual PSU power reading from PMBus READ_POUT (watts).
    ///
    /// When available, used to calibrate the power model (C_eff) for ±1% accuracy
    /// instead of the ±10% theoretical model. Set by the work dispatcher when
    /// PMBus communication is established.
    pub psu_power_w: Option<f64>,
}

/// Per-chip nonce tracker maintained by the work dispatcher.
///
/// Accumulates per-chip nonce and error counts, then produces snapshots
/// at configurable intervals for the autotuner to consume.
///
/// Error types are tracked separately so the autotuner can make smarter
/// decisions:
/// - HW errors (CRC failures): frequency too high, chip can't keep up
/// - Timeouts: communication issue (UART contention, not chip's fault)
/// - Duplicates: stale work_id from WORK_TIME misconfiguration
pub struct ChipNonceTracker {
    /// Per-chain, per-chip valid nonce counts in current window.
    /// Indexed as [chain_idx][chip_idx].
    chip_nonces: Vec<Vec<u64>>,
    /// Per-chain, per-chip error counts in current window (all types combined).
    /// Indexed as [chain_idx][chip_idx].
    chip_errors: Vec<Vec<u64>>,
    /// Per-chain, per-chip HW error counts (CRC failures — frequency too high).
    /// Indexed as [chain_idx][chip_idx].
    chip_hw_errors: Vec<Vec<u64>>,
    /// Per-chain, per-chip timeout counts (no response — communication issue).
    /// Indexed as [chain_idx][chip_idx].
    chip_timeouts: Vec<Vec<u64>>,
    /// Per-chain, per-chip duplicate nonce counts (stale work_id).
    /// Indexed as [chain_idx][chip_idx].
    chip_duplicates: Vec<Vec<u64>>,
    /// Chain IDs corresponding to each chain index.
    chain_ids: Vec<u8>,
    /// Per-chain window start time.
    window_starts: Vec<Instant>,
    /// Per-chain measurement epoch.
    measurement_epochs: Vec<u64>,
    /// Snapshot interval.
    snapshot_interval_s: u64,
    /// ASIC hardware difficulty (TicketMask + 1). Default 256 for BM1387.
    /// This is the difficulty used for per-chip nonce rate calculations in the
    /// autotuner. It must be the ASIC's TicketMask difficulty, NOT the pool's
    /// share difficulty — using pool difficulty (e.g., 8192) makes expected
    /// nonce counts 32x too low, causing unstable chips to look healthy.
    hw_difficulty: u32,
    /// Current pool difficulty — kept for informational purposes but NOT used
    /// in autotuner nonce rate calculations. Updated by the work dispatcher
    /// when new jobs arrive.
    current_pool_difficulty: f64,
    /// Per-chain board temperature readings (degrees C).
    /// Updated by the work dispatcher every 30 seconds via BM1387 I2C passthrough.
    board_temps: Vec<Option<f32>>,
    /// Timestamp of the last valid board temperature per chain.
    board_temp_seen_at: Vec<Option<Instant>>,
}

impl ChipNonceTracker {
    /// Create a new tracker for the given chains.
    ///
    /// `chains` is a list of (chain_id, chip_count) pairs.
    pub fn new(chains: &[(u8, u8)], snapshot_interval_s: u64) -> Self {
        let now = Instant::now();
        let chip_nonces = chains
            .iter()
            .map(|&(_, count)| vec![0u64; count as usize])
            .collect();
        let chip_errors = chains
            .iter()
            .map(|&(_, count)| vec![0u64; count as usize])
            .collect();
        let chip_hw_errors = chains
            .iter()
            .map(|&(_, count)| vec![0u64; count as usize])
            .collect();
        let chip_timeouts = chains
            .iter()
            .map(|&(_, count)| vec![0u64; count as usize])
            .collect();
        let chip_duplicates = chains
            .iter()
            .map(|&(_, count)| vec![0u64; count as usize])
            .collect();
        let chain_ids = chains.iter().map(|&(id, _)| id).collect();
        let board_temps = vec![None; chains.len()];
        let board_temp_seen_at = vec![None; chains.len()];
        let window_starts = vec![now; chains.len()];
        let measurement_epochs = vec![0; chains.len()];

        Self {
            chip_nonces,
            chip_errors,
            chip_hw_errors,
            chip_timeouts,
            chip_duplicates,
            chain_ids,
            window_starts,
            measurement_epochs,
            snapshot_interval_s,
            hw_difficulty: 256,
            current_pool_difficulty: 256.0,
            board_temps,
            board_temp_seen_at,
        }
    }

    fn reset_chain_counters(&mut self, chain_idx: usize) {
        self.chip_nonces[chain_idx].fill(0);
        self.chip_errors[chain_idx].fill(0);
        self.chip_hw_errors[chain_idx].fill(0);
        self.chip_timeouts[chain_idx].fill(0);
        self.chip_duplicates[chain_idx].fill(0);
    }

    /// Record a valid nonce from a specific chip.
    pub fn record_nonce(&mut self, chain_idx: usize, chip_index: u8) {
        if chain_idx < self.chip_nonces.len() {
            let chip_idx = chip_index as usize;
            if chip_idx < self.chip_nonces[chain_idx].len() {
                self.chip_nonces[chain_idx][chip_idx] += 1;
            }
        }
    }

    /// Record a hardware error from a specific chip (legacy — increments combined counter).
    pub fn record_error(&mut self, chain_idx: usize, chip_index: u8) {
        if chain_idx < self.chip_errors.len() {
            let chip_idx = chip_index as usize;
            if chip_idx < self.chip_errors[chain_idx].len() {
                self.chip_errors[chain_idx][chip_idx] += 1;
            }
        }
    }

    /// Record a HW error (CRC failure — frequency too high, chip can't keep up).
    /// Also increments the combined error counter for backward compatibility.
    pub fn record_hw_error(&mut self, chain_idx: usize, chip_index: u8) {
        if chain_idx < self.chip_hw_errors.len() {
            let chip_idx = chip_index as usize;
            if chip_idx < self.chip_hw_errors[chain_idx].len() {
                self.chip_hw_errors[chain_idx][chip_idx] += 1;
            }
        }
        self.record_error(chain_idx, chip_index);
    }

    /// Record a timeout (no ASIC response — communication issue, not chip's fault).
    /// Also increments the combined error counter for backward compatibility.
    pub fn record_timeout(&mut self, chain_idx: usize, chip_index: u8) {
        if chain_idx < self.chip_timeouts.len() {
            let chip_idx = chip_index as usize;
            if chip_idx < self.chip_timeouts[chain_idx].len() {
                self.chip_timeouts[chain_idx][chip_idx] += 1;
            }
        }
        self.record_error(chain_idx, chip_index);
    }

    /// Record a duplicate nonce (stale work_id — WORK_TIME misconfiguration).
    /// Also increments the combined error counter for backward compatibility.
    pub fn record_duplicate(&mut self, chain_idx: usize, chip_index: u8) {
        if chain_idx < self.chip_duplicates.len() {
            let chip_idx = chip_index as usize;
            if chip_idx < self.chip_duplicates[chain_idx].len() {
                self.chip_duplicates[chain_idx][chip_idx] += 1;
            }
        }
        self.record_error(chain_idx, chip_index);
    }

    /// Reset one chain's counters and start a fresh measurement epoch.
    pub fn begin_measurement(&mut self, chain_id: u8) -> Option<u64> {
        let chain_idx = self.chain_ids.iter().position(|&id| id == chain_id)?;
        self.measurement_epochs[chain_idx] = self.measurement_epochs[chain_idx].saturating_add(1);
        self.reset_chain_counters(chain_idx);
        self.window_starts[chain_idx] = Instant::now();
        Some(self.measurement_epochs[chain_idx])
    }

    /// Set the ASIC hardware difficulty (TicketMask + 1).
    ///
    /// This is the difficulty used in autotuner nonce rate calculations.
    /// Default is 256 (BM1387 initial TicketMask). Must be set at construction
    /// or early init — it should NOT change during operation.
    pub fn set_hw_difficulty(&mut self, difficulty: u32) {
        if difficulty > 0 {
            self.hw_difficulty = difficulty;
        }
    }

    /// Update the current pool difficulty.
    ///
    /// Called by the work dispatcher when a new job arrives with a different
    /// difficulty. Kept for informational purposes but NOT used in autotuner
    /// nonce rate calculations (those use hw_difficulty instead).
    pub fn set_pool_difficulty(&mut self, difficulty: f64) {
        if difficulty > 0.0 {
            self.current_pool_difficulty = difficulty;
        }
    }

    /// Update board temperature for a specific chain.
    ///
    /// Called by the work dispatcher every 30 seconds after reading the
    /// BM1387 I2C passthrough temperature sensor.
    pub fn set_board_temp(&mut self, chain_idx: usize, temp_c: f32) {
        if chain_idx < self.board_temps.len() {
            self.board_temps[chain_idx] = Some(temp_c);
            self.board_temp_seen_at[chain_idx] = Some(Instant::now());
        }
    }

    /// Clear board temperature for a specific chain after a failed sensor read.
    pub fn clear_board_temp(&mut self, chain_idx: usize) {
        if chain_idx < self.board_temps.len() {
            self.board_temps[chain_idx] = None;
            self.board_temp_seen_at[chain_idx] = None;
        }
    }

    /// Check if the snapshot interval has elapsed and produce snapshots if so.
    ///
    /// Returns a Vec of snapshots (one per chain) and resets the window.
    /// Returns empty Vec if the interval hasn't elapsed yet.
    pub fn try_snapshot(&mut self) -> Vec<ChipStatsSnapshot> {
        let mut snapshots = Vec::with_capacity(self.chain_ids.len());

        for idx in 0..self.chain_ids.len() {
            let chain_id = self.chain_ids[idx];
            let elapsed = self.window_starts[idx].elapsed();
            if elapsed.as_secs() < self.snapshot_interval_s {
                continue;
            }

            let duration_s = elapsed.as_secs_f64();
            let now = Instant::now();
            let chip_count = self.chip_nonces[idx].len();

            // Populate per-chip HW errors if any were recorded in this window.
            // Use None (not Some(vec![0; N])) when all zeros to avoid wasting
            // memory in the snapshot — the autotuner checks .is_some() first.
            let hw_errors = if self.chip_hw_errors[idx].iter().any(|&e| e > 0) {
                Some(self.chip_hw_errors[idx].clone())
            } else {
                None
            };
            let timeouts = if self.chip_timeouts[idx].iter().any(|&e| e > 0) {
                Some(self.chip_timeouts[idx].clone())
            } else {
                None
            };
            let duplicates = if self.chip_duplicates[idx].iter().any(|&e| e > 0) {
                Some(self.chip_duplicates[idx].clone())
            } else {
                None
            };

            // Board temp: use per-chain reading if available.
            // When board_temp_c is set on the snapshot, the autotuner uses it
            // for thermal compensation. Per-chip temps (chip_temps_c) approximate
            // from the single board sensor — one sensor per board, not per chip.
            let board_temp = match (
                self.board_temps.get(idx).copied().flatten(),
                self.board_temp_seen_at.get(idx).copied().flatten(),
            ) {
                (Some(temp), Some(last_seen))
                    if last_seen.elapsed().as_secs() <= BOARD_TEMP_STALE_TIMEOUT_S =>
                {
                    Some(temp)
                }
                _ => None,
            };
            let chip_temps = board_temp.map(|t| vec![t; chip_count]);

            snapshots.push(ChipStatsSnapshot {
                chain_id,
                measurement_epoch: self.measurement_epochs[idx],
                chip_nonces: self.chip_nonces[idx].clone(),
                chip_errors: self.chip_errors[idx].clone(),
                window_duration_s: duration_s,
                timestamp: now,
                board_temp_c: board_temp,
                chip_hw_errors: hw_errors,
                chip_timeouts: timeouts,
                chip_duplicates: duplicates,
                current_difficulty: self.hw_difficulty,
                chip_temps_c: chip_temps,
                psu_power_w: None,
            });

            self.reset_chain_counters(idx);
            self.window_starts[idx] = now;
        }

        snapshots
    }
}

impl ChipStatsSnapshot {
    #[inline]
    pub fn hw_error_count(&self, chip_idx: usize) -> Option<u64> {
        self.chip_hw_errors
            .as_ref()
            .and_then(|errors| errors.get(chip_idx))
            .copied()
    }

    #[inline]
    pub fn timeout_count(&self, chip_idx: usize) -> u64 {
        self.chip_timeouts
            .as_ref()
            .and_then(|errors| errors.get(chip_idx))
            .copied()
            .unwrap_or(0)
    }

    #[inline]
    pub fn duplicate_count(&self, chip_idx: usize) -> u64 {
        self.chip_duplicates
            .as_ref()
            .and_then(|errors| errors.get(chip_idx))
            .copied()
            .unwrap_or(0)
    }

    /// Errors that should affect frequency stability decisions.
    ///
    /// Prefer explicit HW/CRC counts when the dispatcher provides them. Combined
    /// `chip_errors` includes timeouts and duplicate nonces, which are often
    /// communication-path issues rather than silicon frequency instability.
    #[inline]
    pub fn stability_error_count(&self, chip_idx: usize) -> u64 {
        if let Some(hw_errors) = &self.chip_hw_errors {
            return hw_errors.get(chip_idx).copied().unwrap_or(0);
        }

        // If typed communication counters are present, this is a modern
        // snapshot. Do not let timeout/duplicate-only windows masquerade as
        // silicon instability just because `chip_errors` is maintained as a
        // legacy combined counter.
        if self.chip_timeouts.is_some() || self.chip_duplicates.is_some() {
            return 0;
        }

        self.chip_errors.get(chip_idx).copied().unwrap_or(0)
    }

    #[inline]
    pub fn communication_issue_count(&self, chip_idx: usize) -> u64 {
        self.timeout_count(chip_idx) + self.duplicate_count(chip_idx)
    }

    /// Merge another snapshot from the same chain/measurement epoch.
    pub fn accumulate_from(&mut self, other: &Self) {
        debug_assert_eq!(self.chain_id, other.chain_id);
        debug_assert_eq!(self.measurement_epoch, other.measurement_epoch);

        fn add_vec(dst: &mut Vec<u64>, src: &[u64]) {
            if dst.len() < src.len() {
                dst.resize(src.len(), 0);
            }
            for (idx, value) in src.iter().copied().enumerate() {
                dst[idx] = dst[idx].saturating_add(value);
            }
        }

        fn add_optional_vec(dst: &mut Option<Vec<u64>>, src: &Option<Vec<u64>>) {
            if let Some(src) = src {
                let mut merged = dst.take().unwrap_or_else(|| vec![0; src.len()]);
                add_vec(&mut merged, src);
                *dst = Some(merged);
            }
        }

        add_vec(&mut self.chip_nonces, &other.chip_nonces);
        add_vec(&mut self.chip_errors, &other.chip_errors);
        add_optional_vec(&mut self.chip_hw_errors, &other.chip_hw_errors);
        add_optional_vec(&mut self.chip_timeouts, &other.chip_timeouts);
        add_optional_vec(&mut self.chip_duplicates, &other.chip_duplicates);

        self.window_duration_s += other.window_duration_s;
        self.timestamp = other.timestamp;
        self.board_temp_c = other.board_temp_c.or(self.board_temp_c);
        self.chip_temps_c = other
            .chip_temps_c
            .clone()
            .or_else(|| self.chip_temps_c.clone());
        self.psu_power_w = other.psu_power_w.or(self.psu_power_w);
        if other.current_difficulty != 0 {
            self.current_difficulty = other.current_difficulty;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracker_new() {
        let chains = vec![(6, 63), (7, 63)];
        let tracker = ChipNonceTracker::new(&chains, 3);
        assert_eq!(tracker.chain_ids.len(), 2);
        assert_eq!(tracker.chip_nonces[0].len(), 63);
        assert_eq!(tracker.chip_errors[1].len(), 63);
    }

    #[test]
    fn test_record_nonce_and_error() {
        let chains = vec![(6, 3)];
        let mut tracker = ChipNonceTracker::new(&chains, 3);

        tracker.record_nonce(0, 0);
        tracker.record_nonce(0, 0);
        tracker.record_nonce(0, 2);
        tracker.record_error(0, 1);

        assert_eq!(tracker.chip_nonces[0][0], 2);
        assert_eq!(tracker.chip_nonces[0][2], 1);
        assert_eq!(tracker.chip_errors[0][1], 1);
    }

    #[test]
    fn test_record_hw_error_and_timeout() {
        let chains = vec![(6, 3)];
        let mut tracker = ChipNonceTracker::new(&chains, 3);

        tracker.record_hw_error(0, 0);
        tracker.record_hw_error(0, 0);
        tracker.record_timeout(0, 1);
        tracker.record_duplicate(0, 2);

        // HW errors should be tracked separately
        assert_eq!(tracker.chip_hw_errors[0][0], 2);
        assert_eq!(tracker.chip_timeouts[0][1], 1);
        assert_eq!(tracker.chip_duplicates[0][2], 1);

        // Combined error counter should also be incremented
        assert_eq!(tracker.chip_errors[0][0], 2); // 2 hw errors
        assert_eq!(tracker.chip_errors[0][1], 1); // 1 timeout
        assert_eq!(tracker.chip_errors[0][2], 1); // 1 duplicate
    }

    #[test]
    fn test_hw_difficulty_and_board_temp() {
        let chains = vec![(6, 3), (7, 3)];
        let mut tracker = ChipNonceTracker::new(&chains, 0);

        // Default hw_difficulty should be 256
        assert_eq!(tracker.hw_difficulty, 256);

        // Set hw difficulty (ASIC TicketMask)
        tracker.set_hw_difficulty(512);
        assert_eq!(tracker.hw_difficulty, 512);

        // Pool difficulty should NOT affect snapshot.current_difficulty
        tracker.set_pool_difficulty(10000.0);
        assert_eq!(tracker.current_pool_difficulty, 10000.0);

        // Set board temp for chain 0
        tracker.set_board_temp(0, 55.0);
        assert_eq!(tracker.board_temps[0], Some(55.0));
        assert_eq!(tracker.board_temps[1], None);

        // Snapshot should carry hw_difficulty (512), NOT pool difficulty (10000)
        tracker.record_nonce(0, 0);
        let snapshots = tracker.try_snapshot();
        assert!(!snapshots.is_empty());
        assert_eq!(snapshots[0].measurement_epoch, 0);
        assert_eq!(snapshots[0].current_difficulty, 512);
        assert_eq!(snapshots[0].board_temp_c, Some(55.0));
        assert!(snapshots[0].chip_temps_c.is_some());
        assert_eq!(snapshots[0].chip_temps_c.as_ref().unwrap().len(), 3);
        // Chain 1 should have no temp
        assert_eq!(snapshots[1].board_temp_c, None);
        assert!(snapshots[1].chip_temps_c.is_none());
    }

    #[test]
    fn test_begin_measurement_resets_only_target_chain() {
        let chains = vec![(6, 2), (7, 2)];
        let mut tracker = ChipNonceTracker::new(&chains, 60);
        tracker.record_nonce(0, 0);
        tracker.record_nonce(1, 1);

        let epoch = tracker.begin_measurement(6).unwrap();

        assert_eq!(epoch, 1);
        assert_eq!(tracker.measurement_epochs[0], 1);
        assert_eq!(tracker.measurement_epochs[1], 0);
        assert_eq!(tracker.chip_nonces[0][0], 0);
        assert_eq!(tracker.chip_nonces[1][1], 1);
    }

    #[test]
    fn test_record_out_of_bounds() {
        let chains = vec![(6, 3)];
        let mut tracker = ChipNonceTracker::new(&chains, 3);

        // Out-of-bounds chain index — should not panic
        tracker.record_nonce(5, 0);
        // Out-of-bounds chip index — should not panic
        tracker.record_nonce(0, 10);

        assert_eq!(tracker.chip_nonces[0][0], 0);
    }

    #[test]
    fn test_try_snapshot_not_ready() {
        let chains = vec![(6, 3)];
        let mut tracker = ChipNonceTracker::new(&chains, 60); // 60s interval
        tracker.record_nonce(0, 0);

        // Should return empty — interval not elapsed
        let snapshots = tracker.try_snapshot();
        assert!(snapshots.is_empty());
    }

    #[test]
    fn test_try_snapshot_resets_counters() {
        let chains = vec![(6, 2)];
        // Use 0s interval so snapshot is always ready
        let mut tracker = ChipNonceTracker::new(&chains, 0);
        tracker.record_nonce(0, 0);
        tracker.record_nonce(0, 1);
        tracker.record_hw_error(0, 0);

        let snapshots = tracker.try_snapshot();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].chain_id, 6);
        assert_eq!(snapshots[0].measurement_epoch, 0);
        assert_eq!(snapshots[0].chip_nonces[0], 1);
        assert_eq!(snapshots[0].chip_nonces[1], 1);
        assert_eq!(snapshots[0].chip_errors[0], 1);
        // HW error should be populated (non-zero)
        assert!(snapshots[0].chip_hw_errors.is_some());
        assert_eq!(snapshots[0].chip_hw_errors.as_ref().unwrap()[0], 1);

        // After snapshot, ALL counters should be reset
        let snapshots2 = tracker.try_snapshot();
        if !snapshots2.is_empty() {
            assert_eq!(snapshots2[0].chip_nonces[0], 0);
            // HW errors should be None (all zeros after reset)
            assert!(snapshots2[0].chip_hw_errors.is_none());
        }
    }

    #[test]
    fn test_stability_error_count_prefers_hw_errors() {
        let snapshot = ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 1,
            chip_nonces: vec![100],
            chip_errors: vec![9],
            window_duration_s: 3.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: Some(vec![2]),
            chip_timeouts: Some(vec![3]),
            chip_duplicates: Some(vec![4]),
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        assert_eq!(snapshot.stability_error_count(0), 2);
        assert_eq!(snapshot.communication_issue_count(0), 7);
    }

    #[test]
    fn test_stability_error_count_ignores_typed_comm_only_errors() {
        let snapshot = ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 1,
            chip_nonces: vec![100],
            chip_errors: vec![7],
            window_duration_s: 3.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: Some(vec![3]),
            chip_duplicates: Some(vec![4]),
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        assert_eq!(snapshot.stability_error_count(0), 0);
        assert_eq!(snapshot.communication_issue_count(0), 7);
    }

    #[test]
    fn test_stability_error_count_keeps_legacy_combined_fallback() {
        let snapshot = ChipStatsSnapshot {
            chain_id: 6,
            measurement_epoch: 1,
            chip_nonces: vec![100],
            chip_errors: vec![5],
            window_duration_s: 3.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        };

        assert_eq!(snapshot.stability_error_count(0), 5);
    }
}
