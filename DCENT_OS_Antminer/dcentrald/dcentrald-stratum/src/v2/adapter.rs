//! Adapter layer: translates SV2 messages to/from dcentrald's common types.
//!
//! SV2 Standard Channels differ fundamentally from Stratum V1:
//! - The pool provides a pre-computed `merkle_root` (no coinbase parts)
//! - Share submission uses numeric job_id/nonce/ntime/version (no hex strings)
//! - Difficulty is expressed as a 256-bit target, not a floating-point number
//!
//! This module bridges the two worlds so the rest of dcentrald (work dispatcher,
//! ASIC drivers, thermal manager) can operate identically regardless of which
//! Stratum protocol version is active.

#[cfg(feature = "jd")]
use super::jd::CustomJobCandidate;
use super::types::NewExtendedMiningJob;
use super::types::NewMiningJob;
use super::types::SetNewPrevHash;
use super::types::SubmitSharesExtended;
use crate::types::JobTemplate;

/// Inputs needed to materialize an SV2 NewExtendedMiningJob into a `JobTemplate`.
///
/// Mirrors the `Sv2Event::NewExtendedJob` payload after `SetNewPrevHash` has
/// supplied `prev_hash`/`nbits`/`ntime`. The miner picks one fixed extranonce
/// for the work block, builds the coinbase = `prefix || extranonce_prefix ||
/// extranonce || suffix`, hashes it, and walks `merkle_path` to derive the
/// merkle root. The resulting `JobTemplate` slots into the existing dispatcher
/// the same way `sv2_to_job_template` does (empty coinbase parts, populated
/// `merkle_root`).
#[derive(Debug, Clone)]
pub struct ExtendedJobAssembly<'a> {
    pub job_id: u32,
    pub version: u32,
    pub version_rolling_allowed: bool,
    pub prev_hash: [u8; 32],
    pub nbits: u32,
    pub ntime: u32,
    pub coinbase_tx_prefix: &'a [u8],
    pub coinbase_tx_suffix: &'a [u8],
    pub merkle_path: &'a [[u8; 32]],
    pub extranonce_prefix: &'a [u8],
    pub extranonce: &'a [u8],
    pub version_mask: u32,
    pub share_target: [u8; 32],
}

/// Convert an SV2 `NewExtendedMiningJob` (post-prevhash) into a `JobTemplate`
/// the existing work dispatcher can consume.
///
/// The pool ships the coinbase split (`coinbase_tx_prefix` /
/// `coinbase_tx_suffix`) plus the merkle path. The miner chooses one
/// extranonce, splices it into the coinbase between the prefix-extension
/// (`extranonce_prefix`) and suffix, hashes the full coinbase, and walks the
/// merkle path. The returned template has empty V1 coinbase parts and a
/// pre-computed `merkle_root` so `work::next_work` takes the SV2 path the
/// same way it does for standard channels.
pub fn extended_job_to_job_template(input: ExtendedJobAssembly<'_>) -> JobTemplate {
    let coinbase = build_extended_coinbase(
        input.coinbase_tx_prefix,
        input.extranonce_prefix,
        input.extranonce,
        input.coinbase_tx_suffix,
    );
    let coinbase_hash = crate::work::double_sha256(&coinbase);
    let merkle_root = compute_merkle_root(&coinbase_hash, input.merkle_path);
    let effective_version_mask = if input.version_rolling_allowed {
        input.version_mask
    } else {
        0
    };
    JobTemplate {
        job_id: input.job_id.to_string(),
        prev_block_hash: input.prev_hash,
        coinbase1: Vec::new(),
        coinbase2: Vec::new(),
        merkle_branches: Vec::new(),
        version: input.version,
        nbits: input.nbits,
        ntime: input.ntime,
        clean_jobs: true,
        share_target: input.share_target,
        extranonce1: input.extranonce_prefix.to_vec(),
        extranonce2_size: 0,
        version_mask: effective_version_mask,
        merkle_root,
        pool_difficulty: super::difficulty_autotune::target_to_approximate_difficulty(
            &input.share_target,
        ),
    }
}

fn build_extended_coinbase(
    prefix: &[u8],
    extranonce_prefix: &[u8],
    extranonce: &[u8],
    suffix: &[u8],
) -> Vec<u8> {
    let mut tx = Vec::with_capacity(
        prefix.len() + extranonce_prefix.len() + extranonce.len() + suffix.len(),
    );
    tx.extend_from_slice(prefix);
    tx.extend_from_slice(extranonce_prefix);
    tx.extend_from_slice(extranonce);
    tx.extend_from_slice(suffix);
    tx
}

fn compute_merkle_root(coinbase_hash: &[u8; 32], branches: &[[u8; 32]]) -> [u8; 32] {
    let mut hash = *coinbase_hash;
    for branch in branches {
        let mut combined = [0u8; 64];
        combined[0..32].copy_from_slice(&hash);
        combined[32..64].copy_from_slice(branch);
        hash = crate::work::double_sha256(&combined);
    }
    hash
}

/// Convert SV2 NewMiningJob + SetNewPrevHash into dcentrald's JobTemplate.
///
/// SV2 Standard Channel provides a pre-computed merkle_root (pool-constructed).
/// V1 provides coinbase parts + merkle branches for miner-side construction.
/// For SV2, we set merkle_branches to empty and provide the merkle_root directly.
///
/// # Arguments
/// * `job` - The NewMiningJob message from the pool
/// * `prev_hash` - The SetNewPrevHash message (provides prev_hash, ntime, nbits)
/// * `_channel_id` - The assigned mining channel ID (for future use)
/// * `extranonce_prefix` - Extranonce prefix assigned by pool at channel open
/// * `version_mask` - Version rolling mask from SetupConnection negotiation
/// * `share_target` - Share target from SetTarget (32 bytes, big-endian)
pub fn sv2_to_job_template(
    job: &NewMiningJob,
    prev_hash: &SetNewPrevHash,
    _channel_id: u32,
    extranonce_prefix: &[u8],
    version_mask: u32,
    share_target: [u8; 32],
) -> JobTemplate {
    JobTemplate {
        job_id: job.job_id.to_string(),
        prev_block_hash: prev_hash.prev_hash,
        // SV2 Standard Channels: pool provides the merkle root directly.
        // No coinbase construction needed on the miner side.
        coinbase1: Vec::new(),
        coinbase2: Vec::new(),
        merkle_branches: Vec::new(),
        version: job.version,
        nbits: prev_hash.nbits,
        ntime: prev_hash.min_ntime,
        // New block (SetNewPrevHash) always means clean_jobs
        clean_jobs: true,
        share_target,
        extranonce1: extranonce_prefix.to_vec(),
        // SV2 standard channels don't use extranonce2 (pool controls coinbase)
        extranonce2_size: 0,
        version_mask,
        merkle_root: job.merkle_root,
        // SV2: approximate difficulty from share_target.
        // diff = pdiff_1_target / target ≈ 2^224 / target_as_u256.
        // For the autotuner, an approximate value is fine — it only needs
        // order-of-magnitude accuracy for nonce rate calculations.
        pool_difficulty: super::difficulty_autotune::target_to_approximate_difficulty(
            &share_target,
        ),
    }
}

/// Convert dcentrald's ValidShare into SV2 SubmitSharesStandard parameters.
///
/// Returns (channel_id, sequence_number, job_id, nonce, ntime, version).
///
/// ValidShare stores values as hex strings (V1 legacy), so we parse them back
/// to u32 integers for SV2's binary protocol.
pub fn valid_share_to_sv2_submit(
    share: &crate::types::ValidShare,
    channel_id: u32,
    sequence_number: u32,
) -> (u32, u32, u32, u32, u32, u32) {
    let job_id = share.job_id.parse::<u32>().unwrap_or(0);

    // Nonce: stored as hex string "DEADBEEF" -> 0xDEADBEEF
    let nonce = u32::from_str_radix(&share.nonce, 16).unwrap_or(0);

    // Ntime: stored as hex string
    let ntime = u32::from_str_radix(&share.ntime, 16).unwrap_or(0);

    // Full block header version — stored directly in ValidShare.
    // BUG FIX (2026-04-11): was reconstructing from version_bits delta only,
    // missing the base version entirely. SV2 SubmitSharesStandard needs
    // the actual header version that was hashed.
    let version = share.version;

    (channel_id, sequence_number, job_id, nonce, ntime, version)
}

