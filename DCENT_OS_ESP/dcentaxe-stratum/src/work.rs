// DCENT_axe Mining Work Construction
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Converts Stratum V1 pool jobs into ASIC-ready mining work.
//
// Pipeline (per ESP-Miner / dcentrald reference):
//   1. Construct coinbase TX: coinbase1 + extranonce1 + extranonce2 + coinbase2
//   2. Double-SHA256 the coinbase -> coinbase_tx_hash
//   3. Compute merkle root by folding with merkle_branches
//   4. Assemble 80-byte block header prefix (first 64 bytes)
//   5. Compute SHA-256 midstate of first 64 bytes
//   6. Package ASIC job: midstate(s) + merkle4 + ntime + nbits

use log::warn;
use sha2::{Digest, Sha256};

use crate::types::{StratumJob, MAX_EXTRANONCE2_SIZE, PDIFF1_TARGET};

// ---------------------------------------------------------------------------
// MiningWork — the ASIC-ready work unit
// ---------------------------------------------------------------------------

/// ASIC-ready mining work unit.
///
/// Contains everything an ASIC chip needs to search for valid nonces.
/// For BM1366/BM1368/BM1370: full prev_block_hash and merkle_root are used.
/// For BM1397: midstates + merkle4 are used.
#[derive(Debug, Clone)]
pub struct MiningWork {
    /// Pre-computed SHA-256 midstate(s). One per version-rolled variant.
    /// midstates[0] = original version, midstates[1..3] = rolled versions.
    pub midstates: Vec<[u8; 32]>,

    /// Last 4 bytes of merkle root (bytes [28..31]).
    pub merkle4: [u8; 4],

    /// Block timestamp (from pool, as u32).
    pub ntime: u32,

    /// Compact difficulty target / nbits (from pool, as u32).
    pub nbits: u32,

    /// Block version (before rolling).
    pub version: u32,

    /// Version rolling mask (0 if no rolling).
    pub version_mask: u32,

    /// Full previous block hash (internal byte order, for BM1366+ ASICs).
    pub prev_block_hash: [u8; 32],

    /// Full merkle root (internal byte order, for BM1366+ ASICs).
    pub merkle_root: [u8; 32],

    /// Job ID from the pool (for share submission).
    pub job_id: String,

    /// The extranonce2 value used (hex string).
    pub extranonce2: String,

    /// Pool share target (32 bytes, big-endian).
    pub share_target: [u8; 32],
}

// ---------------------------------------------------------------------------
// WorkBuilder
// ---------------------------------------------------------------------------

/// Builds ASIC-ready work from Stratum pool job templates.
///
/// Maintains the extranonce2 counter and version rolling state.
pub struct WorkBuilder {
    /// Pool-assigned extranonce1 (raw bytes).
    extranonce1: Vec<u8>,

    /// Size of extranonce2 in bytes.
    extranonce2_size: usize,

    /// Incrementing extranonce2 counter.
    extranonce2_counter: u64,

    /// Negotiated version rolling mask. 0 = no rolling.
    version_mask: u32,

    /// Current pool difficulty.
    difficulty: f64,
}

impl WorkBuilder {
    pub fn new(extranonce1: &str, extranonce2_size: usize) -> Self {
        Self {
            extranonce1: hex::decode(extranonce1).unwrap_or_default(),
            extranonce2_size: extranonce2_size.clamp(1, MAX_EXTRANONCE2_SIZE),
            extranonce2_counter: 0,
            version_mask: 0,
            difficulty: 1.0,
        }
    }

    /// Update extranonce1 (e.g., after mining.set_extranonce).
    pub fn set_extranonce(&mut self, extranonce1: &str, extranonce2_size: usize) {
        self.extranonce1 = hex::decode(extranonce1).unwrap_or_default();
        self.extranonce2_size = extranonce2_size.clamp(1, MAX_EXTRANONCE2_SIZE);
    }

    /// Set the version rolling mask after BIP 310 negotiation.
    pub fn set_version_mask(&mut self, mask: u32) {
        self.version_mask = mask;
    }

    /// Set the current pool difficulty.
    pub fn set_difficulty(&mut self, difficulty: f64) {
        self.difficulty = difficulty;
    }

    /// Reset the extranonce2 counter (called on clean_jobs).
    pub fn reset_extranonce2(&mut self) {
        self.extranonce2_counter = 0;
    }

