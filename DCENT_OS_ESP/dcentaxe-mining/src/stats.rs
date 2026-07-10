// DCENT_axe Mining Statistics
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Thread-safe mining statistics with rolling hashrate calculation.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Time window for hashrate averaging (seconds).
const HASHRATE_WINDOW_5S: usize = 5;
const HASHRATE_WINDOW_15S: usize = 15;
const HASHRATE_WINDOW_30S: usize = 30;
const HASHRATE_WINDOW_1M: usize = 60;
const HASHRATE_WINDOW_5M: usize = 300;
const HASHRATE_WINDOW_10M: usize = 600;
const HASHRATE_WINDOW_15M: usize = 900;

/// Per-second nonce count buckets for hashrate calculation.
/// Fixed size: 900 entries (15 minutes at 1-second resolution).
/// Uses ~7 KB regardless of hashrate (vs unbounded VecDeque that OOMs).
const HASHRATE_BUCKETS: usize = 900;

/// Default ASIC TicketMask difficulty (BM1366/BM1368).
/// Used as initial value for MiningStats::ticket_difficulty.
const DEFAULT_TICKET_DIFF: f64 = 256.0;

/// A locally valid share only counts as current share flow while it is recent.
/// This keeps mood/health surfaces from treating old session counters as live
/// evidence after a pool stall or stale work period.
const SHARE_FLOW_FRESH_WINDOW_SECS: u64 = 600;

/// Maximum number of chips tracked per-chip (covers Hex boards with 6 chips).
pub const MAX_CHIPS: usize = 6;

/// Snapshot of the block currently being mined, populated from the latest
/// `mining.notify` pushed by the active pool. Used by the dashboard's
/// "Block Info" modal.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentBlockInfo {
    /// Block height being mined (from BIP34 coinbase).
    pub height: u32,
    /// Previous block hash (hex, 64 chars, pool byte order).
    pub prev_hash: String,
    /// Pool-assigned job id (hex).
    pub job_id: String,
    /// ntime field from mining.notify (hex, 8 chars).
    pub ntime: String,
    /// ntime parsed as a Unix timestamp (seconds).
    pub ntime_unix: u32,
    /// clean_jobs flag from the notify that produced this snapshot.
    pub clean_jobs: bool,
    /// Wall-clock time (ms since Unix epoch) when the notify was received.
    pub received_unix_ms: i64,
    /// Decoded coinbase outputs from the notify that produced this snapshot.
    /// `None` until the dispatcher's per-pool WorkBuilder has been initialized
    /// (extranonce subscribed) and the coinbase TX could be parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coinbase_outputs: Option<Vec<dcentaxe_stratum::CoinbaseOutput>>,
    /// Sum of `value_sats` across all coinbase outputs, in satoshis.
    /// `None` when `coinbase_outputs` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coinbase_total_sats: Option<u64>,
    /// Sum of coinbase output values directed at the user's stratum address.
    /// Left `None` here — the dashboard derives it on the client side from
    /// the user's stratum address (see `block-tile.js: addressToScriptHex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coinbase_user_sats: Option<u64>,
    /// nbits field from the notify (compact difficulty, hex string e.g. "17021369").
    /// Empty string until the first notify is captured.
    #[serde(default)]
    pub nbits: String,
    /// Number of merkle branches in the notify (depth of the merkle tree).
    /// `tx_count >= 2^(merkle_branch_count - 1) + 1`. Used by the dashboard for
    /// an approximate "TXS" indicator on the block card.
    #[serde(default)]
    pub merkle_branch_count: u32,
}

/// Per-chip statistics for nonce/error tracking.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PerChipStats {
    /// Total valid nonces found by this chip.
    pub nonces: u32,
    /// Total HW errors from this chip.
    pub errors: u32,
    /// Estimated hashrate in GH/s (computed externally).
    pub hashrate_ghs: f64,
}

impl Default for PerChipStats {
    fn default() -> Self {
        Self {
            nonces: 0,
            errors: 0,
            hashrate_ghs: 0.0,
        }
    }
}

/// A single second's worth of nonce data.
#[derive(Debug, Clone, Copy)]
struct NonceBucket {
    /// Number of nonces recorded in this second.
    count: u32,
    /// Total difficulty of nonces in this second.
    difficulty_sum: f64,
}

impl Default for NonceBucket {
    fn default() -> Self {
        Self {
            count: 0,
            difficulty_sum: 0.0,
        }
    }
}

/// Mining statistics tracker.
///
/// Tracks accepted/rejected shares, hashrate, best difficulty, and uptime.
/// Designed to be shared between threads via Arc<Mutex<>>.
#[derive(Debug)]
pub struct MiningStats {
    /// Total locally valid shares submitted toward the pool.
    ///
    /// Pool-confirmed accept/reject counters live in `dcentaxe-stratum`.
    pub accepted: u64,

