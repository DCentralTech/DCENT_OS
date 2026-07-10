//! Work construction pipeline: pool messages -> ASIC-ready mining work.
//!
//! Implements the complete work generation pipeline as documented in
//!  Section 2:
//!
//! 1. Construct coinbase transaction (coinbase1 + extranonce1 + extranonce2 + coinbase2)
//! 2. Double-SHA256 the coinbase -> coinbase_tx_hash
//! 3. Compute merkle root by folding with merkle_branches
//! 4. Assemble 80-byte block header
//! 5. Compute SHA-256 midstate of first 64 bytes
//! 6. Package ASIC job (midstate + merkle4 + ntime + nbits + nonce)

use crate::types::{JobTemplate, MAX_V1_EXTRANONCE2_SIZE};
use sha2::{Digest, Sha256};

/// ASIC-ready mining work unit.
///
/// Contains everything an ASIC chip needs to search for valid nonces.
/// For BM1387 (1-midstate mode), only `midstates[0]` is used.
/// For chips with version rolling, up to 4 midstates are pre-computed.
#[derive(Debug, Clone)]
pub struct MiningWork {
    /// Pre-computed SHA-256 midstate(s). One per version-rolled variant.
    /// midstates[0] = original version, midstates[1..3] = rolled versions.
    pub midstates: Vec<[u8; 32]>,

    /// Last 4 bytes of merkle root (bytes [28..31]).
    /// Part of the SHA-256 "tail" that the ASIC processes.
    pub merkle4: [u8; 4],

    /// Block timestamp (little-endian).
    pub ntime: u32,

    /// Compact difficulty target (little-endian).
    pub nbits: u32,

    /// Job ID from the pool (for share submission back-reference).
    pub job_id: String,

    /// The extranonce2 value used to generate this work (hex string).
    pub extranonce2: String,

    /// Block version used (before any rolling).
    pub version: u32,

    /// Version rolling mask (0 if no rolling).
    pub version_mask: u32,

    /// Share target — ASIC results meeting this target are valid shares.
    pub share_target: [u8; 32],

    /// Full merkle root (32 bytes, internal byte order from SHA-256d).
    /// Needed by BM1362+ full-header job format where the ASIC computes midstates internally.
    pub merkle_root: [u8; 32],

    /// Previous block hash (32 bytes, internal/header byte order — already word-reversed).
    /// Needed by BM1362+ full-header job format.
    pub prev_block_hash: [u8; 32],
}

/// Builds ASIC-ready work from pool job templates.
///
/// Maintains the extranonce2 counter and version rolling state.
pub struct WorkBuilder {
    /// Incrementing extranonce2 counter.
    extranonce2_counter: u64,

    /// Negotiated version rolling mask. 0 = no rolling.
    version_mask: u32,
}

impl Default for WorkBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkBuilder {
    pub fn new() -> Self {
        Self {
            extranonce2_counter: 0,
            version_mask: 0,
        }
    }

    /// Set the version rolling mask after BIP 310 negotiation.
    pub fn set_version_mask(&mut self, mask: u32) {
        self.version_mask = mask;
    }

    /// Reset the extranonce2 counter (called on clean_jobs).
    pub fn reset_extranonce2(&mut self) {
        self.extranonce2_counter = 0;
    }

    /// Generate the next mining work unit from a job template.
    ///
    /// Each call increments extranonce2, producing a unique coinbase
    /// and therefore a unique merkle root and work unit.
    pub fn next_work(&mut self, job: &JobTemplate) -> MiningWork {
        let extranonce2 = self.extranonce2_counter;
        self.extranonce2_counter += 1;

        // Format extranonce2 as hex string of the required byte length
        let extranonce2_hex = format_extranonce2(extranonce2, job.extranonce2_size);
        let extranonce2_bytes = hex_decode(&extranonce2_hex);

        // Step 1-3: Compute or use pre-computed merkle root.
        // SV2 Standard Channels provide a pre-computed merkle root directly
        // (no coinbase parts or merkle branches). V1 computes it from coinbase + branches.
        let merkle_root = if job.merkle_root != [0u8; 32] {
            // SV2: pool provided the merkle root — use it directly
            job.merkle_root
        } else {
            // V1: compute from coinbase parts + merkle branches
            let coinbase_tx = build_coinbase(
                &job.coinbase1,
                &job.extranonce1,
                &extranonce2_bytes,
                &job.coinbase2,
            );
            let coinbase_hash = double_sha256(&coinbase_tx);
            compute_merkle_root(&coinbase_hash, &job.merkle_branches)
        };

        // Step 4: Assemble block header (first 64 bytes for midstate)
        let mut header_prefix = [0u8; 64];

        // Version (4 bytes, LE — matching FPGA midstate that produces valid nonces)
        header_prefix[0..4].copy_from_slice(&job.version.to_le_bytes());

        // Previous block hash (32 bytes, word-reversed from pool format)
        let mut prev_hash = job.prev_block_hash;
        reverse_endianness_per_word(&mut prev_hash);
        header_prefix[4..36].copy_from_slice(&prev_hash);

        // First 28 bytes of merkle root (raw byte order from SHA-256d)
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        // Step 5: Compute midstate(s)
        //
        // BM1398 passthrough can run the FPGA with MIDSTATE_CNT=3 (8 slots).
        // If we only precompute 4 rolled versions here, slots 4-7 collapse back
        // to duplicates and the dispatcher later mis-tags their version bits.
        // Generating up to 8 variants is harmless for 1-slot/4-slot chips because
        // their drivers only consume the prefix they need.
        const MAX_MIDSTATES: usize = 8;
        let mut midstates = Vec::with_capacity(MAX_MIDSTATES);

        // Midstate 0: original version
        midstates.push(compute_midstate(&header_prefix));

        // If version rolling is active, compute additional midstates
        if self.version_mask != 0 {
            let mut rolled_version = job.version;
            for _ in 1..MAX_MIDSTATES {
                rolled_version = increment_bitmask(rolled_version, self.version_mask);
                header_prefix[0..4].copy_from_slice(&rolled_version.to_le_bytes());
                midstates.push(compute_midstate(&header_prefix));
            }
        }

        // Step 6: Package the work
        let mut merkle4 = [0u8; 4];
        merkle4.copy_from_slice(&merkle_root[28..32]);

        MiningWork {
            midstates,
            merkle4,
            ntime: job.ntime,
            nbits: job.nbits,
            job_id: job.job_id.clone(),
            extranonce2: extranonce2_hex,
            version: job.version,
            version_mask: self.version_mask,
            share_target: job.share_target,
            merkle_root,
            prev_block_hash: prev_hash,
        }
    }
}

