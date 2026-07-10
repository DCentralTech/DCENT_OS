//! W4.5 heartbeat-field derivations (dcent-pack Change B, staged).
//!
//! These are the **pure** derivation functions that turn raw mining/job data
//! into the real values feeding the ExpansionPack bridge heartbeat's optional
//! Change-B fields (`dcent-expansion-pack/docs/V0.2_DCENTOS_CHANGES.md` +
//! `docs/MESH_MODULE.md`). Every function here is side-effect-free and
//! host-testable — the wiring that reads live share history / the current job
//! and pushes the results into `MinerStatusProvider` lives in the daemon
//! (`dcentrald::bridge_glue`) and stays `None` until it sources these outputs.
//!
//! Three derivations:
//!
//! 1. [`coinbase_height`] — BIP34 block height decoded from the coinbase
//!    transaction's scriptSig. This is **job-metadata provenance**: it is the
//!    height the *pool asserts* in the job it handed us, NOT an independently
//!    validated chain tip. It must be labelled as job-derived and MUST NOT be
//!    conflated with the honest `/api/network/block` source (which reports a
//!    verified network block). Feeds the heartbeat `block_height` field as
//!    "best-block height context", clearly job-sourced.
//! 2. [`session_best_difficulty`] / [`format_difficulty`] — the session-best
//!    (highest) locally-proven achieved share difficulty, rendered as the
//!    compact free-form string the `best_difficulty` field carries (e.g.
//!    `"184.2M"`).
//! 3. [`share_meets_network_target`] — the block-found predicate: does an
//!    achieved share difficulty meet the network target implied by `nbits`?
//!    On `true`, the daemon emits its block-found webhook and latches
//!    `block_found_height`.
//!
//! Difficulty math reuses the canonical [`crate::v1::difficulty`] primitives so
//! the units stay consistent with the rest of the share pipeline (pdiff / the
//! `2^224` convention shared by `hash_to_difficulty`).

use crate::v1::difficulty::{hash_to_difficulty, nbits_to_target};

// ---------------------------------------------------------------- BIP34 height

/// Read a Bitcoin CompactSize (varint) at `*pos`, advancing `*pos` past it.
///
/// Encoding: `< 0xFD` ⇒ 1 byte; `0xFD` ⇒ next 2 bytes LE; `0xFE` ⇒ next 4 bytes
/// LE; `0xFF` ⇒ next 8 bytes LE. Returns `None` on truncation.
fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let first = *buf.get(*pos)?;
    *pos += 1;
    let val = match first {
        0xFD => {
            let bytes = buf.get(*pos..*pos + 2)?;
            *pos += 2;
            u16::from_le_bytes([bytes[0], bytes[1]]) as u64
        }
        0xFE => {
            let bytes = buf.get(*pos..*pos + 4)?;
            *pos += 4;
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64
        }
        0xFF => {
            let bytes = buf.get(*pos..*pos + 8)?;
            *pos += 8;
            u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        }
        n => n as u64,
    };
    Some(val)
}

