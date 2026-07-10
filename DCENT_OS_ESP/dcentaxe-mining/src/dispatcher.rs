// DCENT_axe Mining Dispatcher
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// The mining dispatcher is the core coordination loop that:
//   1. Receives Stratum jobs from the pool(s) (via channels from StratumClient(s))
//   2. Converts jobs into ASIC work (midstate computation, merkle root)
//   3. Sends work to the ASIC driver
//   4. Reads ASIC responses (nonces)
//   5. Validates nonces against pool difficulty
//   6. Submits valid shares back to the correct Stratum client (via channel)
//
// Supports hashrate splitting across up to 2 pools using a deficit-based
// scheduler (job-based alternation). The ASIC does not know or care which
// pool the work came from.
//
// Runs in a dedicated thread on ESP32-S3.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};

use dcentaxe_stratum::work::increment_bitmask;
use dcentaxe_stratum::{
    MiningEvent, MiningWork, ShareSubmission, StratumEvent, StratumJob, WorkBuilder,
};

use crate::stats::MiningStats;

/// Maximum number of in-flight work items tracked.
/// Must be >= the ASIC's job queue depth.
const MAX_ACTIVE_JOBS: usize = 128;
const MAX_WORK_AGE_SECS: u64 = 120;
const BIP320_DEFAULT_VERSION_MASK: u32 = 0x1FFFE000;

/// Bound for the dispatcher-level recent-share dedup set (XPPROTO-1).
/// Mirrors the DCENT_OS `seen_shares` 8192-entry bound. Caps RAM at
/// ~8192 * (sizeof key) which is acceptable on the ESP32-S3 mining thread.
const SHARE_DEDUP_CAPACITY: usize = 8192;

/// Bounded recent-share dedup keyed on `(stratum_job_id, nonce, asic_nr)`.
///
/// XPPROTO-1: complements — does NOT duplicate — the driver-level per-stream
/// consecutive/circular guards in the asic lane (`prev_nonce`,
/// `first_nonce`/`nonce_found`). Those catch consecutive/looped duplicates
/// within a SINGLE chip's stream; this catches the CROSS-stream cases they
/// miss: an interleaved duplicate (A,B,A) on BM1366/68/70 and a slot-scan
/// recovery re-surfacing a nonce the primary path already submitted. A
/// duplicate submit is pool-rejected as "duplicate share", inflates the reject
/// rate, and can mask genuine HW errors — so we drop it before `share_tx.send`.
#[derive(Debug, Default)]
struct ShareDedup {
    seen: HashSet<(String, u32, u8)>,
    order: VecDeque<(String, u32, u8)>,
}

impl ShareDedup {
    /// Returns true if this share key was already submitted (a duplicate).
    /// On a first-seen key, records it (bounded FIFO eviction) and returns false.
    fn check_and_insert(&mut self, job_id: &str, nonce: u32, asic_nr: u8) -> bool {
        let key = (job_id.to_string(), nonce, asic_nr);
        if self.seen.contains(&key) {
            return true;
        }
        if self.order.len() >= SHARE_DEDUP_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        self.seen.insert(key.clone());
        self.order.push_back(key);
        false
    }

    /// Clear all tracked keys — called on clean_jobs so a fresh block-template
    /// epoch never collides with stale keys from a superseded job set.
    fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

fn build_validation_header(work: &MiningWork, version: u32, nonce: u32) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&version.to_le_bytes());
    header[4..36].copy_from_slice(&work.prev_block_hash);
    header[36..68].copy_from_slice(&work.merkle_root);
    header[68..72].copy_from_slice(&work.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&work.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

fn reverse_validation_words(header: &[u8; 80]) -> [u8; 80] {
    let mut reversed_header = [0u8; 80];
    reversed_header[0..4].copy_from_slice(&header[0..4]);
    reversed_header[68..80].copy_from_slice(&header[68..80]);
    for i in 0..8u8 {
        let src_word = 4 + (7 - i as usize) * 4;
        let dst_word = 4 + (i as usize) * 4;
        reversed_header[dst_word..dst_word + 4].copy_from_slice(&header[src_word..src_word + 4]);
    }
    for i in 0..8u8 {
        let src_word = 36 + (7 - i as usize) * 4;
        let dst_word = 36 + (i as usize) * 4;
        reversed_header[dst_word..dst_word + 4].copy_from_slice(&header[src_word..src_word + 4]);
    }
    reversed_header
}

fn validate_work_header(
    work: &MiningWork,
    version: u32,
    nonce: u32,
    warn_on_reversed: bool,
) -> ([u8; 80], f64, bool) {
    let mut header = build_validation_header(work, version, nonce);
    let (mut achieved_diff, mut meets_pool_target) =
        dcentaxe_stratum::work::full_header_difficulty_and_target(&header, &work.share_target);

    if achieved_diff < 1.0 {
        let reversed_header = reverse_validation_words(&header);
        let (reversed_diff, reversed_meets_pool_target) =
            dcentaxe_stratum::work::full_header_difficulty_and_target(
                &reversed_header,
                &work.share_target,
            );
        if reversed_diff > achieved_diff {
            if warn_on_reversed {
                warn!(
                    "Validation: ASIC byte order match (reversed_diff={:.2}, original_diff={:.2e}). ASIC hashes data as-received without word-order reversal.",
                    reversed_diff,
                    achieved_diff
                );
            }
            achieved_diff = reversed_diff;
            meets_pool_target = reversed_meets_pool_target;
            header = reversed_header;
        }
    }

    (header, achieved_diff, meets_pool_target)
}

/// Work item tracking -- maps ASIC job IDs back to Stratum jobs for share submission.
#[derive(Debug, Clone)]
pub struct WorkItem {
    /// Internal job ID (0-127, sent to ASIC as job_id byte).
    pub asic_job_id: u8,

    /// Stratum job ID (for share submission).
    pub stratum_job_id: String,

    /// Extranonce2 used for this work item (hex string).
    pub extranonce2: String,

    /// ntime from the job (hex string for share submission).
    pub ntime_hex: String,

    /// Block version used.
    pub version: u32,

    /// Version mask for rolling.
    pub version_mask: u32,

    /// Version mask programmed into the ASIC when this work was dispatched.
    ///
    /// BM1366/BM1368/BM1370 init programs the canonical BIP320 mask even when
    /// a pool has not sent `mining.set_version_mask`; keep the hardware domain
    /// with the work so rolled nonce reconstruction remains deterministic.
    pub hardware_version_mask: u32,

    /// The MiningWork containing midstates and share_target for validation.
    pub work: MiningWork,

    /// Timestamp when work was dispatched.
    pub dispatched_at: Instant,

    /// Which pool this work came from (index into pools array).
    pub pool_index: u8,

    /// Monotonic dispatch sequence number. Used to detect job-slot aliasing
    /// on multi-chip boards where the small job_id space (8 slots for BM1368)
    /// wraps faster than nonces return from the ASIC chain.
    pub dispatch_seq: u64,
}

/// Configuration for the mining dispatcher.
pub struct DispatcherConfig {
    /// Interval between sending new work to the ASIC (milliseconds).
    /// Depends on ASIC type, frequency, and chain length.
    pub job_interval_ms: u64,
    /// Job ID increment step per dispatched work item.
    /// BM1397: +4 mod 128 (4 midstate slots, extraction: id & 0xFC)
    /// BM1366: +8 mod 128 (3-bit small_core_id, extraction: id & 0xF8)
    /// BM1368: current conservative +16 mod 128; response extraction is `(id & 0xF0) >> 1`.
    /// BM1370: +8 mod 128; response extraction is `(id & 0xF0) >> 1`.
    pub job_id_step: u8,
    /// Maximum job ID (wraps around at this value).
    /// BM1397: 128, BM1366/68/70: 128 (MAX_ACTIVE_JOBS)
    pub job_id_max: u8,
    /// True for BM1397 which uses midstate indices (0-3) in rolled_version field.
    /// False for BM1366/68/70 which return actual rolled version bits.
    pub midstate_mode: bool,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            // Default: ~200ms for single-ASIC BM1366 at 485 MHz
            // ESP-Miner uses ASIC_get_asic_job_frequency_ms()
            job_interval_ms: 200,
            // Default for BM1366: step by 8 (3-bit small_core_id)
            job_id_step: 8,
            job_id_max: 128,
            midstate_mode: false,
        }
    }
}

impl DispatcherConfig {
    fn job_id_cycle_len(&self) -> u64 {
        fn gcd(mut a: u64, mut b: u64) -> u64 {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        }

        let modulus = self.job_id_max as u64;
        let step = self.job_id_step as u64;
        if modulus == 0 || step == 0 {
            return 0;
        }
        modulus / gcd(modulus, step)
    }

    /// Create a dispatcher config scaled for multi-ASIC boards.
    ///
    /// Calculate job interval dynamically based on ASIC specs.
    /// Formula from ESP-Miner: NONCE_SPACE / (freq_MHz * small_core_count * 1000) / asic_count
    /// This ensures the ASIC always has fresh work before exhausting nonce space.
    fn calculate_interval(frequency_mhz: f32, small_core_count: u32, asic_count: u8) -> u64 {
        const NONCE_SPACE: f64 = 4_294_967_296.0; // 2^32
        let freq_khz = frequency_mhz as f64 * 1000.0;
        let interval =
            NONCE_SPACE / (freq_khz * small_core_count as f64) / (asic_count as f64).max(1.0);
        (interval as u64).max(5).min(500) // clamp to 5-500ms (< 5ms overloads UART)
    }

    /// Config for BM1366 (job ID step +8, extraction: id & 0xF8)
    /// BM1366 has 3-bit small_core_id → 16 unique job slots (step 8, mod 128)
    /// ESP-Miner uses hardcoded 2000/asic_count for BM1366 (version rolling means
    /// the ASIC can mine the full nonce space internally for ~2 seconds per job).
    pub fn for_bm1366(frequency_mhz: f32, asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        let interval = (2000u64 / count).max(10);
        info!(
            "Dispatcher: BM1366 job interval = {}ms (freq={:.0}MHz, {} chips, 2000/{})",
            interval, frequency_mhz, asic_count, count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 8,
            job_id_max: 128,
            midstate_mode: false,
        }
    }

    /// Config for BM1368 (current conservative job ID step +16).
    /// The response extractor returns 16 possible IDs via `(id & 0xF0) >> 1`.
    /// TODO(RE): ESP-Miner dispatches BM1368/BM1370 with +24 mod 128. Switch
    /// BM1368 away from +16 only after Hex Supra hardware soak proves no job
    /// aliasing or HW-error regression.
    /// Job interval: ESP-Miner hardcodes 500 / asic_count for BM1368/BM1370.
    /// The formula-based calculation gives ~1ms which overloads UART.
    pub fn for_bm1368(frequency_mhz: f32, asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        let interval = (500u64 / count).max(10);
        info!(
            "Dispatcher: BM1368 job interval = {}ms (freq={:.0}MHz, {} chips, 500/{})",
            interval, frequency_mhz, asic_count, count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 16,
            job_id_max: 128,
            midstate_mode: false,
        }
    }

    /// Config for BM1370 (job ID step +8 mod 128, extraction: (id & 0xf0) >> 1)
    pub fn for_bm1370(frequency_mhz: f32, asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        let interval = (500u64 / count).max(10);
        info!(
            "Dispatcher: BM1370 job interval = {}ms (freq={:.0}MHz, {} chips, 500/{})",
            interval, frequency_mhz, asic_count, count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 8,
            job_id_max: 128,
            midstate_mode: false,
        }
    }

    /// Config for BM1397 (job ID step +4 mod 128, 672 small cores, midstate mode)
    pub fn for_bm1397(frequency_mhz: f32, asic_count: u8) -> Self {
        let interval = Self::calculate_interval(frequency_mhz, 672, asic_count);
        info!(
            "Dispatcher: BM1397 job interval = {}ms (freq={:.0}MHz, 672 cores, {} chips)",
            interval, frequency_mhz, asic_count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 4,
            job_id_max: 128,
            midstate_mode: true,
        }
    }

    /// Config for KF1950 (WhatsMiner K-series).
    ///
    /// UNTESTED — gated by `asic-kf1950` Cargo feature, default OFF.
    /// CONFIDENCE: LOW (placeholder values).
    ///
    /// - `job_interval_ms`: 500ms placeholder. Real value depends on the
    ///   PLL formula (NOT_IMPLEMENTED) and verified core count (LOW: 40%).
    /// - `job_id_step`: 1 (driver increments an internal 8-bit counter; the
    ///   chip echoes it via byte[9] of the nonce response).
    /// - `job_id_max`: 128 — KF1950 forces bit 7 of the job_id high; the
    ///   effective range with bit 7 set is 0x80..=0xFF (128 slots).
    /// - `midstate_mode`: false — upstream fork only fills 1 of the 6
    ///   midstate slots; we mirror that. H3 hypothesis would flip this.
    ///
    /// §4.
    #[cfg(feature = "asic-kf1950")]
    pub fn for_kf1950(frequency_mhz: f32, asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        let interval = (500u64 / count).max(10);
        info!(
            "Dispatcher: KF1950 (UNTESTED RESEARCH) job interval = {}ms \
             (freq={:.0}MHz IGNORED, {} chips, 500/{})",
            interval, frequency_mhz, asic_count, count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 1,
            job_id_max: 128,
            midstate_mode: false,
        }
    }

    /// Legacy: fixed interval for unknown configs
    pub fn for_asic_count(asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        Self {
            job_interval_ms: (200 / count).max(10),
            ..Default::default()
        }
    }

