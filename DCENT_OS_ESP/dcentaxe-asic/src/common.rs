// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — ASIC driver common types
// Faithful port from ESP-Miner C codebase

use std::fmt;

// ── Protocol constants ──────────────────────────────────────────────────────

/// UART preamble bytes: 0x55 0xAA (little-endian on wire)
pub const PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Preamble as u16 for response validation (big-endian: 0xAA55)
pub const PREAMBLE_BE: u16 = 0xAA55;

// Command types
pub const TYPE_JOB: u8 = 0x20;
pub const TYPE_CMD: u8 = 0x40;

// Group flags
pub const GROUP_SINGLE: u8 = 0x00;
pub const GROUP_ALL: u8 = 0x10;

// Command codes
pub const CMD_SETADDRESS: u8 = 0x00;
pub const CMD_WRITE: u8 = 0x01;
pub const CMD_READ: u8 = 0x02;
pub const CMD_INACTIVE: u8 = 0x03;

/// Default UART baud rate
pub const UART_FREQ: u32 = 115200;

/// Default Stratum version mask
pub const STRATUM_DEFAULT_VERSION_MASK: u32 = 0x1FFFE000;

// ── Register types ──────────────────────────────────────────────────────────

/// Register type identifiers matching the C enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegisterType {
    Invalid = 0,
    Hashrate,
    TotalCount,
    Domain0Count,
    Domain1Count,
    Domain2Count,
    Domain3Count,
    ErrorCount,
    PllParam,
}

// ── ASIC model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsicModel {
    BM1366,
    BM1368,
    BM1370,
    BM1373, // S23 chip — SCAFFOLD (pre-hardware, 2026-04-14)
    BM1397,
    // KF1950 (WhatsMiner K-series, M30/M30S/M31S/M32 era).
    // UNTESTED RESEARCH DRIVER — gated by `asic-kf1950` feature, default OFF.
    #[cfg(feature = "asic-kf1950")]
    KF1950,
    // Canaan Avalon A-series (A3197 / A3198 / A3197S / A3198S / etc.).
    // Used by DCENT_axe Avalon (Nano 3/3S/Mini 3) and DCENT_OS Avalon
    // (Avalon Q + A14xx/A15xx/A16xx industrial). Driver lives in the
    // dcentaxe-avalon and dcentos-avalon workspaces; this enum variant lets
    // the shared `dcentaxe-mining::MiningDispatcher` recognise the chip.
    // Gated by `asic-avalon`, default OFF.
    #[cfg(feature = "asic-avalon")]
    Avalon,
}

impl fmt::Display for AsicModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsicModel::BM1366 => write!(f, "BM1366"),
            AsicModel::BM1368 => write!(f, "BM1368"),
            AsicModel::BM1370 => write!(f, "BM1370"),
            AsicModel::BM1373 => write!(f, "BM1373"),
            AsicModel::BM1397 => write!(f, "BM1397"),
            #[cfg(feature = "asic-kf1950")]
            AsicModel::KF1950 => write!(f, "KF1950"),
            #[cfg(feature = "asic-avalon")]
            AsicModel::Avalon => write!(f, "Avalon"),
        }
    }
}

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AsicError {
    /// Serial/UART I/O error
    Serial(String),
    /// Timeout waiting for response
    Timeout,
    /// CRC mismatch
    CrcError,
    /// Preamble mismatch in response
    PreambleMismatch,
    /// No ASICs detected on chain
    NoAsicsFound,
    /// Invalid response length
    InvalidResponse(String),
    /// General initialization failure
    InitFailed(String),
}

impl fmt::Display for AsicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsicError::Serial(msg) => write!(f, "Serial error: {}", msg),
            AsicError::Timeout => write!(f, "UART timeout"),
            AsicError::CrcError => write!(f, "CRC verification failed"),
            AsicError::PreambleMismatch => write!(f, "Preamble mismatch in response"),
            AsicError::NoAsicsFound => write!(f, "No ASIC chips detected on chain"),
            AsicError::InvalidResponse(msg) => write!(f, "Invalid response: {}", msg),
            AsicError::InitFailed(msg) => write!(f, "Init failed: {}", msg),
        }
    }
}

impl std::error::Error for AsicError {}

// ── Mining job (sent to ASIC) ───────────────────────────────────────────────