    /// Generate the next mining work unit from a Stratum job.
    ///
    /// Each call increments extranonce2, producing a unique coinbase
    /// and therefore a unique merkle root and work unit.
    pub fn next_work(&mut self, job: &StratumJob) -> MiningWork {
        let extranonce2 = self.extranonce2_counter;
        self.extranonce2_counter += 1;

        // Cap extranonce2 to max value for the configured byte size
        let max_en2 = match self.extranonce2_size {
            1 => 0xFF_u64,
            2 => 0xFFFF,
            3 => 0xFFFFFF,
            4 => 0xFFFFFFFF,
            _ => u64::MAX,
        };
        if self.extranonce2_counter > max_en2 {
            warn!(
                "WorkBuilder: extranonce2 counter wrapped (size={}), resetting",
                self.extranonce2_size
            );
            self.extranonce2_counter = 0;
        }

        // Format extranonce2 as hex string (little-endian, zero-padded)
        let extranonce2_hex = format_extranonce2(extranonce2, self.extranonce2_size);
        let extranonce2_bytes = hex::decode(&extranonce2_hex).unwrap_or_default();

        // Step 1: Build coinbase transaction
        let coinbase1_bytes = hex::decode(&job.coinbase1).unwrap_or_default();
        let coinbase2_bytes = hex::decode(&job.coinbase2).unwrap_or_default();
        let coinbase_tx = build_coinbase(
            &coinbase1_bytes,
            &self.extranonce1,
            &extranonce2_bytes,
            &coinbase2_bytes,
        );

        // Step 2: Double-SHA256 the coinbase
        let coinbase_hash = double_sha256(&coinbase_tx);

        // Step 3: Compute merkle root
        let merkle_branches = parse_merkle_branches(&job.merkle_branches);
        let merkle_root = compute_merkle_root(&coinbase_hash, &merkle_branches);

        // Step 4: Parse version, ntime, nbits from hex strings
        let version = u32::from_str_radix(&job.version, 16).unwrap_or(0x20000000);
        let ntime = u32::from_str_radix(&job.ntime, 16).unwrap_or(0);
        let nbits = u32::from_str_radix(&job.nbits, 16).unwrap_or(0);

        // Step 5: Build previous block hash (word-reversed from pool format)
        let mut prev_hash = [0u8; 32];
        if let Ok(bytes) = hex::decode(&job.prev_hash) {
            if bytes.len() == 32 {
                prev_hash.copy_from_slice(&bytes);
            }
        }
        // Pool sends prev_hash with each 4-byte word byte-swapped relative
        // to the block header's internal format. Reverse per word.
        reverse_endianness_per_word(&mut prev_hash);

        // Step 6: Assemble header prefix (first 64 bytes for midstate)
        let mut header_prefix = [0u8; 64];
        // Version (4 bytes, LE)
        header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
        // Previous block hash (32 bytes, now in internal byte order)
        header_prefix[4..36].copy_from_slice(&prev_hash);
        // First 28 bytes of merkle root
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        // Step 7: Compute midstate(s)
        let mut midstates = Vec::with_capacity(4);

        // Midstate 0: original version
        midstates.push(compute_midstate(&header_prefix));

        // If version rolling is active, compute additional midstates
        if self.version_mask != 0 {
            let mut rolled_version = version;
            for _ in 0..3 {
                rolled_version = increment_bitmask(rolled_version, self.version_mask);
                header_prefix[0..4].copy_from_slice(&rolled_version.to_le_bytes());
                midstates.push(compute_midstate(&header_prefix));
            }
        }

        // Step 8: Extract merkle4 (last 4 bytes of merkle root)
        let mut merkle4 = [0u8; 4];
        merkle4.copy_from_slice(&merkle_root[28..32]);

        // Step 9: Compute share target from difficulty
        let share_target = difficulty_to_target(self.difficulty);

        MiningWork {
            midstates,
            merkle4,
            ntime,
            nbits,
            version,
            version_mask: self.version_mask,
            prev_block_hash: prev_hash,
            merkle_root,
            job_id: job.job_id.clone(),
            extranonce2: extranonce2_hex,
            share_target,
        }
    }

    /// Decode the outputs of the coinbase transaction the next call to
    /// [`Self::next_work`] would produce, **without** advancing the
    /// extranonce2 counter. Used by the dashboard reward-split verifier.
    ///
    /// `extranonce2_override` lets the caller pin a deterministic en2 value
    /// (typically 0). The OUTPUTS of a coinbase don't depend on the
    /// extranonce — only the input scriptsig does — so any value yields the
    /// same `outputs` and `total_value_sats`. The `scriptsig_hex` field will
    /// reflect whatever en2 was used.
    pub fn decode_coinbase(
        &self,
        job: &StratumJob,
        extranonce2_override: u64,
    ) -> Option<CoinbaseDecoded> {
        let extranonce2_hex = format_extranonce2(extranonce2_override, self.extranonce2_size);
        let extranonce2_bytes = hex::decode(&extranonce2_hex).ok()?;
        let coinbase1_bytes = hex::decode(&job.coinbase1).ok()?;
        let coinbase2_bytes = hex::decode(&job.coinbase2).ok()?;
        let coinbase_tx = build_coinbase(
            &coinbase1_bytes,
            &self.extranonce1,
            &extranonce2_bytes,
            &coinbase2_bytes,
        );
        parse_coinbase(&coinbase_tx)
    }
}

// ---------------------------------------------------------------------------
// Coinbase TX Decoder
// ---------------------------------------------------------------------------

/// One output of a parsed coinbase transaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoinbaseOutput {
    /// Output value in satoshis.
    pub value_sats: u64,
    /// Hex-encoded scriptpubkey bytes (no `0x` prefix).
    pub script_hex: String,
}

/// Result of parsing a full coinbase transaction.
#[derive(Debug, Clone)]
pub struct CoinbaseDecoded {
    /// All outputs in declaration order.
    pub outputs: Vec<CoinbaseOutput>,
    /// Sum of `value_sats` across all outputs.
    pub total_value_sats: u64,
    /// Hex-encoded scriptsig of the (single) coinbase input — contains the
    /// BIP34 block height, extranonce, and pool's miner ID payload.
    pub scriptsig_hex: String,
}