    /// Config for Canaan Avalon A-series (A3197 / A3198 / Nano 3 / Nano 3S /
    /// Mini 3 / Avalon Q / A14xx / A15xx / A16xx).
    ///
    /// UNTESTED on hardware — gated by `asic-avalon` Cargo feature, default OFF.
    /// CONFIDENCE: LOW (placeholder values pending live-unit bring-up).
    ///
    /// Defaults per Plan 2 §B.4 open question 5:
    /// - `job_interval_ms`: scaled from chip count, similar to BM family.
    /// - `job_id_step`: 16 — leaves 4-bit `mid_id` ASICBoost slot free per the
    ///   `miner_nonce` bit-packing in `mm_miner.h:233-243` (4-bit mid_id occupies
    ///   the low nibble of the high byte; using a step of 16 keeps job IDs
    ///   distinguishable from the slot index).
    /// - `job_id_max`: 128.
    /// - `midstate_mode`: false — Avalon's RT-Smart `asic_miner_e` blob
    ///   computes midstates internally on the K230 big core; the host sends
    ///   the full block-header form via P_SET_JOB.
    ///
    /// Refine after first hash via `AVALON_ASIC_PROTOCOL.md` §3 sub-frame
    /// timing analysis.
    #[cfg(feature = "asic-avalon")]
    pub fn for_avalon(frequency_mhz: f32, asic_count: u8) -> Self {
        let count = (asic_count as u64).max(1);
        let interval = (500u64 / count).max(10);
        info!(
            "Dispatcher: Avalon (UNTESTED Phase 1) job interval = {}ms \
             (freq={:.0}MHz, {} chips, 500/{})",
            interval, frequency_mhz, asic_count, count
        );
        Self {
            job_interval_ms: interval,
            job_id_step: 16,
            job_id_max: 128,
            midstate_mode: false,
        }
    }
}

/// Per-pool state within the dispatcher.
///
/// Each pool slot encapsulates everything needed for one Stratum connection:
/// its own event channel, share channel, WorkBuilder, job, difficulty, etc.
pub struct PoolSlot {
    /// Index (0 = primary, 1 = secondary).
    pub index: u8,

    /// Target ratio as percentage (0-100). All pools must sum to 100.
    pub target_ratio: u8,

    /// Number of work units dispatched to this pool (for deficit scheduling).
    pub dispatched_count: u64,

    /// WorkBuilder for this pool's extranonce/difficulty.
    work_builder: Option<WorkBuilder>,

    /// Current job from this pool (latest mining.notify).
    current_job: Option<StratumJob>,

    /// Current difficulty for this pool.
    difficulty: f64,

    /// Current version mask for this pool.
    version_mask: u32,

    /// Channel to receive events from this pool's StratumClient.
    event_rx: mpsc::Receiver<StratumEvent>,

    /// Channel to send shares to this pool's StratumClient.
    share_tx: mpsc::Sender<MiningEvent>,

    /// Per-pool share counters (for API reporting).
    /// Note: These are u64 counters accessed only from the dispatcher thread (single-threaded).
    /// If the dispatcher ever becomes multi-threaded, migrate to AtomicU64.
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,

    /// Is this pool currently connected?
    pub connected: bool,

    /// SV2 prebuilt work (pool sends pre-computed MiningWork directly).
    prebuilt_work: Option<MiningWork>,
}

impl PoolSlot {
    /// Create a new pool slot.
    pub fn new(
        index: u8,
        target_ratio: u8,
        event_rx: mpsc::Receiver<StratumEvent>,
        share_tx: mpsc::Sender<MiningEvent>,
    ) -> Self {
        Self {
            index,
            target_ratio,
            dispatched_count: 0,
            work_builder: None,
            current_job: None,
            difficulty: 1.0,
            version_mask: 0,
            event_rx,
            share_tx,
            shares_submitted: 0,
            shares_accepted: 0,
            shares_rejected: 0,
            connected: false,
            prebuilt_work: None,
        }
    }

    /// Whether this pool has a valid job and work builder ready.
    pub fn is_ready(&self) -> bool {
        self.prebuilt_work.is_some() || (self.current_job.is_some() && self.work_builder.is_some())
    }

    /// Generate a work unit from this pool's current job.
    fn generate_work(&mut self) -> Option<MiningWork> {
        // SV2 prebuilt work takes priority
        if let Some(work) = self.prebuilt_work.take() {
            return Some(work);
        }
        let job = self.current_job.as_ref()?;
        let wb = self.work_builder.as_mut()?;
        Some(wb.next_work(job))
    }
}

/// The mining dispatcher.
///
/// Coordinates work flow between Stratum client thread(s) and the ASIC.
/// Supports 1 or 2 pools with deficit-based hashrate splitting.
/// Designed to run in a dedicated thread.
pub struct MiningDispatcher {
    /// Pool slots (1 for single pool, 2 for split mining).
    pools: Vec<PoolSlot>,

    /// Active work items indexed by ASIC job_id (0-127).
    active_jobs: Vec<Option<WorkItem>>,

    /// Valid job flags -- marks which job IDs are still valid (not stale).
    valid_jobs: Vec<bool>,

    /// Next ASIC job ID (wraps at MAX_ACTIVE_JOBS).
    next_job_id: u8,

    /// Dispatcher configuration.
    config: DispatcherConfig,

    /// Mining statistics (local, lock-free for fast nonce handling).
    pub stats: MiningStats,

    /// Optional shared stats for cross-thread access (API, dashboard).
    /// Synced periodically from local stats to avoid lock contention.
    shared_stats: Option<crate::stats::SharedMiningStats>,

    /// Optional shared pool stats for API access.
    shared_pool_stats: Option<crate::stats::SharedPoolStats>,

    /// Optional shared coinbase snapshot — written on every mining.notify
    /// so the dashboard can verify the actual reward split.
    shared_coinbase: Option<crate::stats::SharedCoinbase>,

    /// The startup time for uptime tracking.
    start_time: Instant,

    /// Last time shared stats were synced.
    last_stats_sync: Instant,

    /// Last time work was sent.
    last_work_sent: Instant,

    /// Whether the dispatcher has been initialized (first run_once call).
    initialized: bool,

    /// Pending ASIC difficulty update (set when pool difficulty changes).
    /// The mining loop reads this to update the ASIC's TicketMask register.
    /// f64 so fractional `mining.set_difficulty` values flow through unclamped.
    pending_asic_difficulty: Option<f64>,

    /// Pending ASIC version-mask update.
    pending_asic_version_mask: Option<u32>,

    /// Last version mask programmed into the ASIC.
    current_asic_version_mask: u32,

    /// Monotonic dispatch counter for detecting job-slot aliasing.
    /// BM1368/BM1370 have only 8 distinguishable job IDs. At 83ms intervals
    /// the ID space wraps in ~664ms. Nonces arriving after the wrap find
    /// wrong work in the slot. This counter lets us detect and skip those.
    dispatch_seq: u64,

    /// Last uptime second used to decay rolling hashrate windows.
    /// The buckets are one-second granularity, so refreshing every loop tick
    /// only burns CPU without changing the cached values.
    last_hashrate_refresh_sec: u64,

    /// Dispatcher-level recent-share dedup (XPPROTO-1). Bounded; cleared on
    /// clean_jobs. Stops cross-stream / recovery-resurface duplicate submits
    /// the per-driver consecutive guards cannot see.
    share_dedup: ShareDedup,
}

impl MiningDispatcher {
    /// Create a new mining dispatcher with a single pool (backward compatible).
    pub fn new(
        event_rx: mpsc::Receiver<StratumEvent>,
        share_tx: mpsc::Sender<MiningEvent>,
        config: DispatcherConfig,
    ) -> Self {
        let pool = PoolSlot::new(0, 100, event_rx, share_tx);
        Self::with_pools(vec![pool], config)
    }

    /// Create a new mining dispatcher with multiple pool slots.
    ///
    /// Each tuple is (event_rx, share_tx, target_ratio_pct).
    /// Ratios must sum to 100.
    pub fn with_pools(pools: Vec<PoolSlot>, config: DispatcherConfig) -> Self {
        let mut active_jobs = Vec::with_capacity(MAX_ACTIVE_JOBS);
        active_jobs.resize_with(MAX_ACTIVE_JOBS, || None);

        let mut valid_jobs = Vec::with_capacity(MAX_ACTIVE_JOBS);
        valid_jobs.resize(MAX_ACTIVE_JOBS, false);

        Self {
            pools,
            active_jobs,
            valid_jobs,
            next_job_id: 0,
            config,
            stats: MiningStats::new(),
            shared_stats: None,
            shared_pool_stats: None,
            start_time: Instant::now(),
            last_stats_sync: Instant::now(),
            last_work_sent: Instant::now(),
            initialized: false,
            pending_asic_difficulty: None,
            pending_asic_version_mask: None,
            current_asic_version_mask: BIP320_DEFAULT_VERSION_MASK,
            dispatch_seq: 0,
            last_hashrate_refresh_sec: 0,
            shared_coinbase: None,
            share_dedup: ShareDedup::default(),
        }
    }

    /// Set shared stats handle for cross-thread access (API, dashboard).
    /// Stats are synced every ~1 second to minimize lock contention.
    pub fn set_shared_stats(&mut self, shared: crate::stats::SharedMiningStats) {
        self.shared_stats = Some(shared);
    }

    /// Set shared pool stats handle for per-pool API reporting.
    pub fn set_shared_pool_stats(&mut self, shared: crate::stats::SharedPoolStats) {
        self.shared_pool_stats = Some(shared);
    }

    /// Set shared coinbase sink — populated on every mining.notify so the
    /// HTTP API can surface the decoded reward split.
    pub fn set_shared_coinbase(&mut self, shared: crate::stats::SharedCoinbase) {
        self.shared_coinbase = Some(shared);
    }

    /// Take the pending ASIC difficulty update, if any.
    /// Call this from the mining loop to update the ASIC's TicketMask.
    pub fn take_pending_asic_difficulty(&mut self) -> Option<f64> {
        self.pending_asic_difficulty.take()
    }

    pub fn take_pending_asic_version_mask(&mut self) -> Option<u32> {
        self.pending_asic_version_mask.take()
    }

    fn retire_expired_jobs(&mut self) {
        for idx in 0..self.active_jobs.len() {
            if !self.valid_jobs[idx] {
                continue;
            }
            if let Some(item) = &self.active_jobs[idx] {
                if item.dispatched_at.elapsed().as_secs() > MAX_WORK_AGE_SECS {
                    self.valid_jobs[idx] = false;
                }
            }
        }
    }

    fn has_live_jobs(&self) -> bool {
        self.valid_jobs.iter().enumerate().any(|(idx, valid)| {
            *valid
                && self.active_jobs[idx]
                    .as_ref()
                    .map(|item| item.dispatched_at.elapsed().as_secs() <= MAX_WORK_AGE_SECS)
                    .unwrap_or(false)
        })
    }

    fn effective_hardware_mask(&self, work_mask: u32) -> u32 {
        if self.config.midstate_mode {
            work_mask
        } else if work_mask == 0 {
            BIP320_DEFAULT_VERSION_MASK
        } else {
            work_mask
        }
    }

    fn active_hardware_mask(&self) -> Option<u32> {
        self.valid_jobs
            .iter()
            .enumerate()
            .filter(|(_, valid)| **valid)
            .find_map(|(idx, _)| {
                self.active_jobs[idx].as_ref().and_then(|item| {
                    if item.dispatched_at.elapsed().as_secs() <= MAX_WORK_AGE_SECS {
                        Some(item.hardware_version_mask)
                    } else {
                        None
                    }
                })
            })
    }

    fn compatible_with_active_mask(&self, pool: &PoolSlot) -> bool {
        if !self.has_live_jobs() {
            true
        } else {
            self.active_hardware_mask()
                .map(|mask| self.effective_hardware_mask(pool.version_mask) == mask)
                .unwrap_or(true)
        }
    }

    fn refresh_asic_ticket_difficulty(&mut self) {
        // Pick the lowest ready-pool difficulty so the TicketMask filters the least
        // aggressive pool. f64 all the way through (fractional diffs supported per
        // ESP-Miner PR #1594).
        let desired = self
            .pools
            .iter()
            .filter(|pool| pool.is_ready())
            .map(|pool| pool.difficulty.max(1.0))
            .fold(f64::INFINITY, f64::min);

        let desired = if desired.is_finite() {
            desired
        } else {
            self.stats.ticket_difficulty.max(1.0)
        };

        if desired != self.stats.ticket_difficulty {
            self.pending_asic_difficulty = Some(desired);
            self.stats.ticket_difficulty = desired;
        }
    }

    fn prime_next_dispatch(&mut self) {
        let interval = Duration::from_millis(self.config.job_interval_ms);
        self.last_work_sent = Instant::now()
            .checked_sub(interval)
            .unwrap_or_else(Instant::now);
    }

    /// Sync local stats to the shared stats handle.
    fn sync_shared_stats(&self) {
        if let Some(ref shared) = self.shared_stats {
            if let Ok(mut s) = shared.try_lock() {
                s.sync_from(&self.stats);
            }
        }

        // Sync per-pool stats
        if let Some(ref shared) = self.shared_pool_stats {
            if let Ok(mut pool_stats) = shared.try_lock() {
                pool_stats.clear();
                for pool in &self.pools {
                    pool_stats.push(crate::stats::PoolStatsSnapshot {
                        index: pool.index,
                        target_pct: pool.target_ratio,
                        dispatched_count: pool.dispatched_count,
                        shares_submitted: pool.shares_submitted,
                        shares_accepted: pool.shares_accepted,
                        shares_rejected: pool.shares_rejected,
                        connected: pool.connected,
                        difficulty: pool.difficulty,
                    });
                }
            }
        }
    }