/// Decode the BIP34 block height from a coinbase transaction.
///
/// Accepts EITHER a full legacy-serialized coinbase transaction OR the Stratum
/// `coinbase1` prefix — the height sits at the very start of the coinbase
/// scriptSig, which is always before the extranonce insertion point, so both
/// share the required prefix bytes.
///
/// BIP34 mandates the coinbase scriptSig begins with the block height pushed as
/// a minimally-encoded little-endian `CScriptNum`: a single push-length byte
/// `n` (1..=8 in practice; `0x03` for current mainnet heights) followed by `n`
/// little-endian height bytes. The minimal encoding appends a `0x00` sign byte
/// when the top data byte has its high bit set; reading `n` LE bytes handles
/// that transparently (the extra byte is a zero high byte).
///
/// Structural walk of the (legacy) coinbase tx:
/// `version(4) | txin_count(varint=1) | prevout_hash(32) | prevout_index(4) |
///  scriptSig_len(varint) | scriptSig[ push_len(1) height_LE(push_len) … ]`.
///
/// Returns `None` if the bytes are truncated, the txin count is 0, the scriptSig
/// is empty, or the first scriptSig byte is not a simple 1..=8-byte push (e.g.
/// `OP_0` or an `OP_PUSHDATA*` opcode) — i.e. anything that is not a
/// well-formed BIP34 height. **Provenance note:** the returned height is the
/// pool's *claim* from the job; treat it as job-derived metadata, never as a
/// validated chain tip (do not conflate with `/api/network/block`).
pub fn coinbase_height(coinbase_tx: &[u8]) -> Option<u64> {
    let mut pos = 0usize;

    // version (4 bytes)
    pos = pos.checked_add(4)?;
    if pos > coinbase_tx.len() {
        return None;
    }

    // txin count (varint) — a coinbase always has exactly one input.
    let txin_count = read_varint(coinbase_tx, &mut pos)?;
    if txin_count == 0 {
        return None;
    }

    // prevout: 32-byte hash (all zero) + 4-byte index (0xFFFFFFFF).
    pos = pos.checked_add(36)?;
    if pos > coinbase_tx.len() {
        return None;
    }

    // scriptSig length (varint).
    let script_len = read_varint(coinbase_tx, &mut pos)? as usize;
    if script_len == 0 {
        return None;
    }

    // First scriptSig byte = the height push opcode.
    let push_len = *coinbase_tx.get(pos)? as usize;
    // Only a direct push of 1..=8 bytes is a decodable BIP34 height. 0x00
    // (OP_0) and anything >= 0x4c (OP_PUSHDATA1/2/4, non-push opcodes) are not.
    if !(1..=8).contains(&push_len) {
        return None;
    }
    // The push (opcode + data) must fit inside the declared scriptSig.
    if push_len + 1 > script_len {
        return None;
    }

    let data_start = pos + 1;
    let data_end = data_start.checked_add(push_len)?;
    let height_bytes = coinbase_tx.get(data_start..data_end)?;

    let mut height: u64 = 0;
    for (i, &b) in height_bytes.iter().enumerate() {
        height |= (b as u64) << (8 * i);
    }
    Some(height)
}

// --------------------------------------------------------- difficulty rendering