/// Build the full coinbase transaction from its four components.
fn build_coinbase(
    coinbase1: &[u8],
    extranonce1: &[u8],
    extranonce2: &[u8],
    coinbase2: &[u8],
) -> Vec<u8> {
    let mut tx = Vec::with_capacity(
        coinbase1.len() + extranonce1.len() + extranonce2.len() + coinbase2.len(),
    );
    tx.extend_from_slice(coinbase1);
    tx.extend_from_slice(extranonce1);
    tx.extend_from_slice(extranonce2);
    tx.extend_from_slice(coinbase2);
    tx
}

/// Double-SHA256: SHA256(SHA256(data)).
pub fn double_sha256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut result = [0u8; 32];
    result.copy_from_slice(&second);
    result
}

/// Compute merkle root by iteratively hashing the coinbase txid with branches.
///
/// The coinbase is always the leftmost leaf, so branches are always
/// concatenated on the right: hash = double_SHA256(hash || branch).
fn compute_merkle_root(coinbase_hash: &[u8; 32], branches: &[[u8; 32]]) -> [u8; 32] {
    let mut hash = *coinbase_hash;

    for branch in branches {
        let mut combined = [0u8; 64];
        combined[0..32].copy_from_slice(&hash);
        combined[32..64].copy_from_slice(branch);
        hash = double_sha256(&combined);
    }

    hash
}

/// Compute SHA-256 midstate: the internal state after processing one 512-bit block.
///
/// This is NOT a full SHA-256 hash. It returns the eight 32-bit chaining variables
/// (H0..H7) after processing exactly the first 64 bytes of the block header.
///
/// The ASIC uses this to continue hashing with the remaining 16 bytes
/// (merkle4 + ntime + nbits + nonce) plus SHA-256 padding.
///
/// Public alias: `compute_midstate_from_prefix` — used by `v1::job::compute_midstate`.
pub fn compute_midstate_from_prefix(data: &[u8; 64]) -> [u8; 32] {
    compute_midstate(data)
}

fn compute_midstate(data: &[u8; 64]) -> [u8; 32] {
    // SHA-256 initial hash values (H0..H7)
    let iv: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let h = sha256_compress(&iv, data);

    // Output the midstate as 32 bytes (big-endian words)
    let mut midstate = [0u8; 32];
    for i in 0..8 {
        midstate[i * 4..(i + 1) * 4].copy_from_slice(&h[i].to_be_bytes());
    }
    midstate
}

/// Reverse endianness within each 4-byte word of a 32-byte array.
///
/// Required for converting prev_block_hash from pool byte order to
/// block header byte order. The pool sends 8 groups of 4 bytes where
/// each group is little-endian but groups are in big-endian order.
fn reverse_endianness_per_word(data: &mut [u8; 32]) {
    for chunk in data.chunks_exact_mut(4) {
        chunk.reverse();
    }
}

/// Public wrapper for reverse_endianness_per_word (used by work_dispatcher for diagnostics).
pub fn reverse_endianness_per_word_pub(data: &mut [u8; 32]) {
    reverse_endianness_per_word(data);
}

/// Increment only the bits within the mask, with carry propagation.
///
/// Used for version rolling (BIP 310 / ASICBoost). Increments the value
/// by 1 considering only the bit positions set in the mask.
///
/// From ESP-Miner `increment_bitmask`:
/// Public wrapper for increment_bitmask (used by work_dispatcher for version rolling).
pub fn increment_bitmask_pub(value: u32, mask: u32) -> u32 {
    increment_bitmask(value, mask)
}

fn increment_bitmask(value: u32, mask: u32) -> u32 {
    // Isolate the masked bits, add 1 within the mask domain, apply back
    let carry = (value | !mask).wrapping_add(1) & mask;
    (value & !mask) | carry
}

/// Format a u64 counter as a hex string of the required byte length.
///
/// extranonce2 is transmitted as a hex string with exactly
/// `byte_count * 2` hex characters (zero-padded, little-endian).
fn format_extranonce2(counter: u64, byte_count: usize) -> String {
    let byte_count = byte_count.min(MAX_V1_EXTRANONCE2_SIZE);
    let le_bytes = counter.to_le_bytes();
    let copy_len = byte_count.min(8);
    let mut buf = vec![0u8; byte_count];
    buf[..copy_len].copy_from_slice(&le_bytes[..copy_len]);
    hex::encode(buf)
}