    /// Run a single iteration of the dispatch loop. Call this from an outer loop
    /// that can also handle other commands (e.g., frequency changes).
    pub fn run_once<F, G, H>(
        &mut self,
        send_work_fn: &mut F,
        process_work_fn: &mut G,
        apply_hardware_fn: &mut H,
    ) where
        F: FnMut(&MiningWork, u8) -> Result<(), String>,
        G: FnMut() -> Vec<(u8, u32, u32, u8)>,
        H: FnMut(Option<f64>, Option<u32>),
    {
        if !self.initialized {
            let pool_count = self.pools.len();
            if pool_count > 1 {
                let ratios: Vec<u8> = self.pools.iter().map(|p| p.target_ratio).collect();
                info!(
                    "Mining dispatcher started, {} pools, ratios={:?}, job interval: {}ms",
                    pool_count, ratios, self.config.job_interval_ms
                );
            } else {
                info!(
                    "Mining dispatcher started, job interval: {}ms",
                    self.config.job_interval_ms
                );
            }
            self.initialized = true;
        }

        {
            // Step 1: Process events from all Stratum clients (non-blocking)
            self.drain_events();

            // Step 1.5: Apply ASIC-side config changes before dispatching new work.
            let pending_diff = self.take_pending_asic_difficulty();
            let pending_mask = self.take_pending_asic_version_mask();
            if pending_diff.is_some() || pending_mask.is_some() {
                apply_hardware_fn(pending_diff, pending_mask);
            }

            // Step 2: Process ASIC responses (nonces)
            let results = process_work_fn();
            for (job_id, nonce, rolled_version, asic_nr) in results {
                self.handle_nonce(job_id, nonce, rolled_version, asic_nr);
            }
            self.retire_expired_jobs();

            // Step 3: Send new work if interval has elapsed
            let interval = Duration::from_millis(self.config.job_interval_ms);
            if self.last_work_sent.elapsed() >= interval {
                if let Some((work, pool_index)) = self.generate_next_work() {
                    let job_id = self.next_job_id;
                    // BM1366: 3-bit small_core_id in [2:0], job_id in [7:3], extraction: id & 0xf8, step=8
                    // BM1368: current conservative dispatch step=16, extractor `(id & 0xf0) >> 1`
                    // BM1370: dispatch step=8, extractor `(id & 0xf0) >> 1`
                    // BM1397: 2-bit midstate_idx in [1:0], job_id in [7:2], extraction: id & 0xfc, step=4
                    self.next_job_id = (self.next_job_id.wrapping_add(self.config.job_id_step))
                        % self.config.job_id_max;

                    let hardware_version_mask = self.effective_hardware_mask(work.version_mask);
                    if hardware_version_mask != self.current_asic_version_mask {
                        self.current_asic_version_mask = hardware_version_mask;
                        self.pending_asic_version_mask = None;
                        apply_hardware_fn(None, Some(hardware_version_mask));
                    }

                    match send_work_fn(&work, job_id) {
                        Ok(()) => {
                            // Store the work item for nonce-to-share mapping
                            let seq = self.dispatch_seq;
                            self.dispatch_seq += 1;
                            let item = WorkItem {
                                asic_job_id: job_id,
                                stratum_job_id: work.job_id.clone(),
                                extranonce2: work.extranonce2.clone(),
                                ntime_hex: format!("{:08x}", work.ntime),
                                version: work.version,
                                version_mask: work.version_mask,
                                hardware_version_mask,
                                work,
                                dispatched_at: Instant::now(),
                                pool_index,
                                dispatch_seq: seq,
                            };
                            self.active_jobs[job_id as usize] = Some(item);
                            self.valid_jobs[job_id as usize] = true;

                            // Track dispatch count for deficit scheduling
                            if let Some(pool) = self.pools.get_mut(pool_index as usize) {
                                pool.dispatched_count += 1;
                            }

                            // Chain is being fed — clear the starvation streak and
                            // arm the interval timer for the next dispatch.
                            self.stats.record_send_success();
                            self.last_work_sent = Instant::now();
                        }
                        Err(e) => {
                            // MD-9: a failed send must NOT burn a job slot or eat a
                            // full interval. Roll back the job_id we pre-advanced so
                            // the slot is reused, leave `last_work_sent` untouched so
                            // the next tick retries immediately instead of waiting a
                            // full `job_interval_ms`, and surface the failure streak
                            // to the watchdog/dashboard.
                            self.next_job_id = job_id;
                            self.stats.record_send_failure();
                            error!(
                                "Failed to send work to ASIC: {} (consecutive failures: {})",
                                e, self.stats.consecutive_send_failures
                            );
                        }
                    }
                }
            }

            // Update uptime and decay rolling hashrate windows only when the
            // one-second bucket can change. Nonce paths still refresh
            // immediately through update_hashrate().
            let uptime_secs = self.start_time.elapsed().as_secs();
            self.stats.uptime_secs = uptime_secs;
            if uptime_secs != self.last_hashrate_refresh_sec {
                self.stats.refresh_hashrate();
                self.last_hashrate_refresh_sec = uptime_secs;
            }

            // Sync stats to shared handle for API/dashboard access (~1/s)
            if self.last_stats_sync.elapsed().as_secs() >= 1 {
                self.sync_shared_stats();
                self.last_stats_sync = Instant::now();
            }
        }
    }

