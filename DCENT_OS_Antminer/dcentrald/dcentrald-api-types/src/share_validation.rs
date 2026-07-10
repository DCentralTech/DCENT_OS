//!  shv-A — Share validation pipeline DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §1-4 (lines 15-220).
//!
//! Maps the canonical share-validation pipeline:
//! - Local SHA-256d hash computed from header + nonce.
//! - Compare against `share_target = 0xFFFF * 2^208 / share_diff`.
//! - Hits below `share_target` get submitted (V1 mining.submit /
//!   V2 SubmitSharesExtended).
//! - Hits below `block_target` get prioritized + forwarded to backup pool.
//! - Hits above both → hardware-error counter; chip flagged at >2 % rate.
//! - Submit dedup: 1024-entry ring keyed by
//!   `(job_id, nonce ^ extranonce2)`.
//!
//! HAL-free: pure pipeline DTOs + dedup ring + target arithmetic. The
//! actual SHA-256d hashing lives in `dcentrald-asic` / `dcentrald-stratum`
//! (the BB chips offload SHA to silicon; the daemon only computes
//! sanity-checks). This module provides:
//! - `share_target_from_diff(diff)` returning the 256-bit target.
//! - `DedupRing` fixed-capacity FIFO.
//! - `ShareRejectCode` (V1) + parser.
//! - `classify_hw_error(rate, threshold)` helper.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 2 % default hardware-error threshold per RE doc §1 line 86. Above
/// this rate, the chip is flagged as having a thermal/freq problem.
pub const DEFAULT_HW_ERROR_THRESHOLD: f64 = 0.02;

/// Bosminer-canonical dedup ring capacity (per RE doc §4 line 212).
pub const DEFAULT_DEDUP_RING_CAPACITY: usize = 1024;

/// Stratum V1 reject codes (per RE doc §3 lines 113-118).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareRejectCode {
    /// 21 — Job not found (stale; expected, count separately).
    JobNotFound,
    /// 22 — Duplicate share (bug — should not happen if dedup is right).
    DuplicateShare,
    /// 23 — Low difficulty (local pre-validation broken).
    LowDifficulty,
    /// 24 — Unauthorized worker (need to re-authorize).
    UnauthorizedWorker,
    /// 25 — Not subscribed (need to reconnect).
    NotSubscribed,
    /// 27 — Invalid version mask (BIP 310 negotiation drift).
    InvalidVersionMask,
    /// Code outside the documented range.
    Unknown(u32),
}

impl ShareRejectCode {
    pub fn from_code(code: u32) -> Self {
        match code {
            21 => ShareRejectCode::JobNotFound,
            22 => ShareRejectCode::DuplicateShare,
            23 => ShareRejectCode::LowDifficulty,
            24 => ShareRejectCode::UnauthorizedWorker,
            25 => ShareRejectCode::NotSubscribed,
            27 => ShareRejectCode::InvalidVersionMask,
            other => ShareRejectCode::Unknown(other),
        }
    }

    pub fn as_code(&self) -> Option<u32> {
        match self {
            ShareRejectCode::JobNotFound => Some(21),
            ShareRejectCode::DuplicateShare => Some(22),
            ShareRejectCode::LowDifficulty => Some(23),
            ShareRejectCode::UnauthorizedWorker => Some(24),
            ShareRejectCode::NotSubscribed => Some(25),
            ShareRejectCode::InvalidVersionMask => Some(27),
            ShareRejectCode::Unknown(c) => Some(*c),
        }
    }

    /// Whether this reject code is "expected operational" (job not
    /// found / unauthorized / not subscribed) vs an actual bug
    /// (duplicate / low-difficulty / version-mask).
    pub fn is_expected_operational(&self) -> bool {
        matches!(
            self,
            ShareRejectCode::JobNotFound
                | ShareRejectCode::UnauthorizedWorker
                | ShareRejectCode::NotSubscribed
        )
    }
}

/// Chip health classification based on hardware-error rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipHealth {
    /// Rate at or below the threshold — chip is operating normally.
    Healthy,
    /// Rate above the threshold — flag as thermal/freq problem.
    BadRate,
}

/// Classify a chip's lie-rate against the configured threshold.
pub fn classify_hw_error(error_rate: f64, threshold: f64) -> ChipHealth {
    if error_rate.is_nan() || error_rate <= threshold {
        ChipHealth::Healthy
    } else {
        ChipHealth::BadRate
    }
}