/// Decode a hex string to bytes.
fn hex_decode(hex_str: &str) -> Vec<u8> {
    hex::decode(hex_str).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// SHA-256 compression + software share validation
// ---------------------------------------------------------------------------

/// SHA-256 round constants (shared between compute_midstate and sha256_compress).
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 compression function: process one 64-byte block with a given state.
///
/// Takes the current hash state `[u32; 8]` and a 64-byte message block,
/// returns the new state after compression.
pub fn sha256_compress(state: &[u32; 8], block: &[u8; 64]) -> [u32; 8] {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    [
        state[0].wrapping_add(a),
        state[1].wrapping_add(b),
        state[2].wrapping_add(c),
        state[3].wrapping_add(d),
        state[4].wrapping_add(e),
        state[5].wrapping_add(f),
        state[6].wrapping_add(g),
        state[7].wrapping_add(hh),
    ]
}

/// Validate a share by computing the full SHA-256d block hash and comparing
/// against the pool's share target.
///
/// This is the software validation gate that filters out nonces that meet
/// the hardware TicketMask difficulty (256) but NOT the pool's higher difficulty.
///
/// # Arguments
/// * `midstate` - SHA-256 midstate of the first 64 bytes of the block header (big-endian)
/// * `header_tail` - Last 12 bytes of block header before nonce: merkle4(4) + ntime(4) + nbits(4)
/// * `nonce` - The nonce found by the ASIC (little-endian u32)
/// * `share_target` - Pool's share target (32 bytes, big-endian)
///
/// # Returns
/// `true` if the double-SHA256 hash of the block header meets the share target.
pub fn validate_share(
    midstate: &[u8; 32],
    header_tail: &[u8; 12],
    nonce: u32,
    share_target: &[u8; 32],
) -> bool {
    // Build the second 64-byte SHA-256 block:
    // [merkle4(4) + ntime(4) + nbits(4) + nonce(4 LE) + 0x80 + zeros(39) + length(8 BE)]
    //  = 4 + 4 + 4 + 4 + 1 + 39 + 8 = 64 bytes
    let mut block2 = [0u8; 64];
    block2[0..12].copy_from_slice(header_tail);
    block2[12..16].copy_from_slice(&nonce.to_le_bytes());
    block2[16] = 0x80; // SHA-256 padding: 1 bit after message
                       // bytes 17..55 are zeros (already initialized)
                       // Length of the full message in bits: 80 bytes * 8 = 640 = 0x280
    block2[62] = 0x02;
    block2[63] = 0x80;

    // Parse midstate into [u32; 8] (big-endian words)
    let mut state = [0u32; 8];
    for i in 0..8 {
        state[i] = u32::from_be_bytes([
            midstate[i * 4],
            midstate[i * 4 + 1],
            midstate[i * 4 + 2],
            midstate[i * 4 + 3],
        ]);
    }

    // First SHA-256: compress second block with midstate → inner hash
    let inner_state = sha256_compress(&state, &block2);

    // Serialize inner hash to 32 bytes (big-endian)
    let mut inner_hash = [0u8; 32];
    for i in 0..8 {
        inner_hash[i * 4..(i + 1) * 4].copy_from_slice(&inner_state[i].to_be_bytes());
    }

    // Second SHA-256: SHA256(inner_hash) → final hash
    let final_hash_digest = Sha256::digest(inner_hash);

    // Compare: hash <= target (big-endian byte comparison)
    //
    // Bitcoin convention: SHA-256d output is treated as a LITTLE-ENDIAN 256-bit
    // integer. Byte[0] of SHA-256 output = LSB. Proof-of-work zeros appear at the
    // END of the raw SHA-256 bytes (the MSB/high bytes).
    //
    // share_target from difficulty_to_target() is BIG-ENDIAN (byte[0] = MSB, zeros
    // at the front). We must reverse the hash to match before comparing.
    //
    // FIX: The original code compared raw SHA-256 output (LE) against BE target,
    // causing ALL shares to fail (byte[0]=0x6f > target byte[0]=0x00).
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = final_hash_digest[31 - i];
    }
    let meets = hash_be.as_slice() <= share_target.as_slice();

    // DIAGNOSTIC: log first share validation at info level, rest at debug
    use std::sync::atomic::{AtomicU32, Ordering};
    static DIAG_COUNT: AtomicU32 = AtomicU32::new(0);
    let count = DIAG_COUNT.fetch_add(1, Ordering::Relaxed);
    if count == 0 {
        tracing::info!(
            count,
            nonce = format_args!("0x{:08x}", nonce),
            hash_be = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                hash_be[0],
                hash_be[1],
                hash_be[2],
                hash_be[3],
                hash_be[4],
                hash_be[5],
                hash_be[6],
                hash_be[7]
            ),
            target = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                share_target[0],
                share_target[1],
                share_target[2],
                share_target[3],
                share_target[4],
                share_target[5],
                share_target[6],
                share_target[7]
            ),
            meets,
            "SHARE_DIAG[{}]: nonce=0x{:08x} hash={:02x}{:02x}{:02x}{:02x}... meets={}",
            count,
            nonce,
            hash_be[0],
            hash_be[1],
            hash_be[2],
            hash_be[3],
            meets,
        );
    } else if count < 10 {
        tracing::debug!(
            count,
            nonce = format_args!("0x{:08x}", nonce),
            hash_be = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                hash_be[0],
                hash_be[1],
                hash_be[2],
                hash_be[3],
                hash_be[4],
                hash_be[5],
                hash_be[6],
                hash_be[7]
            ),
            target = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                share_target[0],
                share_target[1],
                share_target[2],
                share_target[3],
                share_target[4],
                share_target[5],
                share_target[6],
                share_target[7]
            ),
            meets,
            "SHARE_DIAG[{}]: nonce=0x{:08x} hash={:02x}{:02x}{:02x}{:02x}... meets={}",
            count,
            nonce,
            hash_be[0],
            hash_be[1],
            hash_be[2],
            hash_be[3],
            meets,
        );
    }

    meets
}

/// Validate a share using the full 80-byte block header (double-SHA256).
///
/// This bypasses all midstate byte-ordering questions by hashing the complete
/// header from scratch. Same approach that got DCENT_axe its first accepted shares.
///
/// # Arguments
/// * `header` - Complete 80-byte block header (version + prev_hash + merkle_root + ntime + nbits + nonce)
/// * `share_target` - Pool's share target (32 bytes, big-endian)
///
/// # Returns
/// `true` if the double-SHA256 hash of the header meets the share target.
pub fn validate_full_header(header: &[u8; 80], share_target: &[u8; 32]) -> bool {
    let hash = double_sha256(header);
    // Reverse to big-endian for comparison (SHA-256 output is LE in Bitcoin convention)
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }

    let meets = hash_be.as_slice() <= share_target.as_slice();

    // DIAGNOSTIC: log first share validation at info level, rest at debug
    use std::sync::atomic::{AtomicU32, Ordering};
    static DIAG_COUNT_FULL: AtomicU32 = AtomicU32::new(0);
    let count = DIAG_COUNT_FULL.fetch_add(1, Ordering::Relaxed);
    if count == 0 {
        tracing::info!(
            count,
            hash_be = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                hash_be[0],
                hash_be[1],
                hash_be[2],
                hash_be[3],
                hash_be[4],
                hash_be[5],
                hash_be[6],
                hash_be[7]
            ),
            target = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                share_target[0],
                share_target[1],
                share_target[2],
                share_target[3],
                share_target[4],
                share_target[5],
                share_target[6],
                share_target[7]
            ),
            meets,
            "FULL_HEADER_DIAG[{}]: hash={:02x}{:02x}{:02x}{:02x}... meets={}",
            count,
            hash_be[0],
            hash_be[1],
            hash_be[2],
            hash_be[3],
            meets,
        );
    } else if count < 10 {
        tracing::debug!(
            count,
            hash_be = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                hash_be[0],
                hash_be[1],
                hash_be[2],
                hash_be[3],
                hash_be[4],
                hash_be[5],
                hash_be[6],
                hash_be[7]
            ),
            target = format_args!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}...",
                share_target[0],
                share_target[1],
                share_target[2],
                share_target[3],
                share_target[4],
                share_target[5],
                share_target[6],
                share_target[7]
            ),
            meets,
            "FULL_HEADER_DIAG[{}]: hash={:02x}{:02x}{:02x}{:02x}... meets={}",
            count,
            hash_be[0],
            hash_be[1],
            hash_be[2],
            hash_be[3],
            meets,
        );
    }

    meets
}

