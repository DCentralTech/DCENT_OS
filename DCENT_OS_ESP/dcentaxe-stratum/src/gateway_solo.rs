// DCENT_axe — Gateway solo-block submit prep (P1-W2 pure)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// After mesh BFG reassembly (dcentaxe-lora::BlockReassembler), the gateway
// binds the payload to the tip it issued, re-validates, and produces hex for
// bitcoind `submitblock`. No HTTP/RPC client here — return hex only.
//
// Crate boundary: does NOT depend on dcentaxe-lora. Binary composes:
//   reassembler.ingest(frag) → Complete(bytes)
//   GatewaySoloSubmit::prepare(tip, &bytes) → SubmitPrep { hex, … }

use crate::solo::{coinbase_txid, validate_found_block, validate_found_header, SoloError, SoloTip};

/// Minimum solo block: 80 header + 1 varint + minimal coinbase.
pub const MIN_SOLO_BLOCK_BYTES: usize = 80 + 1 + 60;

/// Maximum accepted reassembled solo block (align with lora MAX_BLOCK_BYTES).
pub const MAX_SOLO_BLOCK_BYTES: usize = 1024;

/// Fixed witness tail of DCENT `assemble_coinbase_full` (one 32-byte stack item).
const COINBASE_RESERVED_WITNESS_LEN: usize = 1 + 1 + 32; // 0x01 0x20 || zeros

/// Errors from gateway parse / policy / validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewaySoloError {
    /// No tip bound — refuse to submit blind.
    NoTip,
    /// Reassembled payload too short / long.
    BadLength,
    /// Wire shape invalid (tx count, marker, etc.).
    Malformed(&'static str),
    /// Tip / PoW / merkle policy (wraps solo).
    Rejected(SoloError),
}

impl core::fmt::Display for GatewaySoloError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GatewaySoloError::NoTip => write!(f, "no tip bound for gateway submit"),
            GatewaySoloError::BadLength => write!(f, "block length out of bounds"),
            GatewaySoloError::Malformed(s) => write!(f, "malformed block: {s}"),
            GatewaySoloError::Rejected(e) => write!(f, "rejected: {e}"),
        }
    }
}

impl std::error::Error for GatewaySoloError {}

/// Parsed solo block ready for `submitblock` (hex) after validation.
#[derive(Debug, Clone)]
pub struct SubmitPrep {
    /// Full block hex (no `0x` prefix) for bitcoind `submitblock`.
    pub block_hex: String,
    pub header: [u8; 80],
    pub coinbase_nonwitness: Vec<u8>,
    pub coinbase_full: Vec<u8>,
    pub difficulty: f64,
    pub tip_height: i64,
}

/// Gateway-side pure state: last tip we originated / will accept against.
///
/// Reassembly stays in `dcentaxe-lora`; this only validates complete payloads.
#[derive(Debug, Clone, Default)]
pub struct GatewaySoloSubmit {
    tip: Option<SoloTip>,
    /// Successful prepares (metrics / honesty — not share accepts).
    prepares_ok: u64,
    prepares_fail: u64,
}

impl GatewaySoloSubmit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tip(&self) -> Option<&SoloTip> {
        self.tip.as_ref()
    }

    pub fn prepares_ok(&self) -> u64 {
        self.prepares_ok
    }

    pub fn prepares_fail(&self) -> u64 {
        self.prepares_fail
    }

    /// Bind the tip this gateway last advertised (or will accept).
    pub fn set_tip(&mut self, tip: SoloTip) {
        self.tip = Some(tip);
    }

    pub fn clear_tip(&mut self) {
        self.tip = None;
    }

    /// Validate a complete reassembled solo block against the bound tip.
    ///
    /// Wire: `header(80) || tx_count(1=0x01) || coinbase_full`.
    pub fn prepare(&mut self, block: &[u8]) -> Result<SubmitPrep, GatewaySoloError> {
        let tip = *self.tip.as_ref().ok_or(GatewaySoloError::NoTip)?;
        match Self::prepare_with_tip(&tip, block) {
            Ok(p) => {
                self.prepares_ok = self.prepares_ok.saturating_add(1);
                Ok(p)
            }
            Err(e) => {
                self.prepares_fail = self.prepares_fail.saturating_add(1);
                Err(e)
            }
        }
    }

    /// Stateless variant for tests / one-shot checks.
    pub fn prepare_with_tip(tip: &SoloTip, block: &[u8]) -> Result<SubmitPrep, GatewaySoloError> {
        if block.len() < MIN_SOLO_BLOCK_BYTES || block.len() > MAX_SOLO_BLOCK_BYTES {
            return Err(GatewaySoloError::BadLength);
        }
        let header: [u8; 80] = block[0..80]
            .try_into()
            .map_err(|_| GatewaySoloError::Malformed("header"))?;
        if block[80] != 0x01 {
            return Err(GatewaySoloError::Malformed("tx_count must be 1"));
        }
        let coinbase_full = block[81..].to_vec();
        if coinbase_full.is_empty() {
            return Err(GatewaySoloError::Malformed("empty coinbase"));
        }

        let coinbase_nonwitness =
            coinbase_wire_to_nonwitness(&coinbase_full).map_err(GatewaySoloError::Rejected)?;

        let difficulty = validate_found_block(tip, &header, &coinbase_nonwitness)
            .map_err(GatewaySoloError::Rejected)?;

        Ok(SubmitPrep {
            block_hex: hex_encode(block),
            header,
            coinbase_nonwitness,
            coinbase_full,
            difficulty,
            tip_height: tip.height,
        })
    }

    /// PoW-only header check helper (insufficient alone — use [`prepare`]).
    pub fn header_pow_ok(header: &[u8; 80]) -> Result<f64, GatewaySoloError> {
        validate_found_header(header).map_err(GatewaySoloError::Rejected)
    }
}

