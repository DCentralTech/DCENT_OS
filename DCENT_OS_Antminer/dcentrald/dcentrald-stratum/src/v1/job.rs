//! Job template processing and merkle root computation.
//!
//! Converts pool-received mining.notify data into work items ready for
//! ASIC dispatch. Handles extranonce generation, coinbase construction,
//! merkle root computation, and midstate calculation.
//!
//! This module provides lower-level building blocks. For the full work
//! generation pipeline with version rolling and extranonce2 management,
//! use `crate::work::WorkBuilder`.
//!
//! Work generation pipeline:
//!   1. JobTemplate (from pool)
//!   2. Generate extranonce2 (incrementing counter)
//!   3. Build coinbase: coinbase1 + extranonce1 + extranonce2 + coinbase2
//!   4. SHA256d(coinbase) -> coinbase_hash
//!   5. Build merkle root: fold coinbase_hash with merkle_branches
//!   6. Build block header: version + prevhash + merkle_root + ntime + nbits + nonce
//!   7. Compute SHA-256 midstate of first 64 bytes (for ASIC dispatch)

use sha2::{Digest, Sha256};

use crate::types::{JobTemplate, MAX_V1_EXTRANONCE2_SIZE};
use crate::work::compute_midstate_from_prefix;

/// Compute the SHA256d (double SHA-256) of input data.
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut result = [0u8; 32];
    result.copy_from_slice(&second);
    result
}

/// Build the coinbase transaction from its parts.
///
/// coinbase = coinbase1 + extranonce1 + extranonce2 + coinbase2
///
/// The coinbase is a special transaction that creates new coins. The pool
/// splits it around the extranonce insertion point so each miner session
/// (extranonce1) and each work unit (extranonce2) produces a unique coinbase
/// and therefore a unique merkle root and block header.
pub fn build_coinbase(
    coinbase1: &[u8],
    extranonce1: &[u8],
    extranonce2: &[u8],
    coinbase2: &[u8],
) -> Vec<u8> {
    let mut coinbase = Vec::with_capacity(
        coinbase1.len() + extranonce1.len() + extranonce2.len() + coinbase2.len(),
    );
    coinbase.extend_from_slice(coinbase1);
    coinbase.extend_from_slice(extranonce1);
    coinbase.extend_from_slice(extranonce2);
    coinbase.extend_from_slice(coinbase2);
    coinbase
}

/// Compute the merkle root from the coinbase hash and merkle branches.
///
/// Starting with the coinbase hash, iteratively concatenate each branch
/// hash on the right and SHA256d the result. The coinbase is always the
/// leftmost leaf in Bitcoin's merkle tree.
///
/// If there are no branches (solo mining with a single transaction), the
/// merkle root IS the coinbase hash.
pub fn compute_merkle_root(coinbase_hash: &[u8; 32], branches: &[[u8; 32]]) -> [u8; 32] {
    let mut current = *coinbase_hash;

    for branch in branches {
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&current);
        concat[32..].copy_from_slice(branch);
        current = sha256d(&concat);
    }

    current
}

/// Build an 80-byte block header from its components.
///
/// Layout (all fields little-endian in the header):
///   Bytes  0..3:  version (4 bytes)
///   Bytes  4..35: previous block hash (32 bytes)
///   Bytes 36..67: merkle root (32 bytes)
///   Bytes 68..71: ntime (4 bytes, Unix timestamp)
///   Bytes 72..75: nbits (4 bytes, compact difficulty target)
///   Bytes 76..79: nonce (4 bytes, iterated by ASIC)
pub fn build_block_header(
    version: u32,
    prev_hash: &[u8; 32],
    merkle_root: &[u8; 32],
    ntime: u32,
    nbits: u32,
    nonce: u32,
) -> [u8; 80] {
    let mut header = [0u8; 80];

    header[0..4].copy_from_slice(&version.to_le_bytes());
    header[4..36].copy_from_slice(prev_hash);
    header[36..68].copy_from_slice(merkle_root);
    header[68..72].copy_from_slice(&ntime.to_le_bytes());
    header[72..76].copy_from_slice(&nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());

    header
}