/// Convert an accepted custom job candidate into fixed-root work for the ASIC path.
///
/// DCENT_OS currently dispatches header-only work to the chips. For an upstream
/// extended channel we pick one fixed extranonce value, compute the corresponding
/// coinbase txid and merkle root, then submit any resulting shares back with
/// `SubmitSharesExtended` carrying the same fixed extranonce.
#[cfg(feature = "jd")]
pub fn custom_job_to_job_template(
    candidate: &CustomJobCandidate,
    job_id: u32,
    extranonce_prefix: &[u8],
    extranonce: &[u8],
    version_mask: u32,
    share_target: [u8; 32],
) -> JobTemplate {
    let coinbase = build_custom_coinbase(candidate, extranonce_prefix, extranonce);
    let coinbase_hash = crate::work::double_sha256(&coinbase);
    let merkle_root = compute_merkle_root(&coinbase_hash, &candidate.merkle_path);
    JobTemplate {
        job_id: job_id.to_string(),
        prev_block_hash: candidate.prev_hash,
        coinbase1: Vec::new(),
        coinbase2: Vec::new(),
        merkle_branches: Vec::new(),
        version: candidate.version,
        nbits: candidate.nbits,
        ntime: candidate.min_ntime,
        clean_jobs: true,
        share_target,
        extranonce1: extranonce_prefix.to_vec(),
        extranonce2_size: 0,
        version_mask,
        merkle_root,
        pool_difficulty: super::difficulty_autotune::target_to_approximate_difficulty(
            &share_target,
        ),
    }
}

#[cfg(feature = "jd")]
fn build_custom_coinbase(
    candidate: &CustomJobCandidate,
    extranonce_prefix: &[u8],
    extranonce: &[u8],
) -> Vec<u8> {
    let script_len = candidate
        .coinbase_prefix
        .len()
        .saturating_add(extranonce_prefix.len())
        .saturating_add(extranonce.len());
    let mut tx = Vec::new();
    tx.extend_from_slice(&candidate.coinbase_tx_version.to_le_bytes());
    tx.push(1); // one coinbase input
    tx.extend_from_slice(&[0u8; 32]);
    tx.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    write_compact_size(&mut tx, script_len as u64);
    tx.extend_from_slice(&candidate.coinbase_prefix);
    tx.extend_from_slice(extranonce_prefix);
    tx.extend_from_slice(extranonce);
    tx.extend_from_slice(&candidate.coinbase_tx_input_nsequence.to_le_bytes());
    tx.extend_from_slice(&candidate.coinbase_tx_outputs);
    tx.extend_from_slice(&candidate.coinbase_tx_locktime.to_le_bytes());
    tx
}

#[cfg(feature = "jd")]
fn write_compact_size(buf: &mut Vec<u8>, value: u64) {
    if value <= 0xfc {
        buf.push(value as u8);
    } else if value <= u16::MAX as u64 {
        buf.push(0xfd);
        buf.extend_from_slice(&(value as u16).to_le_bytes());
    } else if value <= u32::MAX as u64 {
        buf.push(0xfe);
        buf.extend_from_slice(&(value as u32).to_le_bytes());
    } else {
        buf.push(0xff);
        buf.extend_from_slice(&value.to_le_bytes());
    }
}

// ===========================================================================
// G3 — SV2 extended-channel ⟷ Stratum V1 proxy translation.
//
// Topology this module serves (the V2↔V1 proxy fork, Braiins-parity):
//
//     downstream V1 ASIC miner  ⟷  dcentrald (proxy)  ⟷  upstream SV2 pool
//          (mining.subscribe /        translate          (OpenExtendedMiningChannel /
//           mining.notify /                                NewExtendedMiningJob /
//           mining.submit)                                 SubmitSharesExtended)
//
// The prior `extended_job_to_job_template` path is the *mining-device*
// direction: dcentrald itself is the SV2 client and picks ONE fixed
// extranonce, collapsing the coinbase to a single merkle root. That
// destroys the per-share extranonce2 entropy a downstream V1 miner needs.
//
// A real proxy must instead hand the downstream V1 miner the coinbase
// SPLIT verbatim so the miner rolls its OWN extranonce2 across the full
// upstream extranonce space — this is what makes the proxy
// production-complete (full V1 coinbase fidelity) rather than a
// single-fixed-extranonce shim.
//
// SV2 extended-channel extranonce model (SV2 spec §6.4 + the live
// `OpenExtendedMiningChannelSuccess` parser in `channel.rs`):
//   * the pool assigns a fixed `extranonce_prefix` (B0_32) it splices
//     into the coinbase ahead of the miner bytes,
//   * plus an `extranonce_size` — the count of miner-controlled bytes the
//     pool reserves space for in the coinbase script.
//   * The full coinbase extranonce field on the wire is therefore
//     `extranonce_prefix || miner_extranonce` where `miner_extranonce`
//     is EXACTLY `extranonce_size` bytes.
//
// SRI/Braiins translator mapping (the canonical V2↔V1 proxy contract):
//   * V1 `extranonce1`     (advertised to the downstream miner in the
//                            `mining.subscribe` response) = SV2
//                            `extranonce_prefix`,
//   * V1 `extranonce2_size`(miner-controlled)            = SV2
//                            `extranonce_size`,
//   * V1 coinbase1/coinbase2 = SV2 `coinbase_tx_prefix` /
//                              `coinbase_tx_suffix` (verbatim — the
//                              miner inserts `extranonce1||extranonce2`
//                              between them exactly as on a V1 pool),
//   * V1 merkle_branches   = SV2 `merkle_path` (verbatim passthrough),
//   * a V1 `mining.submit` maps to `SubmitSharesExtended` where the SV2
//     `extranonce` field is the miner's `extranonce2` (the pool prepends
//     its own `extranonce_prefix`; sending the prefix again would
//     double-count it and the share would be rejected).
//
// Byte-fidelity invariant proven by this module's tests: the coinbase a
// downstream V1 miner reconstructs (coinbase1 || extranonce1 ||
// extranonce2 || coinbase2) is byte-identical to the coinbase the
// upstream SV2 pool reconstructs (coinbase_tx_prefix || extranonce_prefix
// || submitted_extranonce || coinbase_tx_suffix). No coinbase bytes are
// reinterpreted, padded, or reordered in transit.
// ===========================================================================

/// Lower-case hex (V1 wire encoding for byte fields).
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Decode a hex string into bytes. Returns `None` on any non-hex byte or
/// odd length — the proxy MUST NOT silently truncate a miner's
/// extranonce2 (a wrong-length coinbase would mine an invalid share).
pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

/// The extranonce split a V2↔V1 proxy negotiates once per upstream
/// extended channel and advertises to every downstream V1 miner.
///
/// `extranonce1` is the pool's fixed `extranonce_prefix` echoed verbatim;
/// `extranonce2_size` is the upstream `extranonce_size` (the miner-rollable
/// byte count). The V1 `mining.subscribe` response carries
/// `(extranonce1_hex, extranonce2_size)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sv2ProxyExtranonce {
    /// Pool-assigned prefix (SV2 `extranonce_prefix`), advertised to the
    /// downstream V1 miner as `extranonce1`.
    pub extranonce1: Vec<u8>,
    /// Miner-controlled byte count (SV2 `extranonce_size`), advertised to
    /// the downstream V1 miner as `extranonce2_size`.
    pub extranonce2_size: usize,
}