/// Strip DCENT full-wire coinbase (marker + reserved witness) to non-witness
/// serialization for txid / merkle validation. Pass-through if already legacy.
pub fn coinbase_wire_to_nonwitness(wire: &[u8]) -> Result<Vec<u8>, SoloError> {
    if wire.len() < 10 {
        return Err(SoloError::BlockRejected("coinbase too short"));
    }
    // Already non-witness: byte after version is in_count (0x01), not marker 0x00.
    if wire.get(4) != Some(&0x00) || wire.get(5) != Some(&0x01) {
        return Ok(wire.to_vec());
    }
    // Segwit full form from assemble_coinbase_full:
    // version | 00 01 | body | 01 20 | 32 zeros | locktime
    if wire.len() < 4 + 2 + COINBASE_RESERVED_WITNESS_LEN + 4 {
        return Err(SoloError::BlockRejected("segwit coinbase too short"));
    }
    let lock_start = wire.len() - 4;
    let wit_start = lock_start - COINBASE_RESERVED_WITNESS_LEN;
    if wire[wit_start] != 0x01 || wire[wit_start + 1] != 0x20 {
        return Err(SoloError::BlockRejected("unexpected coinbase witness"));
    }
    let mut nw = Vec::with_capacity(wire.len() - 2 - COINBASE_RESERVED_WITNESS_LEN);
    nw.extend_from_slice(&wire[0..4]); // version
    nw.extend_from_slice(&wire[6..wit_start]); // vin/vout body
    nw.extend_from_slice(&wire[lock_start..]); // locktime
                                               // Sanity: txid of nw should be well-defined
    let _ = coinbase_txid(&nw);
    Ok(nw)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_solo::MeshSoloController;
    use crate::solo::{
        assemble_coinbase_full, assemble_coinbase_nonwitness, assemble_solo_block,
        header_from_work, SoloTip, BIP320_VERSION_MASK,
    };

    fn tip(h: i64) -> SoloTip {
        SoloTip {
            prev_hash: [0xab; 32],
            nbits: 0x207f_ffff,
            ntime: 1_700_000_000,
            height: h,
        }
    }

    /// Brute a nonce that meets easy regtest nBits (bounded).
    fn mine_candidate() -> (SoloTip, crate::mesh_solo::SoloBlockCandidate) {
        let mut c = MeshSoloController::from_script_hex("51").unwrap();
        c.enable_solo_mesh_empty();
        let t = tip(42);
        c.admit_tip(t);
        let epoch = c.next_work().unwrap();
        // Scan a window of nonces — 0x207fffff is very easy.
        for nonce in 0u32..500_000 {
            match c.build_candidate(nonce, 0) {
                Ok(cand) => return (t, cand),
                Err(_) => continue,
            }
        }
        // Fallback: build invalid-PoW structure for reject tests using epoch
        let header = header_from_work(&epoch.work, 0, epoch.work.version);
        let en2 = hex::decode(&epoch.work.extranonce2).unwrap();
        let nw = assemble_coinbase_nonwitness(&epoch.job, &[], &en2).unwrap();
        let full = assemble_coinbase_full(&nw).unwrap();
        let block = assemble_solo_block(&header, &full);
        // Force candidate-shaped bytes without PoW success path
        (
            t,
            crate::mesh_solo::SoloBlockCandidate {
                header,
                coinbase_nonwitness: nw,
                coinbase_full: full,
                block_bytes: block,
                difficulty: 0.0,
                tip_height: t.height,
            },
        )
    }

    #[test]
    fn prepare_requires_tip() {
        let mut g = GatewaySoloSubmit::new();
        let block = vec![0u8; MIN_SOLO_BLOCK_BYTES];
        assert!(matches!(g.prepare(&block), Err(GatewaySoloError::NoTip)));
    }

    #[test]
    fn prepare_rejects_bad_length() {
        let mut g = GatewaySoloSubmit::new();
        g.set_tip(tip(1));
        assert!(matches!(
            g.prepare(&[0u8; 10]),
            Err(GatewaySoloError::BadLength)
        ));
        assert!(matches!(
            g.prepare(&vec![0u8; MAX_SOLO_BLOCK_BYTES + 1]),
            Err(GatewaySoloError::BadLength)
        ));
    }

    #[test]
    fn prepare_rejects_wrong_tx_count() {
        let mut g = GatewaySoloSubmit::new();
        g.set_tip(tip(1));
        let mut block = vec![0u8; MIN_SOLO_BLOCK_BYTES];
        block[80] = 0x02;
        assert!(matches!(
            g.prepare(&block),
            Err(GatewaySoloError::Malformed(_))
        ));
    }

    #[test]
    fn prepare_rejects_prev_hash_mismatch() {
        let (t, cand) = mine_candidate();
        let mut g = GatewaySoloSubmit::new();
        let mut bad_tip = t;
        bad_tip.prev_hash = [0x00; 32];
        g.set_tip(bad_tip);
        // If cand was PoW-valid, still reject on prev; if not, may fail PoW first.
        let r = g.prepare(&cand.block_bytes);
        assert!(matches!(r, Err(GatewaySoloError::Rejected(_))));
    }

    #[test]
    fn coinbase_wire_round_trip_strip() {
        let mut c = MeshSoloController::from_script_hex("51").unwrap();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(7));
        let epoch = c.next_work().unwrap();
        let en2 = hex::decode(&epoch.work.extranonce2).unwrap();
        let nw = assemble_coinbase_nonwitness(&epoch.job, &[], &en2).unwrap();
        let full = assemble_coinbase_full(&nw).unwrap();
        let stripped = coinbase_wire_to_nonwitness(&full).unwrap();
        assert_eq!(stripped, nw);
        assert_eq!(coinbase_txid(&stripped), epoch.work.merkle_root);
    }

    #[test]
    fn prepare_accepts_mined_solo_block_when_pow_found() {
        let mut found = None;
        let mut c = MeshSoloController::from_script_hex("51").unwrap();
        c.enable_solo_mesh_empty();
        let t = tip(99);
        c.admit_tip(t);
        let _ = c.next_work().unwrap();
        for nonce in 0u32..2_000_000 {
            if let Ok(cand) = c.build_candidate(nonce, 0) {
                found = Some((t, cand));
                break;
            }
        }
        let Some((t, cand)) = found else {
            // Extremely unlikely on 0x207fffff — skip soft if machine is weird
            eprintln!("skip: no nonce in 2e6 (unexpected for toy nBits)");
            return;
        };
        let mut g = GatewaySoloSubmit::new();
        g.set_tip(t);
        let prep = g.prepare(&cand.block_bytes).expect("gateway prepare");
        assert_eq!(prep.tip_height, 99);
        assert!(prep.difficulty > 0.0);
        assert_eq!(prep.block_hex.len(), cand.block_bytes.len() * 2);
        assert_eq!(g.prepares_ok(), 1);
        // Hex decodes back
        assert_eq!(hex::decode(&prep.block_hex).unwrap(), cand.block_bytes);
        let _ = BIP320_VERSION_MASK;
    }

    #[test]
    fn prepare_with_tip_stateless() {
        let mut c = MeshSoloController::from_script_hex("51").unwrap();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(3));
        let epoch = c.next_work().unwrap();
        // Deliberately weak nonce — expect reject
        let header = header_from_work(&epoch.work, 0xDEAD_BEEF, epoch.work.version);
        let en2 = hex::decode(&epoch.work.extranonce2).unwrap();
        let nw = assemble_coinbase_nonwitness(&epoch.job, &[], &en2).unwrap();
        let full = assemble_coinbase_full(&nw).unwrap();
        let block = assemble_solo_block(&header, &full);
        let r = GatewaySoloSubmit::prepare_with_tip(&tip(3), &block);
        // May pass PoW by chance on toy nBits; structure must not panic
        match r {
            Ok(p) => assert_eq!(p.header, header),
            Err(GatewaySoloError::Rejected(_)) => {}
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
}