/// Read a Bitcoin VarInt at `*pos`, advancing the cursor on success.
///
/// Returns `None` on truncation or any malformed input.
fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let first = *data.get(*pos)?;
    *pos += 1;
    match first {
        0xff => {
            let end = pos.checked_add(8)?;
            let bytes = data.get(*pos..end)?;
            *pos = end;
            Some(u64::from_le_bytes(bytes.try_into().ok()?))
        }
        0xfe => {
            let end = pos.checked_add(4)?;
            let bytes = data.get(*pos..end)?;
            *pos = end;
            Some(u32::from_le_bytes(bytes.try_into().ok()?) as u64)
        }
        0xfd => {
            let end = pos.checked_add(2)?;
            let bytes = data.get(*pos..end)?;
            *pos = end;
            Some(u16::from_le_bytes(bytes.try_into().ok()?) as u64)
        }
        n => Some(n as u64),
    }
}

/// Read exactly `len` bytes at `*pos`, advancing the cursor on success.
fn read_slice<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(len)?;
    let s = data.get(*pos..end)?;
    *pos = end;
    Some(s)
}

/// Parse a serialized Bitcoin coinbase transaction.
///
/// Handles both legacy and segwit-marker formats. Coinbase always has a
/// single input. Returns `None` on any parse failure (malformed VarInts,
/// truncation, illegal field counts) — never panics.
///
/// Layout:
/// ```text
///   version       u32 LE
///   [opt segwit:  0x00 marker + 0x01 flag]
///   in_count      VarInt        (must be 1 for coinbase)
///   prev_txid     [u8; 32]      (all zero for coinbase)
///   prev_vout     u32 LE        (0xffffffff for coinbase)
///   ssig_len      VarInt
///   scriptsig     [u8; ssig_len]
///   sequence      u32 LE
///   out_count     VarInt
///   per output:
///     value       u64 LE
///     spk_len     VarInt
///     scriptpubkey[u8; spk_len]
///   [opt witness data per input]
///   locktime      u32 LE
/// ```
pub fn parse_coinbase(tx: &[u8]) -> Option<CoinbaseDecoded> {
    let mut pos: usize = 0;

    // version (4 bytes LE) — value not used downstream
    let _version = read_slice(tx, &mut pos, 4)?;

    // segwit detect: 0x00 marker + 0x01 flag immediately after version
    if tx.get(pos).copied() == Some(0x00) && tx.get(pos + 1).copied() == Some(0x01) {
        pos += 2;
    }

    // input count — coinbase always has exactly one input
    let in_count = read_varint(tx, &mut pos)?;
    if in_count != 1 {
        return None;
    }

    // input: 32 prev_txid + 4 vout + VarInt ssig_len + ssig + 4 sequence
    let _prev_txid = read_slice(tx, &mut pos, 32)?;
    let _prev_vout = read_slice(tx, &mut pos, 4)?;
    let ssig_len = read_varint(tx, &mut pos)?;
    // Phase L1: Bitcoin consensus bounds coinbase scriptsig to 100 bytes.
    // Tightened from 10_000 → 128 (with margin for legitimate pool ID bytes).
    // The previous 10_000-byte ceiling allowed 20 KB hex Strings that never
    // occur in real coinbases — they were just a fragmentation source under
    // dispatcher.rs's `s.scriptsig_hex = d.scriptsig_hex` assign-and-drop
    // cycle every `mining.notify`. 128 raw → 256 hex chars max.
    if ssig_len > 128 {
        return None;
    }
    let scriptsig = read_slice(tx, &mut pos, ssig_len as usize)?;
    let scriptsig_hex = hex::encode(scriptsig);
    let _sequence = read_slice(tx, &mut pos, 4)?;

    // output count — real-world coinbases have 1-3 outputs (subsidy, OP_RETURN,
    // optional 2nd payout). Cap at 10 to bound httpd-task allocation cost on
    // heap-fragmented ESP32-S3 and to refuse hostile pools that would balloon
    // the JSON tree we serialize on every /api/system/info poll.
    let out_count = read_varint(tx, &mut pos)?;
    if out_count == 0 || out_count > 10 {
        return None;
    }

    let mut outputs: Vec<CoinbaseOutput> = Vec::with_capacity(out_count as usize);
    let mut total: u64 = 0;
    for _ in 0..out_count {
        let value_bytes = read_slice(tx, &mut pos, 8)?;
        let value_sats = u64::from_le_bytes(value_bytes.try_into().ok()?);
        let spk_len = read_varint(tx, &mut pos)?;
        // Standard scriptPubKeys (P2PKH/P2SH/P2WPKH/P2WSH/P2TR/OP_RETURN)
        // top out around a few hundred bytes. 5 KB still leaves room for
        // pathological multisig but rejects clear attempts to bloat the tree.
        if spk_len > 5_000 {
            return None;
        }
        let spk = read_slice(tx, &mut pos, spk_len as usize)?;
        total = total.checked_add(value_sats)?;
        outputs.push(CoinbaseOutput {
            value_sats,
            script_hex: hex::encode(spk),
        });
    }

    // Sanity cap: total reward across all outputs cannot exceed the absolute
    // 21M BTC supply. Catches u64 mischief and any pool feeding nonsense.
    const MAX_TOTAL_SATS: u64 = 21_000_000_u64 * 100_000_000_u64;
    if total > MAX_TOTAL_SATS {
        return None;
    }

    // Witness data + locktime intentionally not validated — we already have
    // everything the dashboard needs.
    Some(CoinbaseDecoded {
        outputs,
        total_value_sats: total,
        scriptsig_hex,
    })
}