impl Sv2ProxyExtranonce {
    /// Build the proxy extranonce split from the upstream
    /// `OpenExtendedMiningChannelSuccess` fields.
    ///
    /// `extranonce_prefix` and `extranonce_size` are exactly the two values
    /// `channel.rs` records as `channel_extranonce_prefix` /
    /// `channel_extranonce_size` when the extended channel opens.
    pub fn from_open_success(extranonce_prefix: &[u8], extranonce_size: u16) -> Self {
        Self {
            extranonce1: extranonce_prefix.to_vec(),
            extranonce2_size: extranonce_size as usize,
        }
    }

    /// The `(extranonce1_hex, extranonce2_size)` pair to put in the V1
    /// `mining.subscribe` response sent to the downstream miner.
    pub fn v1_subscribe_reply(&self) -> (String, usize) {
        (to_hex(&self.extranonce1), self.extranonce2_size)
    }

    /// Validate a downstream V1 miner's submitted `extranonce2` hex string
    /// against the negotiated size, returning the raw bytes the upstream
    /// `SubmitSharesExtended.extranonce` field must carry.
    ///
    /// SV2 requires the miner-extranonce to be EXACTLY `extranonce_size`
    /// bytes — a short or long extranonce2 means the coinbase the miner
    /// hashed differs from the one the pool will reconstruct, so the share
    /// is structurally invalid and must be rejected, never zero-padded.
    pub fn validate_v1_extranonce2(&self, extranonce2_hex: &str) -> Result<Vec<u8>, String> {
        let bytes = from_hex(extranonce2_hex).ok_or_else(|| {
            format!(
                "proxy: V1 extranonce2 {:?} is not valid even-length hex",
                extranonce2_hex
            )
        })?;
        if bytes.len() != self.extranonce2_size {
            return Err(format!(
                "proxy: V1 extranonce2 length {} != negotiated extranonce_size {} \
                 (coinbase would mismatch the upstream pool — share rejected)",
                bytes.len(),
                self.extranonce2_size
            ));
        }
        Ok(bytes)
    }
}

/// V1 `mining.notify` parameters, fully reconstructable by a downstream
/// V1 miner. Field order/encoding matches the V1 `mining.notify` params
/// array exactly (see `v1::messages::parse_notify`):
///   `[job_id, prev_hash, coinbase1, coinbase2, merkle_branches,
///     version, nbits, ntime, clean_jobs]`.
///
/// All byte fields are lower-case hex (V1 wire convention). The V1 miner
/// builds its coinbase as `coinbase1 || extranonce1 || extranonce2 ||
/// coinbase2` — IDENTICAL to talking to a native V1 pool — which is why
/// the upstream SV2 coinbase split passes through verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1NotifyParams {
    pub job_id: String,
    /// Previous block hash, V1 wire order (the SV2 `prev_hash` is already
    /// in the byte order V1 expects on the wire — passed through verbatim).
    pub prev_hash_hex: String,
    pub coinbase1_hex: String,
    pub coinbase2_hex: String,
    pub merkle_branches_hex: Vec<String>,
    pub version_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
    pub clean_jobs: bool,
}

/// Translate an upstream SV2 `NewExtendedMiningJob` (+ the
/// `SetNewPrevHash` that supplies prev_hash/nbits/ntime) into the V1
/// `mining.notify` a downstream V1 miner consumes.
///
/// This is the production-complete extended-channel translation:
/// - `coinbase_tx_prefix` → V1 `coinbase1` (verbatim hex),
/// - `coinbase_tx_suffix` → V1 `coinbase2` (verbatim hex),
/// - `merkle_path` → V1 `merkle_branches` (verbatim hex, in order — V1
///   walks them left-to-right with the coinbase as the leftmost leaf,
///   the same order SV2 uses),
/// - the pool's `extranonce_prefix` becomes the V1 `extranonce1`
///   (handled by [`Sv2ProxyExtranonce`], advertised at subscribe time),
/// - the miner rolls its own `extranonce2` over `extranonce_size` bytes
///   — full V1 coinbase fidelity, NOT a single fixed extranonce.
///
/// `job_id` is the proxy-local V1 job id string the proxy assigned for
/// this upstream `NewExtendedMiningJob.job_id` (proxies remap ids so the
/// V1 namespace stays stable across upstream reconnects).
///
/// `clean_jobs` is `true` when this job follows a new `SetNewPrevHash`
/// (new block) — identical semantics to V1.
pub fn extended_job_to_v1_notify(
    job: &NewExtendedMiningJob,
    prev_hash: &SetNewPrevHash,
    v1_job_id: &str,
    clean_jobs: bool,
) -> V1NotifyParams {
    let merkle_branches_hex = job.merkle_path.iter().map(|h| to_hex(h)).collect();
    V1NotifyParams {
        job_id: v1_job_id.to_string(),
        prev_hash_hex: to_hex(&prev_hash.prev_hash),
        // Coinbase split passes through byte-for-byte. The downstream V1
        // miner inserts extranonce1||extranonce2 between these two halves
        // exactly as it would against a V1 pool.
        coinbase1_hex: to_hex(&job.coinbase_tx_prefix),
        coinbase2_hex: to_hex(&job.coinbase_tx_suffix),
        // Merkle path is order-significant; SV2 and V1 both treat the
        // coinbase as the leftmost leaf and concatenate each branch on
        // the right, so the sequence passes through unchanged.
        merkle_branches_hex,
        version_hex: format!("{:08x}", job.version),
        nbits_hex: format!("{:08x}", prev_hash.nbits),
        ntime_hex: format!("{:08x}", prev_hash.min_ntime),
        clean_jobs,
    }
}

/// Reconstruct the full coinbase transaction the way a downstream V1
/// miner does: `coinbase1 || extranonce1 || extranonce2 || coinbase2`.
///
/// Exposed so the proxy (and tests) can prove byte-fidelity against the
/// coinbase the upstream SV2 pool independently reconstructs from
/// `coinbase_tx_prefix || extranonce_prefix || submitted_extranonce ||
/// coinbase_tx_suffix`.
pub fn reconstruct_v1_coinbase(
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

/// The parsed pieces of a downstream V1 `mining.submit` the proxy needs
/// to forward upstream. Mirrors `v1::messages::submit_request` param
/// order: `[worker, job_id, extranonce2, ntime, nonce, version_bits?]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1SubmitParams {
    pub worker: String,
    pub job_id: String,
    pub extranonce2_hex: String,
    pub ntime_hex: String,
    pub nonce_hex: String,
    /// Optional BIP310 version-bits (present when the downstream miner
    /// negotiated version rolling via `mining.configure`).
    pub version_bits_hex: Option<String>,
}