    /// Total local nonce validation rejects (genuine HW errors only).
    ///
    /// Pool-confirmed accept/reject counters live in `dcentaxe-stratum`.
    pub rejected: u64,

    /// Nonces valid at ASIC difficulty but below pool difficulty (expected, not errors).
    pub filtered: u64,

    /// Total nonces found by ASIC (including those below pool difficulty).
    pub nonces_found: u64,

    /// Nonces dropped due to job-slot aliasing (slot overwritten before nonce returned).
    /// Not a hardware error — expected on multi-chip boards with small job_id space.
    pub stale_nonces: u64,

    /// Nonces recovered by scanning all active slots after primary slot validation failed.
    /// Indicates UART framing or job-slot aliasing caused wrong slot lookup.
    pub slot_recoveries: u64,

    /// Number of work dispatches that failed to send to the ASIC IN A ROW.
    /// Reset to 0 on the first successful send. A sustained non-zero value means
    /// the chain is being starved of fresh work (transient UART congestion or a
    /// wedged driver) — surfaced so the watchdog/dashboard can detect a stalled
    /// chain rather than only seeing per-error log lines. Cumulative total is
    /// tracked separately in `send_failures_total`.
    pub consecutive_send_failures: u32,

    /// Cumulative count of every failed work dispatch over the session.
    /// Unlike `consecutive_send_failures` this never resets on success, so a
    /// flapping link's total error volume stays visible to the dashboard.
    pub send_failures_total: u64,

    /// Shares the dispatcher-level dedup dropped before submission (XPPROTO-1).
    /// These are cross-stream / recovery-resurface duplicates that the
    /// per-driver consecutive-nonce guards do not catch; dropping them keeps a
    /// duplicate from being submitted twice (pool "duplicate share" reject).
    pub duplicate_shares_dropped: u64,

    /// Shares dropped because the ASIC rolled version bits OUTSIDE the pool's
    /// negotiated (sub-)mask (XPPROTO-3). Submitting these earns a pool
    /// "invalid version" reject; dropping them keeps the reject rate honest.
    pub out_of_mask_dropped: u64,

    /// Current ASIC TicketMask difficulty. Each nonce represents this many
    /// difficulty-1 shares worth of hashing work.
    pub ticket_difficulty: f64,

    /// Best difficulty share found this session.
    pub best_difficulty: f64,

    /// Number of clean_jobs=true events (proxy for new blocks on the network).
    pub clean_jobs_count: u64,

    /// Current block height being mined (from BIP34 coinbase).
    pub block_height: u32,

    /// Uptime in seconds (updated by dispatcher).
    pub uptime_secs: u64,

    /// Current streak of consecutive accepted shares (no rejections).
    pub accept_streak: u32,

    /// Uptime second when the last locally valid share was submitted.
    pub last_accepted_share_uptime_secs: Option<u64>,

    /// Uptime second when the last local nonce validation reject was observed.
    pub last_rejected_share_uptime_secs: Option<u64>,

    /// All-time best streak (persisted to NVS separately).
    pub best_streak: u32,

    /// Hashrate history for OLED sparkline (last 16 samples, GH/s).
    /// Updated every stats sync (~5 seconds each).
    pub hashrate_history: [f32; 16],
    hashrate_history_idx: usize,

    /// Ring buffer of per-second nonce counts (fixed 900 entries = 15 min).
    nonce_buckets: Vec<NonceBucket>,

    /// Current write index into the ring buffer.
    bucket_index: usize,

    /// Timestamp of the current bucket (uptime seconds).
    bucket_epoch: u64,

    /// Cached hashrate values (GH/s).
    hashrate_5s: f64,
    hashrate_15s: f64,
    hashrate_30s: f64,
    hashrate_1m: f64,
    hashrate_5m: f64,
    hashrate_10m: f64,
    hashrate_15m: f64,

    /// Temperature history for thermal sparkline (last 16 samples).
    pub temp_history: [f32; 16],
    temp_history_idx: usize,

    /// Mood score history for mood sparkline (last 16 samples, 0-10).
    pub mood_history: [u8; 16],
    mood_history_idx: usize,

    /// Per-chip nonce and error counters (fixed-size, max 6 chips).
    pub per_chip: [PerChipStats; MAX_CHIPS],

    /// Current block being mined, populated from the latest mining.notify.
    /// `None` until the first NewJob event is received.
    pub current_block: Option<CurrentBlockInfo>,
}