/// Compute the 256-bit share target for a given difficulty.
///
/// `share_target = 0xFFFF * 2^208 / share_diff` (per RE doc §1 line 85).
/// Returns big-endian 32-byte representation. `diff <= 0` returns the
/// max target (0xFFFF...).
pub fn share_target_from_diff(share_diff: f64) -> [u8; 32] {
    if share_diff <= 0.0 || !share_diff.is_finite() {
        return [0xFF; 32];
    }
    // target_u256 = (0xFFFF << 208) / share_diff
    // Compute with bytes since u256 isn't in stdlib.
    // We can shortcut: 0xFFFF << 208 = a 226-bit number
    // (2 bytes 0xFFFF at byte positions 26..27, big-endian).
    // Dividing this by share_diff requires arbitrary-precision; we
    // approximate via f64 mantissa / scaled integer division.
    //
    // For the test corpus we care about, share_diff is an integer or
    // a small power-of-two; f64 handles up to 2^53 exactly. Good
    // enough until  wires a real u256 lib.
    let mut target = [0u8; 32];
    target[3] = 0xFF; // 0xFFFF << 208 puts the top 0xFFFF at bytes [3..5] big-endian
    target[4] = 0xFF;
    // Divide by share_diff: walk through the 32 bytes, treating as
    // big-endian, and shift-right by log2(share_diff) approx.
    let log2 = share_diff.log2();
    if log2 < 0.0 || !log2.is_finite() {
        return target;
    }
    let shift_bits = log2.round() as i32;
    if shift_bits <= 0 {
        return target;
    }
    let shift_bytes = (shift_bits / 8) as usize;
    let shift_bits_in_byte = (shift_bits % 8) as u32;
    if shift_bytes >= 32 {
        return [0u8; 32];
    }
    // Right-shift target by shift_bytes whole bytes.
    let mut shifted = [0u8; 32];
    shifted[shift_bytes..32].copy_from_slice(&target[..(32 - shift_bytes)]);
    // Then by shift_bits_in_byte sub-byte. Only powers of two that are
    // exact multiples of 256 (256, 65536, …) are byte-aligned and skip
    // this branch; other powers of two (4 → 2 bits, 1024 → 10 bits) DO
    // hit it and, because the divisor is an exact power of two, produce
    // the EXACT target here. For NON-power-of-two diffs the
    // `log2().round()` above already snapped the divisor to the nearest
    // power of two, so the overall result stays APPROXIMATE — that is
    // the load-bearing reason this function must NOT be wired into live
    // share validation until  swaps in a real u256 divide.
    if shift_bits_in_byte > 0 {
        // Textbook big-endian right-shift by `s` bits (0 < s < 8):
        // walk MSB→LSB; each output byte is this byte shifted right by
        // `s`, OR'd with the low `s` bits carried down from the
        // more-significant byte (placed in the high `8-s` bits).
        // bug-hunt LOW #4 (2026-05-28): the prior version derived the
        // carry from `*byte` AFTER it had already been overwritten with
        // the shifted result, so the bits meant to propagate into the
        // next (lower) byte were taken from the OUTPUT instead of the
        // original byte — corrupting every non-byte-aligned shift (e.g.
        // diff=4 yielded 0xC0/0xC0/0x00 instead of 0x3F/0xFF/0xC0). The
        // carry now comes from `orig`, the unmodified input byte.
        let s = shift_bits_in_byte;
        let mut prev_low: u8 = 0; // low `s` bits of the more-significant byte
        for byte in shifted.iter_mut() {
            let orig = *byte;
            *byte = (orig >> s) | (prev_low << (8 - s));
            prev_low = orig & ((1u8 << s) - 1);
        }
    }
    shifted
}

/// Dedup-ring entry key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    pub job_id: u32,
    /// `nonce ^ extranonce2[0..4]` per RE doc §4. Reduces 8 bytes of
    /// state to 4, with negligible collision risk in 1024-entry ring.
    pub nonce_xor_extranonce2: u32,
}

/// Fixed-capacity ring of recent (job_id, nonce^en2) keys. Insert
/// returns whether the share is a new submission (true) or a duplicate
/// (false).
#[derive(Debug, Clone)]
pub struct DedupRing {
    capacity: usize,
    keys: VecDeque<DedupKey>,
}