/// Translate a downstream V1 `mining.submit` into an upstream SV2
/// `SubmitSharesExtended`.
///
/// Key correctness point: the SV2 `extranonce` field is the miner's
/// `extranonce2` ONLY. The pool re-prepends its own `extranonce_prefix`
/// (the bytes it advertised to the proxy and the proxy re-advertised as
/// V1 `extranonce1`). Sending `extranonce1||extranonce2` here would make
/// the pool's reconstructed coinbase carry the prefix twice → the share
/// hashes a different coinbase than the miner did → rejected. This
/// mirrors SRI's `ExtendedExtranonce` "self-controlled range" contract.
///
/// `version` is the full block-header version the miner hashed:
///   * if the miner rolled BIP310 bits, the proxy must apply them onto
///     the job's base version using the negotiated mask before sending
///     (callers pass the already-resolved full version via
///     `full_header_version`),
///   * SV2 `SubmitSharesExtended` carries the FULL header version, never
///     the BIP310 delta (same contract as `valid_share_to_sv2_submit`).
///
/// Returns an error (never a silently-wrong frame) when the miner's
/// extranonce2 doesn't match the negotiated extranonce size, or any hex
/// field is malformed.
pub fn v1_submit_to_sv2_extended(
    submit: &V1SubmitParams,
    proxy_extranonce: &Sv2ProxyExtranonce,
    channel_id: u32,
    sequence_number: u32,
    upstream_job_id: u32,
    full_header_version: u32,
) -> Result<SubmitSharesExtended, String> {
    // Validate + decode the miner's extranonce2 against the negotiated
    // size. This is the load-bearing check: a wrong-length extranonce2
    // means the coinbase the pool reconstructs differs from the one the
    // miner hashed.
    let extranonce = proxy_extranonce.validate_v1_extranonce2(&submit.extranonce2_hex)?;

    let nonce = u32::from_str_radix(submit.nonce_hex.trim_start_matches("0x"), 16)
        .map_err(|_| format!("proxy: V1 submit nonce {:?} not hex", submit.nonce_hex))?;
    let ntime = u32::from_str_radix(submit.ntime_hex.trim_start_matches("0x"), 16)
        .map_err(|_| format!("proxy: V1 submit ntime {:?} not hex", submit.ntime_hex))?;

    Ok(SubmitSharesExtended {
        channel_id,
        sequence_number,
        job_id: upstream_job_id,
        nonce,
        ntime,
        version: full_header_version,
        // SV2 `extranonce` = miner extranonce2 ONLY (pool prepends its
        // own prefix). NEVER prepend `proxy_extranonce.extranonce1`.
        extranonce,
    })
}