impl MiningStats {
    pub fn new() -> Self {
        Self {
            accepted: 0,
            rejected: 0,
            filtered: 0,
            nonces_found: 0,
            stale_nonces: 0,
            slot_recoveries: 0,
            consecutive_send_failures: 0,
            send_failures_total: 0,
            duplicate_shares_dropped: 0,
            out_of_mask_dropped: 0,
            ticket_difficulty: DEFAULT_TICKET_DIFF,
            best_difficulty: 0.0,
            clean_jobs_count: 0,
            block_height: 0,
            uptime_secs: 0,
            accept_streak: 0,
            last_accepted_share_uptime_secs: None,
            last_rejected_share_uptime_secs: None,
            best_streak: 0,
            hashrate_history: [0.0; 16],
            hashrate_history_idx: 0,
            nonce_buckets: vec![NonceBucket::default(); HASHRATE_BUCKETS],
            bucket_index: 0,
            bucket_epoch: 0,
            hashrate_5s: 0.0,
            hashrate_15s: 0.0,
            hashrate_30s: 0.0,
            hashrate_1m: 0.0,
            hashrate_5m: 0.0,
            hashrate_10m: 0.0,
            hashrate_15m: 0.0,
            temp_history: [0.0; 16],
            temp_history_idx: 0,
            mood_history: [5; 16],
            mood_history_idx: 0,
            per_chip: [PerChipStats::default(); MAX_CHIPS],
            current_block: None,
        }
    }

    fn advance_to_current_second(&mut self) {
        let current_sec = self.uptime_secs;
        if current_sec <= self.bucket_epoch {
            return;
        }

        let elapsed = current_sec
            .saturating_sub(self.bucket_epoch)
            .min(HASHRATE_BUCKETS as u64);
        for _ in 0..elapsed {
            self.bucket_index = (self.bucket_index + 1) % HASHRATE_BUCKETS;
            self.nonce_buckets[self.bucket_index] = NonceBucket::default();
        }
        self.bucket_epoch = current_sec;
    }

    fn recalculate_hashrates(&mut self) {
        self.hashrate_5s = self.calc_hashrate(HASHRATE_WINDOW_5S);
        self.hashrate_15s = self.calc_hashrate(HASHRATE_WINDOW_15S);
        self.hashrate_30s = self.calc_hashrate(HASHRATE_WINDOW_30S);
        self.hashrate_1m = self.calc_hashrate(HASHRATE_WINDOW_1M);
        self.hashrate_5m = self.calc_hashrate(HASHRATE_WINDOW_5M);
        self.hashrate_10m = self.calc_hashrate(HASHRATE_WINDOW_10M);
        self.hashrate_15m = self.calc_hashrate(HASHRATE_WINDOW_15M);
    }

    /// Advance the rolling windows even when no new nonces arrive.
    /// Clear the nonce bucket ring buffer and cached rolling averages.
    ///
    /// Called when the Stratum pool reconnects after an extended disconnect,
    /// matching ESP-Miner's `hashrate_monitor_reset_measurements()` (commit
    /// `83ed1b1` / PR #1564). Without this, pre-disconnect samples still sit in
    /// the ring buffer and produce a large apparent hashrate "spike" on the
    /// first post-reconnect share, poisoning efficiency / J/TH / autotuner
    /// readings.
    ///
    /// Cumulative counters (`accepted`, `rejected`, `nonces_found`,
    /// `best_difficulty`, `best_streak`, `per_chip`) are preserved — only the
    /// rolling window is cleared.
    pub fn reset_hashrate_measurements(&mut self) {
        for bucket in self.nonce_buckets.iter_mut() {
            *bucket = NonceBucket::default();
        }
        self.bucket_index = 0;
        self.bucket_epoch = self.uptime_secs;
        self.hashrate_5s = 0.0;
        self.hashrate_15s = 0.0;
        self.hashrate_30s = 0.0;
        self.hashrate_1m = 0.0;
        self.hashrate_5m = 0.0;
        self.hashrate_10m = 0.0;
        self.hashrate_15m = 0.0;
        // Clear hashrate sparkline so the OLED doesn't show a post-disconnect spike.
        self.hashrate_history = [0.0; 16];
        self.hashrate_history_idx = 0;
    }

    pub fn refresh_hashrate(&mut self) {
        self.advance_to_current_second();
        self.recalculate_hashrates();
    }

    /// Record a nonce found event and update hashrate.
    ///
    /// Uses a fixed-size ring buffer of per-second counters instead of
    /// per-nonce tracking. Memory usage is ~7 KB regardless of hashrate
    /// (vs unbounded growth that would OOM ESP32 at real hashrates).
    pub fn update_hashrate(&mut self) {
        self.advance_to_current_second();
        self.nonce_buckets[self.bucket_index].count += 1;
        self.nonce_buckets[self.bucket_index].difficulty_sum += self.ticket_difficulty;

        self.recalculate_hashrates();
    }

    /// Get the 5-second rolling hashrate in GH/s.
    pub fn hashrate_5s_ghs(&self) -> f64 {
        self.hashrate_5s
    }

    /// Get the 15-second rolling hashrate in GH/s.
    pub fn hashrate_15s_ghs(&self) -> f64 {
        self.hashrate_15s
    }

    /// Get the 30-second rolling hashrate in GH/s.
    pub fn hashrate_30s_ghs(&self) -> f64 {
        self.hashrate_30s
    }