/// A mining job to send to the ASIC chain.
/// For BM1366/BM1368/BM1370: uses full prev_block_hash + merkle_root (82-byte payload).
/// For BM1397: uses midstates + merkle4 (variable-length payload with up to 4 midstates).
#[derive(Debug, Clone)]
pub struct MiningJob {
    pub job_id: u8,
    pub version: u32,
    pub prev_block_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub ntime: u32,
    pub nbits: u32,
    pub starting_nonce: u32,
    /// For BM1397 only: up to 4 midstates (each 32 bytes)
    pub midstates: Vec<[u8; 32]>,
    /// For BM1397 only: last 4 bytes of merkle root
    pub merkle4: [u8; 4],
}

impl MiningJob {
    /// Create a new job for BM1366/BM1368/BM1370 style ASICs (full block header)
    pub fn new_full(
        job_id: u8,
        version: u32,
        prev_block_hash: [u8; 32],
        merkle_root: [u8; 32],
        ntime: u32,
        nbits: u32,
        starting_nonce: u32,
    ) -> Self {
        Self {
            job_id,
            version,
            prev_block_hash,
            merkle_root,
            ntime,
            nbits,
            starting_nonce,
            midstates: Vec::new(),
            merkle4: [0u8; 4],
        }
    }

    /// Create a new job for BM1397 style ASICs (midstate-based)
    pub fn new_midstate(
        job_id: u8,
        version: u32,
        ntime: u32,
        nbits: u32,
        starting_nonce: u32,
        merkle4: [u8; 4],
        midstates: Vec<[u8; 32]>,
    ) -> Self {
        Self {
            job_id,
            version,
            prev_block_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            ntime,
            nbits,
            starting_nonce,
            midstates,
            merkle4,
        }
    }
}

// ── ASIC result (received from ASIC) ────────────────────────────────────────

/// Result from an ASIC: either a nonce (job response) or a register read.
///
/// `timestamp_us` is the uptime micros (`esp_timer_get_time()`) captured at
/// the moment the response was parsed. Ports ESP-Miner `asic_common.c:81-86`
/// (commit `64f8144` / PR #1621) — used for nonce latency / time-of-flight
/// analytics. Set to 0 when a timestamp is unavailable.
#[derive(Debug, Clone)]
pub enum AsicResult {
    /// A nonce result from mining
    Nonce {
        job_id: u8,
        nonce: u32,
        /// Rolled version (with version bits applied)
        rolled_version: u32,
        /// Which ASIC chip in the chain produced this
        asic_nr: u8,
        /// Receive timestamp (microseconds since boot, 0 if unavailable)
        timestamp_us: i64,
    },
    /// A register read response
    Register {
        register_type: RegisterType,
        asic_nr: u8,
        value: u32,
        /// Receive timestamp (microseconds since boot, 0 if unavailable)
        timestamp_us: i64,
    },
}

/// Fetch a monotonic microsecond timestamp. Pure Rust on host (for tests), backed by
/// `esp_timer_get_time()` on ESP-IDF targets.
#[inline]
pub fn now_us() -> i64 {
    #[cfg(target_os = "espidf")]
    unsafe {
        esp_idf_hal::sys::esp_timer_get_time()
    }
    #[cfg(not(target_os = "espidf"))]
    {
        0
    }
}

// ── Register data (for read_registers return) ───────────────────────────────

#[derive(Debug, Clone)]
pub struct RegisterData {
    pub register_type: RegisterType,
    pub asic_nr: u8,
    pub value: u32,
}

// ── Recent-nonce dedup ring (driver-level, per UART stream) ──────────────────

/// Number of recent nonces remembered per driver stream. Small, fixed, and
/// heap-free so it costs nothing meaningful on the ESP32-S3 (8 × 4 bytes = 32 B
/// per driver). Chosen as a bounded superset of upstream ESP-Miner's
/// single-`prev_nonce` filter.
pub const RECENT_NONCE_RING_LEN: usize = 8;