/// Compute the SHA-256 midstate of the first 64 bytes of a block header.
///
/// The midstate is the SHA-256 internal compression state (eight 32-bit
/// chaining variables H0..H7) after processing exactly one 512-bit block
/// (the first 64 bytes of the 80-byte block header).
///
/// The ASIC only needs:
///   - This 32-byte midstate
///   - The remaining 16 bytes: merkle_root[28..32] + ntime + nbits
///   - The 4-byte nonce (which the ASIC iterates over)
///   - SHA-256 padding (computed in hardware)
///
/// This is NOT the same as SHA-256(first_64_bytes) — it's the intermediate
/// compression function output WITHOUT finalization.
///
/// Delegates to `crate::work::compute_midstate_from_prefix` which has the
/// correct manual SHA-256 compression implementation.
pub fn compute_midstate(header_prefix: &[u8; 64]) -> [u8; 32] {
    compute_midstate_from_prefix(header_prefix)
}

/// Generate an extranonce2 value from a counter.
///
/// Returns `size` bytes representing the counter in little-endian format.
/// The counter is truncated to fit the requested size.
///
/// Typical sizes:
///   - 4 bytes (most pools): 2^32 = ~4 billion unique work units per job
///   - 8 bytes (some pools): 2^64 = virtually unlimited
pub fn generate_extranonce2(counter: u64, size: usize) -> Vec<u8> {
    let size = size.min(MAX_V1_EXTRANONCE2_SIZE);
    let bytes = counter.to_le_bytes();
    let copy_len = size.min(8);
    let mut result = vec![0u8; size];
    result[..copy_len].copy_from_slice(&bytes[..copy_len]);
    result
}