    /// Calculate hashrate over a time window by summing ring buffer buckets.
    ///
    /// Each nonce at difficulty D represents D * 2^32 hashes of work.
    /// Hashrate (H/s) = total_hashes / window_seconds
    /// Return value is in GH/s.
    fn calc_hashrate(&self, window_secs: usize) -> f64 {
        // MD-5: always divide by the NOMINAL window length, never by the number
        // of seconds elapsed so far. The old divisor `min(window, bucket_epoch)`
        // shrank toward 0 at cold start, so a single early nonce's difficulty got
        // divided by 1-2 seconds and reported a huge transient hashrate spike
        // (e.g. 256 / 1s ≈ 1.1 TH/s on the 5s window) that briefly misled the
        // autotuner/mood logic. Dividing by the fixed window means cold-start
        // readings RAMP UP from 0 instead of spiking down a tiny divisor —
        // matching ESP-Miner's fixed-window convention. Cold/unfilled buckets are
        // zeroed (`NonceBucket::default()`) so they contribute 0 to `total_diff`.
        let read_window = window_secs.min(HASHRATE_BUCKETS);
        if read_window == 0 {
            return 0.0;
        }

        let mut total_diff: f64 = 0.0;
        for i in 0..read_window {
            let idx = (self.bucket_index + HASHRATE_BUCKETS - i) % HASHRATE_BUCKETS;
            total_diff += self.nonce_buckets[idx].difficulty_sum;
        }

        // total_hashes = total_diff * 2^32
        // GH/s = total_hashes / (window_secs * 10^9)
        let total_hashes = total_diff * 4_294_967_296.0; // 2^32
        total_hashes / (read_window as f64 * 1_000_000_000.0)
    }

    /// Get the 1-minute rolling hashrate in GH/s.
    pub fn hashrate_1m_ghs(&self) -> f64 {
        self.hashrate_1m
    }

    /// Get the 5-minute rolling hashrate in GH/s.
    pub fn hashrate_5m_ghs(&self) -> f64 {
        self.hashrate_5m
    }

    /// Get the 10-minute rolling hashrate in GH/s.
    pub fn hashrate_10m_ghs(&self) -> f64 {
        self.hashrate_10m
    }

    /// Get the 15-minute rolling hashrate in GH/s.
    pub fn hashrate_15m_ghs(&self) -> f64 {
        self.hashrate_15m
    }

    /// Record an accepted share — extends the streak.
    pub fn record_accept(&mut self) {
        self.accepted += 1;
        self.accept_streak += 1;
        self.last_accepted_share_uptime_secs = Some(self.uptime_secs);
        if self.accept_streak > self.best_streak {
            self.best_streak = self.accept_streak;
        }
    }

    /// Record a rejected share — breaks the streak.
    pub fn record_reject(&mut self) {
        self.rejected += 1;
        self.accept_streak = 0;
        self.last_rejected_share_uptime_secs = Some(self.uptime_secs);
    }

    /// Age of the last locally valid share, measured in uptime seconds.
    pub fn last_accepted_share_age_secs(&self) -> Option<u64> {
        self.last_accepted_share_uptime_secs
            .map(|last| self.uptime_secs.saturating_sub(last))
    }

    /// Age of the last local validation reject, measured in uptime seconds.
    pub fn last_rejected_share_age_secs(&self) -> Option<u64> {
        self.last_rejected_share_uptime_secs
            .map(|last| self.uptime_secs.saturating_sub(last))
    }

    /// True when a locally valid share was seen inside the freshness window.
    pub fn has_fresh_accepted_share(&self, freshness_secs: u64) -> bool {
        self.last_accepted_share_age_secs()
            .map(|age| age <= freshness_secs)
            .unwrap_or(false)
    }

    /// Record a successful work dispatch to the ASIC — clears the
    /// consecutive-failure streak (the chain is being fed again).
    pub fn record_send_success(&mut self) {
        self.consecutive_send_failures = 0;
    }

    /// Record a failed work dispatch to the ASIC — extends the consecutive
    /// failure streak and bumps the cumulative total. A sustained streak is a
    /// starvation signal for the watchdog/dashboard.
    pub fn record_send_failure(&mut self) {
        self.consecutive_send_failures = self.consecutive_send_failures.saturating_add(1);
        self.send_failures_total = self.send_failures_total.saturating_add(1);
    }

    /// Record a valid nonce from a specific chip.
    pub fn record_chip_nonce(&mut self, chip_id: u8) {
        let idx = chip_id as usize;
        if idx < MAX_CHIPS {
            self.per_chip[idx].nonces += 1;
        }
    }

    /// Record a HW error from a specific chip.
    pub fn record_chip_error(&mut self, chip_id: u8) {
        let idx = chip_id as usize;
        if idx < MAX_CHIPS {
            self.per_chip[idx].errors += 1;
        }
    }