// ---------------------------------------------------------------------------
// Core Crypto Functions
// ---------------------------------------------------------------------------

/// Double-SHA256: SHA256(SHA256(data)).
pub fn double_sha256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(&first);
    let mut result = [0u8; 32];
    result.copy_from_slice(&second);
    result
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

/// Parse merkle branches from hex strings to byte arrays.
fn parse_merkle_branches(branches_hex: &[String]) -> Vec<[u8; 32]> {
    branches_hex
        .iter()
        .filter_map(|hex_str| {
            let bytes = hex::decode(hex_str).ok()?;
            if bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Some(arr)
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// SHA-256 Midstate Computation
// ---------------------------------------------------------------------------

/// SHA-256 round constants.
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
/// Takes the current hash state [u32; 8] and a 64-byte message block,
/// returns the new state after compression.
pub fn sha256_compress(state: &[u32; 8], block: &[u8; 64]) -> [u32; 8] {
    // Message schedule
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

    // Compression
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

/// Compute SHA-256 midstate: the internal state after processing one 512-bit block.
///
/// This is NOT a full SHA-256 hash. It returns the eight 32-bit chaining variables
/// (H0..H7) after processing exactly the first 64 bytes of the block header.
///
/// The ASIC uses this to continue hashing with the remaining 16 bytes
/// (merkle4 + ntime + nbits + nonce) plus SHA-256 padding.
pub fn compute_midstate(data: &[u8; 64]) -> [u8; 32] {
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

/// Compute multiple midstates for version rolling.
///
/// Returns up to `count` midstates, each with a different version
/// derived by incrementing the version mask.
pub fn compute_multiple_midstates(
    header_prefix: &mut [u8; 64],
    version: u32,
    version_mask: u32,
    count: usize,
) -> Vec<[u8; 32]> {
    let mut midstates = Vec::with_capacity(count);

    // Midstate 0: original version
    header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
    midstates.push(compute_midstate(header_prefix));

    // Additional midstates with rolled versions
    let mut rolled = version;
    for _ in 1..count {
        rolled = increment_bitmask(rolled, version_mask);
        header_prefix[0..4].copy_from_slice(&rolled.to_le_bytes());
        midstates.push(compute_midstate(header_prefix));
    }

    midstates
}

// ---------------------------------------------------------------------------
// Share Validation
// ---------------------------------------------------------------------------

/// Validate a full 80-byte block header by double-SHA256 and comparing
/// against the pool's share target. This is the same approach as ESP-Miner's
/// test_nonce_value() — no midstate shortcut, just hash the full header.
///
/// Returns the achieved difficulty if valid (>= pool difficulty), or 0.0 if invalid.
pub fn validate_full_header(header: &[u8; 80], share_target: &[u8; 32]) -> f64 {
    let (difficulty, meets_target) = full_header_difficulty_and_target(header, share_target);
    if meets_target {
        difficulty
    } else {
        0.0
    }
}

/// Compute achieved difficulty and target match for a full 80-byte block
/// header with a single SHA256d pass.
///
/// This is the hot dispatcher validation path. It keeps the same comparison
/// semantics as [`validate_full_header`] while also returning sub-target
/// difficulty for TicketMask/HW-error classification.
pub fn full_header_difficulty_and_target(
    header: &[u8; 80],
    share_target: &[u8; 32],
) -> (f64, bool) {
    let hash = double_sha256(header);

    // Bitcoin: hash is little-endian 256-bit integer. Convert to big-endian for comparison.
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }

    let difficulty = hash_to_difficulty(&hash_be);
    let meets_target = hash_be.as_slice() <= share_target.as_slice();
    (difficulty, meets_target)
}

/// Validate a nonce by computing the full SHA-256d block hash and comparing
/// against the pool's share target.
///
/// This is the software validation gate that filters ASIC nonces before
/// submitting to the pool. The ASIC hardware uses TicketMask to filter
/// at a lower difficulty -- this function checks the pool's higher difficulty.
///
/// Returns the achieved difficulty if valid (>= pool difficulty), or 0.0 if invalid.
pub fn validate_share(
    midstate: &[u8; 32],
    header_tail: &[u8; 12],
    nonce: u32,
    share_target: &[u8; 32],
) -> f64 {
    // Build the second 64-byte SHA-256 block:
    // [merkle4(4) + ntime(4) + nbits(4) + nonce(4 LE) + 0x80 + zeros(39) + length(8 BE)]
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

    // First SHA-256: compress second block with midstate
    let inner_state = sha256_compress(&state, &block2);

    // Serialize inner hash to 32 bytes (big-endian)
    let mut inner_hash = [0u8; 32];
    for i in 0..8 {
        inner_hash[i * 4..(i + 1) * 4].copy_from_slice(&inner_state[i].to_be_bytes());
    }

    // Second SHA-256: SHA256(inner_hash)
    let final_hash_digest = Sha256::digest(&inner_hash);

    // Bitcoin convention: SHA-256d output is treated as a LITTLE-ENDIAN 256-bit
    // integer. Byte[0] of SHA-256 output = LSB. Proof-of-work zeros appear at
    // the END of the raw SHA-256 bytes (the MSB/high bytes).
    //
    // share_target is BIG-ENDIAN (byte[0] = MSB, zeros at the front).
    // Reverse the hash to match before comparing.
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = final_hash_digest[31 - i];
    }

    // Check if hash meets target
    if hash_be.as_slice() <= share_target.as_slice() {
        // Compute achieved difficulty: pdiff_1 / hash_value
        hash_to_difficulty(&hash_be)
    } else {
        0.0
    }
}

/// Compute the difficulty achieved by a full 80-byte block header.
/// Returns the difficulty (always > 0) regardless of any target.
/// Used to distinguish genuine HW errors from sub-pool-diff nonces.
pub fn header_difficulty(header: &[u8; 80]) -> f64 {
    let (difficulty, _) = full_header_difficulty_and_target(header, &[0u8; 32]);
    difficulty
}

/// Convert a 256-bit hash (big-endian) to its approximate pool difficulty.
///
/// difficulty = pdiff_1_target / hash_value = (2^224 - 1) / hash_value
fn hash_to_difficulty(hash: &[u8; 32]) -> f64 {
    let leading_zeros = hash.iter().take_while(|&&b| b == 0).count();
    if leading_zeros >= 32 {
        return f64::INFINITY;
    }

    let mut hash_top: u64 = 0;
    let bytes_to_read = 8.min(32 - leading_zeros);
    for i in 0..bytes_to_read {
        hash_top = (hash_top << 8) | hash[leading_zeros + i] as u64;
    }

    if bytes_to_read < 8 {
        hash_top <<= (8 - bytes_to_read) * 8;
    }

    let hash_shift = (32 - leading_zeros as i32 - 8) * 8;
    let hash_f64 = (hash_top as f64) * (2.0_f64).powi(hash_shift);

    if hash_f64 == 0.0 {
        return f64::INFINITY;
    }

    (2.0_f64).powi(224) / hash_f64
}

// ---------------------------------------------------------------------------
// Difficulty / Target Conversion
// ---------------------------------------------------------------------------

/// Convert pool difficulty (pdiff) to a 256-bit share target.
///
/// target = pdiff_1_target / difficulty = (2^224 - 1) / difficulty
///
/// Returns a 32-byte big-endian target.
pub fn difficulty_to_target(difficulty: f64) -> [u8; 32] {
    // Fail CLOSED on a non-positive difficulty: return the all-zero target
    // (reject every hash) rather than the all-0xFF target (the LOOSEST target,
    // which every garbage hash "meets" → share-flood / pool ban). A non-positive
    // difficulty is never legitimate here — the mining dispatcher floors
    // mining.set_difficulty to 1.0 (dispatcher.rs) BEFORE this function is
    // reached on the live path. This now agrees with the non-finite branch below
    // (which also catches NaN, since `NaN <= 0.0` is false): both fail reject-all.
    if difficulty <= 0.0 {
        return [0u8; 32];
    }

    if !difficulty.is_finite() {
        return [0u8; 32];
    }

    // DECISION (STRATUM-2): sub-1.0 pool difficulty is intentionally
    // unsupported by DCENT_axe. SHA-256 ASIC pools never set diff<1 for a
    // BitAxe, and the mining dispatcher floors mining.set_difficulty to 1.0
    // (dispatcher.rs DifficultyChanged handler) BEFORE this function is reached
    // on the live path. This branch is therefore the diff==1 path; collapsing
    // any 0<diff<1 to the diff-1 target is a deliberate, SAFE over-tightening
    // (never a silent share-loss bug on the live path, because sub-1 diff never
    // arrives here). Do not add a "loosening" branch for 0<diff<1 in isolation:
    // it would create a latent inconsistency that the dispatcher floor shadows.
    // If sub-1 diff support is ever genuinely needed, remove the dispatcher
    // floor in the SAME change and update test_difficulty_to_target_sub1.
    if (difficulty - 1.0).abs() < f64::EPSILON || difficulty < 1.0 {
        return PDIFF1_TARGET;
    }

    if difficulty.fract() == 0.0 && difficulty <= u64::MAX as f64 {
        return divide_diff1_target_by_u64(difficulty as u64);
    }

    let mut target = [0u8; 32];

    // Use IEEE 754 double-precision to compute target = 2^224 / difficulty
    let value_f64 = (2.0_f64).powi(224) / difficulty;

    let bits = value_f64.to_bits();
    let ieee_exp = ((bits >> 52) & 0x7FF) as i32 - 1023;
    let ieee_mantissa = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;

    let lsb_bit_pos = ieee_exp - 52;

    if lsb_bit_pos < -7 {
        return [0u8; 32];
    }

    let byte_offset = if lsb_bit_pos >= 0 {
        lsb_bit_pos / 8
    } else {
        (lsb_bit_pos - 7) / 8
    };
    let bit_shift = (lsb_bit_pos - byte_offset * 8) as u32;

    for i in 0..8 {
        let src_byte = ((ieee_mantissa >> (i * 8)) & 0xFF) as u8;
        let target_byte_idx = 31i32 - (byte_offset + i as i32);

        if target_byte_idx >= 0 && target_byte_idx < 32 {
            let shifted_lo = (src_byte as u16) << bit_shift;
            target[target_byte_idx as usize] |= (shifted_lo & 0xFF) as u8;

            if bit_shift > 0 {
                let carry = (shifted_lo >> 8) as u8;
                if carry != 0 && target_byte_idx > 0 {
                    target[(target_byte_idx - 1) as usize] |= carry;
                }
            }
        }
    }

    target
}

fn divide_diff1_target_by_u64(divisor: u64) -> [u8; 32] {
    if divisor == 0 {
        return [0xFF; 32];
    }

    let mut target = [0u8; 32];
    let mut remainder = 0u128;
    let divisor = divisor as u128;

    for (i, byte) in PDIFF1_TARGET.iter().enumerate() {
        let value = (remainder << 8) | (*byte as u128);
        target[i] = (value / divisor) as u8;
        remainder = value % divisor;
    }

    target
}

// ---------------------------------------------------------------------------
// Utility Functions
// ---------------------------------------------------------------------------

/// Reverse endianness within each 4-byte word of a 32-byte array.
///
/// Required for converting prev_block_hash from Stratum pool byte order
/// to block header internal byte order.
fn reverse_endianness_per_word(data: &mut [u8; 32]) {
    for chunk in data.chunks_exact_mut(4) {
        chunk.reverse();
    }
}

/// Increment only the bits within the mask, with carry propagation.
///
/// Used for version rolling (BIP 310 / ASICBoost).
pub fn increment_bitmask(value: u32, mask: u32) -> u32 {
    if mask == 0 {
        return value;
    }
    let carry = (value | !mask).wrapping_add(1) & mask;
    (value & !mask) | carry
}

/// Format a u64 counter as a hex string of the required byte length.
///
/// extranonce2 is transmitted as a hex string with exactly
/// byte_count * 2 hex characters (zero-padded, little-endian).
fn format_extranonce2(counter: u64, byte_count: usize) -> String {
    let byte_count = byte_count.clamp(1, MAX_EXTRANONCE2_SIZE);
    let le_bytes = counter.to_le_bytes();
    let copy_len = byte_count.min(8);
    let mut buf = vec![0u8; byte_count];
    buf[..copy_len].copy_from_slice(&le_bytes[..copy_len]);
    hex::encode(buf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        assert_eq!(format_extranonce2(0, 4), "00000000");
        assert_eq!(format_extranonce2(1, 4), "01000000");
        assert_eq!(format_extranonce2(256, 4), "00010000");
        assert_eq!(
            format_extranonce2(1, MAX_EXTRANONCE2_SIZE + 1024),
            "0100000000000000"
        );
    }

    #[test]
    fn test_increment_bitmask() {
        let mask = 0x1fffe000_u32;
        let v0 = 0x20000000_u32;
        let v1 = increment_bitmask(v0, mask);
        assert_eq!(v1, 0x20002000);
        let v2 = increment_bitmask(v1, mask);
        assert_eq!(v2, 0x20004000);
    }

    #[test]
    fn test_merkle_root_no_branches() {
        let hash = [0x42u8; 32];
        let result = compute_merkle_root(&hash, &[]);
        assert_eq!(result, hash);
    }

    #[test]
    fn test_midstate_deterministic() {
        let data = [0u8; 64];
        let ms1 = compute_midstate(&data);
        let ms2 = compute_midstate(&data);
        assert_eq!(ms1, ms2);
        assert_ne!(ms1, [0u8; 32]);
    }

    #[test]
    fn test_difficulty_to_target_diff1() {
        let target = difficulty_to_target(1.0);
        assert_eq!(target[0], 0);
        assert_eq!(target[1], 0);
        assert_eq!(target[2], 0);
        assert_eq!(target[3], 0);
        assert!(target[4] > 0);
    }

    #[test]
    fn test_difficulty_to_target_sub1() {
        // STRATUM-2 documented decision: 0<diff<1 collapses to the diff-1
        // target (deliberate safe over-tightening; sub-1 diff never reaches
        // this function on the live path because the dispatcher floors to 1.0).
        // This test pins the decision so a future "loosening branch" edit must
        // consciously update it.
        assert_eq!(difficulty_to_target(0.5), PDIFF1_TARGET);
        assert_eq!(difficulty_to_target(0.01), PDIFF1_TARGET);
    }

    #[test]
    fn test_difficulty_to_target_diff256() {
        let target = difficulty_to_target(256.0);
        assert_eq!(target[0], 0x00);
        assert_eq!(target[1], 0x00);
        assert_eq!(target[2], 0x00);
        assert_eq!(target[3], 0x00);
        assert_eq!(target[4], 0x00);
        assert!(
            target[5] >= 0xFE,
            "target[5] = 0x{:02X}, expected ~0xFF",
            target[5]
        );
    }

    #[test]
    fn test_difficulty_to_target_nonpositive_fails_closed() {
        // M-strat: a non-positive difficulty must yield the all-zero target
        // (reject-all / fail-CLOSED), NOT the all-0xFF loosest target which would
        // flood the pool with garbage. This matches the non-finite branch.
        assert_eq!(
            difficulty_to_target(0.0),
            [0u8; 32],
            "diff=0.0 must fail closed (reject-all)"
        );
        assert_eq!(
            difficulty_to_target(-5.0),
            [0u8; 32],
            "diff=-5.0 must fail closed (reject-all)"
        );
        // Sanity: a normal positive difficulty is unchanged (not [0;32]).
        let normal = difficulty_to_target(1.0);
        assert_eq!(normal, PDIFF1_TARGET);
        assert_ne!(normal, [0u8; 32]);
    }

    /// Known-good Bitcoin block test: Genesis Block (Block #0).
    ///
    /// Tests the complete SHA-256d mining pipeline:
    ///   1. double_sha256() on full 80-byte header matches known block hash
    ///   2. compute_midstate() on first 64 header bytes
    ///   3. validate_share() with the known nonce returns non-zero difficulty
    #[test]
    fn test_validate_share_genesis_block() {
        // Genesis block header (80 bytes, raw block format)
        let header = hex::decode(concat!(
            "01000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            "29ab5f49",
            "ffff001d",
            "1dac2b7c",
        ))
        .unwrap();
        assert_eq!(header.len(), 80);

        // Verify SHA-256d matches known genesis hash
        let full_hash = double_sha256(&header);
        let expected =
            hex::decode("6fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000")
                .unwrap();
        assert_eq!(hex::encode(&full_hash), hex::encode(&expected));

        // Compute midstate from first 64 bytes
        let mut prefix = [0u8; 64];
        prefix.copy_from_slice(&header[0..64]);
        let midstate = compute_midstate(&prefix);
        assert_ne!(midstate, [0u8; 32]);

        // Build header_tail and nonce
        let mut header_tail = [0u8; 12];
        header_tail.copy_from_slice(&header[64..76]);
        let nonce = u32::from_le_bytes([header[76], header[77], header[78], header[79]]);
        assert_eq!(nonce, 0x7C2BAC1D);

        // validate_share at difficulty 1 should return non-zero difficulty
        let target = difficulty_to_target(1.0);
        let diff = validate_share(&midstate, &header_tail, nonce, &target);
        assert!(diff > 0.0, "validate_share must pass for genesis block");
    }

    #[test]
    fn test_full_header_validation_single_hash_helper_matches_legacy_helpers() {
        let header_vec = hex::decode(concat!(
            "01000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            "29ab5f49",
            "ffff001d",
            "1dac2b7c",
        ))
        .unwrap();
        let mut header = [0u8; 80];
        header.copy_from_slice(&header_vec);

        let target = difficulty_to_target(1.0);
        let legacy_diff = header_difficulty(&header);
        let legacy_valid = validate_full_header(&header, &target);
        let (combined_diff, combined_meets_target) =
            full_header_difficulty_and_target(&header, &target);

        assert_eq!(combined_diff, legacy_diff);
        assert!(combined_meets_target);
        assert_eq!(legacy_valid, combined_diff);

        let impossible_target = [0u8; 32];
        let (combined_diff, combined_meets_target) =
            full_header_difficulty_and_target(&header, &impossible_target);
        assert_eq!(combined_diff, legacy_diff);
        assert!(!combined_meets_target);
        assert_eq!(validate_full_header(&header, &impossible_target), 0.0);
    }

    /// Full end-to-end test using Bitcoin Block #125552.
    ///
    /// This block has a NON-ZERO prev_hash, which exercises the critical
    /// reverse_endianness_per_word byte-order transformation.
    #[test]
    fn test_block_125552_full_pipeline() {
        let header_hex = concat!(
            "01000000",
            "81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000",
            "e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b",
            "c7f5d74d",
            "f2b9441a",
            "42a14695",
        );
        let header = hex::decode(header_hex).unwrap();
        assert_eq!(header.len(), 80);

        let full_hash = double_sha256(&header);
        assert_eq!(
            hex::encode(&full_hash),
            "1dbd981fe6985776b644b173a4d0385ddc1aa2a829688d1e0000000000000000",
        );

        // Stratum prev_hash word-reversal test
        let stratum_prev_hash_hex =
            "ab02cd818b9e567ee21793cddef299feb29ad444a41b85b8000008a300000000";
        let mut prev_hash_bytes = [0u8; 32];
        prev_hash_bytes.copy_from_slice(&hex::decode(stratum_prev_hash_hex).unwrap());
        reverse_endianness_per_word(&mut prev_hash_bytes);
        assert_eq!(
            hex::encode(&prev_hash_bytes),
            "81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000",
        );

        // Simulate WorkBuilder pipeline
        let version: u32 = 1;
        let ntime: u32 = 0x4dd7f5c7;
        let nbits: u32 = 0x1a44b9f2;
        let merkle_root =
            hex::decode("e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b")
                .unwrap();

        let mut header_prefix = [0u8; 64];
        header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
        header_prefix[4..36].copy_from_slice(&prev_hash_bytes);
        header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

        assert_eq!(&header_prefix[..], &header[0..64]);

        let midstate = compute_midstate(&header_prefix);

        let mut header_tail = [0u8; 12];
        header_tail[0..4].copy_from_slice(&merkle_root[28..32]);
        header_tail[4..8].copy_from_slice(&ntime.to_le_bytes());
        header_tail[8..12].copy_from_slice(&nbits.to_le_bytes());

        let nonce: u32 = 0x9546A142;
        let target = difficulty_to_target(1.0);
        let diff = validate_share(&midstate, &header_tail, nonce, &target);
        assert!(
            diff > 0.0,
            "validate_share must pass for block #125552 at difficulty 1"
        );
        assert!(
            diff > 100_000.0,
            "block #125552 should have high difficulty"
        );
    }

    #[test]
    fn test_parse_coinbase_legacy_minimal() {
        // Hand-crafted legacy coinbase: version=1, 1 input (zero prevout, ssig=03ABCDEF,
        // seq=ffffffff), 1 output (5000000000 sats = 50 BTC, scriptpubkey=51 OP_TRUE),
        // locktime=0.
        let tx_hex = concat!(
            "01000000",                                                         // version
            "01",                                                               // in_count = 1
            "0000000000000000000000000000000000000000000000000000000000000000", // prev_txid
            "ffffffff",                                                         // prev_vout
            "03",                                                               // ssig_len = 3
            "abcdef",                                                           // scriptsig
            "ffffffff",                                                         // sequence
            "01",                                                               // out_count = 1
            "00f2052a01000000",                                                 // 5_000_000_000 LE
            "01",                                                               // spk_len
            "51",                                                               // OP_TRUE
            "00000000",                                                         // locktime
        );
        let tx = hex::decode(tx_hex).unwrap();
        let decoded = parse_coinbase(&tx).expect("must parse");
        assert_eq!(decoded.outputs.len(), 1);
        assert_eq!(decoded.outputs[0].value_sats, 5_000_000_000);
        assert_eq!(decoded.outputs[0].script_hex, "51");
        assert_eq!(decoded.total_value_sats, 5_000_000_000);
        assert_eq!(decoded.scriptsig_hex, "abcdef");
    }

    #[test]
    fn test_parse_coinbase_segwit_two_outputs() {
        // Segwit coinbase: marker(00) flag(01), 2 outputs (subsidy + witness commitment).
        // Witness data + locktime are intentionally not validated by the parser.
        let tx_hex = concat!(
            "02000000",                                                         // version=2
            "0001",                                                             // segwit
            "01",                                                               // in_count
            "0000000000000000000000000000000000000000000000000000000000000000", // prev_txid
            "ffffffff",                                                         // prev_vout
            "04",                                                               // ssig_len
            "deadbeef",                                                         // scriptsig
            "ffffffff",                                                         // sequence
            "02",                                                               // 2 outputs
            // out 0: subsidy 6.25 BTC = 625_000_000 sats, P2WPKH 22-byte spk
            "40be402500000000",
            "16",
            "0014abcdef0123456789abcdef0123456789abcdef01",
            // out 1: 0 sats witness commitment, OP_RETURN-ish 38 bytes
            "0000000000000000",
            "26",
            "6a24aa21a9ed0000000000000000000000000000000000000000000000000000000000000000",
            // (witness + locktime omitted — not parsed)
        );
        let tx = hex::decode(tx_hex).unwrap();
        let decoded = parse_coinbase(&tx).expect("segwit must parse");
        assert_eq!(decoded.outputs.len(), 2);
        assert_eq!(decoded.outputs[0].value_sats, 625_000_000);
        assert_eq!(decoded.outputs[1].value_sats, 0);
        assert_eq!(decoded.total_value_sats, 625_000_000);
        assert_eq!(decoded.scriptsig_hex, "deadbeef");
    }

    #[test]
    fn test_parse_coinbase_truncated_returns_none() {
        // Truncated mid-output value field
        let tx_hex = concat!(
            "01000000",
            "01",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "ffffffff",
            "01",
            "ab",
            "ffffffff",
            "01",
            "00f2052a", // only 4 of 8 value bytes
        );
        let tx = hex::decode(tx_hex).unwrap();
        assert!(parse_coinbase(&tx).is_none());
    }

    #[test]
    fn test_parse_coinbase_zero_input_count_rejected() {
        let tx_hex = "0100000000ffffffff";
        let tx = hex::decode(tx_hex).unwrap();
        assert!(parse_coinbase(&tx).is_none());
    }

    #[test]
    fn test_nonce_submission_format() {
        let nonce: u32 = 0x9546A142;
        let nonce_hex = format!("{:08x}", nonce);
        assert_eq!(nonce_hex, "9546a142");

        let nonce_le = nonce.to_le_bytes();
        let pool_decoded = hex::decode(&nonce_hex).unwrap();
        let pool_flipped: Vec<u8> = pool_decoded.iter().rev().cloned().collect();
        assert_eq!(&pool_flipped[..], &nonce_le[..]);
    }
}
