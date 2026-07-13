// DCENT_axe — Solo (coinbase-only) block template from a mesh Tip
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Phase-4 mining-on-mesh pure core: tip + payout address → `StratumJob` that
// `WorkBuilder::next_work` turns into ASIC work. Coinbase-only empty blocks
// (no mempool) for regtest / off-grid solo tip-relay.
//
// C1 (CRITICAL): Stratum coinb1|en|coinb2 is the **non-witness** serialization
// so SHA256d(concat) == Bitcoin **txid** == header merkle leaf. Witness
// marker/flag + reserved stack are attached only by `assemble_coinbase_full`
// when building a submitblock/BFG payload. NEVER point at mainnet until
// regtest e2e is proven (operator-side).

use crate::address::address_to_script_hex;
use crate::types::StratumJob;
use crate::work::{double_sha256, full_header_difficulty_and_target, MiningWork, WorkBuilder};

/// Default extranonce2 width for solo templates.
pub const SOLO_EXTRANONCE2_SIZE: usize = 4;

/// Tag bytes in coinbase scriptSig after BIP34 (identifies DCENT solo mesh).
pub const SOLO_COINBASE_TAG: &[u8] = b"/DCENT-solo/";

/// BIP320 version-rolling mask (bits 13..28). Never re-introduce a
/// `version_bits != 0` rejection guard.
pub const BIP320_VERSION_MASK: u32 = 0x1fff_e000;

/// Mainnet / testnet subsidy halving interval.
pub const MAINNET_HALVING_INTERVAL: u32 = 210_000;

/// Bitcoin Core regtest subsidy halving interval.
pub const REGTEST_HALVING_INTERVAL: u32 = 150;

/// Errors from building or validating a solo template. Fail-closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoloError {
    BadAddress,
    BadHeight,
    BadNbits,
    TipRejected(&'static str),
    BlockRejected(&'static str),
}

impl core::fmt::Display for SoloError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SoloError::BadAddress => write!(f, "unrecognized payout address"),
            SoloError::BadHeight => write!(f, "invalid block height"),
            SoloError::BadNbits => write!(f, "invalid compact nBits"),
            SoloError::TipRejected(s) => write!(f, "tip rejected: {s}"),
            SoloError::BlockRejected(s) => write!(f, "block rejected: {s}"),
        }
    }
}

impl std::error::Error for SoloError {}

/// Network identity for subsidy + tip structural policy (not crypto auth).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainId {
    /// Default for mesh MVP — fail-closed until operator opts into mainnet.
    Regtest,
    Testnet,
    Mainnet,
}

impl ChainId {
    pub fn subsidy_halving_interval(self) -> u32 {
        match self {
            ChainId::Regtest => REGTEST_HALVING_INTERVAL,
            ChainId::Testnet | ChainId::Mainnet => MAINNET_HALVING_INTERVAL,
        }
    }

    /// Label for honest UI / logs (`SOLO MESH · EMPTY · REGTEST`, …).
    pub fn label(self) -> &'static str {
        match self {
            ChainId::Regtest => "REGTEST",
            ChainId::Testnet => "TESTNET",
            ChainId::Mainnet => "MAINNET",
        }
    }
}

/// Chain-dependent solo policy. Default is **Regtest**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoloChainParams {
    pub chain: ChainId,
}

impl Default for SoloChainParams {
    fn default() -> Self {
        Self {
            chain: ChainId::Regtest,
        }
    }
}

impl SoloChainParams {
    pub fn regtest() -> Self {
        Self {
            chain: ChainId::Regtest,
        }
    }
    pub fn mainnet() -> Self {
        Self {
            chain: ChainId::Mainnet,
        }
    }
    pub fn testnet() -> Self {
        Self {
            chain: ChainId::Testnet,
        }
    }
    pub fn subsidy_halving_interval(self) -> u32 {
        self.chain.subsidy_halving_interval()
    }
}

/// Chain tip inputs for a solo template (mirrors mesh `Tip` without depending
/// on the LoRa crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoloTip {
    /// Previous block hash in **header-internal** byte order (header `[4..36]`).
    pub prev_hash: [u8; 32],
    pub nbits: u32,
    pub ntime: u32,
    /// Next block height (BIP34).
    pub height: i64,
}