    /// Push the current 5m hashrate into the sparkline history ring buffer.
    pub fn push_hashrate_sample(&mut self) {
        self.hashrate_history[self.hashrate_history_idx] = self.hashrate_5m as f32;
        self.hashrate_history_idx = (self.hashrate_history_idx + 1) % 16;
    }

    /// Get the hashrate history as an ordered slice (oldest first) for sparkline.
    pub fn hashrate_sparkline(&self) -> [f32; 16] {
        let mut out = [0.0f32; 16];
        for i in 0..16 {
            out[i] = self.hashrate_history[(self.hashrate_history_idx + i) % 16];
        }
        out
    }

    /// Push a temperature sample into the thermal history ring buffer.
    pub fn push_temp_sample(&mut self, temp: f32) {
        self.temp_history[self.temp_history_idx] = temp;
        self.temp_history_idx = (self.temp_history_idx + 1) % 16;
    }

    /// Get temperature history as ordered slice (oldest first) for sparkline.
    pub fn temp_sparkline(&self) -> [f32; 16] {
        let mut out = [0.0f32; 16];
        for i in 0..16 {
            out[i] = self.temp_history[(self.temp_history_idx + i) % 16];
        }
        out
    }

    /// Calculate creature mood score (0-10) based on mining health.
    /// Factors: shares flowing, cool temp, stable hashrate, uptime, variability.
    pub fn mood_score(&self, temp: f32, target_temp: f32) -> u8 {
        let mut score: f32 = 5.0; // baseline
        let fresh_share_flow = self.has_fresh_accepted_share(SHARE_FLOW_FRESH_WINDOW_SECS);

        // Shares flowing? (+2 only when a locally valid share is recent).
        if fresh_share_flow && self.accept_streak > 0 {
            score += 2.0;
        } else if self.accepted > 0 {
            // Had shares this session, but no fresh valid share or streak broken.
            score -= 1.0;
        } else if self.uptime_secs > 300 && self.accepted == 0 {
            score -= 2.0;
        }

        // Temperature health — wider comfort zone for BM1370 (normal 50-70C)
        // Penalty starts at target+15C (was +10C)
        if target_temp > 0.0 {
            let diff = temp - target_temp;
            if diff < -10.0 {
                score += 1.5;
            } else if diff < 0.0 {
                score += 1.0;
            } else if diff < 15.0 {
                score -= 0.3;
            } else {
                score -= 3.0;
            }
        }

        // Hashrate stability (+1 if stable, -1 if dropping)
        let recent_hr = self.hashrate_5m;
        let older_hr = self.hashrate_15m;
        if recent_hr > 0.001 && older_hr > 0.001 {
            let ratio = recent_hr / older_hr;
            if ratio > 0.95 {
                score += 1.0;
            } else if ratio < 0.7 {
                score -= 1.5;
            }
        }

        // Streak bonus
        if self.accept_streak > 20 {
            score += 0.5;
        }

        // Small time-based variability (±0.3 wobble for interesting sparkline)
        score += (self.uptime_secs % 3) as f32 * 0.3 - 0.3;

        score.clamp(0.0, 10.0) as u8
    }

    /// Push a mood score sample into the mood history ring buffer.
    pub fn push_mood_sample(&mut self, mood: u8) {
        self.mood_history[self.mood_history_idx] = mood;
        self.mood_history_idx = (self.mood_history_idx + 1) % 16;
    }

    /// Get mood history as ordered slice (oldest first) for sparkline.
    pub fn mood_sparkline(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for i in 0..16 {
            out[i] = self.mood_history[(self.mood_history_idx + i) % 16];
        }
        out
    }

    /// Copy computed statistics from another MiningStats instance.
    /// Used to sync local (lock-free) stats to a shared (Mutex) copy.
    pub fn sync_from(&mut self, other: &MiningStats) {
        self.accepted = other.accepted;
        self.rejected = other.rejected;
        self.filtered = other.filtered;
        self.nonces_found = other.nonces_found;
        self.stale_nonces = other.stale_nonces;
        self.slot_recoveries = other.slot_recoveries;
        self.consecutive_send_failures = other.consecutive_send_failures;
        self.send_failures_total = other.send_failures_total;
        self.duplicate_shares_dropped = other.duplicate_shares_dropped;
        self.out_of_mask_dropped = other.out_of_mask_dropped;
        self.ticket_difficulty = other.ticket_difficulty;
        self.best_difficulty = other.best_difficulty;
        self.clean_jobs_count = other.clean_jobs_count;
        self.block_height = other.block_height;
        self.uptime_secs = other.uptime_secs;
        self.hashrate_5s = other.hashrate_5s;
        self.hashrate_15s = other.hashrate_15s;
        self.hashrate_30s = other.hashrate_30s;
        self.hashrate_1m = other.hashrate_1m;
        self.hashrate_5m = other.hashrate_5m;
        self.hashrate_10m = other.hashrate_10m;
        self.hashrate_15m = other.hashrate_15m;
        self.accept_streak = other.accept_streak;
        self.last_accepted_share_uptime_secs = other.last_accepted_share_uptime_secs;
        self.last_rejected_share_uptime_secs = other.last_rejected_share_uptime_secs;
        self.best_streak = other.best_streak;
        self.hashrate_history = other.hashrate_history;
        self.hashrate_history_idx = other.hashrate_history_idx;
        self.temp_history = other.temp_history;
        self.temp_history_idx = other.temp_history_idx;
        self.mood_history = other.mood_history;
        self.mood_history_idx = other.mood_history_idx;
        self.per_chip = other.per_chip;
        self.current_block = other.current_block.clone();
    }