/// Bounded per-stream recent-nonce filter (driver-level dedup).
///
/// Bitmain ASICs re-emit the same nonce stream in a loop and can repeat a
/// nonce non-consecutively. Upstream ESP-Miner only filters the *immediately
/// previous* nonce (`static prev_nonce` in `bm1397.c:79`), so a looped
/// duplicate interleaved with other nonces slips through and is re-validated,
/// re-counted, and re-submitted (pool "duplicate share" reject).
///
/// This ring remembers the last `RECENT_NONCE_RING_LEN` *distinct* nonces and
/// reports a hit when the incoming nonce matches any of them. It is a strict
/// superset of `prev_nonce`: a consecutive duplicate is still caught, and a
/// non-consecutive looped duplicate within the window is now caught too.
///
/// Crucially it does **not** permanently blacklist any value — an old nonce
/// ages out of the ring after `RECENT_NONCE_RING_LEN` newer distinct nonces, so
/// a genuinely-rediscovered valid nonce in a later job is accepted again. This
/// fixes the earlier `first_nonce`/`nonce_found` regression (ASIC-1) where the
/// session's very first nonce was filtered forever.
///
/// This is the DRIVER-level (per UART stream) dedup tier. It is complementary
/// to, not a replacement for, any DISPATCHER-level cross-stream dedup keyed by
/// `(job_id, nonce, asic_nr)`.
#[derive(Debug, Clone)]
pub struct RecentNonceRing {
    /// Sentinel-free presence is tracked by `len`; entries `[0..len)` are valid.
    slots: [u32; RECENT_NONCE_RING_LEN],
    /// Number of populated slots (saturates at `RECENT_NONCE_RING_LEN`).
    len: usize,
    /// Next write position (wraps modulo `RECENT_NONCE_RING_LEN`).
    head: usize,
}

impl RecentNonceRing {
    /// Create an empty ring.
    pub const fn new() -> Self {
        Self {
            slots: [0u32; RECENT_NONCE_RING_LEN],
            len: 0,
            head: 0,
        }
    }

    /// Returns `true` if `nonce` is in the recent window (a duplicate to drop).
    pub fn contains(&self, nonce: u32) -> bool {
        self.slots[..self.len].iter().any(|&n| n == nonce)
    }

    /// Record `nonce` as the most-recently-seen value. No-op if it is already
    /// present (keeps the window holding distinct values so the effective
    /// look-back is `RECENT_NONCE_RING_LEN` *distinct* nonces).
    pub fn record(&mut self, nonce: u32) {
        if self.contains(nonce) {
            return;
        }
        self.slots[self.head] = nonce;
        self.head = (self.head + 1) % RECENT_NONCE_RING_LEN;
        if self.len < RECENT_NONCE_RING_LEN {
            self.len += 1;
        }
    }

    /// Combined check-and-record: returns `true` (and records nothing new) when
    /// `nonce` is a recent duplicate that should be dropped; otherwise records
    /// it and returns `false`.
    pub fn is_duplicate(&mut self, nonce: u32) -> bool {
        if self.contains(nonce) {
            return true;
        }
        self.record(nonce);
        false
    }
}

impl Default for RecentNonceRing {
    fn default() -> Self {
        Self::new()
    }
}

// ── Packet type ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Job,
    Cmd,
}

// ── Helper functions ────────────────────────────────────────────────────────

/// Reverse bits in a byte (port of _reverse_bits from C)
pub fn reverse_bits(num: u8) -> u8 {
    let mut reversed: u8 = 0;
    let mut n = num;
    for _ in 0..8 {
        reversed <<= 1;
        reversed |= n & 1;
        n >>= 1;
    }
    reversed
}

/// Find largest power of two <= num (port of _largest_power_of_two from C)
pub fn largest_power_of_two(num: u32) -> u32 {
    let mut power = 0u32;
    let mut n = num;
    while n > 1 {
        n >>= 1;
        power += 1;
    }
    1u32 << power
}

/// Compute the difficulty mask bytes for the TICKET_MASK register.
/// Port of `get_difficulty_mask()` from ESP-Miner `components/asic/asic_common.c`
/// (commit bfc422a / PR #1594 — fractional-difficulty support).
///
/// The mask must be one less than a power of two so there are no holes in the
/// accept range. Match ESP-Miner: ceil the pool difficulty first, then select
/// the largest supported power-of-two bucket at or below that integer value.
pub fn get_difficulty_mask(difficulty: f64) -> [u8; 6] {
    // ceil first to avoid making the ASIC harder than asked; floor at 1.
    let diff_int = difficulty.ceil().max(1.0) as u32;
    let mask = largest_power_of_two(diff_int).saturating_sub(1);
    let mut out = [0u8; 6];
    out[0] = 0x00;
    out[1] = 0x14; // TICKET_MASK register address
    out[2] = reverse_bits(((mask >> 24) & 0xFF) as u8);
    out[3] = reverse_bits(((mask >> 16) & 0xFF) as u8);
    out[4] = reverse_bits(((mask >> 8) & 0xFF) as u8);
    out[5] = reverse_bits((mask & 0xFF) as u8);
    out
}

#[cfg(test)]
mod difficulty_tests {
    use super::*;