/// Process a job template into work-ready components.
///
/// Returns (merkle_root, midstate, header_tail) for ASIC dispatch.
///
/// - `merkle_root`: The full 32-byte merkle root (for full header reconstruction)
/// - `midstate`: SHA-256 intermediate state of first 64 header bytes (for ASIC)
/// - `header_tail`: Last 4 bytes of merkle root + ntime + nbits + padding
///   (the ASIC processes this along with the nonce)
pub fn process_job(job: &JobTemplate, extranonce2_counter: u64) -> ([u8; 32], [u8; 32], [u8; 16]) {
    // Generate extranonce2
    let extranonce2 = generate_extranonce2(extranonce2_counter, job.extranonce2_size);

    // Build and hash coinbase
    let coinbase = build_coinbase(
        &job.coinbase1,
        &job.extranonce1,
        &extranonce2,
        &job.coinbase2,
    );
    let coinbase_hash = sha256d(&coinbase);

    // Compute merkle root
    let merkle_root = compute_merkle_root(&coinbase_hash, &job.merkle_branches);

    // Build first 64 bytes of header for midstate computation
    let mut header_prefix = [0u8; 64];
    header_prefix[0..4].copy_from_slice(&job.version.to_le_bytes());
    // prev_block_hash from the pool is in Stratum wire format (each 4-byte word
    // is byte-swapped relative to the block header's internal format).
    // Reverse bytes within each word to get the correct header byte order.
    let mut prev_hash = job.prev_block_hash;
    for chunk in prev_hash.chunks_exact_mut(4) {
        chunk.reverse();
    }
    header_prefix[4..36].copy_from_slice(&prev_hash);
    header_prefix[36..64].copy_from_slice(&merkle_root[..28]);

    let midstate = compute_midstate(&header_prefix);

    // Header tail: last 4 bytes of merkle root + ntime + nbits + 4 bytes padding
    let mut header_tail = [0u8; 16];
    header_tail[0..4].copy_from_slice(&merkle_root[28..32]);
    header_tail[4..8].copy_from_slice(&job.ntime.to_le_bytes());
    header_tail[8..12].copy_from_slice(&job.nbits.to_le_bytes());
    // bytes 12..15 are the nonce placeholder (zeros — ASIC fills this)

    (merkle_root, midstate, header_tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256d_empty() {
        let result = sha256d(b"");
        let hex_result = hex::encode(result);
        assert_eq!(
            hex_result,
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
        );
    }

    #[test]
    fn test_build_coinbase() {
        let cb1 = hex::decode("01000000").unwrap();
        let en1 = hex::decode("deadbeef").unwrap();
        let en2 = hex::decode("00000001").unwrap();
        let cb2 = hex::decode("ffffffff").unwrap();
        let result = build_coinbase(&cb1, &en1, &en2, &cb2);
        assert_eq!(hex::encode(&result), "01000000deadbeef00000001ffffffff");
    }

    #[test]
    fn test_merkle_root_no_branches() {
        // With 0 branches (solo mining), merkle root IS the coinbase hash
        let hash = [0x42u8; 32];
        let result = compute_merkle_root(&hash, &[]);
        assert_eq!(result, hash);
    }

    #[test]
    fn test_merkle_root_one_branch() {
        let coinbase_hash = [0x01u8; 32];
        let branch = [0x02u8; 32];
        let result = compute_merkle_root(&coinbase_hash, &[branch]);
        // Should be SHA256d(coinbase_hash || branch)
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&coinbase_hash);
        concat[32..].copy_from_slice(&branch);
        let expected = sha256d(&concat);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_build_block_header_length() {
        let header = build_block_header(
            0x20000000, &[0u8; 32], &[0u8; 32], 0x65a7e340, 0x170b3ce9, 0x00000000,
        );
        assert_eq!(header.len(), 80);
    }

    #[test]
    fn test_build_block_header_version() {
        let header = build_block_header(0x20000000, &[0u8; 32], &[0u8; 32], 0, 0, 0);
        // Version should be at bytes 0..4 in little-endian
        assert_eq!(&header[0..4], &[0x00, 0x00, 0x00, 0x20]);
    }

    #[test]
    fn test_generate_extranonce2() {
        let en2_zero = generate_extranonce2(1, 0);
        assert!(en2_zero.is_empty());

        let en2 = generate_extranonce2(1, 4);
        assert_eq!(en2, vec![0x01, 0x00, 0x00, 0x00]); // LE
        assert_eq!(en2.len(), 4);

        let en2_8 = generate_extranonce2(1, 8);
        assert_eq!(en2_8.len(), 8);
        assert_eq!(en2_8[0], 0x01);

        let en2_capped = generate_extranonce2(1, MAX_V1_EXTRANONCE2_SIZE + 1024);
        assert_eq!(en2_capped.len(), MAX_V1_EXTRANONCE2_SIZE);
    }

    #[test]
    fn test_midstate_is_deterministic() {
        let data = [0u8; 64];
        let ms1 = compute_midstate(&data);
        let ms2 = compute_midstate(&data);
        assert_eq!(ms1, ms2);
        assert_ne!(ms1, [0u8; 32]); // Should not be all zeros
    }

    #[test]
    fn test_midstate_differs_from_sha256() {
        // The midstate should NOT equal SHA-256(data) — it's the intermediate
        // compression state, not the finalized hash.
        let data = [0u8; 64];
        let midstate = compute_midstate(&data);
        let sha256_hash = Sha256::digest(&data);
        let mut sha256_arr = [0u8; 32];
        sha256_arr.copy_from_slice(&sha256_hash);
        assert_ne!(
            midstate, sha256_arr,
            "Midstate should differ from SHA-256 hash — midstate is the intermediate \
             compression state without padding and finalization"
        );
    }

    // -----------------------------------------------------------------------
    // Field-layout, multi-branch merkle, and extranonce2 truncation contracts.
    //
    // The existing tests cover happy paths but leave several wire-format
    // invariants and silent edge cases unpinned. Pin them so a refactor of
    // the header builder, merkle walk, or extranonce counter cannot
    // silently mis-encode work for the ASIC.
    // -----------------------------------------------------------------------

    #[test]
    fn sha256d_abc_known_answer_vector() {
        // SHA256d("abc") is a well-known test vector in Bitcoin literature.
        // Pin so a future refactor of the SHA-256 wrapper or Sha2 dependency
        // bump cannot silently change the hash output.
        let result = sha256d(b"abc");
        assert_eq!(
            hex::encode(result),
            "4f8b42c22dd3729b519ba6f68d2da7cc5b2d606d05daed5ad5128cc03e6c6358"
        );
    }

    #[test]
    fn build_coinbase_empty_parts_produces_empty_vec() {
        let coinbase = build_coinbase(&[], &[], &[], &[]);
        assert!(coinbase.is_empty());
    }

    #[test]
    fn build_coinbase_preserves_concatenation_order() {
        // Order matters for coinbase txid. coinbase = c1 || en1 || en2 || c2.
        let coinbase = build_coinbase(b"AAAA", b"BB", b"CC", b"DDDD");
        assert_eq!(coinbase, b"AAAABBCCDDDD");
    }

    #[test]
    fn compute_merkle_root_two_branches_matches_manual_walk() {
        // SV1 merkle walk: hash = sha256d(hash || branch) for each branch.
        // Pin two-branch case so a refactor that flips the walk direction
        // (left/right) is caught.
        let coinbase_hash = [0x11u8; 32];
        let branch_a = [0x22u8; 32];
        let branch_b = [0x33u8; 32];

        let result = compute_merkle_root(&coinbase_hash, &[branch_a, branch_b]);

        // Manual walk:
        // step 1: sha256d(coinbase_hash || branch_a)
        // step 2: sha256d(step1 || branch_b)
        let mut step1_input = [0u8; 64];
        step1_input[..32].copy_from_slice(&coinbase_hash);
        step1_input[32..].copy_from_slice(&branch_a);
        let step1 = sha256d(&step1_input);

        let mut step2_input = [0u8; 64];
        step2_input[..32].copy_from_slice(&step1);
        step2_input[32..].copy_from_slice(&branch_b);
        let expected = sha256d(&step2_input);

        assert_eq!(result, expected);
    }

    #[test]
    fn compute_merkle_root_branch_order_matters() {
        // Different branch ordering must produce different merkle roots
        // (otherwise the merkle path is not authenticating tree position).
        let coinbase_hash = [0x44u8; 32];
        let branch_a = [0x55u8; 32];
        let branch_b = [0x66u8; 32];

        let order_ab = compute_merkle_root(&coinbase_hash, &[branch_a, branch_b]);
        let order_ba = compute_merkle_root(&coinbase_hash, &[branch_b, branch_a]);
        assert_ne!(order_ab, order_ba);
    }

    #[test]
    fn build_block_header_field_positions_are_locked() {
        // Pin every field's byte position in the 80-byte block header.
        // The Bitcoin protocol commits to this layout — a refactor that
        // reordered any field would silently produce invalid blocks.
        let version: u32 = 0xDEAD_BEEF;
        let prev_hash = [0x11u8; 32];
        let merkle_root = [0x22u8; 32];
        let ntime: u32 = 0x65A7_E340;
        let nbits: u32 = 0x170B_3CE9;
        let nonce: u32 = 0xCAFE_BABE;

        let header = build_block_header(version, &prev_hash, &merkle_root, ntime, nbits, nonce);

        // Layout per Bitcoin spec:
        //   bytes  0..4   = version (LE)
        //   bytes  4..36  = prev_hash (raw, no swap)
        //   bytes 36..68  = merkle_root (raw, no swap)
        //   bytes 68..72  = ntime (LE)
        //   bytes 72..76  = nbits (LE)
        //   bytes 76..80  = nonce (LE)
        assert_eq!(&header[0..4], &version.to_le_bytes());
        assert_eq!(&header[4..36], &prev_hash);
        assert_eq!(&header[36..68], &merkle_root);
        assert_eq!(&header[68..72], &ntime.to_le_bytes());
        assert_eq!(&header[72..76], &nbits.to_le_bytes());
        assert_eq!(&header[76..80], &nonce.to_le_bytes());
        assert_eq!(header.len(), 80);
    }

    #[test]
    fn generate_extranonce2_counter_zero_produces_all_zeros() {
        // Counter=0 is the first work unit per job. Some downstream code
        // may treat all-zero extranonce as "no work assigned"; pin that
        // generate_extranonce2 actually produces all-zero bytes for counter=0
        // so any "no work" sentinel logic stays correct.
        let en2 = generate_extranonce2(0, 4);
        assert_eq!(en2, vec![0u8; 4]);
    }

    #[test]
    fn generate_extranonce2_silently_truncates_counter_past_size_capacity() {
        // KNOWN BEHAVIOR: counter > 2^(size*8) silently truncates the high
        // bits. With size=4 (typical V1), counter=0x1_0000_0001 produces
        // [0x01, 0x00, 0x00, 0x00] — same as counter=1. In practice
        // unreachable (4 billion shares per pool job) but pin the
        // truncation behavior so a future fix that adds explicit
        // wraparound detection updates this test.
        let en2_low = generate_extranonce2(1, 4);
        let en2_high = generate_extranonce2(0x1_0000_0001, 4);
        assert_eq!(
            en2_low, en2_high,
            "high bits silently truncated to fit size"
        );
        assert_eq!(en2_low, vec![0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn generate_extranonce2_size_two_holds_uint16_range() {
        // size=2 extranonce gives 65536 unique work units before wrap.
        // Pin the LE byte ordering at the boundary.
        let max_u16 = generate_extranonce2(u16::MAX as u64, 2);
        assert_eq!(max_u16, vec![0xFF, 0xFF]);

        let one_past = generate_extranonce2(u16::MAX as u64 + 1, 2);
        assert_eq!(
            one_past,
            vec![0x00, 0x00],
            "size=2 counter wraps at 65536 — same silent truncation as size=4"
        );
    }

    #[test]
    fn process_job_returns_consistent_merkle_and_midstate_for_same_inputs() {
        // Same job + same counter must produce identical merkle/midstate/tail
        // — work generation is deterministic. Pin so a refactor that
        // introduces nondeterminism (e.g. randomized padding) is caught.
        let job = JobTemplate {
            job_id: "test".to_string(),
            prev_block_hash: [0x42; 32],
            coinbase1: vec![0x01, 0x02, 0x03, 0x04],
            coinbase2: vec![0xFA, 0xFB, 0xFC, 0xFD],
            merkle_branches: vec![[0x55; 32]],
            version: 0x2000_0000,
            nbits: 0x170b_3ce9,
            ntime: 1_700_000_000,
            clean_jobs: true,
            share_target: [0xFF; 32],
            extranonce1: vec![0xAA, 0xBB],
            extranonce2_size: 4,
            version_mask: 0,
            merkle_root: [0u8; 32],
            pool_difficulty: 1.0,
        };

        let (root_a, mid_a, tail_a) = process_job(&job, 7);
        let (root_b, mid_b, tail_b) = process_job(&job, 7);
        assert_eq!(root_a, root_b);
        assert_eq!(mid_a, mid_b);
        assert_eq!(tail_a, tail_b);

        // Different counter must produce different merkle root (otherwise
        // extranonce2 isn't actually salting the coinbase).
        let (root_c, _, _) = process_job(&job, 8);
        assert_ne!(root_a, root_c);
    }

    #[test]
    fn process_job_header_tail_layout_is_locked() {
        // Header tail = merkle_root[28..32] + ntime + nbits + 4 zero bytes
        // The ASIC consumes this 16-byte chunk + 4-byte nonce. Pin the
        // layout so a refactor doesn't silently shift the field offsets
        // and break ASIC nonce search.
        let job = JobTemplate {
            job_id: "test".to_string(),
            prev_block_hash: [0x42; 32],
            coinbase1: vec![0x01],
            coinbase2: vec![0x02],
            merkle_branches: vec![],
            version: 0x2000_0000,
            nbits: 0x1234_5678,
            ntime: 0xABCD_0000,
            clean_jobs: true,
            share_target: [0xFF; 32],
            extranonce1: vec![],
            extranonce2_size: 4,
            version_mask: 0,
            merkle_root: [0u8; 32],
            pool_difficulty: 1.0,
        };

        let (merkle_root, _, tail) = process_job(&job, 0);

        // Tail layout: [merkle_root[28..32]] [ntime LE] [nbits LE] [zeros]
        assert_eq!(&tail[0..4], &merkle_root[28..32]);
        assert_eq!(&tail[4..8], &0xABCD_0000u32.to_le_bytes());
        assert_eq!(&tail[8..12], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&tail[12..16], &[0u8; 4]);
    }
}
