// DCENT_axe — Bitcoin address → scriptPubKey
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Port of the dashboard `addressToScriptHex` (block-tile.js) to pure Rust so
// Phase-4 solo templates can pay the node's own address without a JS runtime.
// Supports P2WPKH/P2WSH (bech32), P2TR (bech32m), P2PKH/P2SH (base58check).

use sha2::{Digest, Sha256};

/// Convert a mainnet/testnet address string to a scriptPubKey hex string
/// (no `0x` prefix). Returns `None` on unrecognized / checksum-fail input —
/// never panics.
pub fn address_to_script_hex(addr: &str) -> Option<String> {
    if addr.is_empty() {
        return None;
    }
    let lower = addr.to_ascii_lowercase();
    if lower.starts_with("bc1") || lower.starts_with("tb1") || lower.starts_with("bcrt1") {
        return segwit_address_to_script_hex(addr);
    }
    let first = addr.chars().next()?;
    // Mainnet P2PKH/P2SH and testnet equivalents (m/n/2).
    if matches!(first, '1' | '3' | 'm' | 'n' | '2') {
        return base58_address_to_script_hex(addr);
    }
    None
}

fn segwit_address_to_script_hex(addr: &str) -> Option<String> {
    let dec = bech32_decode(addr)?;
    if dec.data.is_empty() {
        return None;
    }
    let ver = dec.data[0];
    let prog = convert_bits(&dec.data[1..], 5, 8, false)?;
    if ver == 0 && dec.encoding != Bech32Encoding::Bech32 {
        return None;
    }
    if ver != 0 && dec.encoding != Bech32Encoding::Bech32m {
        return None;
    }
    if ver > 16 {
        return None;
    }
    // BIP-173: a witness program is 2..=40 bytes for EVERY witness version.
    // Without this, a checksum-valid future-version address with an out-of-range
    // program (e.g. empty) would be accepted as a coinbase payout script.
    if prog.len() < 2 || prog.len() > 40 {
        return None;
    }
    // BIP-141: a v0 program must be exactly 20 (P2WPKH) or 32 (P2WSH) bytes.
    if ver == 0 && prog.len() != 20 && prog.len() != 32 {
        return None;
    }
    // BIP-341: a v1 (P2TR) program must be exactly 32 bytes. A checksum-valid
    // v1 program of any other length is an UNENCUMBERED / anyone-can-spend
    // output — paying a solo block reward to such a script makes the reward
    // trivially stealable. Fail closed rather than emit a plausible script.
    if ver == 1 && prog.len() != 32 {
        return None;
    }
    // OP_0 = 0x00; OP_1..OP_16 = 0x51..0x60
    let op: u8 = if ver == 0 { 0x00 } else { 0x50 + ver };
    let mut script = Vec::with_capacity(2 + prog.len());
    script.push(op);
    script.push(prog.len() as u8);
    script.extend_from_slice(&prog);
    Some(hex_encode(&script))
}