/// Render a difficulty value as a compact human string (the shape the bridge
/// `best_difficulty` field carries, e.g. `184_200_000.0 → "184.2M"`).
///
/// Uses ~4 significant digits with SI-style 1000-scaling suffixes
/// (`"" K M G T P E`), trimming trailing zeros. Non-finite or non-positive
/// inputs render as `"0"` (defensive — the session helpers below suppress the
/// "no shares yet" case to `None` before this is called, so a real caller only
/// formats a genuine positive difficulty).
pub fn format_difficulty(difficulty: f64) -> String {
    if !difficulty.is_finite() || difficulty <= 0.0 {
        return "0".to_string();
    }

    const SUFFIXES: [&str; 7] = ["", "K", "M", "G", "T", "P", "E"];
    let mut tier = 0usize;
    let mut scaled = difficulty;
    while scaled >= 1000.0 && tier + 1 < SUFFIXES.len() {
        scaled /= 1000.0;
        tier += 1;
    }

    // Choose decimals for ~4 significant figures. For the un-suffixed tier we
    // bias to whole numbers (difficulties there are exact integers like 512).
    let decimals = if tier == 0 {
        if scaled >= 100.0 {
            0
        } else if scaled >= 10.0 {
            1
        } else {
            2
        }
    } else if scaled >= 100.0 {
        1
    } else if scaled >= 10.0 {
        2
    } else {
        3
    };

    let mut s = format!("{:.*}", decimals, scaled);
    if s.contains('.') {
        // Trim trailing zeros and a bare trailing dot (e.g. "50.00" → "50").
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    s.push_str(SUFFIXES[tier]);
    s
}

/// The session-best (highest) achieved share difficulty from a slice of
/// per-share achieved difficulties.
///
/// Each entry is `Some(diff)` when a share's difficulty was locally proven from
/// the exact accepted header/hash, or `None` when unknown (mirrors
/// `RecentShareEvent.difficulty`, which is never the pool target). Non-finite /
/// non-positive entries are ignored. Returns `None` when no share has a proven
/// positive difficulty yet — the caller keeps `best_difficulty` unset (never a
/// fabricated `0`).
pub fn session_best_difficulty(achieved: &[Option<f64>]) -> Option<f64> {
    achieved
        .iter()
        .filter_map(|&d| d)
        .filter(|d| d.is_finite() && *d > 0.0)
        .fold(None, |acc, d| match acc {
            Some(m) if m >= d => Some(m),
            _ => Some(d),
        })
}

/// [`session_best_difficulty`] rendered through [`format_difficulty`]; `None`
/// when there is no proven session-best yet.
pub fn session_best_difficulty_string(achieved: &[Option<f64>]) -> Option<String> {
    session_best_difficulty(achieved).map(format_difficulty)
}

// ------------------------------------------------------------ block-found gate

/// The network difficulty implied by a compact `nbits`, in the pdiff /
/// `2^224` convention used by [`hash_to_difficulty`] (so it is directly
/// comparable to a share's achieved difficulty from the same helper).
///
/// A malformed / zero `nbits` yields a zero network target
/// ([`nbits_to_target`] returns `[0; 32]`) → `hash_to_difficulty` returns
/// `INFINITY`, which the block-found predicate treats as fail-closed.
pub fn network_difficulty_from_nbits(nbits: u32) -> f64 {
    hash_to_difficulty(&nbits_to_target(nbits))
}

/// Block-found predicate: does an achieved share difficulty meet the network
/// target implied by `nbits`?
///
/// `true` iff the share's proven achieved difficulty is `>=` the network
/// difficulty from `nbits` (equivalently, the share hash `<=` the network
/// target). Because both sides use the same `hash_to_difficulty` convention the
/// difficulty units cancel, so this is exactly `hash <= network_target`.
///
/// Fail-closed on malformed input: a non-finite / non-positive achieved
/// difficulty, or an `nbits` whose network difficulty is non-finite / non-
/// positive, returns `false` (never a false block-found).
///
/// NOTE: achieved difficulty is a coarse (top-bytes) estimate via
/// `hash_to_difficulty`; this predicate is the block-*candidate* trigger. The
/// definitive on-submit check is the exact big-endian
/// `meets_target(hash, nbits_to_target(nbits))` compare over the full header
/// hash — which the share validator already performs.
pub fn share_meets_network_target(achieved_difficulty: f64, nbits: u32) -> bool {
    if !achieved_difficulty.is_finite() || achieved_difficulty <= 0.0 {
        return false;
    }
    let network = network_difficulty_from_nbits(nbits);
    network.is_finite() && network > 0.0 && achieved_difficulty >= network
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ------------------------------------------------------------- BIP34 tests

    /// Build a legacy-serialized coinbase transaction around a scriptSig.
    /// `version` lets us prove both v1 (`01000000`) and v2 (`02000000`) parse.
    fn coinbase_with(version: u32, scriptsig: &[u8]) -> Vec<u8> {
        let mut tx = Vec::new();
        tx.extend_from_slice(&version.to_le_bytes()); // version
        tx.push(0x01); // txin count = 1
        tx.extend_from_slice(&[0u8; 32]); // prevout hash (coinbase = all zero)
        tx.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // prevout index
        assert!(scriptsig.len() < 0xFD, "test scriptsig fits a 1-byte varint");
        tx.push(scriptsig.len() as u8); // scriptSig length
        tx.extend_from_slice(scriptsig);
        tx.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // sequence
        // (outputs + locktime omitted — the height parse never reaches them)
        tx
    }

    /// A realistic coinbase scriptSig: `push_len | height_LE | <extranonce/tag>`.
    fn scriptsig_for_height(push: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        s.push(push.len() as u8); // push opcode = number of height bytes
        s.extend_from_slice(push); // little-endian height
        s.extend_from_slice(tail); // extranonce + pool tag (ignored by parser)
        s
    }

    #[test]
    fn coinbase_height_real_mainnet_vectors() {
        // Each `push` is the exact BIP34 little-endian height encoding a real
        // mainnet coinbase carries. Heights: 227836 (BIP34 activation height),
        // 277316 (the widely-cited tutorial block, scriptSig `03443b04…`),
        // 500000, 840000 (the 4th halving block).
        let cases: &[(&[u8], u64)] = &[
            (&[0xFC, 0x79, 0x03], 227_836), // 0x0379FC
            (&[0x44, 0x3B, 0x04], 277_316), // 0x043B44
            (&[0x20, 0xA1, 0x07], 500_000), // 0x07A120
            (&[0x40, 0xD1, 0x0C], 840_000), // 0x0CD140
        ];
        // Pool/extranonce tail bytes that a real coinbase carries after the
        // height push (mimics extranonce1 || extranonce2 || "/pool/").
        let tail = hex::decode("0000000000000000112f736c7573682f").unwrap();
        for &(push, height) in cases {
            let scriptsig = scriptsig_for_height(push, &tail);
            let tx = coinbase_with(1, &scriptsig);
            assert_eq!(
                coinbase_height(&tx),
                Some(height),
                "height {height} did not decode from {}",
                hex::encode(&tx)
            );
            // v2 coinbase serialization parses identically.
            let tx_v2 = coinbase_with(2, &scriptsig);
            assert_eq!(coinbase_height(&tx_v2), Some(height));
        }
    }

    #[test]
    fn coinbase_height_famous_277316_exact_scriptsig_prefix() {
        // The canonical `03 44 3b 04` scriptSig prefix from block 277316's
        // coinbase, followed by that block's real timestamp/extranonce bytes
        // (only the leading 4 push bytes drive the decode).
        let scriptsig = hex::decode("03443b0403858402062f503253482f").unwrap();
        let tx = coinbase_with(1, &scriptsig);
        assert_eq!(coinbase_height(&tx), Some(277_316));
    }

    #[test]
    fn coinbase_height_accepts_stratum_coinbase1_prefix() {
        // Stratum `coinbase1` = the coinbase tx bytes up to the extranonce
        // insertion point. The height sits before that point, so passing just
        // the coinbase1 prefix (no sequence/outputs) still decodes.
        let scriptsig_prefix_len = 0x25u8; // pool sets a longer scriptSig; only the length + first push matter
        let mut coinbase1 = Vec::new();
        coinbase1.extend_from_slice(&[0x02, 0, 0, 0]); // version 2
        coinbase1.push(0x01); // txin count
        coinbase1.extend_from_slice(&[0u8; 32]); // prevout hash
        coinbase1.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // prevout index
        coinbase1.push(scriptsig_prefix_len); // full scriptSig length
        coinbase1.extend_from_slice(&[0x03, 0x40, 0xD1, 0x0C]); // push3 + height 840000 LE
        coinbase1.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // start of extranonce (cut off here)
        assert_eq!(coinbase_height(&coinbase1), Some(840_000));
    }

    #[test]
    fn coinbase_height_four_byte_push_forward_compat() {
        // Heights above 0xFFFFFF (16,777,215) need a 4-byte push. 16,777,216 =
        // 0x01000000 → LE `00 00 00 01`. Proves the multi-byte LE assembly.
        let scriptsig = scriptsig_for_height(&[0x00, 0x00, 0x00, 0x01], &[0xAA, 0xBB]);
        let tx = coinbase_with(1, &scriptsig);
        assert_eq!(coinbase_height(&tx), Some(16_777_216));
    }

    #[test]
    fn coinbase_height_sign_padded_minimal_encoding() {
        // A height whose top data byte has the high bit set gets a 0x00 sign
        // byte appended (CScriptNum minimal encoding). Height 128 = 0x80 →
        // push2 `80 00`. Reading 2 LE bytes yields 128, not 32768.
        let scriptsig = scriptsig_for_height(&[0x80, 0x00], &[]);
        let tx = coinbase_with(1, &scriptsig);
        assert_eq!(coinbase_height(&tx), Some(128));
    }

    #[test]
    fn coinbase_height_rejects_malformed() {
        // Empty / truncated inputs.
        assert_eq!(coinbase_height(&[]), None);
        assert_eq!(coinbase_height(&[0x01, 0x00, 0x00]), None); // < version
        assert_eq!(coinbase_height(&[0x01, 0x00, 0x00, 0x00]), None); // version only, no txin count

        // Zero-length scriptSig.
        let mut zero_script = Vec::new();
        zero_script.extend_from_slice(&[0x01, 0, 0, 0]);
        zero_script.push(0x01);
        zero_script.extend_from_slice(&[0u8; 32]);
        zero_script.extend_from_slice(&[0xFF; 4]);
        zero_script.push(0x00); // scriptSig length 0
        assert_eq!(coinbase_height(&zero_script), None);

        // OP_0 (0x00) as the first scriptSig byte is not a decodable height.
        let op0 = coinbase_with(1, &[0x00, 0x11]);
        assert_eq!(coinbase_height(&op0), None);

        // OP_PUSHDATA1 (0x4c) is not a simple 1..=8 push.
        let pushdata1 = coinbase_with(1, &[0x4C, 0x03, 0x20, 0xA1, 0x07]);
        assert_eq!(coinbase_height(&pushdata1), None);

        // Push claims 5 bytes but the scriptSig only carries 2 → reject.
        // scriptSig = [0x05, 0x01, 0x02] (len 3, push says 5).
        let overrun = coinbase_with(1, &[0x05, 0x01, 0x02]);
        assert_eq!(coinbase_height(&overrun), None);

        // Truncated right after the push opcode (no height bytes present).
        let mut truncated = Vec::new();
        truncated.extend_from_slice(&[0x01, 0, 0, 0]);
        truncated.push(0x01);
        truncated.extend_from_slice(&[0u8; 32]);
        truncated.extend_from_slice(&[0xFF; 4]);
        truncated.push(0x03); // scriptSig length says 3
        truncated.push(0x03); // push opcode says 3 data bytes … but buffer ends
        assert_eq!(coinbase_height(&truncated), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn coinbase_height_never_panics_on_arbitrary_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..512)
        ) {
            let _ = coinbase_height(&data);
        }
    }

    // -------------------------------------------------------- format_difficulty

    #[test]
    fn format_difficulty_compact_vectors() {
        // Non-positive / non-finite → "0".
        assert_eq!(format_difficulty(0.0), "0");
        assert_eq!(format_difficulty(-5.0), "0");
        assert_eq!(format_difficulty(f64::NAN), "0");
        assert_eq!(format_difficulty(f64::INFINITY), "0");
        assert_eq!(format_difficulty(f64::NEG_INFINITY), "0");

        // Un-suffixed tier.
        assert_eq!(format_difficulty(1.0), "1");
        assert_eq!(format_difficulty(1.5), "1.5");
        assert_eq!(format_difficulty(512.0), "512");
        assert_eq!(format_difficulty(999.0), "999");

        // K / M / G / T tiers.
        assert_eq!(format_difficulty(1_000.0), "1K");
        assert_eq!(format_difficulty(12_500.0), "12.5K");
        assert_eq!(format_difficulty(21_300.0), "21.3K");
        assert_eq!(format_difficulty(50_000.0), "50K");
        assert_eq!(format_difficulty(1_500_000.0), "1.5M");
        // The doc example (`docs/V0.2_DCENTOS_CHANGES.md`): 184.2M.
        assert_eq!(format_difficulty(184_200_000.0), "184.2M");
        assert_eq!(format_difficulty(2_500_000_000.0), "2.5G");
        assert_eq!(format_difficulty(2_500_000_000_000.0), "2.5T");
        assert_eq!(format_difficulty(5.0e18), "5E");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn format_difficulty_never_panics_and_is_nonempty(d in any::<f64>()) {
            let s = format_difficulty(d);
            prop_assert!(!s.is_empty());
        }
    }

    // ----------------------------------------------------- session-best helpers

    #[test]
    fn session_best_difficulty_picks_max_finite_positive() {
        let history = [
            Some(256.0),
            None,
            Some(1_048_576.0),
            Some(f64::NAN),
            Some(-1.0),
            Some(65_536.0),
            Some(f64::INFINITY),
        ];
        assert_eq!(session_best_difficulty(&history), Some(1_048_576.0));
        assert_eq!(
            session_best_difficulty_string(&history),
            Some("1.049M".to_string())
        );
    }

    #[test]
    fn session_best_difficulty_none_when_no_proven_share() {
        // Empty, all-None, and all-invalid windows produce None (never a
        // fabricated 0 / pool-target value on the wire).
        assert_eq!(session_best_difficulty(&[]), None);
        assert_eq!(session_best_difficulty(&[None, None]), None);
        assert_eq!(
            session_best_difficulty(&[Some(f64::NAN), Some(0.0), Some(-3.0)]),
            None
        );
        assert_eq!(session_best_difficulty_string(&[None]), None);
    }

    // ----------------------------------------------------- block-found predicate

    #[test]
    fn network_difficulty_from_nbits_genesis_is_about_one() {
        // Genesis nbits 0x1d00ffff ⇒ network difficulty ≈ 1 (pdiff convention).
        let d = network_difficulty_from_nbits(0x1d00ffff);
        assert!((d - 1.0).abs() < 0.01, "genesis network difficulty {d} != ~1");
    }

    #[test]
    fn share_meets_network_target_genesis_boundary() {
        // At genesis difficulty (~1.0000153), a share proven above it is a
        // block; one below it is not.
        assert!(share_meets_network_target(2.0, 0x1d00ffff));
        assert!(share_meets_network_target(1_000.0, 0x1d00ffff));
        assert!(!share_meets_network_target(0.5, 0x1d00ffff));
    }

    #[test]
    fn share_meets_network_target_modern_difficulty() {
        // A modern nbits (network difficulty ~1e14). A normal pool share
        // (achieved ~65536) is NOT a block; an astronomically lucky share is.
        let nbits = 0x1703_4219;
        let net = network_difficulty_from_nbits(nbits);
        assert!(net.is_finite() && net > 1e12, "modern net diff {net}");
        assert!(!share_meets_network_target(65_536.0, nbits));
        assert!(share_meets_network_target(net * 1.5, nbits));
        assert!(share_meets_network_target(net, nbits)); // exact meet counts
    }

    #[test]
    fn share_meets_network_target_fails_closed_on_malformed() {
        // Non-finite / non-positive achieved difficulty never claims a block.
        assert!(!share_meets_network_target(f64::NAN, 0x1d00ffff));
        assert!(!share_meets_network_target(f64::INFINITY, 0x1d00ffff));
        assert!(!share_meets_network_target(0.0, 0x1d00ffff));
        assert!(!share_meets_network_target(-1.0, 0x1d00ffff));
        // Malformed nbits (zero / negative-flag / oversize exponent) ⇒ zero
        // target ⇒ INFINITY network difficulty ⇒ fail closed.
        assert!(!share_meets_network_target(1e300, 0x00000000));
        assert!(!share_meets_network_target(1e300, 0x1d800000)); // negative flag
        assert!(!share_meets_network_target(1e300, 0xff00ffff)); // exp > 32
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn share_meets_network_target_never_panics(
            achieved in any::<f64>(),
            nbits in any::<u32>()
        ) {
            let _ = share_meets_network_target(achieved, nbits);
        }
    }
}