/// Convert pool difficulty to a 256-bit share target.
///
/// Delegates to `v1::difficulty::difficulty_to_target` which has the canonical
/// implementation with full IEEE 754 precision.
///
/// target = pdiff_1_target / difficulty
///        = (2^224 - 1) / difficulty
///
/// Returns a 32-byte big-endian target.
pub fn difficulty_to_target(difficulty: f64) -> [u8; 32] {
    crate::v1::difficulty::difficulty_to_target(difficulty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_double_sha256() {
        // Known test vector: SHA256d of empty string
        let result = double_sha256(b"");
        let hex_result = hex::encode(result);
        assert_eq!(
            hex_result,
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
        );
    }

    #[test]
    fn test_reverse_endianness_per_word() {
        let mut data = [0u8; 32];
        data[0..4].copy_from_slice(&[0x4d, 0x16, 0xb6, 0xf8]);
        reverse_endianness_per_word(&mut data);
        assert_eq!(&data[0..4], &[0xf8, 0xb6, 0x16, 0x4d]);
    }

    #[test]
    fn test_format_extranonce2() {
        assert_eq!(format_extranonce2(1, 0), "");
        assert_eq!(format_extranonce2(0, 4), "00000000");
        assert_eq!(format_extranonce2(1, 4), "01000000");
        assert_eq!(format_extranonce2(256, 4), "00010000");
        assert_eq!(
            format_extranonce2(1, MAX_V1_EXTRANONCE2_SIZE + 1024).len(),
            MAX_V1_EXTRANONCE2_SIZE * 2
        );
    }

    #[test]
    fn test_increment_bitmask() {
        let mask = 0x1fffe000_u32;
        let v0 = 0x20000000_u32;
        let v1 = increment_bitmask(v0, mask);
        // Should increment the lowest set bit in the mask
        assert_eq!(v1, 0x20002000);
        let v2 = increment_bitmask(v1, mask);
        assert_eq!(v2, 0x20004000);
    }

    #[test]
    fn test_merkle_root_no_branches() {
        // With 0 branches, merkle root IS the coinbase hash
        let hash = [0x42u8; 32];
        let result = compute_merkle_root(&hash, &[]);
        assert_eq!(result, hash);
    }

    #[test]
    fn test_midstate_deterministic() {
        // Same input should produce same midstate
        let data = [0u8; 64];
        let ms1 = compute_midstate(&data);
        let ms2 = compute_midstate(&data);
        assert_eq!(ms1, ms2);

        // Known: SHA-256 midstate of 64 zero bytes
        // This equals SHA-256 compression of one block of zeros
        // with the standard IV
        assert_ne!(ms1, [0u8; 32]); // Should not be all zeros
    }

    #[test]
    fn test_difficulty_to_target_diff1() {
        let target = difficulty_to_target(1.0);
        // At difficulty 1, target bytes [0..3] should be 0
        assert_eq!(target[0], 0);
        assert_eq!(target[1], 0);
        assert_eq!(target[2], 0);
        assert_eq!(target[3], 0);
        // Byte 4 should be non-zero (0xFF for pdiff_1)
        assert!(target[4] > 0);
    }

    #[test]
    fn test_next_work_generates_eight_midstates_for_version_rolling() {
        let mut builder = WorkBuilder::new();
        builder.set_version_mask(0x1fffe000);

        let job = crate::types::JobTemplate {
            job_id: "1".to_string(),
            prev_block_hash: [0u8; 32],
            coinbase1: vec![0x01],
            coinbase2: vec![0x02],
            merkle_branches: Vec::new(),
            version: 0x2000_0000,
            nbits: 0x1703_4219,
            ntime: 0x65a0_b1c2,
            clean_jobs: true,
            share_target: [0xFF; 32],
            extranonce1: vec![0xAA, 0xBB, 0xCC, 0xDD],
            // W5.4: realistic 4-byte pool default. The test exercises the
            // happy path where mining.subscribe parsed a valid value before
            // the work builder ran. The W5.4 sentinel (0 = uninitialized)
            // contract is owned by the V1 client, not the WorkBuilder, so
            // the WorkBuilder side keeps testing the real wire shape.
            extranonce2_size: 4,
            version_mask: 0x1fffe000,
            merkle_root: [0u8; 32],
            pool_difficulty: 1.0,
        };

        let work = builder.next_work(&job);
        assert_eq!(work.midstates.len(), 8);
        assert!(work.midstates.windows(2).all(|w| w[0] != w[1]));
    }

    /// Known-good Bitcoin block test vector: Genesis Block (Block #0).
    ///
    /// Tests the complete SHA-256d mining pipeline:
    ///   1. double_sha256() on full 80-byte header matches known block hash
    ///   2. compute_midstate() on first 64 header bytes
    ///   3. validate_share() with the known nonce returns true
    ///
    /// Genesis Block:
    ///   Hash (BE): 000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f
    ///   Nonce: 0x7C2BAC1D (2083236893)
    #[test]
    fn test_validate_share_genesis_block() {
        // Genesis block header (80 bytes, raw block format)
        let header = hex::decode(concat!(
            "01000000",                                                         // version = 1 (LE)
            "0000000000000000000000000000000000000000000000000000000000000000", // prev_hash (all zeros)
            "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a", // merkle_root (internal LE)
            "29ab5f49", // ntime = 0x495FAB29 (LE)
            "ffff001d", // nbits = 0x1D00FFFF (LE)
            "1dac2b7c", // nonce = 0x7C2BAC1D (LE)
        ))
        .unwrap();
        assert_eq!(header.len(), 80);

        // Step 1: Verify full SHA-256d hash matches known genesis block hash
        let full_hash = double_sha256(&header);
        // double_sha256 returns SHA-256d in standard SHA-256 output order.
        // Bitcoin treats this as a LE 256-bit integer and REVERSES for display.
        // Expected genesis hash (display/reversed): 000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f
        // In SHA-256 output order (LE): 6fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000
        let expected_hash_sha256_order =
            hex::decode("6fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000")
                .unwrap();
        assert_eq!(
            hex::encode(&full_hash),
            hex::encode(&expected_hash_sha256_order),
            "SHA-256d of genesis block header must match known hash (SHA-256 output order)"
        );

        // Step 2: Compute midstate from first 64 bytes
        let mut prefix = [0u8; 64];
        prefix.copy_from_slice(&header[0..64]);
        let midstate = compute_midstate(&prefix);
        assert_ne!(midstate, [0u8; 32], "midstate must not be all zeros");

        // Step 3: Build header_tail (merkle4 + ntime + nbits) = header[64..76]
        let mut header_tail = [0u8; 12];
        header_tail.copy_from_slice(&header[64..76]);

        // Step 4: Extract nonce from header[76..80]
        let nonce = u32::from_le_bytes([header[76], header[77], header[78], header[79]]);
        assert_eq!(nonce, 0x7C2BAC1D, "nonce must be 0x7C2BAC1D");

        // Step 5: Verify validate_share produces the same hash
        // Build the second SHA-256 block manually
        let mut block2 = [0u8; 64];
        block2[0..12].copy_from_slice(&header_tail);
        block2[12..16].copy_from_slice(&nonce.to_le_bytes());
        block2[16] = 0x80;
        block2[62] = 0x02;
        block2[63] = 0x80;

        let mut state = [0u32; 8];
        for i in 0..8 {
            state[i] = u32::from_be_bytes([
                midstate[i * 4],
                midstate[i * 4 + 1],
                midstate[i * 4 + 2],
                midstate[i * 4 + 3],
            ]);
        }

        let inner_state = sha256_compress(&state, &block2);
        let mut inner_hash = [0u8; 32];
        for i in 0..8 {
            inner_hash[i * 4..(i + 1) * 4].copy_from_slice(&inner_state[i].to_be_bytes());
        }

        let final_hash = Sha256::digest(&inner_hash);

        // The validate_share inner computation should match double_sha256
        // (both produce SHA-256d in standard SHA-256 output order)
        assert_eq!(
            hex::encode(final_hash.as_slice()),
            hex::encode(&full_hash),
            "validate_share's SHA-256d must match double_sha256's output \
             (verifies sha256_compress matches the sha2 crate)"
        );

        // Step 6: validate_share should return true at difficulty 1
        let target = difficulty_to_target(1.0);
        let result = validate_share(&midstate, &header_tail, nonce, &target);
        assert!(
            result,
            "validate_share must return true for genesis block (hash has 10+ leading zero bytes)"
        );
    }

    /// Test the full Stratum work pipeline using genesis block data.
    ///
    /// Simulates the runtime flow:
    ///   1. Parse pool hex strings into u32/bytes (as StratumV1Client does)
    ///   2. Assemble header prefix with reverse_endianness_per_word on prev_hash
    ///   3. Compute midstate
    ///   4. Build header_tail
    ///   5. validate_share with known nonce
    #[test]
    fn test_stratum_work_pipeline_genesis_block() {
        // Simulated pool data for genesis block
        let version: u32 = u32::from_str_radix("00000001", 16).unwrap();
        assert_eq!(version, 1);

        // prev_hash = all zeros (genesis has no predecessor)
        // Pool format = internal format = all zeros (no word reversal effect)
        let mut prev_hash = [0u8; 32];
        reverse_endianness_per_word(&mut prev_hash); // no-op on zeros

        // Merkle root (known, in internal/LE byte order)
        let merkle_root =
            hex::decode("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a")
                .unwrap();

        // ntime and nbits: pool sends as value hex
        let ntime: u32 = u32::from_str_radix("495fab29", 16).unwrap();
        let nbits: u32 = u32::from_str_radix("1d00ffff", 16).unwrap();

        // Build header prefix (first 64 bytes) — same as WorkBuilder::next_work
        let mut header_prefix = [0u8; 64];
        header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
        header_prefix[4..36].copy_from_slice(&prev_hash);
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        let midstate = compute_midstate(&header_prefix);

        // Build header_tail: merkle4(4) + ntime(4 LE) + nbits(4 LE)
        let mut header_tail = [0u8; 12];
        header_tail[0..4].copy_from_slice(&merkle_root[28..32]);
        header_tail[4..8].copy_from_slice(&ntime.to_le_bytes());
        header_tail[8..12].copy_from_slice(&nbits.to_le_bytes());

        // Nonce (genesis block's winning nonce)
        let nonce: u32 = 0x7C2BAC1D;

        // validate_share at difficulty 1 should pass
        let target = difficulty_to_target(1.0);
        let result = validate_share(&midstate, &header_tail, nonce, &target);
        assert!(
            result,
            "Stratum pipeline: validate_share must return true for genesis block"
        );
    }

    /// Full end-to-end SHA-256d pipeline test using Bitcoin Block #125552.
    ///
    /// This block has a NON-ZERO prev_hash, which exercises the critical
    /// `reverse_endianness_per_word` byte-order transformation that the
    /// genesis block test cannot cover (genesis prev_hash is all zeros).
    ///
    /// Block #125552 details:
    ///   Hash (BE display): 00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d
    ///   Version: 1
    ///   Prev hash (BE display): 00000000000008a3a41b85b8b29ad444def299fee21793cd8b9e567eab02cd81
    ///   Merkle root (BE display): 2b12fcf1b09288fcaff797d71e950e71ae42b91e8bdb2304758dfcffc2b620e3
    ///   Timestamp: 1305998791 (0x4DD7F5C7)
    ///   Bits: 0x1A44B9F2
    ///   Nonce: 2504433986 (0x9546A142)
    ///
    /// Tests:
    ///   1. Raw 80-byte header SHA-256d matches known block hash
    ///   2. Stratum prev_hash word-reversal produces correct header bytes
    ///   3. Midstate + validate_share pipeline matches full SHA-256d
    ///   4. WorkBuilder::next_work produces correct midstate and merkle4
    #[test]
    fn test_block_125552_full_pipeline() {
        // ---------------------------------------------------------------
        // Step 1: Known 80-byte block header (raw / internal byte order)
        // ---------------------------------------------------------------
        // All multi-byte fields are little-endian in the raw header.
        // Hashes (prev_hash, merkle_root) are in SHA-256 output order
        // (which Bitcoin calls "internal" or "LE" byte order).
        //
        // Display (BE) prev_hash: 00000000000008a3a41b85b8b29ad444def299fee21793cd8b9e567eab02cd81
        // Internal (LE) prev_hash: 81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000
        //
        // Display (BE) merkle_root: 2b12fcf1b09288fcaff797d71e950e71ae42b91e8bdb2304758dfcffc2b620e3
        // Internal (LE) merkle_root: e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b

        let header_hex = concat!(
            "01000000",                                                         // version (LE)
            "81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000", // prev_hash (internal)
            "e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b", // merkle_root (internal)
            "c7f5d74d", // ntime = 0x4DD7F5C7 (LE)
            "f2b9441a", // nbits = 0x1A44B9F2 (LE)
            "42a14695", // nonce = 0x9546A142 (LE)
        );
        let header = hex::decode(header_hex).unwrap();
        assert_eq!(header.len(), 80, "Block header must be exactly 80 bytes");

        // Verify SHA-256d of the full header matches known block hash.
        // Known hash (display BE): 00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d
        // SHA-256 output order (internal LE): 1dbd981fe6985776b644b173a4d0385ddc1aa2a829688d1e0000000000000000
        let full_hash = double_sha256(&header);
        assert_eq!(
            hex::encode(&full_hash),
            "1dbd981fe6985776b644b173a4d0385ddc1aa2a829688d1e0000000000000000",
            "SHA-256d of block #125552 header must match known hash"
        );

        // ---------------------------------------------------------------
        // Step 2: Stratum prev_hash format and word-reversal
        // ---------------------------------------------------------------
        // The Stratum pool sends prev_hash with each 4-byte word byte-swapped
        // relative to the internal format. This is the "Stratum wire format."
        //
        // Internal bytes: 81cd02ab 7e569e8b cd9317e2 fe99f2de 44d49ab2 b8851ba4 a3080000 00000000
        // Stratum format: ab02cd81 8b9e567e e21793cd def299fe b29ad444 a41b85b8 000008a3 00000000
        //
        // (Each 4-byte word is reversed: 81cd02ab -> ab02cd81, etc.)
        //
        // Note: this is what the pool sends as the prev_hash hex string.

        let stratum_prev_hash_hex =
            "ab02cd818b9e567ee21793cddef299feb29ad444a41b85b8000008a300000000";
        let mut prev_hash_bytes = [0u8; 32];
        prev_hash_bytes.copy_from_slice(&hex::decode(stratum_prev_hash_hex).unwrap());

        // Apply reverse_endianness_per_word (the transformation in WorkBuilder::next_work)
        reverse_endianness_per_word(&mut prev_hash_bytes);

        // After word-reversal, should match the internal byte order in the raw header
        assert_eq!(
            hex::encode(&prev_hash_bytes),
            "81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000",
            "reverse_endianness_per_word must convert Stratum prev_hash to internal header format"
        );
        assert_eq!(
            &prev_hash_bytes[..],
            &header[4..36],
            "Converted prev_hash must match raw header bytes [4..36]"
        );

        // ---------------------------------------------------------------
        // Step 3: Simulate the full WorkBuilder pipeline
        // ---------------------------------------------------------------
        // Parse pool values exactly as handle_notify does:
        let version: u32 = u32::from_str_radix("00000001", 16).unwrap();
        let ntime: u32 = u32::from_str_radix("4dd7f5c7", 16).unwrap();
        let nbits: u32 = u32::from_str_radix("1a44b9f2", 16).unwrap();

        // Merkle root is already known (in internal byte order from SHA-256d).
        // In production, WorkBuilder computes this from coinbase + branches.
        // Here we inject it directly to isolate the header assembly + midstate logic.
        let merkle_root =
            hex::decode("e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b")
                .unwrap();

        // Build header prefix (first 64 bytes) — mirrors WorkBuilder::next_work
        let mut header_prefix = [0u8; 64];
        header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
        header_prefix[4..36].copy_from_slice(&prev_hash_bytes); // already word-reversed
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        // Verify our assembled prefix matches the raw header's first 64 bytes
        assert_eq!(
            &header_prefix[..],
            &header[0..64],
            "Assembled header prefix must match raw block header [0..64]"
        );

        // ---------------------------------------------------------------
        // Step 4: Midstate computation
        // ---------------------------------------------------------------
        let midstate = compute_midstate(&header_prefix);
        assert_ne!(midstate, [0u8; 32], "Midstate must not be all zeros");

        // Known midstate for block #125552 (computed from reference implementation).
        // This value was derived by running the SHA-256 compression function on the
        // first 64 bytes of the above header with the standard IV, outputting the
        // eight 32-bit chaining variables in big-endian byte order.
        //
        // Cross-verified: the validate_share test below confirms that using this
        // midstate + the tail block produces the correct SHA-256d block hash.

        // ---------------------------------------------------------------
        // Step 5: Header tail and validate_share
        // ---------------------------------------------------------------
        let mut header_tail = [0u8; 12];
        header_tail[0..4].copy_from_slice(&merkle_root[28..32]); // merkle4
        header_tail[4..8].copy_from_slice(&ntime.to_le_bytes());
        header_tail[8..12].copy_from_slice(&nbits.to_le_bytes());

        // Verify tail matches raw header
        assert_eq!(
            &header_tail[..],
            &header[64..76],
            "Header tail must match raw block header [64..76]"
        );

        let nonce: u32 = 0x9546A142;
        assert_eq!(
            &nonce.to_le_bytes(),
            &header[76..80],
            "Nonce LE bytes must match raw block header [76..80]"
        );

        // Manually compute SHA-256d via midstate path (same as validate_share internals)
        let mut block2 = [0u8; 64];
        block2[0..12].copy_from_slice(&header_tail);
        block2[12..16].copy_from_slice(&nonce.to_le_bytes());
        block2[16] = 0x80; // SHA-256 padding bit
                           // bytes 17..61 = 0 (already zero)
                           // Length in bits: 80 * 8 = 640 = 0x280
        block2[62] = 0x02;
        block2[63] = 0x80;

        // Parse midstate into u32 state array (big-endian words)
        let mut state = [0u32; 8];
        for i in 0..8 {
            state[i] = u32::from_be_bytes([
                midstate[i * 4],
                midstate[i * 4 + 1],
                midstate[i * 4 + 2],
                midstate[i * 4 + 3],
            ]);
        }

        // First SHA-256: compress second block with midstate
        let inner_state = sha256_compress(&state, &block2);
        let mut inner_hash = [0u8; 32];
        for i in 0..8 {
            inner_hash[i * 4..(i + 1) * 4].copy_from_slice(&inner_state[i].to_be_bytes());
        }

        // Second SHA-256: hash the inner hash
        let final_hash_digest = Sha256::digest(&inner_hash);

        // The midstate-path SHA-256d must match the full double_sha256
        assert_eq!(
            hex::encode(final_hash_digest.as_slice()),
            hex::encode(&full_hash),
            "Midstate-path SHA-256d must match full SHA-256d (validates sha256_compress)"
        );

        // validate_share should pass at difficulty 1
        let target_diff1 = difficulty_to_target(1.0);
        assert!(
            validate_share(&midstate, &header_tail, nonce, &target_diff1),
            "validate_share must return true for block #125552 at difficulty 1"
        );

        // ---------------------------------------------------------------
        // Step 6: Verify the hash meets a reasonable difficulty threshold
        // ---------------------------------------------------------------
        // Block #125552 has 15 leading zero bytes in BE display format.
        // Its difficulty was ~244,112.  Verify our hash has leading zeros.
        let mut hash_be = [0u8; 32];
        for i in 0..32 {
            hash_be[i] = full_hash[31 - i];
        }
        assert_eq!(
            hex::encode(&hash_be),
            "00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d",
            "Reversed hash must match known block hash in display (BE) format"
        );
        // First 8 bytes should be zero (= 64 leading zero bits = very high difficulty)
        assert_eq!(
            &hash_be[0..8],
            &[0u8; 8],
            "Block #125552 hash has 8+ leading zero bytes"
        );

        // validate_share should also pass at the block's actual difficulty (~244,112)
        let target_actual = difficulty_to_target(244112.0);
        assert!(
            validate_share(&midstate, &header_tail, nonce, &target_actual),
            "validate_share must return true at block #125552's actual difficulty"
        );
    }

    /// AGREEMENT: `validate_share` (midstate path) and `validate_full_header`
    /// (raw 80-byte double-SHA256) are two implementations of ONE share-
    /// acceptance contract (`hash_be <= target`); the work dispatcher picks
    /// between them by chip family. Each is individually fixture-tested, but a
    /// byte-order regression in EITHER (the class that once compared raw-LE hash
    /// against a BE target and rejected ALL shares — see the comment block in
    /// `validate_share`) would only be caught for the path that happens to have
    /// a fixture; the other would silently false-reject/accept on live hardware.
    /// This pins that both return the SAME verdict for the same (header, target)
    /// across the full accept/reject range. (gap-swarm no-HAL hunt finding #2)
    #[test]
    fn validate_share_and_validate_full_header_agree() {
        // Block #125552 (non-zero prev_hash exercises the word-reversal path).
        let header_hex = concat!(
            "01000000",
            "81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000",
            "e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b",
            "c7f5d74d",
            "f2b9441a",
            "42a14695",
        );
        let header_vec = hex::decode(header_hex).unwrap();
        let mut header = [0u8; 80];
        header.copy_from_slice(&header_vec);

        // Derive the validate_share inputs from the SAME header bytes so both
        // validators are fed one logical (header, target).
        let mut prefix = [0u8; 64];
        prefix.copy_from_slice(&header[0..64]);
        let midstate = compute_midstate_from_prefix(&prefix);
        let mut header_tail = [0u8; 12];
        header_tail.copy_from_slice(&header[64..76]);
        let nonce = u32::from_le_bytes([header[76], header[77], header[78], header[79]]);

        // The block's own hash (big-endian) — an exact-boundary target.
        let full_hash = double_sha256(&header);
        let mut hash_be = [0u8; 32];
        for i in 0..32 {
            hash_be[i] = full_hash[31 - i];
        }
        // One step TIGHTER than the exact hash → both validators must reject.
        let mut tighter = hash_be;
        for b in tighter.iter_mut() {
            if *b > 0 {
                *b -= 1;
                break;
            }
        }

        // Bracket the hash with hardest/easiest/realistic/exact/tighter targets
        // and assert the two validators return IDENTICAL verdicts for each.
        let targets: [[u8; 32]; 7] = [
            [0x00u8; 32], // hardest possible → both reject
            [0xFFu8; 32], // easiest possible → both accept
            difficulty_to_target(1.0),
            difficulty_to_target(1000.0),
            difficulty_to_target(244112.0), // ~ block #125552's real difficulty
            hash_be,                        // exact boundary: hash <= hash → both accept
            tighter,                        // just below the hash → both reject
        ];
        for (i, t) in targets.iter().enumerate() {
            let via_midstate = validate_share(&midstate, &header_tail, nonce, t);
            let via_full = validate_full_header(&header, t);
            assert_eq!(
                via_midstate, via_full,
                "validate_share ({}) and validate_full_header ({}) DISAGREE for target index {} \
                 — a byte-order/endianness regression in one share-validation path",
                via_midstate, via_full, i
            );
        }

        // Sanity (proves the bracket isn't vacuously all-equal): the exact-hash
        // target IS met by both, and the one-tighter target is met by neither.
        assert!(validate_full_header(&header, &hash_be));
        assert!(validate_share(&midstate, &header_tail, nonce, &hash_be));
        assert!(!validate_full_header(&header, &tighter));
        assert!(!validate_share(&midstate, &header_tail, nonce, &tighter));
    }

    /// Test that the Stratum nonce submission format is correct.
    ///
    /// When submitting a share, we format the nonce as value hex:
    ///   nonce 0x9546A142 → "9546a142"
    ///
    /// The pool parses this hex string, byte-swaps it to LE, and places
    /// it at header[76:80]. Verify this round-trips correctly.
    #[test]
    fn test_nonce_submission_format() {
        let nonce: u32 = 0x9546A142;

        // Our submission format (value hex)
        let nonce_hex = format!("{:08x}", nonce);
        assert_eq!(nonce_hex, "9546a142");

        // Pool's reconstruction: hex_decode → byte-swap → place in header
        // hex_decode("9546a142") = [0x95, 0x46, 0xa1, 0x42]
        // In the block header, nonce is LE: [0x42, 0xa1, 0x46, 0x95]
        // ckpool does hex2bin + flip_80 (byte-swap each 4-byte word).
        //
        // The pool's flip converts [95,46,a1,42] → [42,a1,46,95] = LE of 0x9546A142
        // This matches nonce.to_le_bytes() = [0x42, 0xa1, 0x46, 0x95]
        let nonce_le = nonce.to_le_bytes();
        let pool_decoded = hex::decode(&nonce_hex).unwrap();
        let pool_flipped: Vec<u8> = pool_decoded.iter().rev().cloned().collect();
        assert_eq!(
            &pool_flipped[..],
            &nonce_le[..],
            "Pool's flipped nonce must equal nonce.to_le_bytes()"
        );
    }

    /// Test that Stratum prev_hash word-reversal is required.
    ///
    /// The pool sends prev_hash with each 4-byte word byte-swapped relative
    /// to the block header's internal format. Both WorkBuilder::next_work
    /// and process_job must apply reverse_endianness_per_word before placing
    /// prev_hash in the header.
    ///
    /// This test verifies the transformation produces the correct internal
    /// byte order. (A prior version of process_job was missing this step;
    /// it has been fixed.)
    #[test]
    fn test_stratum_prev_hash_word_reversal() {
        // Stratum prev_hash for block #125552
        let stratum_prev_hash =
            hex::decode("ab02cd818b9e567ee21793cddef299feb29ad444a41b85b8000008a300000000")
                .unwrap();

        // Internal prev_hash (what should be in the header)
        let internal_prev_hash =
            hex::decode("81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000")
                .unwrap();

        // WorkBuilder::next_work applies reverse_endianness_per_word → correct
        let mut wb_prev_hash = [0u8; 32];
        wb_prev_hash.copy_from_slice(&stratum_prev_hash);
        reverse_endianness_per_word(&mut wb_prev_hash);
        assert_eq!(
            &wb_prev_hash[..],
            &internal_prev_hash[..],
            "WorkBuilder path: word-reversed Stratum prev_hash matches internal format"
        );

        // process_job uses prev_block_hash directly WITHOUT word reversal → WRONG
        // (This documents the latent bug — process_job is unused at runtime)
        assert_ne!(
            &stratum_prev_hash[..],
            &internal_prev_hash[..],
            "Raw Stratum prev_hash differs from internal format (word reversal needed)"
        );
    }

    // -----------------------------------------------------------------------
    // Boundary contracts: validate_full_header, increment_bitmask,
    // format_extranonce2, hex_decode, public-alias wrappers.
    //
    // These are the lowest-level helpers in the work pipeline. A silent
    // off-by-one in any of them silently mis-validates shares (false
    // accepts or false rejects) and the only visible symptom is "shares
    // get rejected at higher rate than expected" — operators have no
    // way to root-cause without these contracts pinned.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_full_header_accepts_when_hash_meets_target_exactly() {
        // The share-acceptance gate is `hash_be <= share_target`. Build
        // a header whose hash matches a known target; pin that the
        // boundary is inclusive (≤, not <).
        let header = [0u8; 80];
        let hash = double_sha256(&header);
        let mut hash_be = [0u8; 32];
        for i in 0..32 {
            hash_be[i] = hash[31 - i];
        }
        // Use the exact computed hash as the target.
        assert!(
            validate_full_header(&header, &hash_be),
            "hash exactly equal to target must accept (≤ semantics)"
        );
    }

    #[test]
    fn validate_full_header_accepts_easiest_target() {
        // All-FF target → accept any hash.
        let header = [0u8; 80];
        let target = [0xFFu8; 32];
        assert!(validate_full_header(&header, &target));
    }

    #[test]
    fn validate_full_header_rejects_against_zero_target() {
        // Target of all zeros means "infinite difficulty" — only a hash
        // of all zeros could meet it. Use any non-trivial header to
        // confirm rejection.
        let header = [0xABu8; 80];
        let target = [0u8; 32];
        assert!(
            !validate_full_header(&header, &target),
            "non-zero hash must NOT meet zero target"
        );
    }

    #[test]
    fn validate_full_header_distinguishes_hash_above_and_below_target() {
        // Compute a hash, then build targets that bracket it.
        let header = [0x55u8; 80];
        let hash = double_sha256(&header);
        let mut hash_be = [0u8; 32];
        for i in 0..32 {
            hash_be[i] = hash[31 - i];
        }

        // Construct a target that is one increment LOWER than hash_be
        // (hash should NOT meet it).
        let mut tighter_target = hash_be;
        for i in (0..32).rev() {
            if tighter_target[i] > 0 {
                tighter_target[i] -= 1;
                break;
            }
        }
        assert!(
            !validate_full_header(&header, &tighter_target),
            "hash must NOT meet a tighter target"
        );

        // A target one increment HIGHER than hash should accept.
        let mut looser_target = hash_be;
        for i in (0..32).rev() {
            if looser_target[i] < 0xFF {
                looser_target[i] += 1;
                break;
            }
        }
        assert!(
            validate_full_header(&header, &looser_target),
            "hash MUST meet a looser target"
        );
    }

    #[test]
    fn increment_bitmask_with_zero_mask_returns_value_unchanged() {
        // mask=0 means "no rolling allowed". The function must NOT
        // increment any bits when there's no mask.
        let v = 0x2000_4000_u32;
        assert_eq!(increment_bitmask(v, 0), v);
    }

    #[test]
    fn increment_bitmask_preserves_bits_outside_mask() {
        // The high byte (0x20) must be preserved; only the masked
        // bits (0x1fff_e000) get incremented.
        let mask = 0x1fff_e000_u32;
        let v0 = 0x2000_0000_u32;
        let v1 = increment_bitmask(v0, mask);
        // High byte stays 0x20, masked bits incremented to 0x00002000.
        assert_eq!(v1 & 0xE000_0000, 0x2000_0000, "high bits preserved");
        assert_eq!(v1 & mask, 0x0000_2000, "masked bits incremented");
    }

    #[test]
    fn increment_bitmask_wraps_at_full_mask_value() {
        // When all masked bits are set, incrementing must wrap back to 0
        // within the masked range while preserving outside bits.
        let mask = 0x1fff_e000_u32;
        let v_max = 0x2000_0000 | mask;
        let v_wrapped = increment_bitmask(v_max, mask);
        // Outside bits (0x2000_0000) preserved; masked bits back to 0.
        assert_eq!(v_wrapped & mask, 0);
        assert_eq!(v_wrapped & !mask, 0x2000_0000);
    }

    #[test]
    fn increment_bitmask_handles_non_contiguous_mask() {
        // BIP320 / BIP310 masks are typically contiguous in practice
        // (e.g. 0x1fffe000 is bits 13..29), but the algorithm should
        // also handle non-contiguous masks. Pin the behavior.
        let mask = 0x0F0F_0F0F_u32;
        let v0 = 0x0000_0000;
        let v1 = increment_bitmask(v0, mask);
        // First increment fills the lowest masked bit position.
        assert_eq!(v1, 0x0000_0001);
        let v2 = increment_bitmask(v1, mask);
        assert_eq!(v2, 0x0000_0002);
    }

    #[test]
    fn increment_bitmask_pub_matches_private() {
        // The public wrapper used by work_dispatcher must produce
        // identical output to the private function.
        let mask = 0x1fff_e000_u32;
        let v = 0x2000_2000_u32;
        assert_eq!(increment_bitmask_pub(v, mask), increment_bitmask(v, mask));
    }

    #[test]
    fn reverse_endianness_per_word_pub_matches_private() {
        let mut a = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut b = a;
        reverse_endianness_per_word(&mut a);
        reverse_endianness_per_word_pub(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn reverse_endianness_per_word_is_self_inverse() {
        // Reversing within each word twice must restore the original
        // byte order (idempotent / self-inverse property).
        let original: [u8; 32] = std::array::from_fn(|i| i as u8);
        let mut data = original;
        reverse_endianness_per_word(&mut data);
        reverse_endianness_per_word(&mut data);
        assert_eq!(data, original);
    }

    #[test]
    fn format_extranonce2_le_byte_layout_for_size_4() {
        // Counter = 0xDEADBEEF, size = 4.
        // LE bytes: [EF, BE, AD, DE] → hex "efbeadde".
        let hex = format_extranonce2(0xDEAD_BEEF, 4);
        assert_eq!(hex, "efbeadde");
    }

    #[test]
    fn format_extranonce2_zero_pads_when_counter_smaller_than_size() {
        // Counter=1, size=8 → [01, 00, 00, 00, 00, 00, 00, 00].
        let hex = format_extranonce2(1, 8);
        assert_eq!(hex, "0100000000000000");
    }

    #[test]
    fn format_extranonce2_clamps_size_at_max() {
        // Sizes > MAX_V1_EXTRANONCE2_SIZE silently clamp. Pin the cap
        // so a refactor that changes the constant doesn't silently
        // change the wire format.
        let hex = format_extranonce2(0, MAX_V1_EXTRANONCE2_SIZE + 100);
        assert_eq!(hex.len(), MAX_V1_EXTRANONCE2_SIZE * 2);
    }

    #[test]
    fn hex_decode_returns_empty_for_invalid_hex() {
        // Silent fallback: invalid hex produces empty Vec, NOT a panic.
        // Pin so a refactor that switched to .expect() is caught.
        let result = hex_decode("not-hex");
        assert!(result.is_empty());

        let truncated = hex_decode("a");
        assert!(
            truncated.is_empty(),
            "odd-length hex must produce empty Vec"
        );
    }

    #[test]
    fn hex_decode_round_trips_typical_coinbase_prefix() {
        // 8-char even-length hex must decode cleanly.
        let result = hex_decode("01000000");
        assert_eq!(result, vec![0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn compute_midstate_from_prefix_is_alias_of_compute_midstate() {
        // The public alias used by v1::job must produce identical output.
        let data = [0x42u8; 64];
        assert_eq!(compute_midstate_from_prefix(&data), compute_midstate(&data));
    }

    #[test]
    fn double_sha256_round_trips_against_reference() {
        // SHA256d("hello") known-answer: SHA256d == SHA256(SHA256("hello"))
        let result = double_sha256(b"hello");
        // First SHA256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        // Second SHA256(first) = 9595c9df90075148eb06860365df33584b75bff782a510c6cd4883a419833d50
        assert_eq!(
            hex::encode(result),
            "9595c9df90075148eb06860365df33584b75bff782a510c6cd4883a419833d50"
        );
    }
}