/// Host-pure builder: tip + payout → [`StratumJob`] for [`WorkBuilder::next_work`].
#[derive(Debug, Clone)]
pub struct SoloTemplateBuilder {
    script_hex: String,
    extranonce1_hex: String,
    extranonce2_size: usize,
    version: u32,
    params: SoloChainParams,
}

impl SoloTemplateBuilder {
    /// Build from a Bitcoin address. Defaults to **Regtest** chain params.
    pub fn from_address(address: &str) -> Result<Self, SoloError> {
        let script_hex = address_to_script_hex(address).ok_or(SoloError::BadAddress)?;
        Ok(Self::from_script_hex_inner(
            script_hex,
            SoloChainParams::default(),
        ))
    }

    /// Chain-aware address decode: refuse HRPs that do not match `chain`.
    pub fn from_address_for_chain(address: &str, chain: ChainId) -> Result<Self, SoloError> {
        if !address_matches_chain(address, chain) {
            return Err(SoloError::BadAddress);
        }
        let script_hex = address_to_script_hex(address).ok_or(SoloError::BadAddress)?;
        Ok(Self::from_script_hex_inner(
            script_hex,
            SoloChainParams { chain },
        ))
    }

    /// Pre-derived scriptPubKey hex (lab/tests). Defaults to Regtest.
    pub fn from_script_hex(script_hex: &str) -> Result<Self, SoloError> {
        if script_hex.is_empty() || script_hex.len() % 2 != 0 {
            return Err(SoloError::BadAddress);
        }
        if hex::decode(script_hex).is_err() {
            return Err(SoloError::BadAddress);
        }
        Ok(Self::from_script_hex_inner(
            script_hex.to_ascii_lowercase(),
            SoloChainParams::default(),
        ))
    }

    fn from_script_hex_inner(script_hex: String, params: SoloChainParams) -> Self {
        Self {
            script_hex,
            extranonce1_hex: String::new(),
            extranonce2_size: SOLO_EXTRANONCE2_SIZE,
            version: 0x2000_0000,
            params,
        }
    }

    pub fn with_chain_params(mut self, params: SoloChainParams) -> Self {
        self.params = params;
        self
    }

    pub fn for_chain(mut self, chain: ChainId) -> Self {
        self.params = SoloChainParams { chain };
        self
    }

    pub fn with_extranonce1_hex(mut self, hex: &str) -> Self {
        self.extranonce1_hex = hex.to_string();
        self
    }

    pub fn with_extranonce2_size(mut self, size: usize) -> Self {
        self.extranonce2_size = size.clamp(1, crate::types::MAX_EXTRANONCE2_SIZE);
        self
    }

    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    pub fn extranonce1_hex(&self) -> &str {
        &self.extranonce1_hex
    }

    pub fn extranonce2_size(&self) -> usize {
        self.extranonce2_size
    }

    pub fn chain_params(&self) -> SoloChainParams {
        self.params
    }

    pub fn script_hex(&self) -> &str {
        &self.script_hex
    }

    /// Validate tip (with this builder's chain policy) then produce a job.
    pub fn build_job(&self, tip: &SoloTip) -> Result<StratumJob, SoloError> {
        validate_tip_for_chain(tip, self.params)?;
        let height = tip.height;
        if height < 0 || height > i64::from(u32::MAX) {
            return Err(SoloError::BadHeight);
        }
        let height_u32 = height as u32;
        let subsidy =
            block_subsidy_sats_with_interval(height_u32, self.params.subsidy_halving_interval());

        let en1 = hex::decode(&self.extranonce1_hex).unwrap_or_default();
        let en2_size = self.extranonce2_size;

        let bip34 = bip34_height_script(height_u32);
        let script_sig_prefix_len = bip34.len() + SOLO_COINBASE_TAG.len();
        let script_sig_len = script_sig_prefix_len + en1.len() + en2_size;
        if script_sig_len > 252 {
            return Err(SoloError::BadHeight);
        }

        // ---- coinb1: NON-WITNESS path (C1) — no marker/flag ----
        // version | in_count | prevout | ssig_len | bip34 | tag
        // WorkBuilder inserts en1|en2 then coinb2; SHA256d of full concat = txid.
        let mut coinb1 = Vec::with_capacity(64);
        coinb1.extend_from_slice(&2u32.to_le_bytes()); // version 2
        coinb1.push(0x01); // one input (legacy layout — no 0x00 0x01)
        coinb1.extend_from_slice(&[0u8; 32]);
        coinb1.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
        coinb1.push(script_sig_len as u8);
        coinb1.extend_from_slice(&bip34);
        coinb1.extend_from_slice(SOLO_COINBASE_TAG);

        // ---- coinb2: sequence | outs | locktime — no witness stack ----
        let script_pk = hex::decode(&self.script_hex).map_err(|_| SoloError::BadAddress)?;
        let witness_commitment = witness_commitment_script();

        let mut coinb2 = Vec::with_capacity(128);
        coinb2.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
        coinb2.push(0x02); // two outputs
        coinb2.extend_from_slice(&subsidy.to_le_bytes());
        push_var_slice(&mut coinb2, &script_pk);
        coinb2.extend_from_slice(&0u64.to_le_bytes());
        push_var_slice(&mut coinb2, &witness_commitment);
        coinb2.extend_from_slice(&0u32.to_le_bytes()); // locktime

        let pool_prev = header_prev_to_pool_hex(&tip.prev_hash);

        Ok(StratumJob {
            // Honest label: solo empty-block mesh work (not a pool job).
            job_id: format!(
                "solo-{}-{}",
                self.params.chain.label().to_ascii_lowercase(),
                height_u32
            ),
            prev_hash: pool_prev,
            coinbase1: hex::encode(&coinb1),
            coinbase2: hex::encode(&coinb2),
            merkle_branches: Vec::new(),
            version: format!("{:08x}", self.version),
            nbits: format!("{:08x}", tip.nbits),
            block_height: height_u32,
            ntime: format!("{:08x}", tip.ntime),
            clean_jobs: true,
        })
    }