    #[test]
    fn diff_256_is_power_of_two() {
        let mask = get_difficulty_mask(256.0);
        // 256 - 1 = 0x000000FF → reversed by byte: [0, 0x14, 0, 0, 0, 0xFF]
        assert_eq!(mask[0], 0x00);
        assert_eq!(mask[1], 0x14);
        assert_eq!(&mask[2..6], &[0x00, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn fractional_diff_can_cross_power_bucket_after_ceil() {
        // 511.5 ceil -> 512 -> largest_power_of_two(512) = 512 -> mask 511.
        // 511.0 stays in the 256 bucket.
        let frac = get_difficulty_mask(511.5);
        let next = get_difficulty_mask(512.0);
        assert_eq!(frac, next);
        let whole = get_difficulty_mask(511.0);
        assert_ne!(frac, whole);
    }

    #[test]
    fn zero_and_subunit_clamp_to_one() {
        let zero = get_difficulty_mask(0.0);
        let one = get_difficulty_mask(1.0);
        let half = get_difficulty_mask(0.5);
        assert_eq!(zero, one);
        // 0.5 ceils to 1
        assert_eq!(half, one);
    }

    #[test]
    fn large_u32_range_difficulty() {
        // Main-net difficulties can exceed u16 range. 1_048_576 (2^20) must not overflow.
        let mask = get_difficulty_mask(1_048_576.0);
        // largest_power_of_two(1048576) = 1048576 → mask = 1048575 = 0x000FFFFF
        // Bytes: 0x00, 0x0F, 0xFF, 0xFF → reversed: 0x00, 0xF0, 0xFF, 0xFF
        assert_eq!(&mask[2..6], &[0x00, 0xF0, 0xFF, 0xFF]);
    }
}

#[cfg(test)]
mod recent_nonce_ring_tests {
    use super::*;

    #[test]
    fn first_nonce_is_never_permanently_filtered() {
        // ASIC-1 regression guard: the session's first nonce must NOT be
        // blacklisted forever. It is filtered only while it is still in the
        // recent window; once RECENT_NONCE_RING_LEN newer distinct nonces have
        // arrived it ages out and is accepted again.
        let mut ring = RecentNonceRing::new();
        let first = 0xDEAD_BEEFu32;
        assert!(!ring.is_duplicate(first), "first sighting must pass");
        assert!(ring.is_duplicate(first), "immediate repeat is a duplicate");
        // Flush the window with RECENT_NONCE_RING_LEN distinct other nonces.
        for k in 0..RECENT_NONCE_RING_LEN as u32 {
            assert!(!ring.is_duplicate(0x1000_0000 + k));
        }
        // `first` has now aged out — a genuine rediscovery is accepted again.
        assert!(
            !ring.is_duplicate(first),
            "aged-out nonce must be accepted, not blacklisted forever"
        );
    }

    #[test]
    fn catches_consecutive_and_nonconsecutive_loop_duplicates() {
        // MD-4: a non-consecutive looped duplicate within the window is caught,
        // which the single-element prev_nonce filter misses.
        let mut ring = RecentNonceRing::new();
        assert!(!ring.is_duplicate(0xA));
        assert!(!ring.is_duplicate(0xB));
        assert!(!ring.is_duplicate(0xC));
        // 0xA was not the immediately-previous nonce, but is still in-window.
        assert!(ring.is_duplicate(0xA), "in-window loop duplicate dropped");
        // consecutive duplicate still caught (prev_nonce superset).
        assert!(!ring.is_duplicate(0xD));
        assert!(ring.is_duplicate(0xD));
    }

    #[test]
    fn record_keeps_window_to_distinct_values() {
        // Re-recording an already-present nonce must not evict other window
        // entries (keeps the effective look-back at RECENT_NONCE_RING_LEN
        // DISTINCT nonces, not raw insertions).
        let mut ring = RecentNonceRing::new();
        for k in 0..RECENT_NONCE_RING_LEN as u32 {
            ring.record(k);
        }
        // Re-record the oldest several times — must not push out 0..LEN.
        for _ in 0..100 {
            ring.record(0);
        }
        for k in 0..RECENT_NONCE_RING_LEN as u32 {
            assert!(ring.contains(k), "distinct nonce {k} must stay in window");
        }
    }

    #[test]
    fn empty_ring_reports_no_duplicates() {
        let ring = RecentNonceRing::new();
        assert!(!ring.contains(0));
        assert!(!ring.contains(0xFFFF_FFFF));
    }
}

// NOTE: increment_bitmask was previously duplicated here.
// The canonical implementation lives in dcentaxe_stratum::work::increment_bitmask.
// Removed to avoid divergence (Phase 6.2 dedup).