/// Resolve the full block-header version a downstream V1 miner hashed,
/// from the job's base version + the miner's optional BIP310 version-bits
/// + the negotiated rolling mask.
///
/// Mirrors the load-bearing AM2 BIP320 reconstruction
/// (`rolled = (base & !mask) | (vbits & mask)`): the proxy MUST forward
/// the version the chip actually hashed or the upstream pool rejects
/// every share. When the miner sent no version-bits (no rolling), the
/// base job version passes through unchanged.
pub fn resolve_full_header_version(
    base_version: u32,
    version_bits_hex: Option<&str>,
    version_mask: u32,
) -> Result<u32, String> {
    match version_bits_hex {
        None => Ok(base_version),
        Some(vb) => {
            let vbits = u32::from_str_radix(vb.trim_start_matches("0x"), 16)
                .map_err(|_| format!("proxy: V1 version_bits {:?} not hex", vb))?;
            // Only the masked bits may come from the miner; the rest stay
            // from the job (BIP310 §"version rolling" — out-of-mask bits
            // MUST be preserved).
            Ok((base_version & !version_mask) | (vbits & version_mask))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sv2_to_job_template_basic() {
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 42,
            future_job: false,
            version: 0x20000000,
            merkle_root: [0xAA; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 42,
            prev_hash: [0xBB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x17034219,
        };
        let extranonce = vec![0x01, 0x02, 0x03];
        let version_mask = 0x1FFFE000;
        let share_target = [0x00; 32];

        let template =
            sv2_to_job_template(&job, &prev_hash, 1, &extranonce, version_mask, share_target);

        assert_eq!(template.job_id, "42");
        assert_eq!(template.prev_block_hash, [0xBB; 32]);
        assert_eq!(template.merkle_root, [0xAA; 32]);
        assert!(template.coinbase1.is_empty());
        assert!(template.coinbase2.is_empty());
        assert!(template.merkle_branches.is_empty());
        assert_eq!(template.version, 0x20000000);
        assert_eq!(template.nbits, 0x17034219);
        assert_eq!(template.ntime, 1_700_000_000);
        assert!(template.clean_jobs);
        assert_eq!(template.extranonce1, vec![0x01, 0x02, 0x03]);
        assert_eq!(template.extranonce2_size, 0);
        assert_eq!(template.version_mask, version_mask);
    }

    #[test]
    fn test_valid_share_to_sv2_submit() {
        let share = crate::types::ValidShare {
            worker_name: "test.worker".to_string(),
            job_id: "42".to_string(),
            extranonce2: "00000000".to_string(),
            ntime: "65a0b1c2".to_string(),
            nonce: "deadbeef".to_string(),
            version_bits: Some("20004000".to_string()),
            version: 0x20004000, // full rolled version
            achieved_difficulty: None,
        };

        let (ch, seq, job_id, nonce, ntime, version) = valid_share_to_sv2_submit(&share, 1, 7);

        assert_eq!(ch, 1);
        assert_eq!(seq, 7);
        assert_eq!(job_id, 42);
        assert_eq!(nonce, 0xDEADBEEF);
        assert_eq!(ntime, 0x65A0B1C2);
        assert_eq!(version, 0x20004000);
    }

    #[test]
    fn test_valid_share_no_version_bits() {
        let share = crate::types::ValidShare {
            worker_name: "test".to_string(),
            job_id: "1".to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "00000001".to_string(),
            version_bits: None,
            version: 0x20000000, // base version, no rolling
            achieved_difficulty: None,
        };

        let (_, _, _, _, _, version) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(version, 0x20000000);
    }

    #[cfg(feature = "jd")]
    #[test]
    fn test_custom_job_to_job_template_builds_fixed_merkle_root() {
        let candidate = CustomJobCandidate {
            template_id: 9,
            mining_job_token: vec![1, 2],
            version: 0x2000_0000,
            prev_hash: [0x11; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
            target: [0x22; 32],
            coinbase_tx_version: 2,
            coinbase_prefix: vec![0x03, 0x04],
            coinbase_tx_input_nsequence: 0xffff_fffe,
            coinbase_tx_outputs: {
                let mut out = Vec::new();
                out.push(1);
                out.extend_from_slice(&50_000u64.to_le_bytes());
                out.push(1);
                out.push(0x51);
                out
            },
            coinbase_tx_locktime: 0,
            merkle_path: vec![[0x44; 32]],
            tx_count: 2,
            coinbase_value_remaining_sats: 50_000,
        };

        let job = custom_job_to_job_template(
            &candidate,
            77,
            &[0xaa],
            &[0xbb, 0xcc],
            0x1fffe000,
            [0xff; 32],
        );

        assert_eq!(job.job_id, "77");
        assert_eq!(job.prev_block_hash, [0x11; 32]);
        assert_eq!(job.version, candidate.version);
        assert_eq!(job.nbits, candidate.nbits);
        assert_eq!(job.ntime, candidate.min_ntime);
        assert_ne!(job.merkle_root, [0u8; 32]);
        assert!(job.coinbase1.is_empty());
        assert_eq!(job.extranonce1, vec![0xaa]);
        assert_eq!(job.extranonce2_size, 0);
    }

    // -----------------------------------------------------------------------
    // extended_job_to_job_template — strat-04 extended-channel work assembly
    // -----------------------------------------------------------------------

    fn manual_extended_merkle_root(
        prefix: &[u8],
        extranonce_prefix: &[u8],
        extranonce: &[u8],
        suffix: &[u8],
        merkle_path: &[[u8; 32]],
    ) -> [u8; 32] {
        let mut tx = Vec::new();
        tx.extend_from_slice(prefix);
        tx.extend_from_slice(extranonce_prefix);
        tx.extend_from_slice(extranonce);
        tx.extend_from_slice(suffix);
        let mut hash = crate::work::double_sha256(&tx);
        for branch in merkle_path {
            let mut combined = [0u8; 64];
            combined[..32].copy_from_slice(&hash);
            combined[32..].copy_from_slice(branch);
            hash = crate::work::double_sha256(&combined);
        }
        hash
    }

    #[test]
    fn extended_job_template_empty_merkle_path_matches_double_sha256() {
        let prefix = b"\x01\x00\x00\x00\xff\xff\xff\xff";
        let prefix_ext = b"\xa0\xa1";
        let extranonce = b"\x10\x20\x30\x40";
        let suffix = b"\x00\x00\x00\x00";

        let template = extended_job_to_job_template(ExtendedJobAssembly {
            job_id: 99,
            version: 0x2000_0000,
            version_rolling_allowed: true,
            prev_hash: [0x33; 32],
            nbits: 0x1703_4219,
            ntime: 1_700_000_000,
            coinbase_tx_prefix: prefix,
            coinbase_tx_suffix: suffix,
            merkle_path: &[],
            extranonce_prefix: prefix_ext,
            extranonce,
            version_mask: 0x1fff_e000,
            share_target: [0xff; 32],
        });

        let expected = manual_extended_merkle_root(prefix, prefix_ext, extranonce, suffix, &[]);
        assert_eq!(template.merkle_root, expected);
        assert_eq!(template.job_id, "99");
        assert_eq!(template.prev_block_hash, [0x33; 32]);
        assert_eq!(template.version, 0x2000_0000);
        assert_eq!(template.nbits, 0x1703_4219);
        assert_eq!(template.ntime, 1_700_000_000);
        assert!(template.coinbase1.is_empty());
        assert!(template.coinbase2.is_empty());
        assert!(template.merkle_branches.is_empty());
        assert_eq!(template.extranonce1, prefix_ext.to_vec());
        assert_eq!(template.extranonce2_size, 0);
        assert_eq!(template.version_mask, 0x1fff_e000);
        assert!(template.clean_jobs);
    }

    #[test]
    fn extended_job_template_walks_merkle_path_in_order() {
        let prefix = b"\x02\x00\x00\x00";
        let suffix = b"\x00\x00\x00\x00";
        let path = [[0x44; 32], [0x55; 32], [0x66; 32]];

        let template = extended_job_to_job_template(ExtendedJobAssembly {
            job_id: 1,
            version: 0,
            version_rolling_allowed: false,
            prev_hash: [0; 32],
            nbits: 0,
            ntime: 0,
            coinbase_tx_prefix: prefix,
            coinbase_tx_suffix: suffix,
            merkle_path: &path,
            extranonce_prefix: &[0x77, 0x88],
            extranonce: &[0x99, 0xaa, 0xbb, 0xcc],
            version_mask: 0xffff_ffff,
            share_target: [0; 32],
        });

        let expected = manual_extended_merkle_root(
            prefix,
            &[0x77, 0x88],
            &[0x99, 0xaa, 0xbb, 0xcc],
            suffix,
            &path,
        );
        assert_eq!(template.merkle_root, expected);
    }

    #[test]
    fn extended_job_template_disables_version_mask_when_pool_disallows_rolling() {
        let template = extended_job_to_job_template(ExtendedJobAssembly {
            job_id: 7,
            version: 0x2000_0000,
            version_rolling_allowed: false,
            prev_hash: [0; 32],
            nbits: 0,
            ntime: 0,
            coinbase_tx_prefix: b"\x00",
            coinbase_tx_suffix: b"\x00",
            merkle_path: &[],
            extranonce_prefix: &[],
            extranonce: &[],
            // Operator-configured mask is non-zero, but version_rolling_allowed=false
            // means the SV2 pool refuses rolled versions on this job.
            version_mask: 0x1fff_e000,
            share_target: [0xff; 32],
        });
        assert_eq!(template.version_mask, 0);
    }

    #[test]
    fn extended_job_template_propagates_share_target_into_pool_difficulty() {
        // Pool target with high bits zeroed should produce a non-trivial difficulty.
        let mut share_target = [0u8; 32];
        // Set bytes 26..32 so the target is small (high difficulty).
        share_target[26] = 0x01;
        let template = extended_job_to_job_template(ExtendedJobAssembly {
            job_id: 5,
            version: 0,
            version_rolling_allowed: true,
            prev_hash: [0; 32],
            nbits: 0,
            ntime: 0,
            coinbase_tx_prefix: b"\x00",
            coinbase_tx_suffix: b"\x00",
            merkle_path: &[],
            extranonce_prefix: &[],
            extranonce: &[],
            version_mask: 0,
            share_target,
        });
        assert_eq!(template.share_target, share_target);
        assert!(
            template.pool_difficulty > 0.0,
            "non-trivial share target must yield non-zero pool_difficulty (got {})",
            template.pool_difficulty
        );
    }

    #[test]
    fn extended_job_template_is_deterministic_for_same_inputs() {
        let prefix = b"\x01\x00\x00\x00\x01\x00\x00\x00\x00\x00";
        let suffix = b"\xff\xff\xff\xff";
        let path = [[0x12; 32]];

        let assembly = || ExtendedJobAssembly {
            job_id: 3,
            version: 0x2000_4000,
            version_rolling_allowed: true,
            prev_hash: [0xab; 32],
            nbits: 0x1703_a30c,
            ntime: 1_700_009_000,
            coinbase_tx_prefix: prefix,
            coinbase_tx_suffix: suffix,
            merkle_path: &path,
            extranonce_prefix: &[0xbe, 0xef],
            extranonce: &[0xca, 0xfe],
            version_mask: 0x1fff_e000,
            share_target: [0x7f; 32],
        };
        let a = extended_job_to_job_template(assembly());
        let b = extended_job_to_job_template(assembly());
        assert_eq!(a.merkle_root, b.merkle_root);
        assert_eq!(a.pool_difficulty, b.pool_difficulty);
    }

    // -----------------------------------------------------------------------
    // valid_share_to_sv2_submit silent-fallback contracts.
    //
    // The conversion parses hex strings from V1-shaped ValidShare fields.
    // Malformed input silently coerces to 0 — pin so a refactor that
    // switched to .expect() is caught (would panic the SV2 client) AND
    // a refactor that flipped the default value is caught (silent share
    // submission with wrong nonce).
    // -----------------------------------------------------------------------

    #[test]
    fn valid_share_to_sv2_submit_malformed_nonce_hex_falls_back_to_zero() {
        let share = crate::types::ValidShare {
            worker_name: "test".to_string(),
            job_id: "1".to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "not-hex".to_string(),
            version_bits: None,
            version: 0x2000_0000,
            achieved_difficulty: None,
        };
        let (_, _, _, nonce, _, _) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(
            nonce, 0,
            "malformed nonce hex must silently fall back to 0 (NOT panic)"
        );
    }

    #[test]
    fn valid_share_to_sv2_submit_malformed_ntime_hex_falls_back_to_zero() {
        let share = crate::types::ValidShare {
            worker_name: "test".to_string(),
            job_id: "1".to_string(),
            extranonce2: "00".to_string(),
            ntime: "garbage".to_string(),
            nonce: "deadbeef".to_string(),
            version_bits: None,
            version: 0x2000_0000,
            achieved_difficulty: None,
        };
        let (_, _, _, _, ntime, _) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(ntime, 0, "malformed ntime hex must silently fall back to 0");
    }

    #[test]
    fn valid_share_to_sv2_submit_non_numeric_job_id_falls_back_to_zero() {
        let share = crate::types::ValidShare {
            worker_name: "test".to_string(),
            job_id: "not-a-number".to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "deadbeef".to_string(),
            version_bits: None,
            version: 0x2000_0000,
            achieved_difficulty: None,
        };
        let (_, _, job_id, _, _, _) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(job_id, 0, "non-numeric job_id must silently fall back to 0");
    }

    #[test]
    fn valid_share_to_sv2_submit_passes_through_channel_id_and_sequence() {
        let share = crate::types::ValidShare {
            worker_name: "test".to_string(),
            job_id: "42".to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "01020304".to_string(),
            version_bits: None,
            version: 0x2000_0000,
            achieved_difficulty: None,
        };
        let (ch, seq, _, _, _, _) = valid_share_to_sv2_submit(&share, 0xAABBCCDD, 0x11223344);
        assert_eq!(ch, 0xAABBCCDD);
        assert_eq!(seq, 0x11223344);
    }

    #[test]
    fn valid_share_to_sv2_submit_handles_uppercase_hex() {
        // Pin that uppercase hex parses correctly. `u32::from_str_radix(_, 16)`
        // is case-insensitive, so a refactor to manual hex parsing must
        // preserve this.
        let share = crate::types::ValidShare {
            worker_name: "t".to_string(),
            job_id: "1".to_string(),
            extranonce2: "00".to_string(),
            ntime: "ABCDEF12".to_string(),
            nonce: "DEADBEEF".to_string(),
            version_bits: None,
            version: 0,
            achieved_difficulty: None,
        };
        let (_, _, _, nonce, ntime, _) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(nonce, 0xDEADBEEF);
        assert_eq!(ntime, 0xABCDEF12);
    }

    #[test]
    fn valid_share_to_sv2_submit_uses_version_field_directly_not_version_bits() {
        // Pin: SV2 uses the FULL block-header `version` field, NOT the
        // BIP310 version-bits delta. This was a real bug fixed earlier
        // (was reconstructing from version_bits delta only). Pin so the
        // bug doesn't return.
        let share = crate::types::ValidShare {
            worker_name: "t".to_string(),
            job_id: "1".to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "deadbeef".to_string(),
            // BIP310 delta string indicating "rolled bits" — must NOT
            // be used for the SV2 submission's version field.
            version_bits: Some("00004000".to_string()),
            version: 0x2000_4000, // full version with rolled bits applied
            achieved_difficulty: None,
        };
        let (_, _, _, _, _, version) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(
            version, 0x2000_4000,
            "SV2 must use full header version, not the BIP310 delta"
        );
    }

    #[test]
    fn valid_share_to_sv2_submit_max_u32_job_id_round_trips() {
        let share = crate::types::ValidShare {
            worker_name: "t".to_string(),
            job_id: u32::MAX.to_string(),
            extranonce2: "00".to_string(),
            ntime: "00000001".to_string(),
            nonce: "ffffffff".to_string(),
            version_bits: None,
            version: 0,
            achieved_difficulty: None,
        };
        let (_, _, job_id, nonce, _, _) = valid_share_to_sv2_submit(&share, 1, 1);
        assert_eq!(job_id, u32::MAX);
        assert_eq!(nonce, u32::MAX);
    }

    // -----------------------------------------------------------------------
    // sv2_to_job_template edge cases.
    // -----------------------------------------------------------------------

    #[test]
    fn sv2_to_job_template_with_empty_extranonce_prefix_is_legal() {
        // Standard channels can have empty extranonce_prefix when the
        // pool doesn't need per-miner salt. Pin so a refactor doesn't
        // accidentally inject a non-empty default.
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 5,
            future_job: false,
            version: 0x2000_0000,
            merkle_root: [0xAA; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 5,
            prev_hash: [0xBB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
        };
        let template = sv2_to_job_template(&job, &prev_hash, 1, &[], 0, [0xFF; 32]);
        assert!(template.extranonce1.is_empty());
        assert_eq!(template.extranonce2_size, 0);
    }

    #[test]
    fn sv2_to_job_template_zero_version_mask_disables_rolling() {
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 5,
            future_job: false,
            version: 0x2000_0000,
            merkle_root: [0xAA; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 5,
            prev_hash: [0xBB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
        };
        let template = sv2_to_job_template(&job, &prev_hash, 1, &[], 0, [0xFF; 32]);
        assert_eq!(template.version_mask, 0);
    }

    #[test]
    fn sv2_to_job_template_propagates_pool_difficulty_from_share_target() {
        // share_target should drive pool_difficulty via the autotune helper.
        // Pin so a refactor that introduced a manual difficulty calculation
        // (instead of using target_to_approximate_difficulty) is caught.
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 1,
            future_job: false,
            version: 0,
            merkle_root: [0u8; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 1,
            prev_hash: [0u8; 32],
            min_ntime: 0,
            nbits: 0,
        };
        // Smaller share_target = higher pool_difficulty.
        let mut tight_target = [0u8; 32];
        tight_target[5] = 0xFF;
        let tight = sv2_to_job_template(&job, &prev_hash, 1, &[], 0, tight_target);

        // Larger share_target = lower difficulty.
        let loose_target = [0xFFu8; 32];
        let loose = sv2_to_job_template(&job, &prev_hash, 1, &[], 0, loose_target);

        assert!(
            tight.pool_difficulty > loose.pool_difficulty,
            "tighter target must yield higher pool_difficulty: tight={} loose={}",
            tight.pool_difficulty,
            loose.pool_difficulty
        );
    }

    #[test]
    fn sv2_to_job_template_uses_clean_jobs_true_for_new_block() {
        // SetNewPrevHash always means a new block, so clean_jobs MUST be
        // true on the template. Pin so a refactor doesn't propagate
        // future_job into clean_jobs.
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 1,
            future_job: true, // future_job is irrelevant for clean_jobs
            version: 0,
            merkle_root: [0u8; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 1,
            prev_hash: [0u8; 32],
            min_ntime: 0,
            nbits: 0,
        };
        let template = sv2_to_job_template(&job, &prev_hash, 1, &[], 0, [0xFF; 32]);
        assert!(template.clean_jobs);
    }

    #[test]
    fn sv2_standard_channel_golden_vector_preserves_full_header_version() {
        let job = NewMiningJob {
            channel_id: 9,
            job_id: 0xCAFE,
            future_job: false,
            version: 0x2000_6000,
            merkle_root: [0x42; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 9,
            job_id: 0xCAFE,
            prev_hash: [0x24; 32],
            min_ntime: 0x65A0_B1C2,
            nbits: 0x1703_4219,
        };
        let extranonce_prefix = [0xDE, 0xAD, 0xBE, 0xEF];
        let version_mask = 0x1FFF_E000;
        let mut share_target = [0xFF; 32];
        share_target[0] = 0x00;

        let template = sv2_to_job_template(
            &job,
            &prev_hash,
            job.channel_id,
            &extranonce_prefix,
            version_mask,
            share_target,
        );

        assert_eq!(template.job_id, "51966");
        assert_eq!(template.version, 0x2000_6000);
        assert_eq!(template.version_mask, 0x1FFF_E000);
        assert_eq!(template.prev_block_hash, [0x24; 32]);
        assert_eq!(template.merkle_root, [0x42; 32]);
        assert_eq!(template.nbits, 0x1703_4219);
        assert_eq!(template.ntime, 0x65A0_B1C2);
        assert_eq!(template.extranonce1, extranonce_prefix.to_vec());
        assert_eq!(template.share_target, share_target);
        assert!(template.clean_jobs);

        let share = crate::types::ValidShare {
            worker_name: "rig.1".to_string(),
            job_id: job.job_id.to_string(),
            extranonce2: String::new(),
            ntime: format!("{:08x}", prev_hash.min_ntime),
            nonce: "deadbeef".to_string(),
            // BIP310 rolled-bits delta. SubmitSharesStandard must carry the
            // full header version that was hashed, not this delta alone.
            version_bits: Some("00006000".to_string()),
            version: template.version,
            achieved_difficulty: Some(8192.0),
        };
        let (channel_id, sequence, submit_job_id, nonce, ntime, version) =
            valid_share_to_sv2_submit(&share, job.channel_id, 7);
        assert_eq!(channel_id, 9);
        assert_eq!(sequence, 7);
        assert_eq!(submit_job_id, job.job_id);
        assert_eq!(nonce, 0xDEAD_BEEF);
        assert_eq!(ntime, prev_hash.min_ntime);
        assert_eq!(version, 0x2000_6000);
        assert_ne!(version, 0x0000_6000);
    }

    // =======================================================================
    // G3 — SV2 extended-channel ⟷ V1 proxy translation.
    //
    // The headline correctness property these tests pin: the coinbase a
    // downstream V1 miner reconstructs is BYTE-IDENTICAL to the coinbase
    // the upstream SV2 pool reconstructs. If that ever drifts the proxy
    // mines invalid shares silently — so it is regression-pinned hard.
    // =======================================================================

    fn sample_extended_job() -> NewExtendedMiningJob {
        NewExtendedMiningJob {
            channel_id: 7,
            job_id: 0x1234,
            future_job: false,
            version: 0x2000_0000,
            version_rolling_allowed: true,
            merkle_path: vec![[0x11; 32], [0x22; 32], [0x33; 32]],
            // Realistic-shaped coinbase split: version + 1 input + outpoint
            // + script-len, then the suffix is nSequence + outputs + locktime.
            coinbase_tx_prefix: vec![
                0x01, 0x00, 0x00, 0x00, // tx version
                0x01, // input count
            ],
            coinbase_tx_suffix: vec![
                0xff, 0xff, 0xff, 0xff, // nSequence
                0x00, 0x00, 0x00, 0x00, // locktime
            ],
        }
    }

    fn sample_prev_hash() -> SetNewPrevHash {
        SetNewPrevHash {
            channel_id: 7,
            job_id: 0x1234,
            prev_hash: [0xAB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
        }
    }

    // ---- extranonce-size negotiation -------------------------------------

    #[test]
    fn proxy_extranonce_maps_sv2_prefix_to_v1_extranonce1() {
        let split = Sv2ProxyExtranonce::from_open_success(&[0xDE, 0xAD, 0xBE, 0xEF], 8);
        assert_eq!(split.extranonce1, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(split.extranonce2_size, 8);
        let (e1_hex, e2_size) = split.v1_subscribe_reply();
        assert_eq!(e1_hex, "deadbeef");
        assert_eq!(e2_size, 8);
    }

    #[test]
    fn proxy_extranonce_accepts_exact_size_extranonce2() {
        let split = Sv2ProxyExtranonce::from_open_success(&[0x01, 0x02], 4);
        let bytes = split.validate_v1_extranonce2("aabbccdd").unwrap();
        assert_eq!(bytes, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn proxy_extranonce_rejects_short_extranonce2_never_pads() {
        let split = Sv2ProxyExtranonce::from_open_success(&[0x01], 4);
        // 3 bytes when 4 are required: a zero-padded coinbase would NOT
        // match what the upstream pool reconstructs → must hard-error.
        let err = split.validate_v1_extranonce2("aabbcc").unwrap_err();
        assert!(
            err.contains("length 3 != negotiated extranonce_size 4"),
            "{err}"
        );
    }

    #[test]
    fn proxy_extranonce_rejects_long_extranonce2() {
        let split = Sv2ProxyExtranonce::from_open_success(&[], 2);
        let err = split.validate_v1_extranonce2("aabbcc").unwrap_err();
        assert!(
            err.contains("length 3 != negotiated extranonce_size 2"),
            "{err}"
        );
    }

    #[test]
    fn proxy_extranonce_rejects_odd_length_hex() {
        let split = Sv2ProxyExtranonce::from_open_success(&[], 2);
        assert!(split.validate_v1_extranonce2("abc").is_err());
    }

    #[test]
    fn proxy_extranonce_rejects_non_hex() {
        let split = Sv2ProxyExtranonce::from_open_success(&[], 2);
        assert!(split.validate_v1_extranonce2("zzzz").is_err());
    }

    #[test]
    fn proxy_extranonce_zero_size_requires_empty_extranonce2() {
        // Some pools fully control the coinbase (extranonce_size=0). The
        // miner must then submit an empty extranonce2.
        let split = Sv2ProxyExtranonce::from_open_success(&[0xAA; 16], 0);
        assert_eq!(split.validate_v1_extranonce2("").unwrap(), Vec::<u8>::new());
        assert!(split.validate_v1_extranonce2("00").is_err());
    }

    // ---- NewExtendedMiningJob → V1 mining.notify -------------------------

    #[test]
    fn extended_job_translates_to_v1_notify_with_verbatim_coinbase_split() {
        let job = sample_extended_job();
        let prev = sample_prev_hash();
        let notify = extended_job_to_v1_notify(&job, &prev, "0a", true);

        assert_eq!(notify.job_id, "0a");
        assert_eq!(notify.prev_hash_hex, to_hex(&[0xAB; 32]));
        // Coinbase split passes through byte-for-byte as hex.
        assert_eq!(notify.coinbase1_hex, "0100000001");
        assert_eq!(notify.coinbase2_hex, "ffffffff00000000");
        assert_eq!(notify.version_hex, "20000000");
        assert_eq!(notify.nbits_hex, "17034219");
        assert_eq!(notify.ntime_hex, format!("{:08x}", 1_700_000_000u32));
        assert!(notify.clean_jobs);
    }

    #[test]
    fn extended_job_notify_passes_merkle_path_through_in_order() {
        let job = sample_extended_job();
        let prev = sample_prev_hash();
        let notify = extended_job_to_v1_notify(&job, &prev, "1", false);
        assert_eq!(notify.merkle_branches_hex.len(), 3);
        assert_eq!(notify.merkle_branches_hex[0], to_hex(&[0x11; 32]));
        assert_eq!(notify.merkle_branches_hex[1], to_hex(&[0x22; 32]));
        assert_eq!(notify.merkle_branches_hex[2], to_hex(&[0x33; 32]));
        assert!(!notify.clean_jobs);
    }

    #[test]
    fn extended_job_notify_empty_merkle_path_is_legal() {
        let mut job = sample_extended_job();
        job.merkle_path.clear();
        let notify = extended_job_to_v1_notify(&job, &sample_prev_hash(), "z", true);
        assert!(notify.merkle_branches_hex.is_empty());
    }

    // ---- The byte-fidelity invariant (the load-bearing proof) -----------

    #[test]
    fn proxy_coinbase_byte_fidelity_v1_miner_matches_upstream_pool() {
        // Upstream SV2 channel: prefix = 4 bytes, extranonce_size = 8.
        let job = sample_extended_job();
        let prev = sample_prev_hash();
        let extranonce_prefix = vec![0xCA, 0xFE, 0xBA, 0xBE];
        let split = Sv2ProxyExtranonce::from_open_success(&extranonce_prefix, 8);

        // Proxy → downstream V1 miner: it gets coinbase1/coinbase2 and
        // (extranonce1, extranonce2_size) and rolls its own extranonce2.
        let notify = extended_job_to_v1_notify(&job, &prev, "42", true);
        let (e1_hex, e2_size) = split.v1_subscribe_reply();
        let e1 = from_hex(&e1_hex).unwrap();
        let cb1 = from_hex(&notify.coinbase1_hex).unwrap();
        let cb2 = from_hex(&notify.coinbase2_hex).unwrap();

        // The miner picks an arbitrary 8-byte extranonce2.
        let miner_en2 = vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        assert_eq!(miner_en2.len(), e2_size);

        // (1) Coinbase the downstream V1 miner reconstructs+hashes:
        let v1_coinbase = reconstruct_v1_coinbase(&cb1, &e1, &miner_en2, &cb2);

        // (2) Coinbase the upstream SV2 pool independently reconstructs
        //     from the SubmitSharesExtended it receives:
        //       coinbase_tx_prefix || extranonce_prefix
        //         || submitted_extranonce || coinbase_tx_suffix
        let submitted_en2_hex = to_hex(&miner_en2);
        let submit = V1SubmitParams {
            worker: "w".to_string(),
            job_id: "42".to_string(),
            extranonce2_hex: submitted_en2_hex,
            ntime_hex: "65a0b1c2".to_string(),
            nonce_hex: "deadbeef".to_string(),
            version_bits_hex: None,
        };
        let ext =
            v1_submit_to_sv2_extended(&submit, &split, 7, 1, job.job_id, job.version).unwrap();
        let mut pool_coinbase = Vec::new();
        pool_coinbase.extend_from_slice(&job.coinbase_tx_prefix);
        pool_coinbase.extend_from_slice(&extranonce_prefix);
        pool_coinbase.extend_from_slice(&ext.extranonce); // == miner_en2
        pool_coinbase.extend_from_slice(&job.coinbase_tx_suffix);

        assert_eq!(
            v1_coinbase, pool_coinbase,
            "BYTE-FIDELITY VIOLATION: downstream V1 coinbase != upstream SV2 \
             coinbase. The proxy would mine invalid shares."
        );
        // And the SV2 extranonce carries the miner's en2 ONLY (no prefix).
        assert_eq!(ext.extranonce, miner_en2);
    }

    // ---- V1 mining.submit → SubmitSharesExtended ------------------------

    #[test]
    fn v1_submit_maps_to_submit_shares_extended_with_en2_only() {
        let split = Sv2ProxyExtranonce::from_open_success(&[0xAA, 0xBB], 4);
        let submit = V1SubmitParams {
            worker: "miner.1".to_string(),
            job_id: "ff".to_string(),
            extranonce2_hex: "01020304".to_string(),
            ntime_hex: "65a0b1c2".to_string(),
            nonce_hex: "deadbeef".to_string(),
            version_bits_hex: None,
        };
        let ext = v1_submit_to_sv2_extended(&submit, &split, 9, 3, 0xABCD, 0x2000_0000).unwrap();
        assert_eq!(ext.channel_id, 9);
        assert_eq!(ext.sequence_number, 3);
        assert_eq!(ext.job_id, 0xABCD);
        assert_eq!(ext.nonce, 0xDEADBEEF);
        assert_eq!(ext.ntime, 0x65A0B1C2);
        assert_eq!(ext.version, 0x2000_0000);
        // CRITICAL: extranonce is the miner's en2 ONLY — the pool prepends
        // its own prefix. Sending [0xAA,0xBB,...] would double-count it.
        assert_eq!(ext.extranonce, vec![0x01, 0x02, 0x03, 0x04]);
        assert!(
            !ext.extranonce.starts_with(&[0xAA, 0xBB]),
            "proxy must NOT prepend extranonce1 into the SV2 extranonce"
        );
    }

    #[test]
    fn v1_submit_rejects_wrong_size_extranonce2() {
        let split = Sv2ProxyExtranonce::from_open_success(&[0x01], 4);
        let submit = V1SubmitParams {
            worker: "w".to_string(),
            job_id: "1".to_string(),
            extranonce2_hex: "0102".to_string(), // 2 bytes, need 4
            ntime_hex: "00000001".to_string(),
            nonce_hex: "00000001".to_string(),
            version_bits_hex: None,
        };
        assert!(v1_submit_to_sv2_extended(&submit, &split, 1, 1, 1, 0).is_err());
    }

    #[test]
    fn v1_submit_rejects_malformed_nonce_never_silently_zeroes() {
        // Contrast with valid_share_to_sv2_submit (V1-native path, which
        // tolerates 0-fallback). The proxy MUST hard-fail: a wrong nonce
        // forwarded upstream is a guaranteed pool reject + wasted work.
        let split = Sv2ProxyExtranonce::from_open_success(&[], 0);
        let submit = V1SubmitParams {
            worker: "w".to_string(),
            job_id: "1".to_string(),
            extranonce2_hex: "".to_string(),
            ntime_hex: "00000001".to_string(),
            nonce_hex: "not-hex".to_string(),
            version_bits_hex: None,
        };
        assert!(v1_submit_to_sv2_extended(&submit, &split, 1, 1, 1, 0).is_err());
    }

    #[test]
    fn v1_submit_handles_0x_prefixed_hex() {
        let split = Sv2ProxyExtranonce::from_open_success(&[], 2);
        let submit = V1SubmitParams {
            worker: "w".to_string(),
            job_id: "1".to_string(),
            extranonce2_hex: "abcd".to_string(),
            ntime_hex: "0x65a0b1c2".to_string(),
            nonce_hex: "0xdeadbeef".to_string(),
            version_bits_hex: None,
        };
        let ext = v1_submit_to_sv2_extended(&submit, &split, 1, 1, 1, 0x2000_0000).unwrap();
        assert_eq!(ext.nonce, 0xDEADBEEF);
        assert_eq!(ext.ntime, 0x65A0B1C2);
    }

    // ---- full-header version reconstruction (BIP310/BIP320) -------------

    #[test]
    fn resolve_version_passthrough_when_no_rolling() {
        let v = resolve_full_header_version(0x2000_0000, None, 0x1FFF_E000).unwrap();
        assert_eq!(v, 0x2000_0000);
    }

    #[test]
    fn resolve_version_applies_masked_bits_only() {
        // base keeps its out-of-mask bits; only masked bits come from vbits.
        // mask 0x1FFFE000, base 0x20000000, vbits 0x1FFFE000 ⇒ all rolled.
        let v = resolve_full_header_version(0x2000_0000, Some("1fffe000"), 0x1FFF_E000).unwrap();
        assert_eq!(v, 0x2000_0000 | 0x1FFF_E000);
    }

    #[test]
    fn resolve_version_ignores_out_of_mask_bits_from_miner() {
        // A miner that (incorrectly) set bits outside the mask must NOT
        // corrupt the header version — only masked bits are honored.
        let v = resolve_full_header_version(
            0x2000_0000,
            Some("ffffffff"), // miner set everything
            0x1FFF_E000,
        )
        .unwrap();
        // base bits preserved outside mask, masked bits all 1.
        assert_eq!(v, (0x2000_0000 & !0x1FFF_E000) | 0x1FFF_E000);
    }

    #[test]
    fn resolve_version_rejects_malformed_version_bits() {
        assert!(resolve_full_header_version(0x2000_0000, Some("nope"), 0x1FFF_E000).is_err());
    }

    #[test]
    fn v1_submit_uses_resolved_full_version_end_to_end() {
        // Wire resolve_full_header_version into v1_submit_to_sv2_extended
        // the way the proxy does: base job version + miner's rolled bits.
        let split = Sv2ProxyExtranonce::from_open_success(&[], 0);
        let submit = V1SubmitParams {
            worker: "w".to_string(),
            job_id: "1".to_string(),
            extranonce2_hex: "".to_string(),
            ntime_hex: "00000001".to_string(),
            nonce_hex: "00000001".to_string(),
            version_bits_hex: Some("00004000".to_string()),
        };
        let full = resolve_full_header_version(
            0x2000_0000,
            submit.version_bits_hex.as_deref(),
            0x1FFF_E000,
        )
        .unwrap();
        let ext = v1_submit_to_sv2_extended(&submit, &split, 1, 1, 1, full).unwrap();
        assert_eq!(ext.version, 0x2000_4000);
        assert_eq!(ext.version, full);
    }

    // ---- hex helpers (the proxy's wire codec — must be exact) -----------

    #[test]
    fn to_hex_from_hex_round_trip() {
        let bytes = vec![0x00, 0x0f, 0xa5, 0xff, 0x10];
        let hex = to_hex(&bytes);
        assert_eq!(hex, "000fa5ff10");
        assert_eq!(from_hex(&hex).unwrap(), bytes);
    }

    #[test]
    fn from_hex_rejects_odd_length_and_non_hex() {
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
        assert!(from_hex("").unwrap().is_empty());
    }

    #[test]
    fn v1_unregressed_standard_channel_path_untouched_by_proxy_module() {
        // Belt-and-braces: the proxy module added pub fns but did NOT
        // change the existing standard-channel sv2_to_job_template path.
        // (The full V1 contract is the 573-test suite; this pins the
        //  specific surface the proxy work sits next to.)
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 99,
            future_job: false,
            version: 0x2000_0000,
            merkle_root: [0xAA; 32],
        };
        let prev_hash = SetNewPrevHash {
            channel_id: 1,
            job_id: 99,
            prev_hash: [0xBB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
        };
        let t = sv2_to_job_template(&job, &prev_hash, 1, &[0x01], 0x1FFFE000, [0xFF; 32]);
        assert_eq!(t.job_id, "99");
        assert_eq!(t.merkle_root, [0xAA; 32]);
        assert!(t.coinbase1.is_empty());
        assert!(t.merkle_branches.is_empty());
        assert_eq!(t.version_mask, 0x1FFFE000);
    }
}