    /// Job + one [`MiningWork`]. Share target is the **compact nBits target**
    /// (not pdiff), so easy regtest nonces are not software-filtered away.
    pub fn build_work(&self, tip: &SoloTip) -> Result<(StratumJob, MiningWork), SoloError> {
        let job = self.build_job(tip)?;
        let target = compact_target_be(tip.nbits).ok_or(SoloError::BadNbits)?;
        let mut wb = WorkBuilder::new(&self.extranonce1_hex, self.extranonce2_size);
        wb.set_version_mask(BIP320_VERSION_MASK);
        wb.set_difficulty(1.0); // placeholder; overwritten below
        let mut work = wb.next_work(&job);
        work.share_target = target;
        Ok((job, work))
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Tip structural check with **Regtest** defaults (easy nBits allowed).
pub fn validate_tip(tip: &SoloTip) -> Result<(), SoloError> {
    validate_tip_for_chain(tip, SoloChainParams::default())
}

/// Tip check under chain policy (structural — not cryptographic auth).
pub fn validate_tip_for_chain(tip: &SoloTip, params: SoloChainParams) -> Result<(), SoloError> {
    if tip.height < 0 {
        return Err(SoloError::TipRejected("negative height"));
    }
    let target =
        compact_target_be(tip.nbits).ok_or(SoloError::TipRejected("nbits does not decode"))?;
    // Mainnet/testnet: refuse toy/easy compact targets (energy DoS / spoof).
    // Regtest intentionally allows easy nBits (e.g. 0x207fffff).
    if params.chain != ChainId::Regtest && is_toy_compact_target(&target) {
        return Err(SoloError::TipRejected("nbits too easy for non-regtest"));
    }
    Ok(())
}

/// True if target is looser than Bitcoin difficulty-1 (`0x1d00ffff`).
/// Structural floor only — not a substitute for tip authenticity.
fn is_toy_compact_target(target: &[u8; 32]) -> bool {
    let Some(diff1) = compact_target_be(0x1d00_ffff) else {
        return true;
    };
    // BE compare: larger target = easier. Refuse target > difficulty-1.
    target.as_slice() > diff1.as_slice()
}

/// PoW-only header check. Prefer [`validate_found_block`] before relay/submit.
pub fn validate_found_header(header: &[u8; 80]) -> Result<f64, SoloError> {
    let nbits = u32::from_le_bytes([header[72], header[73], header[74], header[75]]);
    let target = compact_target_be(nbits).ok_or(SoloError::BlockRejected("bad nbits in header"))?;
    let (diff, meets) = full_header_difficulty_and_target(header, &target);
    if !meets {
        return Err(SoloError::BlockRejected("hash does not meet nbits target"));
    }
    if !(diff > 0.0) {
        return Err(SoloError::BlockRejected("non-positive difficulty"));
    }
    Ok(diff)
}

/// Full local re-validation before fragment TX / gateway submitblock.
///
/// `coinbase_nonwitness` must be the non-witness serialization (txid path).
pub fn validate_found_block(
    tip: &SoloTip,
    header: &[u8; 80],
    coinbase_nonwitness: &[u8],
) -> Result<f64, SoloError> {
    if coinbase_nonwitness.is_empty() {
        return Err(SoloError::BlockRejected("empty coinbase"));
    }
    // Bind header fields to accepted tip.
    if header[4..36] != tip.prev_hash {
        return Err(SoloError::BlockRejected("prev_hash mismatch"));
    }
    let nbits = u32::from_le_bytes([header[72], header[73], header[74], header[75]]);
    if nbits != tip.nbits {
        return Err(SoloError::BlockRejected("nbits mismatch"));
    }
    let merkle: [u8; 32] = header[36..68]
        .try_into()
        .map_err(|_| SoloError::BlockRejected("merkle slice"))?;
    let txid = coinbase_txid(coinbase_nonwitness);
    if merkle != txid {
        return Err(SoloError::BlockRejected("merkle != coinbase txid"));
    }
    validate_found_header(header)
}

/// Clock-free tip ordering: strictly higher height supersedes.
pub fn tip_supersedes(old: &SoloTip, new: &SoloTip) -> bool {
    new.height > old.height
}

// ---------------------------------------------------------------------------
// Coinbase / block assembly (C1 + H7)
// ---------------------------------------------------------------------------

/// Concatenate coinb1|en1|en2|coinb2 (non-witness Stratum shape).
pub fn assemble_coinbase_nonwitness(
    job: &StratumJob,
    extranonce1: &[u8],
    extranonce2: &[u8],
) -> Result<Vec<u8>, SoloError> {
    let c1 = hex::decode(&job.coinbase1).map_err(|_| SoloError::BlockRejected("bad coinb1"))?;
    let c2 = hex::decode(&job.coinbase2).map_err(|_| SoloError::BlockRejected("bad coinb2"))?;
    let mut tx = Vec::with_capacity(c1.len() + extranonce1.len() + extranonce2.len() + c2.len());
    tx.extend_from_slice(&c1);
    tx.extend_from_slice(extranonce1);
    tx.extend_from_slice(extranonce2);
    tx.extend_from_slice(&c2);
    Ok(tx)
}

/// Bitcoin **txid** of a non-witness coinbase serialization.
pub fn coinbase_txid(nonwitness: &[u8]) -> [u8; 32] {
    double_sha256(nonwitness)
}

/// Inject BIP141 marker/flag + reserved coinbase witness for wire/submitblock.
///
/// Input must be non-witness layout: `version(4) | rest…| locktime(4)`.
/// Output: `version | 00 01 | body_without_locktime | witness | locktime`.
pub fn assemble_coinbase_full(nonwitness: &[u8]) -> Result<Vec<u8>, SoloError> {
    if nonwitness.len() < 10 {
        return Err(SoloError::BlockRejected("coinbase too short"));
    }
    // Refuse if already looks segwit-marked (defense).
    if nonwitness.get(4) == Some(&0x00) && nonwitness.get(5) == Some(&0x01) {
        return Err(SoloError::BlockRejected("already has witness marker"));
    }
    let version = &nonwitness[0..4];
    let locktime = &nonwitness[nonwitness.len() - 4..];
    let body = &nonwitness[4..nonwitness.len() - 4];

    let mut full = Vec::with_capacity(nonwitness.len() + 2 + 1 + 1 + 32);
    full.extend_from_slice(version);
    full.push(0x00); // marker
    full.push(0x01); // flag
    full.extend_from_slice(body);
    // one witness item of 32 zero bytes (BIP141 coinbase reserved)
    full.push(0x01);
    full.push(0x20);
    full.extend_from_slice(&[0u8; 32]);
    full.extend_from_slice(locktime);
    Ok(full)
}

/// Full block bytes: `header(80) || tx_count(1) || coinbase_full`.
pub fn assemble_solo_block(header: &[u8; 80], coinbase_full: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(80 + 1 + coinbase_full.len());
    out.extend_from_slice(header);
    out.push(0x01); // one transaction
    out.extend_from_slice(coinbase_full);
    out
}

/// BIP320: apply `version_bits` under `mask` onto `base`.
pub fn rolled_version(base: u32, mask: u32, version_bits: u32) -> u32 {
    (base & !mask) | (version_bits & mask)
}

/// Assemble 80-byte header from work + nonce + version (use [`rolled_version`]
/// when the ASIC rolled bits).
pub fn header_from_work(work: &MiningWork, nonce: u32, version: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&version.to_le_bytes());
    h[4..36].copy_from_slice(&work.prev_block_hash);
    h[36..68].copy_from_slice(&work.merkle_root);
    h[68..72].copy_from_slice(&work.ntime.to_le_bytes());
    h[72..76].copy_from_slice(&work.nbits.to_le_bytes());
    h[76..80].copy_from_slice(&nonce.to_le_bytes());
    h
}

// ---------------------------------------------------------------------------
// Subsidy / BIP34 / witness commitment
// ---------------------------------------------------------------------------

/// Mainnet-schedule subsidy (`interval = 210_000`). Prefer
/// [`block_subsidy_sats_with_interval`] for regtest.
pub fn block_subsidy_sats(height: u32) -> u64 {
    block_subsidy_sats_with_interval(height, MAINNET_HALVING_INTERVAL)
}

/// Subsidy with explicit halving interval (regtest = 150).
pub fn block_subsidy_sats_with_interval(height: u32, interval: u32) -> u64 {
    if interval == 0 {
        return 0;
    }
    let halvings = height / interval;
    if halvings >= 64 {
        return 0;
    }
    50_00_000_000u64.checked_shr(halvings).unwrap_or(0)
}

/// BIP34 height as minimal CScript push (OP_0..OP_16 or minimal LE push).
pub fn bip34_height_script(height: u32) -> Vec<u8> {
    let n = height as i64;
    if n == 0 {
        return vec![0x00];
    }
    if (1..=16).contains(&n) {
        return vec![0x50 + n as u8];
    }
    let mut abs = (n as u64).to_le_bytes().to_vec();
    while abs.len() > 1 && abs[abs.len() - 1] == 0 && abs[abs.len() - 2] & 0x80 == 0 {
        abs.pop();
    }
    if abs.last().map(|b| b & 0x80 != 0).unwrap_or(false) {
        abs.push(0x00);
    }
    let mut out = Vec::with_capacity(1 + abs.len());
    out.push(abs.len() as u8);
    out.extend_from_slice(&abs);
    out
}

/// Witness commitment scriptPubKey for coinbase-only (BIP141 zeros root).
pub fn witness_commitment_script() -> Vec<u8> {
    let preimage = [0u8; 64];
    let commitment = double_sha256(&preimage);
    let mut script = Vec::with_capacity(38);
    script.push(0x6a);
    script.push(0x24);
    script.extend_from_slice(&[0xaa, 0x21, 0xa9, 0xed]);
    script.extend_from_slice(&commitment);
    script
}

pub fn witness_commitment_hash_zeros() -> [u8; 32] {
    double_sha256(&[0u8; 64])
}

// ---------------------------------------------------------------------------
// nBits / helpers
// ---------------------------------------------------------------------------

fn push_var_slice(buf: &mut Vec<u8>, data: &[u8]) {
    buf.push(data.len() as u8);
    buf.extend_from_slice(data);
}

fn header_prev_to_pool_hex(header_prev: &[u8; 32]) -> String {
    let mut pool = *header_prev;
    reverse_endianness_per_word(&mut pool);
    hex::encode(pool)
}

fn reverse_endianness_per_word(data: &mut [u8; 32]) {
    for chunk in data.chunks_exact_mut(4) {
        chunk.reverse();
    }
}

/// Compact nBits → 32-byte big-endian target (Bitcoin Core SetCompact shape).
pub fn compact_target_be(nbits: u32) -> Option<[u8; 32]> {
    let mantissa = nbits & 0x007f_ffff;
    let exponent = ((nbits >> 24) & 0xff) as i32;
    let negative = nbits & 0x0080_0000 != 0;
    if mantissa == 0 || negative {
        return None;
    }
    let mut target = [0u8; 32];
    if exponent <= 3 {
        let m = mantissa >> (8 * (3 - exponent));
        target[29] = ((m >> 16) & 0xff) as u8;
        target[30] = ((m >> 8) & 0xff) as u8;
        target[31] = (m & 0xff) as u8;
    } else {
        let start = 32i32 - exponent;
        if start < 0 {
            return None;
        }
        let start = start as usize;
        if start + 2 >= 32 {
            return None;
        }
        target[start] = ((mantissa >> 16) & 0xff) as u8;
        target[start + 1] = ((mantissa >> 8) & 0xff) as u8;
        target[start + 2] = (mantissa & 0xff) as u8;
    }
    Some(target)
}

/// HRP / version prefix must match chain (fail-closed for wrong-network address).
fn address_matches_chain(addr: &str, chain: ChainId) -> bool {
    let lower = addr.to_ascii_lowercase();
    match chain {
        ChainId::Mainnet => {
            lower.starts_with("bc1") || addr.starts_with('1') || addr.starts_with('3')
        }
        ChainId::Testnet => {
            lower.starts_with("tb1")
                || addr.starts_with('m')
                || addr.starts_with('n')
                || addr.starts_with('2')
        }
        ChainId::Regtest => {
            // bcrt1 preferred; also allow script_hex path via from_script_hex.
            // Accept bcrt1 and also mainnet-looking only if operator used
            // from_address without for_chain — this helper is for for_chain only.
            lower.starts_with("bcrt1")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::parse_coinbase;

    fn regtest_tip(height: i64) -> SoloTip {
        SoloTip {
            prev_hash: [0xab; 32],
            nbits: 0x207f_ffff,
            ntime: 1_700_000_000,
            height,
        }
    }

    fn en2_zeros(n: usize) -> Vec<u8> {
        vec![0u8; n]
    }

    #[test]
    fn subsidy_mainnet_schedule() {
        assert_eq!(block_subsidy_sats(0), 5_000_000_000);
        assert_eq!(block_subsidy_sats(209_999), 5_000_000_000);
        assert_eq!(block_subsidy_sats(210_000), 2_500_000_000);
        assert_eq!(block_subsidy_sats(420_000), 1_250_000_000);
        assert_eq!(block_subsidy_sats(210_000 * 33), 0);
    }

    #[test]
    fn regtest_subsidy_halves_at_150() {
        assert_eq!(
            block_subsidy_sats_with_interval(149, REGTEST_HALVING_INTERVAL),
            5_000_000_000
        );
        assert_eq!(
            block_subsidy_sats_with_interval(150, REGTEST_HALVING_INTERVAL),
            2_500_000_000
        );
        assert_eq!(
            block_subsidy_sats_with_interval(300, REGTEST_HALVING_INTERVAL),
            1_250_000_000
        );
    }

    #[test]
    fn bip34_known_encodings() {
        assert_eq!(bip34_height_script(0), vec![0x00]);
        assert_eq!(bip34_height_script(1), vec![0x51]);
        assert_eq!(bip34_height_script(16), vec![0x60]);
        assert_eq!(bip34_height_script(17), vec![0x01, 0x11]);
        assert_eq!(bip34_height_script(100), vec![0x01, 0x64]);
        assert_eq!(bip34_height_script(255), vec![0x02, 0xff, 0x00]);
        assert_eq!(bip34_height_script(901_234), vec![0x03, 0x72, 0xc0, 0x0d]);
    }

    #[test]
    fn witness_commitment_kat_zeros() {
        let h = witness_commitment_hash_zeros();
        assert_eq!(h, double_sha256(&[0u8; 64]));
        assert_eq!(
            hex::encode(h),
            "e2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf9"
        );
        let script = witness_commitment_script();
        assert_eq!(script[0], 0x6a);
        assert_eq!(&script[2..6], &[0xaa, 0x21, 0xa9, 0xed]);
        assert_eq!(&script[6..38], &h);
    }

    #[test]
    fn merkle_root_equals_nonwitness_txid() {
        let b = SoloTemplateBuilder::from_address("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .unwrap()
            .for_chain(ChainId::Regtest);
        let tip = regtest_tip(100);
        let (job, work) = b.build_work(&tip).unwrap();
        let nw =
            assemble_coinbase_nonwitness(&job, &[], &en2_zeros(SOLO_EXTRANONCE2_SIZE)).unwrap();
        // Legacy layout: after version LE, next byte is in_count=1 (not marker 0x00).
        assert_eq!(
            nw[4], 0x01,
            "non-witness coinbase must not start body with marker"
        );
        assert_eq!(
            work.merkle_root,
            coinbase_txid(&nw),
            "C1: merkle must be txid"
        );
    }

    #[test]
    fn full_coinbase_has_witness_job_does_not() {
        let b = SoloTemplateBuilder::from_script_hex("51")
            .unwrap()
            .for_chain(ChainId::Regtest);
        let job = b.build_job(&regtest_tip(5)).unwrap();
        let nw = assemble_coinbase_nonwitness(&job, &[], &en2_zeros(4)).unwrap();
        let full = assemble_coinbase_full(&nw).unwrap();
        assert_eq!(&full[4..6], &[0x00, 0x01]);
        assert!(full.len() > nw.len());
        // txid of nonwitness still the merkle leaf
        let mut wb = WorkBuilder::new("", 4);
        let work = wb.next_work(&job);
        assert_eq!(work.merkle_root, coinbase_txid(&nw));
    }

    #[test]
    fn assemble_solo_block_prefix_and_txcount() {
        let header = [0x11u8; 80];
        let cb = vec![0x22u8; 40];
        let block = assemble_solo_block(&header, &cb);
        assert_eq!(block.len(), 80 + 1 + 40);
        assert_eq!(&block[0..80], &header);
        assert_eq!(block[80], 0x01);
        assert_eq!(&block[81..], &cb[..]);
    }

    #[test]
    fn build_work_share_target_equals_compact() {
        let b = SoloTemplateBuilder::from_script_hex("51").unwrap();
        let tip = regtest_tip(7);
        let (_, work) = b.build_work(&tip).unwrap();
        assert_eq!(work.share_target, compact_target_be(tip.nbits).unwrap());
    }

    #[test]
    fn build_job_round_trips_through_work_builder() {
        let b = SoloTemplateBuilder::from_address("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .unwrap()
            .for_chain(ChainId::Regtest);
        let tip = regtest_tip(100);
        let job = b.build_job(&tip).unwrap();
        assert!(job.job_id.starts_with("solo-regtest-"));
        assert!(job.merkle_branches.is_empty());
        assert_eq!(job.block_height, 100);

        let mut wb = WorkBuilder::new("", SOLO_EXTRANONCE2_SIZE);
        wb.set_version_mask(BIP320_VERSION_MASK);
        let work = wb.next_work(&job);
        assert_eq!(work.version_mask, BIP320_VERSION_MASK);

        let decoded = wb.decode_coinbase(&job, 0).expect("decode");
        assert_eq!(decoded.outputs.len(), 2);
        // Regtest height 100 < 150 → full 50 BTC subsidy
        assert_eq!(
            decoded.outputs[0].value_sats,
            block_subsidy_sats_with_interval(100, REGTEST_HALVING_INTERVAL)
        );
        assert_eq!(
            decoded.outputs[0].script_hex,
            "0014751e76e8199196d454941c45d1b3a323f1433bd6"
        );
        assert!(decoded.outputs[1].script_hex.starts_with("6a24aa21a9ed"));
    }

    #[test]
    fn regtest_builder_uses_interval_150_for_height_150() {
        let b = SoloTemplateBuilder::from_script_hex("51")
            .unwrap()
            .for_chain(ChainId::Regtest);
        let job = b.build_job(&regtest_tip(150)).unwrap();
        let wb = WorkBuilder::new("", 4);
        let d = wb.decode_coinbase(&job, 0).unwrap();
        assert_eq!(d.outputs[0].value_sats, 2_500_000_000);
    }

    #[test]
    fn p2pkh_payout_in_coinbase() {
        let b = SoloTemplateBuilder::from_address("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
            .unwrap()
            .for_chain(ChainId::Mainnet);
        // Mainnet builder rejects toy nBits — use diff1-ish hard enough target.
        let tip = SoloTip {
            prev_hash: [0xab; 32],
            nbits: 0x1d00_ffff, // difficulty 1 — not toy
            ntime: 1_700_000_000,
            height: 1,
        };
        let job = b.build_job(&tip).unwrap();
        let wb = WorkBuilder::new("", SOLO_EXTRANONCE2_SIZE);
        let decoded = wb.decode_coinbase(&job, 0).unwrap();
        assert_eq!(
            decoded.outputs[0].script_hex,
            "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac"
        );
    }

    #[test]
    fn validate_tip_regtest_allows_easy_nbits() {
        assert!(validate_tip(&regtest_tip(0)).is_ok());
        let mut tip = regtest_tip(10);
        tip.nbits = 0;
        assert!(matches!(validate_tip(&tip), Err(SoloError::TipRejected(_))));
        tip = regtest_tip(-1);
        assert!(matches!(validate_tip(&tip), Err(SoloError::TipRejected(_))));
    }

    #[test]
    fn mainnet_refuses_toy_nbits() {
        let tip = regtest_tip(10); // 0x207fffff is toy
        assert!(matches!(
            validate_tip_for_chain(&tip, SoloChainParams::mainnet()),
            Err(SoloError::TipRejected(_))
        ));
        let ok = SoloTip {
            prev_hash: [0; 32],
            nbits: 0x1d00_ffff,
            ntime: 1,
            height: 1,
        };
        assert!(validate_tip_for_chain(&ok, SoloChainParams::mainnet()).is_ok());
    }

    #[test]
    fn validate_found_block_binds_tip() {
        let b = SoloTemplateBuilder::from_script_hex("51").unwrap();
        let tip = regtest_tip(9);
        let (job, work) = b.build_work(&tip).unwrap();
        let nw = assemble_coinbase_nonwitness(&job, &[], &en2_zeros(4)).unwrap();
        let header = header_from_work(&work, 0, work.version);
        // Wrong prev
        let mut tip2 = tip;
        tip2.prev_hash = [0x00; 32];
        assert!(matches!(
            validate_found_block(&tip2, &header, &nw),
            Err(SoloError::BlockRejected(_))
        ));
        // Merkle mismatch
        let bad_cb = vec![0u8; nw.len()];
        assert!(matches!(
            validate_found_block(&tip, &header, &bad_cb),
            Err(SoloError::BlockRejected(_))
        ));
        // Correct binding for merkle/prev/nbits; PoW may still fail on this nonce
        match validate_found_block(&tip, &header, &nw) {
            Ok(_) => {}
            Err(SoloError::BlockRejected(s)) => {
                assert_eq!(s, "hash does not meet nbits target");
            }
            Err(e) => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn validate_found_header_rejects_weak_hash() {
        let mut header = [0u8; 80];
        header[72..76].copy_from_slice(&0x0300_0001_u32.to_le_bytes());
        assert!(matches!(
            validate_found_header(&header),
            Err(SoloError::BlockRejected(_))
        ));
    }

    #[test]
    fn tip_supersedes_by_height() {
        let a = regtest_tip(10);
        let b = regtest_tip(11);
        assert!(tip_supersedes(&a, &b));
        assert!(!tip_supersedes(&b, &a));
        assert!(!tip_supersedes(&a, &a));
    }

    #[test]
    fn rolled_version_applies_mask() {
        let base = 0x2000_0000;
        let bits = 0x1fff_e000;
        assert_eq!(
            rolled_version(base, BIP320_VERSION_MASK, bits),
            base | (bits & BIP320_VERSION_MASK)
        );
    }

    #[test]
    fn address_for_chain_rejects_cross_network() {
        assert!(matches!(
            SoloTemplateBuilder::from_address_for_chain(
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
                ChainId::Regtest
            ),
            Err(SoloError::BadAddress)
        ));
        assert!(SoloTemplateBuilder::from_address_for_chain(
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            ChainId::Mainnet
        )
        .is_ok());
    }

    #[test]
    fn bad_address_refused() {
        assert!(matches!(
            SoloTemplateBuilder::from_address("not-valid"),
            Err(SoloError::BadAddress)
        ));
    }

    #[test]
    fn compact_target_diff1_shape() {
        let t = compact_target_be(0x1d00_ffff).unwrap();
        assert_eq!(t[4], 0xff);
        assert_eq!(t[5], 0xff);
        assert!(t[0..4].iter().all(|&b| b == 0));
    }

    #[test]
    fn header_from_work_fields_line_up() {
        let b = SoloTemplateBuilder::from_script_hex("51").unwrap();
        let (_, work) = b.build_work(&regtest_tip(7)).unwrap();
        let header = header_from_work(&work, 0x1234_5678, work.version);
        assert_eq!(&header[76..80], &0x1234_5678_u32.to_le_bytes());
        assert_eq!(&header[4..36], &work.prev_block_hash);
    }

    #[test]
    fn parse_coinbase_via_assembled_tx() {
        let b = SoloTemplateBuilder::from_address("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .unwrap();
        let job = b.build_job(&regtest_tip(50)).unwrap();
        let nw = assemble_coinbase_nonwitness(&job, &[], &en2_zeros(4)).unwrap();
        let decoded = parse_coinbase(&nw).expect("non-witness solo coinbase must parse");
        assert_eq!(decoded.outputs.len(), 2);
    }
}