    /// Export statistics as a serializable snapshot.
    pub fn snapshot(&self) -> MiningStatsSnapshot {
        MiningStatsSnapshot {
            accepted_shares: self.accepted,
            rejected_shares: self.rejected,
            filtered_shares: self.filtered,
            nonces_found: self.nonces_found,
            hashrate_5s_ghs: self.hashrate_5s,
            hashrate_15s_ghs: self.hashrate_15s,
            hashrate_30s_ghs: self.hashrate_30s,
            hashrate_1m_ghs: self.hashrate_1m,
            hashrate_5m_ghs: self.hashrate_5m,
            hashrate_10m_ghs: self.hashrate_10m,
            hashrate_15m_ghs: self.hashrate_15m,
            ticket_difficulty: self.ticket_difficulty,
            best_difficulty: self.best_difficulty,
            clean_jobs_count: self.clean_jobs_count,
            block_height: self.block_height,
            uptime_secs: self.uptime_secs,
            accept_streak: self.accept_streak,
            last_accepted_share_age_secs: self.last_accepted_share_age_secs(),
            last_rejected_share_age_secs: self.last_rejected_share_age_secs(),
            best_streak: self.best_streak,
            hashrate_sparkline: self.hashrate_sparkline(),
            per_chip: self.per_chip,
            stale_nonces: self.stale_nonces,
            slot_recoveries: self.slot_recoveries,
            consecutive_send_failures: self.consecutive_send_failures,
            send_failures_total: self.send_failures_total,
            duplicate_shares_dropped: self.duplicate_shares_dropped,
            out_of_mask_dropped: self.out_of_mask_dropped,
            current_block: self.current_block.clone(),
        }
    }
}

/// Thread-safe shared mining statistics.
///
/// Wrap MiningStats in Arc<Mutex<>> for sharing between:
/// - Mining dispatcher thread (writes nonces, shares)
/// - API/web server thread (reads for display)
pub type SharedMiningStats = Arc<Mutex<MiningStats>>;

/// Create a new shared mining stats instance.
pub fn new_shared_stats() -> SharedMiningStats {
    Arc::new(Mutex::new(MiningStats::new()))
}

/// Per-pool statistics snapshot for API reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatsSnapshot {
    /// Pool index (0 = primary, 1 = secondary).
    pub index: u8,
    /// Target hashrate percentage for this pool.
    pub target_pct: u8,
    /// Number of work units dispatched to this pool.
    pub dispatched_count: u64,
    /// Number of shares submitted to this pool.
    pub shares_submitted: u64,
    /// Number of shares accepted by this pool.
    pub shares_accepted: u64,
    /// Number of shares rejected by this pool.
    pub shares_rejected: u64,
    /// Whether this pool is currently connected.
    pub connected: bool,
    /// Current pool difficulty.
    pub difficulty: f64,
}

impl PoolStatsSnapshot {
    /// Calculate the actual hashrate percentage for this pool given total dispatches.
    pub fn actual_pct(&self, total_dispatched: u64) -> f64 {
        if total_dispatched == 0 {
            0.0
        } else {
            (self.dispatched_count as f64 / total_dispatched as f64) * 100.0
        }
    }
}

/// Thread-safe shared per-pool statistics.
pub type SharedPoolStats = Arc<Mutex<Vec<PoolStatsSnapshot>>>;

/// Create a new shared pool stats instance.
pub fn new_shared_pool_stats() -> SharedPoolStats {
    Arc::new(Mutex::new(Vec::new()))
}

/// Lightweight cross-thread sink for the most recently decoded coinbase TX.
///
/// The mining dispatcher writes this once per `mining.notify`; the HTTP API
/// reads it to populate the dashboard's reward-split / scriptsig display.
/// Using a dedicated tiny struct (rather than `dcentaxe::shared::Telemetry`)
/// keeps `dcentaxe-mining` free of any reverse dependency on the binary
/// crate.
#[derive(Debug, Clone, Default)]
pub struct CoinbaseSnapshot {
    pub outputs: Vec<dcentaxe_stratum::CoinbaseOutput>,
    pub total_value_sats: u64,
    pub scriptsig_hex: String,
}

/// Thread-safe shared coinbase snapshot.
pub type SharedCoinbase = Arc<Mutex<CoinbaseSnapshot>>;