fn base58_address_to_script_hex(addr: &str) -> Option<String> {
    let payload = base58check_decode(addr)?;
    if payload.len() != 21 {
        return None;
    }
    let ver = payload[0];
    let hash = &payload[1..];
    match ver {
        // Mainnet P2PKH / testnet P2PKH
        0x00 | 0x6f => {
            let mut s = Vec::with_capacity(25);
            s.extend_from_slice(&[0x76, 0xa9, 0x14]); // OP_DUP OP_HASH160 PUSH20
            s.extend_from_slice(hash);
            s.extend_from_slice(&[0x88, 0xac]); // OP_EQUALVERIFY OP_CHECKSIG
            Some(hex_encode(&s))
        }
        // Mainnet P2SH / testnet P2SH
        0x05 | 0xc4 => {
            let mut s = Vec::with_capacity(23);
            s.extend_from_slice(&[0xa9, 0x14]); // OP_HASH160 PUSH20
            s.extend_from_slice(hash);
            s.push(0x87); // OP_EQUAL
            Some(hex_encode(&s))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Bech32 / Bech32m (BIP-173 / BIP-350)
// ---------------------------------------------------------------------------

const BECH32_ALPHA: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bech32Encoding {
    Bech32,
    Bech32m,
}

struct Bech32Decoded {
    #[allow(dead_code)]
    hrp: String,
    data: Vec<u8>,
    encoding: Bech32Encoding,
}

fn bech32_polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ (v as u32);
        for (j, g) in GEN.iter().enumerate() {
            if ((top >> j) & 1) != 0 {
                chk ^= g;
            }
        }
    }
    chk
}

fn bech32_hrp_expand(hrp: &str) -> Vec<u8> {
    let mut ret = Vec::with_capacity(hrp.len() * 2 + 1);
    for b in hrp.bytes() {
        ret.push(b >> 5);
    }
    ret.push(0);
    for b in hrp.bytes() {
        ret.push(b & 31);
    }
    ret
}

fn bech32_decode(addr: &str) -> Option<Bech32Decoded> {
    // BIP-173: the overall bech32/bech32m string is capped at 90 characters.
    if addr.len() > 90 {
        return None;
    }
    let lower = addr.to_ascii_lowercase();
    // Reject mixed case.
    if addr.to_ascii_uppercase() != addr && addr.to_ascii_lowercase() != addr {
        // Allow pure lower or pure upper only.
        let has_lower = addr.bytes().any(|b| b.is_ascii_lowercase());
        let has_upper = addr.bytes().any(|b| b.is_ascii_uppercase());
        if has_lower && has_upper {
            return None;
        }
    }
    let pos = lower.rfind('1')?;
    if pos < 1 || pos + 7 > lower.len() {
        return None;
    }
    let hrp = lower[..pos].to_string();
    let mut data = Vec::new();
    for c in lower[pos + 1..].bytes() {
        let d = BECH32_ALPHA.iter().position(|&a| a == c)? as u8;
        data.push(d);
    }
    let mut values = bech32_hrp_expand(&hrp);
    values.extend_from_slice(&data);
    let chk = bech32_polymod(&values);
    let encoding = match chk {
        1 => Bech32Encoding::Bech32,
        0x2bc8_30a3 => Bech32Encoding::Bech32m,
        _ => return None,
    };
    if data.len() < 6 {
        return None;
    }
    data.truncate(data.len() - 6);
    Some(Bech32Decoded {
        hrp,
        data,
        encoding,
    })
}

fn convert_bits(data: &[u8], from_bits: u32, to_bits: u32, pad: bool) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut ret = Vec::new();
    let maxv = (1u32 << to_bits) - 1;
    for &v in data {
        if (v as u32) >> from_bits != 0 {
            return None;
        }
        acc = (acc << from_bits) | (v as u32);
        bits += from_bits;
        while bits >= to_bits {
            bits -= to_bits;
            ret.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits > 0 {
            ret.push(((acc << (to_bits - bits)) & maxv) as u8);
        }
    } else if bits >= from_bits || ((acc << (to_bits - bits)) & maxv) != 0 {
        return None;
    }
    Some(ret)
}

// ---------------------------------------------------------------------------
// Base58Check
// ---------------------------------------------------------------------------

const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58_decode(s: &str) -> Option<Vec<u8>> {
    // Big-endian base58 → bytes. Leading '1' characters map to 0x00 bytes.
    let mut acc: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let val = B58.iter().position(|&a| a == c)? as u32;
        let mut carry = val;
        for byte in acc.iter_mut().rev() {
            let v = (*byte as u32) * 58 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            acc.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let leading = s.bytes().take_while(|&c| c == b'1').count();
    let mut out = vec![0u8; leading];
    out.extend_from_slice(&acc);
    Some(out)
}

fn sha256d(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

fn base58check_decode(s: &str) -> Option<Vec<u8>> {
    let raw = base58_decode(s)?;
    if raw.len() < 5 {
        return None;
    }
    let (payload, checksum) = raw.split_at(raw.len() - 4);
    let hash = sha256d(payload);
    if checksum != &hash[..4] {
        return None;
    }
    Some(payload.to_vec())
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

    #[test]
    fn p2wpkh_genesis_pubkey_hash_vector() {
        // BIP-173 test vector: public key hash of the genesis coinbase.
        // bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4
        // → 0014751e76e8199196d454941c45d1b3a323f1433bd6
        let script =
            address_to_script_hex("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4").expect("p2wpkh");
        assert_eq!(script, "0014751e76e8199196d454941c45d1b3a323f1433bd6");
    }

    #[test]
    fn p2pkh_genesis_address() {
        // Satoshi's genesis address.
        let script = address_to_script_hex("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa").expect("p2pkh");
        assert_eq!(script, "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac");
    }

    #[test]
    fn p2sh_example() {
        // Valid mainnet P2SH (version 0x05 + hash160 000102…13 + base58check).
        let script = address_to_script_hex("31h38a54tFMrR8kzBnP2241MFD2EUHtGha").expect("p2sh");
        assert_eq!(script, "a914000102030405060708090a0b0c0d0e0f1011121387");
    }

    #[test]
    fn rejects_garbage() {
        assert!(address_to_script_hex("").is_none());
        assert!(address_to_script_hex("not-an-address").is_none());
        assert!(address_to_script_hex("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5").is_none());
        // Mixed case bech32 is invalid.
        assert!(
            address_to_script_hex("bc1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4").is_none()
                || address_to_script_hex("Bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4").is_none()
        );
    }

    #[test]
    fn upper_bech32_accepted() {
        // Pure uppercase is valid per BIP-173.
        let script = address_to_script_hex("BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4")
            .expect("upper bech32");
        assert_eq!(script, "0014751e76e8199196d454941c45d1b3a323f1433bd6");
    }

    /// Test-only bech32/bech32m encoder built from the module's own private
    /// helpers, so we can construct CHECKSUM-VALID addresses with arbitrary
    /// witness version + program length to exercise the validation that real
    /// wallets would never emit but a malicious/garbage input could.
    fn encode_segwit(hrp: &str, ver: u8, prog: &[u8]) -> String {
        let mut data = vec![ver];
        data.extend(convert_bits(prog, 8, 5, true).expect("convert_bits pad"));
        // Checksum constant: v0 = bech32 (1), v1..16 = bech32m (0x2bc830a3).
        let konst: u32 = if ver == 0 { 1 } else { 0x2bc8_30a3 };
        let mut values = bech32_hrp_expand(hrp);
        values.extend_from_slice(&data);
        values.extend_from_slice(&[0u8; 6]);
        let polymod = bech32_polymod(&values) ^ konst;
        let mut checksum = [0u8; 6];
        for (i, c) in checksum.iter_mut().enumerate() {
            *c = ((polymod >> (5 * (5 - i))) & 31) as u8;
        }
        let mut s = String::from(hrp);
        s.push('1');
        for &d in data.iter().chain(checksum.iter()) {
            s.push(BECH32_ALPHA[d as usize] as char);
        }
        s
    }

    #[test]
    fn p2tr_valid_program_builds_correct_script() {
        // A v1 / 32-byte program is P2TR: OP_1 (0x51) PUSH32 (0x20) <32 bytes>.
        // Round-trips through the module's production bech32m decode (the
        // 0x2bc830a3 constant), so this validates both the checksum path and the
        // byte-exact script construction for the witness-v1 branch.
        let prog = [0xABu8; 32];
        let addr = encode_segwit("bc", 1, &prog);
        let script = address_to_script_hex(&addr).expect("valid p2tr must parse");
        let expected = format!("5120{}", hex_encode(&prog));
        assert_eq!(script, expected);
    }

    #[test]
    fn encoder_roundtrips_valid_programs() {
        // Sanity that the test encoder produces addresses the parser accepts for
        // IN-RANGE programs (so the negative cases below fail on length, not on a
        // broken encoder): v1/32 (P2TR), and future v2 at the 2- and 40-byte bounds.
        assert!(address_to_script_hex(&encode_segwit("bc", 1, &[0x22; 32])).is_some());
        assert!(address_to_script_hex(&encode_segwit("bc", 2, &[0x33; 2])).is_some());
        assert!(address_to_script_hex(&encode_segwit("bc", 2, &[0x44; 40])).is_some());
    }

    #[test]
    fn rejects_invalid_witness_programs_fail_closed() {
        // v1 (P2TR) with a 20-byte program: NOT 32 bytes → anyone-can-spend → reject.
        assert!(
            address_to_script_hex(&encode_segwit("bc", 1, &[0x11; 20])).is_none(),
            "v1/20-byte program must be rejected (anyone-can-spend)"
        );
        // v1 with an empty program → below the BIP-173 minimum → reject.
        assert!(address_to_script_hex(&encode_segwit("bc", 1, &[])).is_none());
        // v2 with a 1-byte program → below BIP-173 min (2) → reject.
        assert!(address_to_script_hex(&encode_segwit("bc", 2, &[0x11])).is_none());
        // v16 with a 41-byte program → above BIP-173 max (40) → reject.
        assert!(address_to_script_hex(&encode_segwit("bc", 16, &[0x11; 41])).is_none());
    }
}