impl DedupRing {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "DedupRing capacity must be > 0");
        Self {
            capacity,
            keys: VecDeque::with_capacity(capacity),
        }
    }

    pub fn bosminer_canonical() -> Self {
        Self::new(DEFAULT_DEDUP_RING_CAPACITY)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Insert a new key. Returns `true` if the key was novel (insert
    /// proceeded), `false` if it was already present (duplicate
    /// submission rejected).
    ///
    /// Drops the oldest entry when at capacity.
    pub fn try_insert(&mut self, key: DedupKey) -> bool {
        if self.keys.iter().any(|k| *k == key) {
            return false;
        }
        if self.keys.len() == self.capacity {
            self.keys.pop_front();
        }
        self.keys.push_back(key);
        true
    }

    /// Wipe every entry (e.g. on `clean_jobs` / `SetNewPrevHash` per
    /// RE doc §4 invariant 3).
    pub fn clear(&mut self) {
        self.keys.clear();
    }
}

// ---------------------------------------------------------------------------
//  W1 — LocalRejectDiagnostic / LocalRejectReason / LocalRejectRing
//
// Pure observability DTOs for the local share-validation drop site
// (`local_share_rejects_legacy` in work_dispatcher.rs). The runtime adapter
// pushes a `LocalRejectDiagnostic` into a `LocalRejectRing` every time
// `validate_share()` returns false, so operators can inspect the drop
// distribution post-flash via `GET /api/diagnostics/shares/local_rejects`.
// Drives  root-cause analysis on `.39`.
// ---------------------------------------------------------------------------

/// Why the local share-validation step rejected a nonce. Categories
/// match the actual decision points in `work_dispatcher.rs::tick()` /
/// `bm1387.rs::decode_nonce()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRejectReason {
    /// SHA-256d(midstate, header_tail, nonce) > share_target. The most
    /// common case if the midstate index is wrong (8-bit FPGA work_id
    /// ring wrap aliasing).
    HashAboveTarget,
    /// The work_id slot in the dispatcher work table was overwritten by
    /// a newer generation more than `work_id_space` ago — stale slot.
    StaleSlot,
    /// The work_id slot was never populated. Dispatch never wrote here,
    /// or wrote and immediately cleared.
    EmptyWorkSlot,
    /// The dedup ring already saw this `(job_id, nonce^extranonce2)`
    /// key from this miner — duplicate submission attempt.
    DuplicateDedupHit,
    /// The midstate index decoded from the FPGA was out of range for
    /// the current `fpga_midstate_cnt`. FPGA decode bug or memory
    /// corruption.
    MidstateIdxOutOfRange,
}

/// One captured local-validation rejection. All fields are operator-
/// inspectable — no PII, no secrets. Hashes are big-endian first 8 bytes
/// (a 64-bit prefix) which is enough to compare close-to-target vs
/// far-above-target without leaking anything sensitive.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LocalRejectDiagnostic {
    /// Monotonic counter — total rejects since daemon start (NOT ring index).
    pub seq: u64,
    /// Wall-clock milliseconds since the unix epoch when the reject occurred.
    pub timestamp_ms: u64,
    /// FPGA chain id (S9: 6/7/8; S19: 6/7/8; S21: 0/1/2).
    pub chain_id: u8,
    /// Per-chain chip index decoded from the nonce (BM1387: nonce >> 2 & 0x3F).
    pub chip_index: u8,
    /// Raw 32-bit nonce returned by the chip.
    pub nonce: u32,
    /// Decoded dispatcher work_id (post 8-bit truncation for BM1387).
    pub work_id: u16,
    /// Midstate index inside the work entry (0..=fpga_midstate_cnt-1).
    pub midstate_idx: u8,
    /// Raw `hw_work_id` extracted from FPGA WORK_RX_FIFO w1 before
    /// decode (helps debug the 8-bit ring wraparound).
    pub fpga_work_id_raw: u16,
    /// Generation distance between the work-table entry's generation
    /// and the current dispatch generation. Saturating-subtracted in
    /// the runtime; 0 means the entry is current, non-zero means stale.
    pub generation_age: u64,
    /// First 8 bytes (big-endian) of the SHA-256d hash. Lets operators
    /// compare hash-vs-target close-margin (likely cold ASIC / unlucky)
    /// vs far-above-target (midstate corruption).
    pub computed_hash_be_first8: [u8; 8],
    /// First 8 bytes (big-endian) of the pool's share_target.
    pub share_target_be_first8: [u8; 8],
    /// Categorical reason for the reject.
    pub reason: LocalRejectReason,
}