/// Create a new shared coinbase sink.
pub fn new_shared_coinbase() -> SharedCoinbase {
    Arc::new(Mutex::new(CoinbaseSnapshot::default()))
}

/// Serializable snapshot of mining statistics for API export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningStatsSnapshot {
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub filtered_shares: u64,
    pub nonces_found: u64,
    pub hashrate_5s_ghs: f64,
    pub hashrate_15s_ghs: f64,
    pub hashrate_30s_ghs: f64,
    pub hashrate_1m_ghs: f64,
    pub hashrate_5m_ghs: f64,
    pub hashrate_10m_ghs: f64,
    pub hashrate_15m_ghs: f64,
    pub ticket_difficulty: f64,
    pub best_difficulty: f64,
    pub clean_jobs_count: u64,
    pub block_height: u32,
    pub uptime_secs: u64,
    pub accept_streak: u32,
    #[serde(default)]
    pub last_accepted_share_age_secs: Option<u64>,
    #[serde(default)]
    pub last_rejected_share_age_secs: Option<u64>,
    pub best_streak: u32,
    pub hashrate_sparkline: [f32; 16],
    pub per_chip: [PerChipStats; MAX_CHIPS],
    pub stale_nonces: u64,
    pub slot_recoveries: u64,
    /// Consecutive work-dispatch send failures (0 = chain being fed normally).
    /// A sustained non-zero value is a work-starvation signal for the watchdog.
    #[serde(default)]
    pub consecutive_send_failures: u32,
    /// Cumulative work-dispatch send failures over the session.
    #[serde(default)]
    pub send_failures_total: u64,
    /// Duplicate shares dropped by the dispatcher-level dedup (XPPROTO-1).
    #[serde(default)]
    pub duplicate_shares_dropped: u64,
    /// Shares dropped for rolling outside the negotiated version sub-mask (XPPROTO-3).
    #[serde(default)]
    pub out_of_mask_dropped: u64,
    /// Current block being mined (from latest mining.notify). `None` before the
    /// first NewJob event is received.
    #[serde(default)]
    pub current_block: Option<CurrentBlockInfo>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_new() {
        let stats = MiningStats::new();
        assert_eq!(stats.accepted, 0);
        assert_eq!(stats.rejected, 0);
        assert_eq!(stats.nonces_found, 0);
        assert_eq!(stats.best_difficulty, 0.0);
        assert_eq!(stats.hashrate_1m_ghs(), 0.0);
        assert_eq!(stats.last_accepted_share_age_secs(), None);
        assert_eq!(stats.last_rejected_share_age_secs(), None);
    }

    #[test]
    fn test_stats_snapshot() {
        let mut stats = MiningStats::new();
        stats.accepted = 5;
        stats.rejected = 1;
        stats.filtered = 7;
        stats.nonces_found = 100;
        stats.stale_nonces = 2;
        stats.slot_recoveries = 3;
        stats.ticket_difficulty = 512.0;
        stats.best_difficulty = 42.0;
        stats.uptime_secs = 300;
        stats.last_accepted_share_uptime_secs = Some(275);
        stats.last_rejected_share_uptime_secs = Some(250);

        let snap = stats.snapshot();
        assert_eq!(snap.accepted_shares, 5);
        assert_eq!(snap.rejected_shares, 1);
        assert_eq!(snap.filtered_shares, 7);
        assert_eq!(snap.nonces_found, 100);
        assert_eq!(snap.stale_nonces, 2);
        assert_eq!(snap.slot_recoveries, 3);
        assert_eq!(snap.ticket_difficulty, 512.0);
        assert_eq!(snap.best_difficulty, 42.0);
        assert_eq!(snap.uptime_secs, 300);
        assert_eq!(snap.last_accepted_share_age_secs, Some(25));
        assert_eq!(snap.last_rejected_share_age_secs, Some(50));
    }

    #[test]
    fn test_share_freshness_ages_are_uptime_based() {
        let mut stats = MiningStats::new();
        stats.uptime_secs = 42;

        stats.record_accept();
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.last_accepted_share_age_secs(), Some(0));
        assert!(stats.has_fresh_accepted_share(10));

        stats.uptime_secs = 53;
        assert_eq!(stats.last_accepted_share_age_secs(), Some(11));
        assert!(!stats.has_fresh_accepted_share(10));

        stats.record_reject();
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.accept_streak, 0);
        assert_eq!(stats.last_rejected_share_age_secs(), Some(0));
    }

    #[test]
    fn test_mood_share_flow_requires_fresh_accept() {
        let mut fresh = MiningStats::new();
        fresh.uptime_secs = 100;
        fresh.record_accept();

        let mut stale = MiningStats::new();
        stale.uptime_secs = 100;
        stale.record_accept();
        stale.uptime_secs += SHARE_FLOW_FRESH_WINDOW_SECS + 1;

        assert!(fresh.has_fresh_accepted_share(SHARE_FLOW_FRESH_WINDOW_SECS));
        assert!(!stale.has_fresh_accepted_share(SHARE_FLOW_FRESH_WINDOW_SECS));
        assert!(fresh.mood_score(0.0, 0.0) > stale.mood_score(0.0, 0.0));
    }

    #[test]
    fn test_hashrate_calculation() {
        let mut stats = MiningStats::new();

        // Simulate 10 nonces at second 1
        stats.uptime_secs = 1;
        for _ in 0..10 {
            stats.nonces_found += 1;
            stats.update_hashrate();
        }

        // With 10 nonces at difficulty 256 in 1 second,
        // hashrate should be positive
        assert!(
            stats.hashrate_1m_ghs() > 0.0,
            "1m hashrate should be positive after nonces"
        );
    }

    #[test]
    fn test_cold_start_hashrate_uses_nominal_window_no_spike() {
        // MD-5: at cold start (bucket_epoch ramping from 0) a single early nonce
        // must be divided by the NOMINAL window, not by the 1-2 seconds elapsed,
        // so the reading ramps up instead of spiking.
        let mut stats = MiningStats::new();
        stats.ticket_difficulty = 256.0;
        stats.uptime_secs = 1; // only 1 second of history
        stats.update_hashrate();

        // Expected with the nominal 5s window divisor:
        //   256 diff * 2^32 hashes / (5 s * 1e9) GH/s.
        let expected_5s = 256.0 * 4_294_967_296.0 / (HASHRATE_WINDOW_5S as f64 * 1_000_000_000.0);
        let got_5s = stats.hashrate_5s_ghs();
        assert!(
            (got_5s - expected_5s).abs() < 1e-6,
            "5s hashrate must divide by the nominal 5s window (got {got_5s}, expected {expected_5s})"
        );

        // The buggy divide-by-1 would have produced ~5x this value — assert the
        // reading is NOT the spiked single-second figure.
        let spiked = 256.0 * 4_294_967_296.0 / (1.0 * 1_000_000_000.0);
        assert!(
            got_5s < spiked * 0.5,
            "cold-start 5s hashrate must not show the divide-by-elapsed spike"
        );

        // Longer windows divide by their own larger nominal divisor (ramp down).
        assert!(stats.hashrate_1m_ghs() < got_5s);
    }

    #[test]
    fn test_hashrate_decays_without_new_nonces() {
        let mut stats = MiningStats::new();

        stats.uptime_secs = 1;
        for _ in 0..10 {
            stats.nonces_found += 1;
            stats.update_hashrate();
        }
        assert!(stats.hashrate_15s_ghs() > 0.0);

        stats.uptime_secs = (HASHRATE_WINDOW_15S + 2) as u64;
        stats.refresh_hashrate();

        assert_eq!(stats.hashrate_15s_ghs(), 0.0);
    }

    #[test]
    fn test_ring_buffer_fixed_size() {
        let stats = MiningStats::new();
        // Ring buffer is always exactly HASHRATE_BUCKETS entries
        assert_eq!(stats.nonce_buckets.len(), HASHRATE_BUCKETS);
        // Memory is bounded regardless of nonce count
        assert_eq!(
            std::mem::size_of::<NonceBucket>() * HASHRATE_BUCKETS,
            900 * std::mem::size_of::<NonceBucket>()
        );
    }

    #[test]
    fn test_ring_buffer_wraps() {
        let mut stats = MiningStats::new();

        // Fill beyond the buffer size to ensure wrapping works
        for sec in 1..=(HASHRATE_BUCKETS as u64 + 10) {
            stats.uptime_secs = sec;
            stats.update_hashrate();
        }

        // Should still have valid hashrate (no panic, no OOM)
        assert!(stats.hashrate_15m_ghs() > 0.0);
    }

    #[test]
    fn test_send_failure_counters() {
        let mut stats = MiningStats::new();
        assert_eq!(stats.consecutive_send_failures, 0);
        assert_eq!(stats.send_failures_total, 0);

        stats.record_send_failure();
        stats.record_send_failure();
        assert_eq!(stats.consecutive_send_failures, 2);
        assert_eq!(stats.send_failures_total, 2);

        // A success clears the consecutive streak but not the cumulative total.
        stats.record_send_success();
        assert_eq!(stats.consecutive_send_failures, 0);
        assert_eq!(stats.send_failures_total, 2);

        // Snapshot carries both counters.
        let snap = stats.snapshot();
        assert_eq!(snap.consecutive_send_failures, 0);
        assert_eq!(snap.send_failures_total, 2);
    }

    #[test]
    fn test_shared_stats() {
        let shared = new_shared_stats();
        {
            let mut stats = shared.lock().unwrap();
            stats.accepted = 42;
        }
        {
            let stats = shared.lock().unwrap();
            assert_eq!(stats.accepted, 42);
        }
    }
}