    /// Run the dispatcher forever (legacy interface). Calls run_once in a loop.
    pub fn run<F, G, H>(
        &mut self,
        mut send_work_fn: F,
        mut process_work_fn: G,
        mut apply_hardware_fn: H,
    ) where
        F: FnMut(&MiningWork, u8) -> Result<(), String>,
        G: FnMut() -> Vec<(u8, u32, u32, u8)>,
        H: FnMut(Option<f64>, Option<u32>),
    {
        loop {
            self.run_once(
                &mut send_work_fn,
                &mut process_work_fn,
                &mut apply_hardware_fn,
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Drain all pending events from all Stratum clients.
    fn drain_events(&mut self) {
        for pool_idx in 0..self.pools.len() {
            loop {
                let event = match self.pools[pool_idx].event_rx.try_recv() {
                    Ok(e) => e,
                    Err(_) => break,
                };

                match event {
                    StratumEvent::NewJob(job) => {
                        let clean = job.clean_jobs;
                        let should_prime_dispatch = clean || !self.has_live_jobs();
                        info!(
                            "Dispatcher: pool[{}] new job #{} clean={}",
                            pool_idx, job.job_id, clean
                        );

                        // Track block height from coinbase (BIP34)
                        if job.block_height > 0 {
                            self.stats.block_height = job.block_height;
                        }

                        // Promote full block context into stats for the
                        // dashboard "Block Info" modal. Additive — block_height
                        // and clean_jobs_count above are unchanged.
                        let received_unix_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        let ntime_unix = u32::from_str_radix(&job.ntime, 16).unwrap_or(0);

                        // Decode the coinbase TX for the dashboard's reward-split
                        // verifier. Outputs don't depend on extranonce2, so we
                        // pin en2=0 for a deterministic snapshot. Only pool 0's
                        // coinbase is surfaced (matches single-pool API shape;
                        // split mining can extend later).
                        let decoded = if pool_idx == 0 {
                            self.pools[pool_idx]
                                .work_builder
                                .as_ref()
                                .and_then(|wb| wb.decode_coinbase(&job, 0))
                        } else {
                            None
                        };

                        let (cb_outputs, cb_total) = match decoded.as_ref() {
                            Some(d) => (Some(d.outputs.clone()), Some(d.total_value_sats)),
                            None => (None, None),
                        };

                        self.stats.current_block = Some(crate::stats::CurrentBlockInfo {
                            height: job.block_height,
                            prev_hash: job.prev_hash.clone(),
                            job_id: job.job_id.clone(),
                            ntime: job.ntime.clone(),
                            ntime_unix,
                            clean_jobs: clean,
                            received_unix_ms,
                            coinbase_outputs: cb_outputs,
                            coinbase_total_sats: cb_total,
                            // Per-user split derived client-side in
                            // dashboard/block-tile.js (addressToScriptHex).
                            coinbase_user_sats: None,
                            nbits: job.nbits.clone(),
                            merkle_branch_count: job.merkle_branches.len() as u32,
                        });

                        if clean {
                            self.stats.clean_jobs_count += 1;
                            // Invalidate only this pool's pending work (not other pool's)
                            for (i, item) in self.active_jobs.iter().enumerate() {
                                if let Some(ref w) = item {
                                    if w.pool_index == pool_idx as u8 {
                                        self.valid_jobs[i] = false;
                                    }
                                }
                            }
                            // Reset extranonce2 counter for this pool
                            if let Some(ref mut wb) = self.pools[pool_idx].work_builder {
                                wb.reset_extranonce2();
                            }
                            // XPPROTO-1: a clean_jobs epoch supersedes all prior
                            // job ids, so drop the recent-share dedup memory to
                            // bound it and avoid stale keys outliving their jobs.
                            self.share_dedup.clear();
                        }

                        // Mirror the decoded coinbase into the legacy shared
                        // sink (still consumed by the dashboard reward-split
                        // verifier on platforms that haven't migrated to the
                        // current_block path yet).
                        if let Some(d) = decoded {
                            if let Some(ref sink) = self.shared_coinbase {
                                if let Ok(mut s) = sink.lock() {
                                    // Phase L2: reuse existing buffer
                                    // capacity instead of assign-and-drop.
                                    // Each `mining.notify` would otherwise
                                    // free + alloc fresh Vec<CoinbaseOutput>
                                    // and String, churning the heap. Now
                                    // we clear and refill — the underlying
                                    // capacity persists across notifications.
                                    s.outputs.clear();
                                    s.outputs.extend(d.outputs.into_iter());
                                    s.total_value_sats = d.total_value_sats;
                                    s.scriptsig_hex.clear();
                                    s.scriptsig_hex.push_str(&d.scriptsig_hex);
                                }
                            }
                        }

                        self.pools[pool_idx].current_job = Some(job);
                        self.refresh_asic_ticket_difficulty();
                        if should_prime_dispatch
                            && self.pools[pool_idx].connected
                            && self.pools[pool_idx].is_ready()
                        {
                            self.prime_next_dispatch();
                        }
                    }
                    StratumEvent::DifficultyChanged(diff) => {
                        // Floor at 1.0 to prevent share flooding on difficulty 0
                        let safe_diff = if diff < 1.0 { 1.0 } else { diff };
                        info!(
                            "Dispatcher: pool[{}] difficulty changed to {} (clamped: {})",
                            pool_idx, diff, safe_diff
                        );
                        self.pools[pool_idx].difficulty = safe_diff;
                        if let Some(ref mut wb) = self.pools[pool_idx].work_builder {
                            wb.set_difficulty(safe_diff);
                        }
                        self.refresh_asic_ticket_difficulty();
                    }
                    StratumEvent::VersionMaskChanged(mask) => {
                        info!(
                            "Dispatcher: pool[{}] version mask changed to 0x{:08x}",
                            pool_idx, mask
                        );
                        self.pools[pool_idx].version_mask = mask;
                        if let Some(ref mut wb) = self.pools[pool_idx].work_builder {
                            wb.set_version_mask(mask);
                        }
                        // MD-3: in-flight work dispatched under this pool's OLD mask
                        // would reconstruct rolled versions against a stale mask
                        // domain once the ASIC is reprogrammed to the new mask.
                        // Invalidate this pool's in-flight items whose hardware mask
                        // differs from the new effective mask, mirroring the
                        // clean_jobs / Disconnected invalidation loops. Items already
                        // on the new effective mask are left valid (nested/identical
                        // BIP320 masks are the common case → no needless work drop).
                        let new_effective = self.effective_hardware_mask(mask);
                        for (i, item) in self.active_jobs.iter().enumerate() {
                            if let Some(ref w) = item {
                                if w.pool_index == pool_idx as u8
                                    && w.hardware_version_mask != new_effective
                                {
                                    self.valid_jobs[i] = false;
                                }
                            }
                        }
                    }
                    StratumEvent::ExtranonceChanged {
                        extranonce1,
                        extranonce2_size,
                    } => {
                        info!(
                            "Dispatcher: pool[{}] extranonce changed -- en1={}, en2_size={}",
                            pool_idx, extranonce1, extranonce2_size
                        );
                        let pool = &mut self.pools[pool_idx];
                        if let Some(ref mut wb) = pool.work_builder {
                            wb.set_extranonce(&extranonce1, extranonce2_size);
                        } else {
                            let mut wb = WorkBuilder::new(&extranonce1, extranonce2_size);
                            wb.set_version_mask(pool.version_mask);
                            wb.set_difficulty(pool.difficulty);
                            pool.work_builder = Some(wb);
                        }
                        self.refresh_asic_ticket_difficulty();
                    }
                    StratumEvent::Disconnected => {
                        warn!(
                            "Dispatcher: pool[{}] disconnected -- continuing with other pools",
                            pool_idx
                        );
                        let pool = &mut self.pools[pool_idx];
                        pool.connected = false;
                        pool.prebuilt_work = None;
                        pool.current_job = None;
                        pool.work_builder = None;
                        for (i, item) in self.active_jobs.iter().enumerate() {
                            if let Some(ref work) = item {
                                if work.pool_index == pool_idx as u8 {
                                    self.valid_jobs[i] = false;
                                }
                            }
                        }
                        self.refresh_asic_ticket_difficulty();
                    }
                    StratumEvent::Reconnected => {
                        info!("Dispatcher: pool[{}] reconnected", pool_idx);
                        self.pools[pool_idx].connected = true;
                        // Drop pre-disconnect nonce history so the first new share
                        // doesn't produce a hashrate spike. Mirrors ESP-Miner's
                        // `hashrate_monitor_reset_measurements()` from PR #1564.
                        self.stats.reset_hashrate_measurements();
                        if self.pools[pool_idx].is_ready() {
                            self.prime_next_dispatch();
                        }
                    }
                    StratumEvent::PrebuiltWork { work, clean_jobs } => {
                        // SV2 path: pool sends pre-computed MiningWork directly
                        info!(
                            "Dispatcher: pool[{}] SV2 prebuilt work job={} clean={}",
                            pool_idx, work.job_id, clean_jobs
                        );
                        if clean_jobs {
                            self.stats.clean_jobs_count += 1;
                            for (i, item) in self.active_jobs.iter().enumerate() {
                                if let Some(ref w) = item {
                                    if w.pool_index == pool_idx as u8 {
                                        self.valid_jobs[i] = false;
                                    }
                                }
                            }
                            // XPPROTO-1: clear recent-share dedup on a clean epoch.
                            self.share_dedup.clear();
                        }
                        self.pools[pool_idx].prebuilt_work = Some(work);
                        self.pools[pool_idx].connected = true;
                        self.refresh_asic_ticket_difficulty();
                        self.prime_next_dispatch();
                    }
                }
            }
        }
    }

    /// Initialize the WorkBuilder for pool 0 with session data.
    ///
    /// Called after the Stratum handshake completes. For single-pool backward compat.
    pub fn init_work_builder(&mut self, extranonce1: &str, extranonce2_size: usize) {
        if let Some(pool) = self.pools.get_mut(0) {
            let mut wb = WorkBuilder::new(extranonce1, extranonce2_size);
            wb.set_version_mask(pool.version_mask);
            wb.set_difficulty(pool.difficulty);
            pool.work_builder = Some(wb);
        }
        self.refresh_asic_ticket_difficulty();
    }

    /// Select which pool to generate the next work unit from.
    ///
    /// Uses a deficit-based scheduler: picks the pool that is most "behind"
    /// its target ratio. This naturally converges to the target split and
    /// handles pool disconnections gracefully.
    fn select_pool(&self) -> Option<usize> {
        // Single pool fast path
        if self.pools.len() == 1 {
            return if self.pools[0].connected
                && self.pools[0].is_ready()
                && self.compatible_with_active_mask(&self.pools[0])
            {
                Some(0)
            } else {
                None
            };
        }

        let total_dispatched: u64 = self.pools.iter().map(|p| p.dispatched_count).sum();

        // First dispatch always goes to primary
        if total_dispatched == 0 {
            // Return first ready pool (prefer primary)
            for (i, pool) in self.pools.iter().enumerate() {
                if pool.is_ready() && self.compatible_with_active_mask(pool) {
                    return Some(i);
                }
            }
            return None;
        }

        let any_connected_ready = self
            .pools
            .iter()
            .any(|pool| pool.connected && pool.is_ready());

        // Find the pool with the largest deficit (target_ratio - actual_ratio)
        let mut best_pool = 0;
        let mut best_deficit = f64::NEG_INFINITY;

        for (i, pool) in self.pools.iter().enumerate() {
            if !pool.is_ready() {
                continue; // Skip pools with no active job
            }
            if any_connected_ready && !pool.connected {
                continue; // Prefer live pools when at least one live pool has work
            }
            if !self.compatible_with_active_mask(pool) {
                continue;
            }
            let actual_ratio = pool.dispatched_count as f64 / total_dispatched as f64;
            let target_ratio = pool.target_ratio as f64 / 100.0;
            let deficit = target_ratio - actual_ratio;
            if deficit > best_deficit {
                best_deficit = deficit;
                best_pool = i;
            }
        }

        if best_deficit == f64::NEG_INFINITY {
            None
        } else {
            Some(best_pool)
        }
    }

    /// Generate the next work unit, selecting from the appropriate pool.
    ///
    /// Returns the MiningWork and the pool_index it came from, or None if
    /// no pool has a valid job ready.
    fn generate_next_work(&mut self) -> Option<(MiningWork, u8)> {
        let pool_idx = self.select_pool()?;
        let pool = self.pools.get_mut(pool_idx)?;
        let work = pool.generate_work()?;
        Some((work, pool_idx as u8))
    }

    fn actual_version_for(&self, item: &WorkItem, rolled_version: u32) -> u32 {
        if self.config.midstate_mode {
            if item.work.version_mask == 0 {
                return item.work.version;
            }
            // BM1397: midstate_index -> compute rolled version via increment_bitmask
            let mut ver = item.work.version;
            for _ in 0..rolled_version {
                ver = increment_bitmask(ver, item.work.version_mask);
            }
            ver
        } else {
            // BM1366/68/70: merge ASIC-returned version bits with base version
            let mask = if item.work.version_mask != 0 {
                item.work.version_mask
            } else if rolled_version != 0 {
                item.hardware_version_mask
            } else {
                0
            };
            if mask == 0 {
                item.work.version
            } else {
                (item.work.version & !mask) | (rolled_version & mask)
            }
        }
    }

    /// XPPROTO-3: true when the ASIC rolled version bits that fall OUTSIDE the
    /// pool's explicitly negotiated version sub-mask.
    ///
    /// Only meaningful for full-header boards (`midstate_mode == false`), where
    /// `rolled_version` carries the actual ASIC-returned version bits. For
    /// BM1397 (`midstate_mode == true`) `rolled_version` is a midstate INDEX,
    /// not version bits, so this never applies. It also only applies when the
    /// pool advertised a concrete sub-mask (`item.work.version_mask != 0`) —
    /// with mask 0 we fall back to the canonical BIP320 mask elsewhere and there
    /// is no narrower negotiated set to violate. When this returns true the
    /// reconstructed version no longer matches what the chip hashed, so most of
    /// these already self-filter via header validation; dropping them
    /// explicitly keeps the per-pool reject accounting honest and avoids any
    /// edge case where a masked reconstruction coincidentally still validates.
    fn rolled_outside_negotiated_mask(&self, item: &WorkItem, rolled_version: u32) -> bool {
        if self.config.midstate_mode {
            return false;
        }
        let negotiated = item.work.version_mask;
        negotiated != 0 && (rolled_version & !negotiated) != 0
    }

    fn share_version_bits(item: &WorkItem, actual_version: u32) -> Option<String> {
        let bits = actual_version ^ item.work.version;
        if bits == 0 {
            None
        } else {
            Some(format!("{:08x}", bits))
        }
    }

    fn find_recovery_candidate(
        &self,
        skip_idx: Option<usize>,
        nonce: u32,
        rolled_version: u32,
        n_slots: u64,
    ) -> Option<(usize, WorkItem, u32, f64, bool)> {
        let step = self.config.job_id_step as usize;
        let modulus = (self.config.job_id_max as usize).min(MAX_ACTIVE_JOBS);
        let scan_slots = self.config.job_id_cycle_len().min(MAX_ACTIVE_JOBS as u64) as usize;
        if step == 0 || modulus == 0 || scan_slots == 0 {
            return None;
        }

        let mut slot = 0usize;
        for _ in 0..scan_slots {
            if Some(slot) == skip_idx || !self.valid_jobs[slot] {
                slot = (slot + step) % modulus;
                continue;
            }

            if let Some(ref alt) = self.active_jobs[slot] {
                if alt.dispatched_at.elapsed().as_secs() > MAX_WORK_AGE_SECS {
                    slot = (slot + step) % modulus;
                    continue;
                }
                if self.dispatch_seq > alt.dispatch_seq + n_slots * 2 {
                    slot = (slot + step) % modulus;
                    continue;
                }

                let alt_version = self.actual_version_for(alt, rolled_version);
                let (_alt_header, alt_diff, alt_meets_pool_target) =
                    validate_work_header(&alt.work, alt_version, nonce, false);
                if alt_diff >= self.stats.ticket_difficulty {
                    return Some((
                        slot,
                        alt.clone(),
                        alt_version,
                        alt_diff,
                        alt_meets_pool_target,
                    ));
                }
            }

            slot = (slot + step) % modulus;
        }

        None
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_recovered_nonce(
        stats: &mut MiningStats,
        pools: &mut [PoolSlot],
        dedup: &mut ShareDedup,
        original_job_id: u8,
        recovered_slot: usize,
        item: WorkItem,
        actual_version: u32,
        achieved_diff: f64,
        meets_pool_target: bool,
        asic_nr: u8,
        nonce: u32,
    ) {
        let pool_index = item.pool_index;
        stats.slot_recoveries += 1;
        if stats.slot_recoveries <= 5 {
            info!(
                "Slot recovery: job_id {:02x}->{:02x} diff={:.1}",
                original_job_id, recovered_slot, achieved_diff
            );
        }

        if meets_pool_target {
            // XPPROTO-1: slot-scan recovery can re-surface a nonce the primary
            // path already submitted (or an interleaved cross-stream duplicate).
            // Drop it before counting/submitting so it is not double-submitted
            // (pool would reject it as a duplicate share).
            if dedup.check_and_insert(&item.stratum_job_id, nonce, asic_nr) {
                stats.duplicate_shares_dropped += 1;
                debug!(
                    "Dispatcher: dropped duplicate recovered share job={} nonce=0x{:08x} asic={}",
                    item.stratum_job_id, nonce, asic_nr
                );
                return;
            }
            stats.record_chip_nonce(asic_nr);
            stats.record_accept();
            stats.update_hashrate();
            if achieved_diff > stats.best_difficulty {
                stats.best_difficulty = achieved_diff;
                info!("Dispatcher: new best difficulty: {:.2}", achieved_diff);
            }
            if let Some(pool) = pools.get_mut(pool_index as usize) {
                pool.shares_submitted += 1;
                pool.shares_accepted += 1;
            }

            let version_bits = Self::share_version_bits(&item, actual_version);
            let share = ShareSubmission {
                job_id: item.stratum_job_id,
                extranonce2: item.extranonce2,
                ntime: item.ntime_hex,
                nonce: format!("{:08x}", nonce),
                version: actual_version,
                version_bits,
                difficulty: achieved_diff,
            };
            info!(
                "Dispatcher: share! (recovered) job={} nonce=0x{:08x} diff={:.1}",
                share.job_id, nonce, achieved_diff
            );
            if let Some(pool) = pools.get(pool_index as usize) {
                let _ = pool.share_tx.send(MiningEvent::SubmitShare(share));
            }
        } else {
            stats.record_chip_nonce(asic_nr);
            stats.filtered += 1;
            stats.update_hashrate();
            if achieved_diff > stats.best_difficulty {
                stats.best_difficulty = achieved_diff;
            }
        }
    }

    /// Handle a nonce result from the ASIC.
    ///
    /// Validates the nonce against the originating pool's difficulty target and,
    /// if valid, sends a share submission to that pool's Stratum client thread.
    fn handle_nonce(&mut self, job_id: u8, nonce: u32, rolled_version: u32, asic_nr: u8) {
        self.stats.nonces_found += 1;

        // Look up the work item
        let idx = job_id as usize;
        if idx >= MAX_ACTIVE_JOBS {
            warn!("Dispatcher: nonce with invalid job_id {}", job_id);
            return;
        }

        // Detect job-slot aliasing: on multi-chip boards with small job_id spaces,
        // nonces may arrive after the slot was overwritten or may map to an
        // adjacent extracted ID. Allow up to 2 full wraps as grace period.
        let n_slots = self.config.job_id_cycle_len().max(1);
        let max_age_seq = n_slots * 2; // 2 full wraps

        if let Some(ref item) = self.active_jobs[idx] {
            if item.dispatched_at.elapsed().as_secs() > MAX_WORK_AGE_SECS {
                if let Some((slot, alt, alt_version, alt_diff, alt_meets_pool_target)) =
                    self.find_recovery_candidate(Some(idx), nonce, rolled_version, n_slots)
                {
                    Self::handle_recovered_nonce(
                        &mut self.stats,
                        &mut self.pools,
                        &mut self.share_dedup,
                        job_id,
                        slot,
                        alt,
                        alt_version,
                        alt_diff,
                        alt_meets_pool_target,
                        asic_nr,
                        nonce,
                    );
                    return;
                }
                debug!(
                    "Dispatcher: nonce for expired work (age={}s, job_id={})",
                    item.dispatched_at.elapsed().as_secs(),
                    job_id
                );
                return;
            }
        }

        if !self.valid_jobs[idx] {
            if let Some((slot, alt, alt_version, alt_diff, alt_meets_pool_target)) =
                self.find_recovery_candidate(Some(idx), nonce, rolled_version, n_slots)
            {
                Self::handle_recovered_nonce(
                    &mut self.stats,
                    &mut self.pools,
                    &mut self.share_dedup,
                    job_id,
                    slot,
                    alt,
                    alt_version,
                    alt_diff,
                    alt_meets_pool_target,
                    asic_nr,
                    nonce,
                );
                return;
            }
            debug!("Dispatcher: nonce for stale job_id {}", job_id);
            return;
        }

        let item = match &self.active_jobs[idx] {
            Some(item) => item,
            None => {
                if let Some((slot, alt, alt_version, alt_diff, alt_meets_pool_target)) =
                    self.find_recovery_candidate(Some(idx), nonce, rolled_version, n_slots)
                {
                    Self::handle_recovered_nonce(
                        &mut self.stats,
                        &mut self.pools,
                        &mut self.share_dedup,
                        job_id,
                        slot,
                        alt,
                        alt_version,
                        alt_diff,
                        alt_meets_pool_target,
                        asic_nr,
                        nonce,
                    );
                    return;
                }
                warn!("Dispatcher: nonce for unknown job_id {}", job_id);
                return;
            }
        };

        if self.dispatch_seq > item.dispatch_seq + max_age_seq {
            if let Some((slot, alt, alt_version, alt_diff, alt_meets_pool_target)) =
                self.find_recovery_candidate(Some(idx), nonce, rolled_version, n_slots)
            {
                Self::handle_recovered_nonce(
                    &mut self.stats,
                    &mut self.pools,
                    &mut self.share_dedup,
                    job_id,
                    slot,
                    alt,
                    alt_version,
                    alt_diff,
                    alt_meets_pool_target,
                    asic_nr,
                    nonce,
                );
                return;
            }
            self.stats.stale_nonces += 1;
            if self.stats.stale_nonces <= 5 {
                debug!("Dispatcher: aliased nonce (slot overwritten): job_id={:02x} seq={} current={} age={}",
                    job_id, item.dispatch_seq, self.dispatch_seq,
                    self.dispatch_seq - item.dispatch_seq);
            }
            return;
        }

        let pool_index = item.pool_index;

        // Full 80-byte header validation (matching ESP-Miner's test_nonce_value).
        // Reconstruct the complete block header and double-SHA256 it.
        // Compute the actual block version for this nonce.
        // rolled_version semantics differ by ASIC type:
        //   BM1397 (midstate_mode=true): midstate_index (0-3) -> increment_bitmask N times
        //   BM1366/68/70 (midstate_mode=false): ASIC-returned rolled version bits -> merge
        let actual_version = self.actual_version_for(item, rolled_version);

        // Full 80-byte header SHA256d validation (matching ESP-Miner's test_nonce_value).
        //
        // BM1366/68/70 ASICs receive prev_block_hash and merkle_root with 32-bit
        // words in reversed order (see bridge.rs reverse_32bit_words). ESP-Miner's
        // test_nonce_value() applies reverse_32bit_words() during validation to
        // convert from ASIC byte order back to block-header byte order.
        //
        // Our MiningWork stores the ORIGINAL (block-header) byte order, so we
        // should be able to use it directly. However, if the ASIC internally
        // hashes the data AS-RECEIVED (without un-reversing), the nonce is only
        // valid against the reversed byte order.
        //
        // To handle both cases robustly, we try the original order first, and
        // if that fails (diff < 1.0 = clearly wrong), try the ASIC byte order
        // (word-reversed). This ensures single-chip boards that work with the
        // original order continue to work, while multi-chip boards (Hex) that
        // may need the reversed order also work.
        let (_header, achieved_diff, meets_pool_target) = validate_work_header(
            &item.work,
            actual_version,
            nonce,
            self.stats.nonces_found <= 5,
        );

        if meets_pool_target {
            // Case 1: Meets pool difficulty — submit share
            //
            // XPPROTO-1: drop a cross-stream / looped duplicate before counting
            // or submitting. The per-driver consecutive-nonce guard only sees a
            // single chip's stream; an interleaved (A,B,A) duplicate on
            // BM1366/68/70 reaches here twice. Submitting twice earns a pool
            // "duplicate share" reject and inflates the reject rate.
            if self
                .share_dedup
                .check_and_insert(&item.stratum_job_id, nonce, asic_nr)
            {
                self.stats.duplicate_shares_dropped += 1;
                debug!(
                    "Dispatcher: dropped duplicate share job={} nonce=0x{:08x} asic={}",
                    item.stratum_job_id, nonce, asic_nr
                );
                return;
            }
            // XPPROTO-3: if the chip rolled version bits outside the pool's
            // negotiated sub-mask, the reconstructed version differs from what
            // the ASIC hashed and a sub-mask pool would reject it as an invalid
            // version. Drop + count it instead of submitting.
            if self.rolled_outside_negotiated_mask(item, rolled_version) {
                self.stats.out_of_mask_dropped += 1;
                debug!(
                    "Dispatcher: dropped out-of-mask share job={} rolled=0x{:08x} negotiated_mask=0x{:08x}",
                    item.stratum_job_id, rolled_version, item.work.version_mask
                );
                return;
            }
            // Note: record_accept() is based on local validation, not pool confirmation.
            // The pool may still reject (stale, duplicate). Pool-side accept/reject
            // is tracked separately in the Stratum client's response handler.
            self.stats.record_chip_nonce(asic_nr);
            self.stats.record_accept();
            self.stats.update_hashrate();
            if achieved_diff > self.stats.best_difficulty {
                self.stats.best_difficulty = achieved_diff;
                info!("Dispatcher: new best difficulty: {:.2}", achieved_diff);
            }

            // Update per-pool share counters
            if let Some(pool) = self.pools.get_mut(pool_index as usize) {
                pool.shares_submitted += 1;
                pool.shares_accepted += 1;
            }

            let version_bits = Self::share_version_bits(item, actual_version);

            let share = ShareSubmission {
                job_id: item.stratum_job_id.clone(),
                extranonce2: item.extranonce2.clone(),
                ntime: item.ntime_hex.clone(),
                nonce: format!("{:08x}", nonce),
                version: actual_version,
                version_bits,
                difficulty: achieved_diff,
            };

            if self.pools.len() > 1 {
                info!(
                    "Dispatcher: share! pool[{}] job={} nonce=0x{:08x} diff={:.1}",
                    pool_index, share.job_id, nonce, achieved_diff
                );
            } else {
                info!(
                    "Dispatcher: share! job={} nonce=0x{:08x} diff={:.1}",
                    share.job_id, nonce, achieved_diff
                );
            }

            // Route share to the correct pool's channel
            if let Some(pool) = self.pools.get(pool_index as usize) {
                if let Err(e) = pool.share_tx.send(MiningEvent::SubmitShare(share)) {
                    error!(
                        "Dispatcher: share send to pool[{}] failed: {}",
                        pool_index, e
                    );
                }
            } else {
                error!("Dispatcher: pool_index {} out of range", pool_index);
            }
        } else if achieved_diff >= self.stats.ticket_difficulty {
            // Case 2: Valid ASIC nonce, below pool difficulty — expected, just count
            self.stats.record_chip_nonce(asic_nr);
            self.stats.filtered += 1;
            self.stats.update_hashrate();
            if achieved_diff > self.stats.best_difficulty {
                self.stats.best_difficulty = achieved_diff;
            }
        } else {
            // Case 3: Below ASIC ticket difficulty — try all other active slots
            // before declaring a genuine HW error. On multi-chip boards (Hex),
            // UART frame misalignment or job-slot aliasing can cause the job_id
            // to map to the wrong slot. Scanning all slots recovers these nonces.
            let recovered = self
                .find_recovery_candidate(Some(idx), nonce, rolled_version, n_slots)
                .map(
                    |(slot, alt, alt_version, alt_diff, alt_meets_pool_target)| {
                        Self::handle_recovered_nonce(
                            &mut self.stats,
                            &mut self.pools,
                            &mut self.share_dedup,
                            job_id,
                            slot,
                            alt,
                            alt_version,
                            alt_diff,
                            alt_meets_pool_target,
                            asic_nr,
                            nonce,
                        );
                    },
                )
                .is_some();

            if !recovered {
                // Genuine HW error — no slot matched. The nonce belongs to NO valid
                // job, so the primary slot's `pool_index` is essentially arbitrary
                // (and on a split board mis-charges the wrong pool). MD-7: keep this
                // board-level error in the local HW counters (stats.rejected +
                // per-chip errors) only; per-pool `shares_rejected` is reserved for
                // pool-CONFIRMED rejects flowing from the Stratum client, not local
                // HW errors.
                self.stats.record_chip_error(asic_nr);
                self.stats.record_reject();
                if self.stats.rejected <= 10 {
                    warn!("HW reject #{}: job_id={:02x} nonce=0x{:08x} achieved_diff={:.2e} ticket_diff={:.0} ver=0x{:08x} nbits=0x{:08x} ntime=0x{:08x}",
                        self.stats.rejected, job_id, nonce, achieved_diff,
                        self.stats.ticket_difficulty, actual_version,
                        item.work.nbits, item.work.ntime);
                } else if self.stats.rejected % 1000 == 0 {
                    warn!(
                        "HW: {} nonces failed validation (below ticket diff)",
                        self.stats.rejected
                    );
                }
            }
        }

        // Note: update_hashrate() is called in Cases 1 and 2 only (valid nonces).
        // HW errors (Case 3) are excluded to avoid inflating hashrate estimates.
    }

    /// Get the number of pools configured.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Access active jobs array (for BM1397 midstate→version lookup).
    pub fn active_jobs(&self) -> &[Option<WorkItem>] {
        &self.active_jobs
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a single-pool dispatcher for testing.
    fn make_single_pool_dispatcher() -> (
        MiningDispatcher,
        mpsc::Sender<StratumEvent>,
        mpsc::Receiver<MiningEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::channel();
        let (share_tx, share_rx) = mpsc::channel();
        let dispatcher = MiningDispatcher::new(event_rx, share_tx, DispatcherConfig::default());
        (dispatcher, event_tx, share_rx)
    }

    /// Helper: create a two-pool dispatcher for testing.
    fn make_split_dispatcher(
        ratio_a: u8,
        ratio_b: u8,
    ) -> (
        MiningDispatcher,
        mpsc::Sender<StratumEvent>,
        mpsc::Sender<StratumEvent>,
        mpsc::Receiver<MiningEvent>,
        mpsc::Receiver<MiningEvent>,
    ) {
        let (event_tx_a, event_rx_a) = mpsc::channel();
        let (share_tx_a, share_rx_a) = mpsc::channel();
        let (event_tx_b, event_rx_b) = mpsc::channel();
        let (share_tx_b, share_rx_b) = mpsc::channel();

        let pool_a = PoolSlot::new(0, ratio_a, event_rx_a, share_tx_a);
        let pool_b = PoolSlot::new(1, ratio_b, event_rx_b, share_tx_b);

        let dispatcher =
            MiningDispatcher::with_pools(vec![pool_a, pool_b], DispatcherConfig::default());

        (dispatcher, event_tx_a, event_tx_b, share_rx_a, share_rx_b)
    }

    fn make_test_work(job_id: &str) -> MiningWork {
        MiningWork {
            job_id: job_id.into(),
            midstates: vec![[0u8; 32]],
            merkle4: [0u8; 4],
            merkle_root: [0u8; 32],
            prev_block_hash: [0u8; 32],
            version: 0x20000000,
            version_mask: 0,
            ntime: 0,
            nbits: 0x1d00ffff,
            extranonce2: "00".into(),
            share_target: [0xFFu8; 32],
        }
    }

    fn make_stratum_job(job_id: &str, clean_jobs: bool) -> StratumJob {
        StratumJob {
            job_id: job_id.into(),
            prev_hash: "0".repeat(64),
            coinbase1: String::new(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "1d00ffff".into(),
            block_height: 1,
            ntime: "00000000".into(),
            clean_jobs,
        }
    }

    fn make_test_item(
        job_id: &str,
        asic_job_id: u8,
        pool_index: u8,
        dispatch_seq: u64,
    ) -> WorkItem {
        let work = make_test_work(job_id);
        WorkItem {
            asic_job_id,
            stratum_job_id: job_id.into(),
            extranonce2: "00".into(),
            ntime_hex: "00000000".into(),
            version: work.version,
            version_mask: work.version_mask,
            hardware_version_mask: BIP320_DEFAULT_VERSION_MASK,
            work,
            dispatched_at: Instant::now(),
            pool_index,
            dispatch_seq,
        }
    }

    fn insert_test_item(
        dispatcher: &mut MiningDispatcher,
        slot: usize,
        job_id: &str,
        pool_index: u8,
        dispatch_seq: u64,
    ) {
        dispatcher.active_jobs[slot] =
            Some(make_test_item(job_id, slot as u8, pool_index, dispatch_seq));
        dispatcher.valid_jobs[slot] = true;
    }

    #[test]
    fn test_recovery_runs_for_empty_primary_slot() {
        let (event_tx, event_rx) = mpsc::channel();
        let (share_tx, share_rx) = mpsc::channel();
        let mut dispatcher = MiningDispatcher::new(
            event_rx,
            share_tx,
            DispatcherConfig {
                job_interval_ms: 10,
                job_id_step: 16,
                job_id_max: 128,
                midstate_mode: false,
            },
        );

        let work = make_test_work("stratum-job");
        dispatcher.active_jobs[0] = Some(WorkItem {
            asic_job_id: 0,
            stratum_job_id: "stratum-job".into(),
            extranonce2: "00".into(),
            ntime_hex: "00000000".into(),
            version: 0x20000000,
            version_mask: 0,
            hardware_version_mask: BIP320_DEFAULT_VERSION_MASK,
            work,
            dispatched_at: Instant::now(),
            pool_index: 0,
            dispatch_seq: 0,
        });
        dispatcher.valid_jobs[0] = true;
        dispatcher.dispatch_seq = 1;
        dispatcher.stats.ticket_difficulty = 0.0;

        // BM1368 can return an extracted ID such as 0x08 while the conservative
        // +16 dispatch path has only slot 0 active. Recovery must scan live
        // slots before dropping the nonce as stale/unknown.
        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        assert_eq!(dispatcher.stats.accepted, 1);
        match share_rx.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "stratum-job"),
        }
        drop(event_tx);
    }

    #[test]
    fn test_recovery_runs_for_stale_primary_slot() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_id_step = 16;
        dispatcher.config.job_id_max = 128;
        dispatcher.stats.ticket_difficulty = 0.0;

        insert_test_item(&mut dispatcher, 0, "live-job", 0, 1);
        insert_test_item(&mut dispatcher, 8, "stale-job", 0, 0);
        dispatcher.valid_jobs[8] = false;

        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        assert_eq!(dispatcher.stats.accepted, 1);
        match share_rx.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "live-job"),
        }
    }

    #[test]
    fn test_recovery_runs_for_expired_primary_slot() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_id_step = 16;
        dispatcher.config.job_id_max = 128;
        dispatcher.stats.ticket_difficulty = 0.0;

        insert_test_item(&mut dispatcher, 0, "live-job", 0, 1);
        insert_test_item(&mut dispatcher, 8, "expired-job", 0, 0);
        dispatcher.active_jobs[8].as_mut().unwrap().dispatched_at =
            Instant::now() - Duration::from_secs(MAX_WORK_AGE_SECS + 1);

        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        assert_eq!(dispatcher.stats.accepted, 1);
        match share_rx.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "live-job"),
        }
    }

    #[test]
    fn test_recovery_runs_for_aliased_primary_slot() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_id_step = 16;
        dispatcher.config.job_id_max = 128;
        dispatcher.stats.ticket_difficulty = 0.0;

        insert_test_item(&mut dispatcher, 0, "current-job", 0, 20);
        insert_test_item(&mut dispatcher, 8, "overwritten-job", 0, 0);
        dispatcher.dispatch_seq = 20;

        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        assert_eq!(dispatcher.stats.accepted, 1);
        match share_rx.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "current-job"),
        }
    }

    #[test]
    fn test_recovered_share_routes_to_origin_pool() {
        let (mut dispatcher, _event_tx_a, _event_tx_b, share_rx_a, share_rx_b) =
            make_split_dispatcher(50, 50);
        dispatcher.config.job_id_step = 16;
        dispatcher.config.job_id_max = 128;
        dispatcher.stats.ticket_difficulty = 0.0;

        insert_test_item(&mut dispatcher, 16, "pool-b-job", 1, 1);
        dispatcher.dispatch_seq = 1;

        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        assert!(share_rx_a.try_recv().is_err());
        match share_rx_b.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "pool-b-job"),
        }
        assert_eq!(dispatcher.pools[1].shares_submitted, 1);
        assert_eq!(dispatcher.pools[0].shares_submitted, 0);
    }

    #[test]
    fn test_recovery_scans_wrapped_job_id_cycle() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_id_step = 24;
        dispatcher.config.job_id_max = 128;
        dispatcher.stats.ticket_difficulty = 0.0;

        // A +24 mod 128 sequence reaches slot 0x10 only after wrapping:
        // 00,18,30,48,60,78,10...
        insert_test_item(&mut dispatcher, 0x10, "wrapped-job", 0, 1);
        dispatcher.dispatch_seq = 1;

        dispatcher.handle_nonce(0x08, 0x12345678, 0, 0);

        assert_eq!(dispatcher.stats.slot_recoveries, 1);
        match share_rx.try_recv().unwrap() {
            MiningEvent::SubmitShare(share) => assert_eq!(share.job_id, "wrapped-job"),
        }
    }

    #[test]
    fn test_single_pool_backward_compat() {
        let (dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        assert_eq!(dispatcher.pool_count(), 1);
        assert_eq!(dispatcher.pools[0].target_ratio, 100);
    }

    #[test]
    fn test_split_pool_creation() {
        let (dispatcher, _, _, _, _) = make_split_dispatcher(70, 30);
        assert_eq!(dispatcher.pool_count(), 2);
        assert_eq!(dispatcher.pools[0].target_ratio, 70);
        assert_eq!(dispatcher.pools[1].target_ratio, 30);
    }

    #[test]
    fn test_select_pool_single() {
        let (mut dispatcher, _, _) = make_single_pool_dispatcher();
        dispatcher.pools[0].connected = true;
        dispatcher.pools[0].prebuilt_work = Some(make_test_work("a"));
        assert_eq!(dispatcher.select_pool(), Some(0));
    }

    #[test]
    fn test_select_pool_deficit_scheduler() {
        let (mut dispatcher, _, _, _, _) = make_split_dispatcher(70, 30);

        // Simulate both pools being ready
        dispatcher.pools[0].current_job = Some(StratumJob {
            job_id: "a".into(),
            prev_hash: "0".repeat(64),
            coinbase1: String::new(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "1d00ffff".into(),
            block_height: 1,
            ntime: "00000000".into(),
            clean_jobs: false,
        });
        dispatcher.pools[0].work_builder = Some(WorkBuilder::new("00000000", 4));

        dispatcher.pools[1].current_job = Some(StratumJob {
            job_id: "b".into(),
            prev_hash: "0".repeat(64),
            coinbase1: String::new(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "1d00ffff".into(),
            block_height: 1,
            ntime: "00000000".into(),
            clean_jobs: false,
        });
        dispatcher.pools[1].work_builder = Some(WorkBuilder::new("11111111", 4));

        // First dispatch: both at 0, should go to primary (pool 0)
        assert_eq!(dispatcher.select_pool(), Some(0));

        // Simulate 7 dispatches to pool 0, 0 to pool 1
        dispatcher.pools[0].dispatched_count = 7;
        dispatcher.pools[1].dispatched_count = 0;

        // Pool 0: actual=100%, target=70% -> deficit=-30%
        // Pool 1: actual=0%, target=30% -> deficit=+30%
        // Should select pool 1 (largest deficit)
        assert_eq!(dispatcher.select_pool(), Some(1));

        // After 7:3 split
        dispatcher.pools[0].dispatched_count = 7;
        dispatcher.pools[1].dispatched_count = 3;

        // Pool 0: actual=70%, target=70% -> deficit=0%
        // Pool 1: actual=30%, target=30% -> deficit=0%
        // Both even -- first one with highest deficit wins (pool 0 since 0.0 == 0.0)
        let selected = dispatcher.select_pool();
        assert!(
            selected == Some(0) || selected == Some(1),
            "Either pool OK when balanced"
        );
    }

    #[test]
    fn test_select_pool_fallback_when_one_down() {
        let (mut dispatcher, _, _, _, _) = make_split_dispatcher(70, 30);

        // Only pool 0 is ready
        dispatcher.pools[0].current_job = Some(StratumJob {
            job_id: "a".into(),
            prev_hash: "0".repeat(64),
            coinbase1: String::new(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "1d00ffff".into(),
            block_height: 1,
            ntime: "00000000".into(),
            clean_jobs: false,
        });
        dispatcher.pools[0].work_builder = Some(WorkBuilder::new("00000000", 4));

        // Pool 1 has no job (disconnected)
        assert_eq!(dispatcher.select_pool(), Some(0));
    }

    #[test]
    fn test_first_ready_job_dispatches_without_interval_delay() {
        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_interval_ms = 60_000;
        dispatcher.last_work_sent = Instant::now();

        event_tx
            .send(StratumEvent::ExtranonceChanged {
                extranonce1: "00000000".into(),
                extranonce2_size: 4,
            })
            .unwrap();
        event_tx.send(StratumEvent::Reconnected).unwrap();
        event_tx
            .send(StratumEvent::NewJob(StratumJob {
                job_id: "first".into(),
                prev_hash: "0".repeat(64),
                coinbase1: String::new(),
                coinbase2: String::new(),
                merkle_branches: vec![],
                version: "20000000".into(),
                nbits: "1d00ffff".into(),
                block_height: 1,
                ntime: "00000000".into(),
                clean_jobs: false,
            }))
            .unwrap();

        let mut sent = 0usize;
        dispatcher.run_once(
            &mut |_work, _job_id| {
                sent += 1;
                Ok(())
            },
            &mut || Vec::new(),
            &mut |_, _| {},
        );

        assert_eq!(sent, 1);
    }

    #[test]
    fn test_clean_job_dispatches_without_interval_delay() {
        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_interval_ms = 60_000;
        dispatcher.last_work_sent = Instant::now();
        dispatcher.pools[0].connected = true;
        dispatcher.pools[0].work_builder = Some(WorkBuilder::new("00000000", 4));
        dispatcher.pools[0].current_job = Some(StratumJob {
            job_id: "old".into(),
            prev_hash: "0".repeat(64),
            coinbase1: String::new(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "1d00ffff".into(),
            block_height: 1,
            ntime: "00000000".into(),
            clean_jobs: false,
        });
        insert_test_item(&mut dispatcher, 0, "old", 0, 0);

        event_tx
            .send(StratumEvent::NewJob(StratumJob {
                job_id: "new".into(),
                prev_hash: "0".repeat(64),
                coinbase1: String::new(),
                coinbase2: String::new(),
                merkle_branches: vec![],
                version: "20000000".into(),
                nbits: "1d00ffff".into(),
                block_height: 2,
                ntime: "00000001".into(),
                clean_jobs: true,
            }))
            .unwrap();

        let mut sent = 0usize;
        dispatcher.run_once(
            &mut |work, _job_id| {
                assert_eq!(work.job_id, "new");
                sent += 1;
                Ok(())
            },
            &mut || Vec::new(),
            &mut |_, _| {},
        );

        assert_eq!(sent, 1);
        assert_eq!(
            dispatcher.active_jobs[0]
                .as_ref()
                .map(|item| item.stratum_job_id.as_str()),
            Some("new")
        );
        assert!(dispatcher.valid_jobs[0]);
    }

    #[test]
    fn test_drain_events_scoped_clean_jobs() {
        let (event_tx_a, event_rx_a) = mpsc::channel();
        let (share_tx_a, _share_rx_a) = mpsc::channel();
        let (_event_tx_b, event_rx_b) = mpsc::channel();
        let (share_tx_b, _share_rx_b) = mpsc::channel();

        let pool_a = PoolSlot::new(0, 70, event_rx_a, share_tx_a);
        let pool_b = PoolSlot::new(1, 30, event_rx_b, share_tx_b);

        let mut dispatcher =
            MiningDispatcher::with_pools(vec![pool_a, pool_b], DispatcherConfig::default());

        // Seed some work items from both pools
        dispatcher.active_jobs[0] = Some(WorkItem {
            asic_job_id: 0,
            stratum_job_id: "a1".into(),
            extranonce2: "00".into(),
            ntime_hex: "00000000".into(),
            version: 0x20000000,
            version_mask: 0,
            hardware_version_mask: BIP320_DEFAULT_VERSION_MASK,
            work: MiningWork {
                job_id: "a1".into(),
                midstates: vec![[0u8; 32]],
                merkle4: [0u8; 4],
                merkle_root: [0u8; 32],
                prev_block_hash: [0u8; 32],
                version: 0x20000000,
                version_mask: 0,
                ntime: 0,
                nbits: 0,
                extranonce2: "00".into(),
                share_target: [0xFFu8; 32],
            },
            dispatched_at: Instant::now(),
            pool_index: 0,
            dispatch_seq: 0,
        });
        dispatcher.valid_jobs[0] = true;

        dispatcher.active_jobs[16] = Some(WorkItem {
            asic_job_id: 16,
            stratum_job_id: "b1".into(),
            extranonce2: "00".into(),
            ntime_hex: "00000000".into(),
            version: 0x20000000,
            version_mask: 0,
            hardware_version_mask: BIP320_DEFAULT_VERSION_MASK,
            work: MiningWork {
                job_id: "b1".into(),
                midstates: vec![[0u8; 32]],
                merkle4: [0u8; 4],
                merkle_root: [0u8; 32],
                prev_block_hash: [0u8; 32],
                version: 0x20000000,
                version_mask: 0,
                ntime: 0,
                nbits: 0,
                extranonce2: "00".into(),
                share_target: [0xFFu8; 32],
            },
            dispatched_at: Instant::now(),
            pool_index: 1,
            dispatch_seq: 1,
        });
        dispatcher.valid_jobs[16] = true;

        // Send clean_jobs from pool A only
        event_tx_a
            .send(StratumEvent::NewJob(StratumJob {
                job_id: "a2".into(),
                prev_hash: "0".repeat(64),
                coinbase1: String::new(),
                coinbase2: String::new(),
                merkle_branches: vec![],
                version: "20000000".into(),
                nbits: "1d00ffff".into(),
                block_height: 2,
                ntime: "00000001".into(),
                clean_jobs: true,
            }))
            .unwrap();

        dispatcher.drain_events();

        // Pool A's work (index 0) should be invalidated
        assert!(
            !dispatcher.valid_jobs[0],
            "Pool A work should be invalidated by clean_jobs"
        );
        // Pool B's work (index 16) should still be valid
        assert!(
            dispatcher.valid_jobs[16],
            "Pool B work should NOT be invalidated by Pool A's clean_jobs"
        );
    }

    #[test]
    fn test_bm136x_reconstructs_rolled_version_when_work_mask_zero() {
        let (dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        let item = make_test_item("rolled-default-mask", 0, 0, 0);

        let actual_version = dispatcher.actual_version_for(&item, 0x0000_4000);

        assert_eq!(actual_version, 0x2000_4000);
        assert_eq!(
            MiningDispatcher::share_version_bits(&item, actual_version).as_deref(),
            Some("00004000")
        );
    }

    /// MD-10 cross-repo regression pin — BIP320 version-rolling contract.
    ///
    /// This test exists to keep DCENT_axe's BM136x version reconstruction
    /// byte-faithful to the DCENT_OS am2 BM1362 load-bearing rule
    /// :
    /// AM2 BM1362 chips DO roll BIP320 (mask `0x1FFFE000`, positions 13..28).
    /// The inherited `.79` code that REFUSED all parsed nonces with
    /// `version_bits_raw != 0` discarded ~95% of valid work; the fix
    /// reconstructs `rolled_version = (version & !0x1FFFE000) | (rolled &
    /// 0x1FFFE000)`. DCENT_OS has a CI ban-gate + Rust pin so the rejection
    /// guard can never be re-introduced. This test is the DCENT_axe half of
    /// that cross-repo contract — if either repo regresses the mask, the
    /// positions, the reconstruction formula, OR the "mask==0 still rolls"
    /// behavior, a pin fails. TEST-ONLY: no production logic changes here.
    #[test]
    fn test_bip320_canonical_mask_and_formula_are_pinned() {
        // (a) The canonical BIP320 version-rolling mask must stay 0x1FFFE000.
        //     This is the exact constant DCENT_OS pins for am2 BM1362 and the
        //     value the dispatcher falls back to when work metadata reports 0.
        assert_eq!(
            BIP320_DEFAULT_VERSION_MASK, 0x1FFF_E000,
            "BIP320 canonical version-rolling mask regressed — must be 0x1FFFE000 \
             (see DCENT_OS )"
        );

        // (b) The mask must occupy exactly bit positions 13..=28 (16 bits),
        //     matching the DCENT_OS `(vbits << 13) & 0x1FFFE000` placement.
        assert_eq!(
            BIP320_DEFAULT_VERSION_MASK.trailing_zeros(),
            13,
            "BIP320 mask low edge regressed — version-rolling bits must start at position 13"
        );
        // Highest set bit index = 31 - leading_zeros. The top rollable bit is
        // position 28 (`1 << 28`), i.e. inclusive positions 13..=28 = 16 bits.
        assert_eq!(
            31 - BIP320_DEFAULT_VERSION_MASK.leading_zeros(),
            28,
            "BIP320 mask high edge regressed — top version-rolling bit must be position 28"
        );
        assert_eq!(
            BIP320_DEFAULT_VERSION_MASK.count_ones(),
            16,
            "BIP320 mask width regressed — must be exactly 16 rollable bits (positions 13..=28)"
        );
        // Verify the mask is a single contiguous run of set bits at positions 13..=28.
        let mut expected_mask = 0u32;
        for pos in 13..=28 {
            expected_mask |= 1 << pos;
        }
        assert_eq!(
            BIP320_DEFAULT_VERSION_MASK, expected_mask,
            "BIP320 mask is not the contiguous positions-13..=28 run it must be"
        );

        // (c) The reconstruction formula must be
        //     (base_version & !mask) | (rolled & mask) — byte-identical to the
        //     DCENT_OS `hybrid_build_header` reconstruction. We pin it against
        //     the live BM136x branch of `actual_version_for` (midstate_mode=false,
        //     the default), proving the dispatcher applies exactly that formula.
        let (dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        assert!(
            !dispatcher.config.midstate_mode,
            "default dispatcher must exercise the BM136x reconstruction branch"
        );

        // Item whose work metadata carries the canonical mask explicitly.
        let mut item = make_test_item("bip320-canonical", 0, 0, 0);
        item.work.version = 0x2000_0000;
        item.work.version_mask = BIP320_DEFAULT_VERSION_MASK;

        // A rolled value that touches several bits inside the mask AND bits
        // outside it — the outside bits MUST be discarded by the `& mask`.
        let rolled = 0x1234_5678u32;
        let actual = dispatcher.actual_version_for(&item, rolled);
        let expected = (item.work.version & !BIP320_DEFAULT_VERSION_MASK)
            | (rolled & BIP320_DEFAULT_VERSION_MASK);
        assert_eq!(
            actual, expected,
            "BM136x version reconstruction must be (version & !mask) | (rolled & mask)"
        );
        // No bit outside the mask may have changed from the base version.
        assert_eq!(
            actual & !BIP320_DEFAULT_VERSION_MASK,
            item.work.version & !BIP320_DEFAULT_VERSION_MASK,
            "reconstruction leaked rolled bits OUTSIDE the BIP320 mask"
        );

        // (d) The exact bug the DCENT_OS CI ban-gate prevents: a non-zero
        //     rolled/version-bits input with work.version_mask == 0 must STILL
        //     roll (fall back to the canonical hardware mask), NOT be rejected
        //     or dropped to the base version. This is the "never re-introduce
        //     the rejection guard" invariant.
        let mut zero_mask_item = make_test_item("bip320-zero-mask", 0, 0, 0);
        zero_mask_item.work.version = 0x2000_0000;
        zero_mask_item.work.version_mask = 0;
        // hardware_version_mask is the canonical fallback (set by make_test_item).
        assert_eq!(
            zero_mask_item.hardware_version_mask, BIP320_DEFAULT_VERSION_MASK,
            "hardware fallback mask must be the canonical BIP320 mask"
        );
        let rolled_nonzero = 0x0000_4000u32; // one bit inside the mask
        let zero_mask_actual = dispatcher.actual_version_for(&zero_mask_item, rolled_nonzero);
        assert_ne!(
            zero_mask_actual, zero_mask_item.work.version,
            "REGRESSION: non-zero version bits with work_mask==0 were dropped — \
             this is exactly the rejection guard DCENT_OS forbids re-introducing"
        );
        assert_eq!(
            zero_mask_actual,
            (zero_mask_item.work.version & !BIP320_DEFAULT_VERSION_MASK)
                | (rolled_nonzero & BIP320_DEFAULT_VERSION_MASK),
            "work_mask==0 path must roll using the canonical fallback mask + formula"
        );
    }

    #[test]
    fn test_zero_rolled_version_with_zero_work_mask_keeps_base_version() {
        let (dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        let item = make_test_item("unrolled", 0, 0, 0);

        let actual_version = dispatcher.actual_version_for(&item, 0);

        assert_eq!(actual_version, 0x2000_0000);
        assert_eq!(
            MiningDispatcher::share_version_bits(&item, actual_version),
            None
        );
    }

    #[test]
    fn test_mixed_version_mask_split_pool_is_not_dispatched_while_live_jobs_exist() {
        let (mut dispatcher, _event_tx_a, _event_tx_b, _share_rx_a, _share_rx_b) =
            make_split_dispatcher(50, 50);
        insert_test_item(&mut dispatcher, 0, "pool-a-live", 0, 0);

        dispatcher.pools[1].connected = true;
        dispatcher.pools[1].version_mask = 0x0000_e000;
        let mut wb = WorkBuilder::new("11111111", 4);
        wb.set_version_mask(0x0000_e000);
        dispatcher.pools[1].work_builder = Some(wb);
        dispatcher.pools[1].current_job = Some(make_stratum_job("pool-b-new-mask", false));

        assert_eq!(dispatcher.select_pool(), None);
    }

    #[test]
    fn test_first_work_with_new_mask_applies_hardware_mask_before_send() {
        use std::cell::RefCell;

        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_interval_ms = 60_000;
        dispatcher.last_work_sent = Instant::now();

        event_tx
            .send(StratumEvent::ExtranonceChanged {
                extranonce1: "00000000".into(),
                extranonce2_size: 4,
            })
            .unwrap();
        event_tx.send(StratumEvent::Reconnected).unwrap();
        event_tx
            .send(StratumEvent::VersionMaskChanged(0x0000_e000))
            .unwrap();
        event_tx
            .send(StratumEvent::NewJob(make_stratum_job(
                "first-new-mask",
                false,
            )))
            .unwrap();

        let calls = RefCell::new(Vec::new());
        dispatcher.run_once(
            &mut |work, _job_id| {
                calls
                    .borrow_mut()
                    .push(format!("send:{:08x}:{}", work.version_mask, work.job_id));
                Ok(())
            },
            &mut || Vec::new(),
            &mut |_diff, mask| {
                if let Some(mask) = mask {
                    calls.borrow_mut().push(format!("mask:{mask:08x}"));
                }
            },
        );

        let calls = calls.into_inner();
        let mask_pos = calls
            .iter()
            .position(|call| call == "mask:0000e000")
            .expect("hardware mask applied");
        let send_pos = calls
            .iter()
            .position(|call| call == "send:0000e000:first-new-mask")
            .expect("work sent");
        assert!(
            mask_pos < send_pos,
            "hardware mask must be applied before dispatch, calls={calls:?}"
        );
    }

    // -----------------------------------------------------------------------
    // SV2-5 regression pins: BIP320 canonical mask on the SV2 work path.
    //
    // The SV2 NewJob handler builds work via bridge::sv2_job_to_mining_work,
    // which lives in the non-host-compilable dcentaxe binary crate. These
    // tests replicate that function's EXACT midstate-construction calls
    // (compute_midstate + increment_bitmask, both pub in dcentaxe_stratum)
    // and the dispatcher's rolled-version reconstruction, proving end-to-end
    // that BM1397 gets 4 midstates (ASICBoost ON) under SV2 while
    // BM1366/68/70/73 behavior is byte-identical (default-preserving).
    //
    // MUST equal bridge.rs BIP320_CANONICAL_VERSION_MASK and DCENT_OS's am2
    // BM1362 load-bearing rule (SERIAL_VERSION_ROLLING_FIELD_MASK = 0x1FFF_E000).
    // -----------------------------------------------------------------------

    /// Faithful replica of bridge::sv2_job_to_mining_work's midstate loop,
    /// including the mask-0 -> canonical upgrade the bridge applies.
    fn sv2_replica_midstates(version: u32, version_mask: u32) -> (Vec<[u8; 32]>, u32) {
        use dcentaxe_stratum::work::{compute_midstate, increment_bitmask};

        // Bridge upgrade: SV2 channels carry no negotiated mask (0) -> canonical.
        let version_mask = if version_mask == 0 {
            BIP320_DEFAULT_VERSION_MASK
        } else {
            version_mask
        };

        // Dummy prev_hash / merkle_root: only the first 4 bytes (version) of the
        // 64-byte header prefix change between midstates, which is what makes the
        // midstates distinct. The actual hash bytes don't matter for this property.
        let prev_hash = [0x11u8; 32];
        let merkle_root = [0x22u8; 32];

        let mut header_prefix = [0u8; 64];
        header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
        header_prefix[4..36].copy_from_slice(&prev_hash);
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        let mut midstates = Vec::with_capacity(4);
        midstates.push(compute_midstate(&header_prefix));

        if version_mask != 0 {
            let mut rolled = version;
            for _ in 0..3 {
                rolled = increment_bitmask(rolled, version_mask);
                header_prefix[0..4].copy_from_slice(&rolled.to_le_bytes());
                midstates.push(compute_midstate(&header_prefix));
            }
        }

        (midstates, version_mask)
    }

    /// SV2-5 direct regression pin: a BM1397 SV2 job arriving with the
    /// negotiated mask = 0 must still produce 4 DISTINCT midstates (ASICBoost
    /// ON). Against the pre-fix bridge (which gated the loop on `mask != 0`),
    /// this yields only 1 midstate and the assertion fails.
    #[test]
    fn sv2_bm1397_mask0_produces_four_midstates_asicboost_on() {
        let version = 0x2000_0000u32;
        let (midstates, effective_mask) = sv2_replica_midstates(version, 0);

        // Mask was upgraded to the canonical BIP320 value.
        assert_eq!(effective_mask, 0x1FFF_E000);

        // ASICBoost ON: exactly 4 midstates instead of the pre-fix single one.
        assert_eq!(
            midstates.len(),
            4,
            "BM1397 under SV2 with mask 0 must roll 4 midstates (ASICBoost)"
        );

        // Each rolled midstate must differ from midstate[0] (distinct versions).
        for (idx, ms) in midstates.iter().enumerate().skip(1) {
            assert_ne!(
                *ms, midstates[0],
                "midstate[{idx}] must differ from midstate[0] (distinct rolled version)"
            );
        }
        // And all 4 must be pairwise distinct.
        for i in 0..midstates.len() {
            for j in (i + 1)..midstates.len() {
                assert_ne!(
                    midstates[i], midstates[j],
                    "midstates[{i}] and midstates[{j}] must be distinct"
                );
            }
        }
    }

    /// Prove the dispatcher reconstructs the correct rolled version per
    /// midstate index for a BM1397 (midstate_mode) SV2 work item whose
    /// version_mask carries the canonical BIP320 mask, matching the V1
    /// increment_bitmask chaining used to build the midstates.
    #[test]
    fn sv2_bm1397_reconstructs_rolled_version_per_midstate_idx() {
        use dcentaxe_stratum::work::increment_bitmask;

        let (event_tx, event_rx) = mpsc::channel();
        let (share_tx, _share_rx) = mpsc::channel();
        // midstate_mode = true mirrors DispatcherConfig::for_bm1397.
        let dispatcher = MiningDispatcher::new(
            event_rx,
            share_tx,
            DispatcherConfig {
                job_interval_ms: 10,
                job_id_step: 4,
                job_id_max: 128,
                midstate_mode: true,
            },
        );

        let base_version = 0x2000_0000u32;
        // SV2 work item: version_mask now carries the canonical mask (post-fix).
        let mut item = make_test_item("sv2-bm1397", 0, 0, 0);
        item.work.version = base_version;
        item.work.version_mask = 0x1FFF_E000;
        item.version = base_version;
        item.version_mask = 0x1FFF_E000;

        // The dispatcher receives the midstate index in `rolled_version` (0..3)
        // and reconstructs the actual version via increment_bitmask chaining,
        // exactly matching how the bridge built each midstate.
        let mut expected = base_version;
        for idx in 0..4u32 {
            let actual = dispatcher.actual_version_for(&item, idx);
            assert_eq!(
                actual, expected,
                "midstate_idx {idx} must reconstruct to the chained rolled version"
            );
            expected = increment_bitmask(expected, 0x1FFF_E000);
        }

        // Sanity: the four reconstructed versions are all distinct (real rolling).
        let v0 = dispatcher.actual_version_for(&item, 0);
        let v1 = dispatcher.actual_version_for(&item, 1);
        let v2 = dispatcher.actual_version_for(&item, 2);
        let v3 = dispatcher.actual_version_for(&item, 3);
        assert!(v0 != v1 && v1 != v2 && v2 != v3 && v0 != v3);
        drop(event_tx);
    }

    /// DCENT_axe BM1397 family scaling: `for_bm1397` accepts the chip count and
    /// the single BM1397 driver path covers the 1x / 4x / 6x SKUs. The job-id
    /// mechanics (step +4 mod 128, midstate ASICBoost mode) are chip-family
    /// constants identical for every chip count; only the work interval scales
    /// down with more chips so a longer chain never exhausts its nonce space.
    #[test]
    fn for_bm1397_scales_to_single_quad_hex() {
        // 100 MHz keeps all three intervals above the 5 ms UART floor AND
        // distinct, so the proportional-to-1/N relationship is observable.
        // interval = floor(2^32 / (freq_khz * 672) / N), clamped to [5, 500].
        // 2^32 / (100_000 * 672) = 63.91, so N=1->63, N=4->15, N=6->10.
        let one = DispatcherConfig::for_bm1397(100.0, 1);
        let quad = DispatcherConfig::for_bm1397(100.0, 4);
        let hex = DispatcherConfig::for_bm1397(100.0, 6);

        for cfg in [&one, &quad, &hex] {
            assert_eq!(
                cfg.job_id_step, 4,
                "BM1397 dispatches +4 mod 128 (4 midstate slots)"
            );
            assert_eq!(cfg.job_id_max, 128);
            assert!(cfg.midstate_mode, "BM1397 uses midstate (ASICBoost) mode");
            // 128 / gcd(128, 4) = 32 distinguishable job-id slots, independent of N.
            assert_eq!(cfg.job_id_cycle_len(), 32);
        }

        assert_eq!(one.job_interval_ms, 63, "single-chip interval");
        assert_eq!(quad.job_interval_ms, 15, "4-chip interval (~1/4 of single)");
        assert_eq!(hex.job_interval_ms, 10, "6-chip interval (~1/6 of single)");
        // More chips => strictly shorter interval (down to the 5 ms floor).
        assert!(one.job_interval_ms > quad.job_interval_ms);
        assert!(quad.job_interval_ms > hex.job_interval_ms);
        assert!(hex.job_interval_ms >= 5);

        // At realistic eco frequency the 4x/6x intervals hit the 5 ms floor,
        // which is the intended "always fresh work" behavior for dense chains.
        assert_eq!(DispatcherConfig::for_bm1397(425.0, 4).job_interval_ms, 5);
        assert_eq!(DispatcherConfig::for_bm1397(425.0, 6).job_interval_ms, 5);
    }

    /// Default-preserving guarantee: full-header boards (BM1366/68/70/73,
    /// midstate_mode=false) behave identically whether the SV2 work carries
    /// version_mask=0 or the canonical 0x1FFFE000. The dispatcher already
    /// upgrades 0 -> canonical in effective_hardware_mask, so making the
    /// SV2-built work carry the canonical mask explicitly is a no-op for them.
    #[test]
    fn sv2_full_header_boards_unaffected_by_canonical_mask() {
        let (dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        // make_single_pool_dispatcher uses DispatcherConfig::default()
        // (midstate_mode = false), i.e. a full-header board.
        assert!(!dispatcher.config.midstate_mode);

        // effective_hardware_mask upgrades 0 -> canonical and leaves canonical
        // unchanged: both inputs resolve to the same programmed hardware mask.
        assert_eq!(dispatcher.effective_hardware_mask(0), 0x1FFF_E000);
        assert_eq!(dispatcher.effective_hardware_mask(0x1FFF_E000), 0x1FFF_E000);
        assert_eq!(
            dispatcher.effective_hardware_mask(0),
            dispatcher.effective_hardware_mask(0x1FFF_E000)
        );

        // Reconstruction is identical for a full-header item whether the work
        // carries mask 0 (relying on hardware_version_mask) or the explicit
        // canonical mask. The ASIC returns the rolled version bits directly.
        let rolled_bits = 0x0000_4000u32; // a value within the canonical field

        // Item A: work.version_mask = 0 (pre-fix SV2 work shape). Reconstruction
        // falls back to hardware_version_mask (canonical) because rolled != 0.
        let item_mask0 = make_test_item("full-header-mask0", 0, 0, 0);
        assert_eq!(item_mask0.work.version_mask, 0);
        assert_eq!(item_mask0.hardware_version_mask, 0x1FFF_E000);
        let recon_mask0 = dispatcher.actual_version_for(&item_mask0, rolled_bits);

        // Item B: work.version_mask = canonical (post-fix SV2 work shape).
        let mut item_canon = make_test_item("full-header-canon", 0, 0, 0);
        item_canon.work.version_mask = 0x1FFF_E000;
        item_canon.version_mask = 0x1FFF_E000;
        let recon_canon = dispatcher.actual_version_for(&item_canon, rolled_bits);

        assert_eq!(
            recon_mask0, recon_canon,
            "full-header reconstruction must be byte-identical pre/post canonical-mask"
        );
        // And the reconstructed version actually applies the rolled bits.
        assert_eq!(recon_canon, 0x2000_4000);
    }

    // -----------------------------------------------------------------------
    // MD-9: work-queue starvation guard on send failure.
    // -----------------------------------------------------------------------

    /// A failed `send_work_fn` must NOT burn the job_id slot, NOT reset the
    /// dispatch interval timer, and MUST surface a consecutive-failure streak.
    #[test]
    fn test_send_failure_rolls_back_job_id_and_counts_streak() {
        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();
        dispatcher.config.job_interval_ms = 0; // dispatch every tick
        dispatcher.config.job_id_step = 8;
        dispatcher.pools[0].connected = true;
        dispatcher.pools[0].work_builder = Some(WorkBuilder::new("00000000", 4));
        dispatcher.pools[0].current_job = Some(make_stratum_job("job", false));

        let job_id_before = dispatcher.next_job_id;
        let last_sent_before = dispatcher.last_work_sent;

        // First tick: send fails.
        dispatcher.run_once(
            &mut |_work, _job_id| Err("uart busy".to_string()),
            &mut || Vec::new(),
            &mut |_, _| {},
        );

        // job_id was rolled back (slot reused), no work item stored, and the
        // interval timer was NOT advanced so the next tick retries immediately.
        assert_eq!(
            dispatcher.next_job_id, job_id_before,
            "failed send must roll back the pre-advanced job_id"
        );
        assert!(
            dispatcher.active_jobs[job_id_before as usize].is_none(),
            "failed send must not store a work item"
        );
        assert!(
            !dispatcher.valid_jobs[job_id_before as usize],
            "failed send must not mark the slot valid"
        );
        assert_eq!(dispatcher.stats.consecutive_send_failures, 1);
        assert_eq!(dispatcher.stats.send_failures_total, 1);
        assert_eq!(
            dispatcher.last_work_sent, last_sent_before,
            "failed send must NOT reset the interval timer (retry next tick)"
        );

        // Second tick: send succeeds → streak clears, slot is consumed.
        let mut sent = 0usize;
        dispatcher.run_once(
            &mut |_work, _job_id| {
                sent += 1;
                Ok(())
            },
            &mut || Vec::new(),
            &mut |_, _| {},
        );
        assert_eq!(sent, 1);
        assert_eq!(
            dispatcher.stats.consecutive_send_failures, 0,
            "a successful send must clear the consecutive-failure streak"
        );
        assert_eq!(
            dispatcher.stats.send_failures_total, 1,
            "cumulative total persists across a later success"
        );
        assert!(dispatcher.valid_jobs[job_id_before as usize]);
        drop(event_tx);
    }

    // -----------------------------------------------------------------------
    // MD-3: VersionMaskChanged invalidates in-flight items on a stale mask.
    // -----------------------------------------------------------------------

    #[test]
    fn test_version_mask_change_invalidates_stale_inflight_items() {
        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();

        // Two in-flight items: one on the canonical mask, one on a narrower mask.
        insert_test_item(&mut dispatcher, 0, "canonical-mask", 0, 0);
        insert_test_item(&mut dispatcher, 8, "narrow-mask", 0, 1);
        dispatcher.active_jobs[0]
            .as_mut()
            .unwrap()
            .hardware_version_mask = BIP320_DEFAULT_VERSION_MASK;
        dispatcher.active_jobs[8]
            .as_mut()
            .unwrap()
            .hardware_version_mask = 0x0000_e000;

        // Pool negotiates the canonical mask. effective_hardware_mask(canonical)
        // == canonical, so the canonical item stays valid and the narrow item
        // (whose hardware mask differs) is invalidated.
        event_tx
            .send(StratumEvent::VersionMaskChanged(
                BIP320_DEFAULT_VERSION_MASK,
            ))
            .unwrap();
        dispatcher.drain_events();

        assert!(
            dispatcher.valid_jobs[0],
            "item already on the new effective mask must stay valid"
        );
        assert!(
            !dispatcher.valid_jobs[8],
            "in-flight item on a different hardware mask must be invalidated"
        );
    }

    // -----------------------------------------------------------------------
    // MD-7: a board-level HW error must not charge any pool's shares_rejected.
    // -----------------------------------------------------------------------

    #[test]
    fn test_hw_error_does_not_charge_pool_shares_rejected() {
        let (mut dispatcher, _event_tx, _share_rx) = make_single_pool_dispatcher();
        // High ticket difficulty so the nonce is below ticket → Case 3 HW error.
        dispatcher.stats.ticket_difficulty = f64::MAX;
        // A live primary slot whose work is genuinely not matched by the nonce.
        insert_test_item(&mut dispatcher, 0, "primary", 0, 0);
        // share_target all-0x00 makes meets_pool_target impossible and diff tiny.
        dispatcher.active_jobs[0]
            .as_mut()
            .unwrap()
            .work
            .share_target = [0u8; 32];
        dispatcher.dispatch_seq = 1;

        dispatcher.handle_nonce(0, 0x0000_0001, 0, 0);

        assert_eq!(
            dispatcher.stats.rejected, 1,
            "a genuine HW error increments the local reject counter"
        );
        assert_eq!(
            dispatcher.pools[0].shares_rejected, 0,
            "a board-level HW error must NOT charge the pool's shares_rejected"
        );
        assert_eq!(dispatcher.stats.per_chip[0].errors, 1);
    }

    // -----------------------------------------------------------------------
    // XPPROTO-1: dispatcher-level cross-stream / recovery-resurface dedup.
    // -----------------------------------------------------------------------

    #[test]
    fn test_duplicate_nonce_is_dropped_before_resubmission() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.stats.ticket_difficulty = 0.0;
        insert_test_item(&mut dispatcher, 0, "dup-job", 0, 0);
        dispatcher.dispatch_seq = 1;

        // First arrival: submitted.
        dispatcher.handle_nonce(0, 0x1234_5678, 0, 0);
        // Second identical (job_id, nonce, asic_nr) arrival: dropped.
        dispatcher.handle_nonce(0, 0x1234_5678, 0, 0);

        assert_eq!(
            dispatcher.stats.accepted, 1,
            "only the first of two identical shares should be accepted"
        );
        assert_eq!(
            dispatcher.stats.duplicate_shares_dropped, 1,
            "the duplicate must be counted as dropped"
        );
        assert_eq!(dispatcher.pools[0].shares_submitted, 1);
        // Exactly one share reached the pool channel.
        assert!(share_rx.try_recv().is_ok());
        assert!(share_rx.try_recv().is_err());
    }

    #[test]
    fn test_dedup_is_cleared_on_clean_jobs() {
        let (mut dispatcher, event_tx, _share_rx) = make_single_pool_dispatcher();
        dispatcher.stats.ticket_difficulty = 0.0;
        dispatcher.pools[0].connected = true;
        dispatcher.pools[0].work_builder = Some(WorkBuilder::new("00000000", 4));
        dispatcher.pools[0].current_job = Some(make_stratum_job("old", false));

        // Seed the dedup with one key, then a clean_jobs notify must clear it so
        // the SAME key is accepted again under the new epoch.
        assert!(!dispatcher.share_dedup.check_and_insert("old", 0xABCD, 0));
        assert!(dispatcher.share_dedup.check_and_insert("old", 0xABCD, 0));

        event_tx
            .send(StratumEvent::NewJob(make_stratum_job("new", true)))
            .unwrap();
        dispatcher.drain_events();

        assert!(
            !dispatcher.share_dedup.check_and_insert("old", 0xABCD, 0),
            "clean_jobs must clear the recent-share dedup memory"
        );
    }

    #[test]
    fn test_dedup_bound_evicts_oldest_fifo() {
        let mut dedup = ShareDedup::default();
        // Fill to capacity with distinct keys.
        for n in 0..SHARE_DEDUP_CAPACITY as u32 {
            assert!(!dedup.check_and_insert("j", n, 0));
        }
        assert_eq!(dedup.order.len(), SHARE_DEDUP_CAPACITY);
        // One more eviction-triggering insert pops the oldest (nonce 0).
        assert!(!dedup.check_and_insert("j", 999_999, 0));
        assert_eq!(dedup.order.len(), SHARE_DEDUP_CAPACITY);
        // The evicted key is now treated as first-seen again.
        assert!(!dedup.check_and_insert("j", 0, 0));
    }

    // -----------------------------------------------------------------------
    // XPPROTO-3: explicit drop of rolls outside a negotiated sub-mask.
    // -----------------------------------------------------------------------

    #[test]
    fn test_out_of_mask_roll_is_dropped_not_submitted() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.stats.ticket_difficulty = 0.0;
        insert_test_item(&mut dispatcher, 0, "submask-job", 0, 0);
        // Pool negotiated a NARROW sub-mask (only bits 13..=15).
        let narrow = 0x0000_e000u32;
        dispatcher.active_jobs[0]
            .as_mut()
            .unwrap()
            .work
            .version_mask = narrow;
        dispatcher.dispatch_seq = 1;

        // rolled_version touches a bit OUTSIDE the negotiated mask (bit 20).
        let rolled_outside = 0x0010_0000u32;
        assert_ne!(
            rolled_outside & !narrow,
            0,
            "test vector must be out-of-mask"
        );
        dispatcher.handle_nonce(0, 0x1111_2222, rolled_outside, 0);

        assert_eq!(
            dispatcher.stats.out_of_mask_dropped, 1,
            "an out-of-negotiated-mask roll must be dropped + counted"
        );
        assert_eq!(
            dispatcher.stats.accepted, 0,
            "an out-of-mask roll must not be accepted/submitted"
        );
        assert!(
            share_rx.try_recv().is_err(),
            "no share should reach the pool channel for an out-of-mask roll"
        );
    }

    #[test]
    fn test_in_mask_roll_is_not_dropped() {
        let (mut dispatcher, _event_tx, share_rx) = make_single_pool_dispatcher();
        dispatcher.stats.ticket_difficulty = 0.0;
        insert_test_item(&mut dispatcher, 0, "submask-job", 0, 0);
        let narrow = 0x0000_e000u32;
        dispatcher.active_jobs[0]
            .as_mut()
            .unwrap()
            .work
            .version_mask = narrow;
        dispatcher.dispatch_seq = 1;

        // rolled_version stays WITHIN the negotiated sub-mask.
        let rolled_inside = 0x0000_4000u32;
        assert_eq!(rolled_inside & !narrow, 0, "test vector must be in-mask");
        dispatcher.handle_nonce(0, 0x1111_2222, rolled_inside, 0);

        assert_eq!(
            dispatcher.stats.out_of_mask_dropped, 0,
            "an in-mask roll must NOT be counted as out-of-mask"
        );
        assert_eq!(dispatcher.stats.accepted, 1);
        assert!(share_rx.try_recv().is_ok());
    }

    /// XPPROTO-3 must never fire on BM1397 (midstate_mode), where the
    /// `rolled_version` field is a midstate INDEX, not version bits.
    #[test]
    fn test_midstate_mode_never_treated_as_out_of_mask() {
        let (event_tx, event_rx) = mpsc::channel();
        let (share_tx, _share_rx) = mpsc::channel();
        let dispatcher = MiningDispatcher::new(
            event_rx,
            share_tx,
            DispatcherConfig {
                job_interval_ms: 10,
                job_id_step: 4,
                job_id_max: 128,
                midstate_mode: true,
            },
        );
        let mut item = make_test_item("bm1397", 0, 0, 0);
        item.work.version_mask = 0x0000_e000; // narrow mask present
                                              // A midstate index of 3 would look "out of mask" if mis-applied.
        assert!(
            !dispatcher.rolled_outside_negotiated_mask(&item, 3),
            "midstate_mode must never treat a midstate index as an out-of-mask roll"
        );
        drop(event_tx);
    }
}