/// Fixed-capacity ring buffer of the most recent rejects. Designed to
/// be cheap on the hot path: pushing is O(1) amortized, snapshot is
/// O(N) where N is the configured capacity.
#[derive(Debug, Clone)]
pub struct LocalRejectRing {
    capacity: usize,
    entries: VecDeque<LocalRejectDiagnostic>,
    total_seq: u64,
}

/// Default ring capacity for the runtime adapter. 64 entries is enough
/// to characterize the drop distribution (chi-squared over 64 samples
/// distinguishes uniform-stale vs hash-above-target with p<0.01) without
/// holding meaningful memory.
pub const DEFAULT_LOCAL_REJECT_RING_CAPACITY: usize = 64;

impl LocalRejectRing {
    /// Construct an empty ring with the given capacity.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "LocalRejectRing capacity must be > 0");
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
            total_seq: 0,
        }
    }

    /// Default-capacity (64) ring — see `DEFAULT_LOCAL_REJECT_RING_CAPACITY`.
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_LOCAL_REJECT_RING_CAPACITY)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of rejects seen since this ring was created (NOT
    /// the same as `len()` — entries get evicted at capacity).
    pub fn total_seen(&self) -> u64 {
        self.total_seq
    }

    /// Push a new diagnostic, evicting the oldest entry at capacity.
    /// Caller must populate `seq` before calling — the ring does not
    /// auto-assign seq numbers because the runtime may already maintain
    /// its own sequence counter (e.g. tied to `local_share_rejects_legacy`).
    pub fn push(&mut self, diag: LocalRejectDiagnostic) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(diag);
        self.total_seq = self.total_seq.max(diag.seq);
    }

    /// Snapshot the most recent `limit` entries (newest last). If
    /// `limit` is greater than `len()`, returns all entries.
    pub fn snapshot(&self, limit: usize) -> Vec<LocalRejectDiagnostic> {
        let take = limit.min(self.entries.len());
        if take == 0 {
            return Vec::new();
        }
        let skip = self.entries.len() - take;
        self.entries.iter().skip(skip).copied().collect()
    }

    /// Snapshot ALL entries (newest last).
    pub fn snapshot_all(&self) -> Vec<LocalRejectDiagnostic> {
        self.entries.iter().copied().collect()
    }

    /// Wipe every entry. Resets `total_seq` to 0 — useful when the
    /// daemon reconnects to a fresh pool / clean_jobs flushes work.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_seq = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_reject_code_round_trip_through_codes() {
        let cases = [
            (21u32, ShareRejectCode::JobNotFound),
            (22, ShareRejectCode::DuplicateShare),
            (23, ShareRejectCode::LowDifficulty),
            (24, ShareRejectCode::UnauthorizedWorker),
            (25, ShareRejectCode::NotSubscribed),
            (27, ShareRejectCode::InvalidVersionMask),
        ];
        for (code, expected) in cases {
            let parsed = ShareRejectCode::from_code(code);
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_code(), Some(code));
        }
        // Unknown code preserves value.
        let unknown = ShareRejectCode::from_code(99);
        assert_eq!(unknown, ShareRejectCode::Unknown(99));
        assert_eq!(unknown.as_code(), Some(99));
    }

    #[test]
    fn expected_operational_codes_classified_correctly() {
        assert!(ShareRejectCode::JobNotFound.is_expected_operational());
        assert!(ShareRejectCode::UnauthorizedWorker.is_expected_operational());
        assert!(ShareRejectCode::NotSubscribed.is_expected_operational());
        // Bug indicators:
        assert!(!ShareRejectCode::DuplicateShare.is_expected_operational());
        assert!(!ShareRejectCode::LowDifficulty.is_expected_operational());
        assert!(!ShareRejectCode::InvalidVersionMask.is_expected_operational());
    }

    #[test]
    fn share_reject_code_round_trips_through_serde() {
        for code in [
            ShareRejectCode::JobNotFound,
            ShareRejectCode::DuplicateShare,
            ShareRejectCode::LowDifficulty,
            ShareRejectCode::UnauthorizedWorker,
            ShareRejectCode::NotSubscribed,
            ShareRejectCode::InvalidVersionMask,
            ShareRejectCode::Unknown(42),
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let back: ShareRejectCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back);
        }
    }

    #[test]
    fn classify_hw_error_threshold_split() {
        // Below threshold → Healthy.
        assert_eq!(
            classify_hw_error(0.005, DEFAULT_HW_ERROR_THRESHOLD),
            ChipHealth::Healthy
        );
        // At threshold → Healthy (not strictly above).
        assert_eq!(
            classify_hw_error(DEFAULT_HW_ERROR_THRESHOLD, DEFAULT_HW_ERROR_THRESHOLD),
            ChipHealth::Healthy
        );
        // Above threshold → BadRate.
        assert_eq!(
            classify_hw_error(0.05, DEFAULT_HW_ERROR_THRESHOLD),
            ChipHealth::BadRate
        );
    }

    #[test]
    fn classify_hw_error_handles_nan() {
        // NaN error rate (no shares yet) is treated as Healthy.
        assert_eq!(
            classify_hw_error(f64::NAN, DEFAULT_HW_ERROR_THRESHOLD),
            ChipHealth::Healthy
        );
    }

    #[test]
    fn share_target_diff_one_returns_max_target() {
        // diff=1 → target is the network max (0xFFFF * 2^208).
        let t = share_target_from_diff(1.0);
        // Top non-zero bytes are 0xFF 0xFF at offsets 3..5 big-endian.
        assert_eq!(t[3], 0xFF);
        assert_eq!(t[4], 0xFF);
    }

    #[test]
    fn share_target_diff_invalid_returns_max() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let t = share_target_from_diff(bad);
            assert_eq!(t, [0xFF; 32], "invalid diff {} should return max", bad);
        }
    }

    #[test]
    fn share_target_higher_diff_yields_smaller_target() {
        // Two diff values; the higher should produce a target whose
        // first non-zero byte is at a higher index (smaller number).
        let t1 = share_target_from_diff(1.0);
        let t1k = share_target_from_diff(1024.0);
        // Find first non-zero byte in each.
        let first_nz = |t: &[u8; 32]| t.iter().position(|b| *b != 0).unwrap_or(32);
        assert!(first_nz(&t1k) >= first_nz(&t1));
    }

    #[test]
    fn share_target_subbyte_shift_is_exact_for_power_of_two() {
        // bug-hunt LOW #4 regression: a non-byte-aligned power-of-two
        // diff exercises the sub-byte right-shift. Because the divisor is
        // an exact power of two, the result must be the EXACT target
        // 0xFFFF * 2^208 / diff (not the log2-rounded approximation that
        // applies to non-power-of-two diffs). The prior read-after-
        // overwrite carry produced 0xC0/0xC0/0x00 here; the textbook
        // carry-from-original-byte produces 0x3F/0xFF/0xC0.
        //
        // diff = 4 → shift right 2 bits (shift_bytes=0, s=2):
        // 0xFFFF<<208 (bytes [3]=FF,[4]=FF) >> 2 = [3]=0x3F,[4]=0xFF,[5]=0xC0.
        let t = share_target_from_diff(4.0);
        let mut expected = [0u8; 32];
        expected[3] = 0x3F;
        expected[4] = 0xFF;
        expected[5] = 0xC0;
        assert_eq!(t, expected, "diff=4 sub-byte shift must be exact");

        // diff = 1024 → shift right 10 bits (shift_bytes=1, s=2): the same
        // 0xFFFF window lands one byte lower.
        let t1k = share_target_from_diff(1024.0);
        let mut expected_1k = [0u8; 32];
        expected_1k[4] = 0x3F;
        expected_1k[5] = 0xFF;
        expected_1k[6] = 0xC0;
        assert_eq!(t1k, expected_1k, "diff=1024 sub-byte shift must be exact");
    }

    #[test]
    fn dedup_ring_rejects_duplicate_key() {
        let mut ring = DedupRing::new(8);
        let k = DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xDEADBEEF,
        };
        assert!(ring.try_insert(k));
        // Same key again → false.
        assert!(!ring.try_insert(k));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn dedup_ring_drops_oldest_at_capacity() {
        let mut ring = DedupRing::new(3);
        for i in 0..5 {
            ring.try_insert(DedupKey {
                job_id: 1,
                nonce_xor_extranonce2: i,
            });
        }
        // Should retain the last 3 (2, 3, 4).
        assert_eq!(ring.len(), 3);
        // Re-inserting an old key (0, 1) should now succeed because it
        // was dropped from the ring.
        assert!(ring.try_insert(DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0
        }));
    }

    #[test]
    fn dedup_ring_clear_wipes_state() {
        let mut ring = DedupRing::new(8);
        ring.try_insert(DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xAA,
        });
        ring.try_insert(DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xBB,
        });
        ring.clear();
        assert_eq!(ring.len(), 0);
        assert!(ring.is_empty());
        // Same key now permitted (cleared).
        assert!(ring.try_insert(DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xAA
        }));
    }

    #[test]
    fn dedup_ring_bosminer_canonical_capacity_is_1024() {
        let r = DedupRing::bosminer_canonical();
        assert_eq!(r.capacity(), 1024);
        assert_eq!(DEFAULT_DEDUP_RING_CAPACITY, 1024);
    }

    #[test]
    fn dedup_ring_distinguishes_job_id_and_nonce() {
        let mut ring = DedupRing::new(8);
        // Same nonce but different job_id → both insert.
        let a = DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xAA,
        };
        let b = DedupKey {
            job_id: 2,
            nonce_xor_extranonce2: 0xAA,
        };
        assert!(ring.try_insert(a));
        assert!(ring.try_insert(b));
        // Same job_id but different nonce → both insert.
        let c = DedupKey {
            job_id: 1,
            nonce_xor_extranonce2: 0xBB,
        };
        assert!(ring.try_insert(c));
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn default_hw_error_threshold_pinned() {
        // Per RE doc §1 line 86, the threshold is 2 %.
        assert!((DEFAULT_HW_ERROR_THRESHOLD - 0.02).abs() < 1e-9);
    }

    #[test]
    fn dedup_key_round_trips_through_serde() {
        let k = DedupKey {
            job_id: 0xDEAD_BEEF,
            nonce_xor_extranonce2: 0xCAFE_BABE,
        };
        let json = serde_json::to_string(&k).unwrap();
        let back: DedupKey = serde_json::from_str(&json).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn chip_health_round_trips_through_serde() {
        for h in [ChipHealth::Healthy, ChipHealth::BadRate] {
            let json = serde_json::to_string(&h).unwrap();
            let back: ChipHealth = serde_json::from_str(&json).unwrap();
            assert_eq!(h, back);
        }
    }

    // -----------------------------------------------------------------------
    //  W1 — LocalRejectDiagnostic / LocalRejectReason / LocalRejectRing
    // -----------------------------------------------------------------------

    fn sample_diag(seq: u64, reason: LocalRejectReason) -> LocalRejectDiagnostic {
        LocalRejectDiagnostic {
            seq,
            timestamp_ms: 1_700_000_000_000 + seq,
            chain_id: 6,
            chip_index: 12,
            nonce: 0xDEAD_BEEF,
            work_id: 0x42,
            midstate_idx: 1,
            fpga_work_id_raw: 0x108,
            generation_age: 0,
            computed_hash_be_first8: [0xFF; 8],
            share_target_be_first8: [0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00],
            reason,
        }
    }

    #[test]
    fn local_reject_reason_round_trips_through_serde() {
        for r in [
            LocalRejectReason::HashAboveTarget,
            LocalRejectReason::StaleSlot,
            LocalRejectReason::EmptyWorkSlot,
            LocalRejectReason::DuplicateDedupHit,
            LocalRejectReason::MidstateIdxOutOfRange,
        ] {
            let json = serde_json::to_string(&r).unwrap();
            let back: LocalRejectReason = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn local_reject_reason_serializes_in_snake_case() {
        // pyasic / dashboard decode by name. Pin the wire form.
        assert_eq!(
            serde_json::to_string(&LocalRejectReason::HashAboveTarget).unwrap(),
            "\"hash_above_target\""
        );
        assert_eq!(
            serde_json::to_string(&LocalRejectReason::StaleSlot).unwrap(),
            "\"stale_slot\""
        );
        assert_eq!(
            serde_json::to_string(&LocalRejectReason::EmptyWorkSlot).unwrap(),
            "\"empty_work_slot\""
        );
        assert_eq!(
            serde_json::to_string(&LocalRejectReason::DuplicateDedupHit).unwrap(),
            "\"duplicate_dedup_hit\""
        );
        assert_eq!(
            serde_json::to_string(&LocalRejectReason::MidstateIdxOutOfRange).unwrap(),
            "\"midstate_idx_out_of_range\""
        );
    }

    #[test]
    fn local_reject_diagnostic_round_trips_through_serde() {
        let d = sample_diag(42, LocalRejectReason::HashAboveTarget);
        let json = serde_json::to_string(&d).unwrap();
        let back: LocalRejectDiagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn local_reject_ring_capacity_evicts_oldest() {
        let mut ring = LocalRejectRing::new(3);
        for seq in 0..5 {
            ring.push(sample_diag(seq, LocalRejectReason::HashAboveTarget));
        }
        assert_eq!(ring.len(), 3);
        // Oldest two (seq 0, 1) evicted; seq 2/3/4 remain.
        let snap = ring.snapshot_all();
        assert_eq!(
            snap.iter().map(|d| d.seq).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(ring.total_seen(), 4);
    }

    #[test]
    fn local_reject_ring_snapshot_limit_caps_to_len() {
        let mut ring = LocalRejectRing::new(8);
        for seq in 0..3 {
            ring.push(sample_diag(seq, LocalRejectReason::StaleSlot));
        }
        // Limit > len returns all 3.
        let snap = ring.snapshot(10);
        assert_eq!(snap.len(), 3);
        // Limit < len returns the most recent N.
        let snap = ring.snapshot(2);
        assert_eq!(snap.iter().map(|d| d.seq).collect::<Vec<_>>(), vec![1, 2]);
        // Limit 0 returns empty.
        assert!(ring.snapshot(0).is_empty());
    }

    #[test]
    fn local_reject_ring_default_capacity_matches_constant() {
        let ring = LocalRejectRing::with_default_capacity();
        assert_eq!(ring.capacity(), DEFAULT_LOCAL_REJECT_RING_CAPACITY);
        assert_eq!(DEFAULT_LOCAL_REJECT_RING_CAPACITY, 64);
        assert!(ring.is_empty());
    }

    #[test]
    fn local_reject_ring_clear_resets_state() {
        let mut ring = LocalRejectRing::new(4);
        for seq in 0..3 {
            ring.push(sample_diag(seq, LocalRejectReason::EmptyWorkSlot));
        }
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.total_seen(), 0);
    }

    #[test]
    fn local_reject_ring_total_seen_tracks_max_seq_not_count() {
        // total_seen reflects the highest seq pushed (sequence may skip
        // numbers if the runtime adapter dedups upstream). Don't conflate
        // it with insertion count.
        let mut ring = LocalRejectRing::new(4);
        ring.push(sample_diag(10, LocalRejectReason::HashAboveTarget));
        ring.push(sample_diag(50, LocalRejectReason::HashAboveTarget));
        ring.push(sample_diag(30, LocalRejectReason::HashAboveTarget));
        assert_eq!(ring.total_seen(), 50);
    }

    #[test]
    #[should_panic(expected = "LocalRejectRing capacity must be > 0")]
    fn local_reject_ring_zero_capacity_panics() {
        let _ = LocalRejectRing::new(0);
    }

    #[test]
    fn local_reject_diagnostic_serialize_in_snake_case() {
        // Pin field names — dashboard / pyasic decode by name.
        let d = sample_diag(1, LocalRejectReason::HashAboveTarget);
        let json = serde_json::to_string(&d).unwrap();
        for needle in [
            "\"seq\":1",
            "\"timestamp_ms\":",
            "\"chain_id\":6",
            "\"chip_index\":12",
            "\"work_id\":66",
            "\"midstate_idx\":1",
            "\"fpga_work_id_raw\":264",
            "\"generation_age\":0",
            "\"computed_hash_be_first8\":",
            "\"share_target_be_first8\":",
            "\"reason\":\"hash_above_target\"",
        ] {
            assert!(json.contains(needle), "missing {} in {}", needle, json);
        }
    }
}
