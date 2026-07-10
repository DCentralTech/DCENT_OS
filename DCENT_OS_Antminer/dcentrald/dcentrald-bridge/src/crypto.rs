//! Cryptographic primitives for the DCENT Expansion Pack ("dcent-pack")
//! bridge client.
//!
//! Every signing function in this module mirrors the EXACT byte layout the
//! bridge firmware verifies against. The authoritative implementations live in
//! `dcent-expansion-pack/DCENT_OS_ESP-idf/main/pack_id.c` (`pack_id_verify_hmac`,
//! `pack_id_hmac_message`) and `ota_handler.c`. When this module and the spec
//! doc disagree, the firmware wins — the message layouts here were verified
//! against `pack_id.c:137-183` (pair) and the OTA/WS sig call sites.
//!
//! ## Signing-message layouts (all HMAC-SHA256, lowercase-hex output)
//!
//! | sig          | message bytes                                              |
//! | ---          | ---                                                        |
//! | `pair_hmac`  | `device_id` ++ `":"` ++ `miner_mac` ++ `":"` ++ `ts_ascii` |
//! | `ota_sig`    | `"ota:"` ++ lowerhex(sha256(body))                         |
//! | `ota_pull`   | `"ota_pull:"` ++ url ++ `":"` ++ expected_sha256_hex       |
//! | `ws_sig`     | `"ws:"` ++ path ++ `":"` ++ sec_websocket_key              |
//!
//! `ts_ascii` is the decimal ASCII rendering of the integer `ts` with no
//! leading zeros and no padding (`u64::to_string()`), matching the firmware's
//! `snprintf("%PRId64", ts)`.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Render a byte slice as lowercase hex (no separators).
fn lower_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // {:02x} — fixed two lowercase nibbles per byte.
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// HMAC-SHA256 over `msg` keyed by `secret`, returned as lowercase hex.
///
/// `Hmac::new_from_slice` only errors for key lengths a backend cannot accept;
/// HMAC-SHA256 accepts any key length, so this is infallible in practice. We
/// still avoid `expect` in library code by falling back to an empty key clone
/// path is unnecessary — the slice constructor never errors for HMAC — so we
/// surface the (impossible) error as an empty string would be a silent footgun.
/// Instead we use the documented-infallible constructor contract.
fn hmac_hex(secret: &[u8], msg: &[u8]) -> String {
    // new_from_slice is infallible for HMAC (any key length is valid). Guard
    // anyway so the crate stays panic-free even if a future backend changes.
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    mac.update(msg);
    lower_hex(&mac.finalize().into_bytes())
}

/// Compute the `/pair` HMAC.
///
/// `HMAC-SHA256(secret, device_id || ":" || miner_mac || ":" || ts_ascii)`,
/// lowercase hex. Verified byte-for-byte against `pack_id.c:164-178`.
///
/// The bridge accepts upper- or lowercase hex, but we always emit lowercase.
pub fn pair_hmac(secret: &[u8], device_id: &str, miner_mac: &str, ts: u64) -> String {
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    mac.update(device_id.as_bytes());
    mac.update(b":");
    mac.update(miner_mac.as_bytes());
    mac.update(b":");
    mac.update(ts.to_string().as_bytes());
    lower_hex(&mac.finalize().into_bytes())
}

/// Compute the `X-DCent-Ota-Sig` value for a Mode-A OTA upload.
///
/// `HMAC-SHA256(secret, "ota:" || lowerhex(sha256(body)))`, lowercase hex.
pub fn ota_sig(secret: &[u8], body: &[u8]) -> String {
    let body_sha = Sha256::digest(body);
    let message = format!("ota:{}", lower_hex(&body_sha));
    hmac_hex(secret, message.as_bytes())
}

/// Compute the `hmac` field for a Mode-B OTA URL-pull.
///
/// `HMAC-SHA256(secret, "ota_pull:" || url || ":" || expected_sha256_hex)`,
/// lowercase hex. The body is not available at signing time, so this signs the
/// URL + expected hash instead of the body hash — do NOT reuse `ota_sig`.
pub fn ota_pull_sig(secret: &[u8], url: &str, expected_sha_hex: &str) -> String {
    let message = format!("ota_pull:{}:{}", url, expected_sha_hex);
    hmac_hex(secret, message.as_bytes())
}

/// Compute the `X-DCent-WS-Sig` value for a WebSocket upgrade.
///
/// `HMAC-SHA256(secret, "ws:" || path || ":" || sec_websocket_key)`, lowercase
/// hex. `ws_key` is the per-upgrade `Sec-WebSocket-Key` (RFC 6455 §4.1) used as
/// the freshness anchor — generate a fresh one on every reconnect.
pub fn ws_sig(secret: &[u8], path: &str, ws_key: &str) -> String {
    let message = format!("ws:{}:{}", path, ws_key);
    hmac_hex(secret, message.as_bytes())
}

/// Compute the `X-DCent-Heartbeat-Sig` value for a signed heartbeat POST
/// (V0.2 Change A — `dcent-expansion-pack/docs/V0.2_DCENTOS_CHANGES.md`).
///
/// `HMAC-SHA256(secret, "heartbeat:" || ts_ascii || ":" || lowerhex(sha256(body)))`,
/// lowercase hex. The signature is **body-bound** (it commits to `sha256(body)`),
/// so a captured `(ts, sig)` pair cannot be replayed against a different body.
///
/// `ts` renders as canonical decimal (`u64` `Display`), matching the firmware's
/// `snprintf("%PRId64", ts)`. Sign over the EXACT serialized body bytes you POST
/// (the wire bytes, not a re-serialization) so the bridge verifier hashes the
/// identical bytes and the tags agree.
///
/// The authoritative cross-language KAT lives in
/// `dcent-expansion-pack/tools/heartbeat_hmac_kat.py` (Python reference signer)
/// and `firmware/test/host/test_heartbeat_auth.c` (C verifier). This signer
/// reproduces the frozen `988aa10a…4f81` vector — pinned in the KAT test below.
pub fn heartbeat_sig(secret: &[u8], ts: u64, body: &[u8]) -> String {
    let body_sha = Sha256::digest(body);
    let message = format!("heartbeat:{}:{}", ts, lower_hex(&body_sha));
    hmac_hex(secret, message.as_bytes())
}

/// Errors decoding a base32 (RFC 4648, no padding) unit secret.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretDecodeError {
    /// A character outside the RFC 4648 base32 alphabet was found.
    #[error("invalid base32 character: {0:?}")]
    InvalidChar(char),
    /// The decoded byte length was not exactly 32.
    #[error("decoded secret is {0} bytes, expected 32")]
    WrongLength(usize),
    /// Trailing (normally-discarded) bits were non-zero — a non-canonical
    /// encoding. Distinct base32 strings must not decode to the same secret;
    /// reject the non-canonical one so a garbled QR fails at provisioning with a
    /// clear error instead of an opaque 401 AuthFailed later. (gap-swarm no-HAL
    /// hunt #8)
    #[error("non-canonical base32: {0} trailing bit(s) were non-zero")]
    NonCanonical(u32),
}

/// Decode an RFC 4648 base32 string (NO padding) into raw bytes.
///
/// The QR `k=` field carries the 32-byte unit secret as base32-no-pad. The
/// alphabet is `A-Z2-7`, case-insensitive on input. This is a minimal decoder
/// (no external base32 dependency) so the leaf crate stays light.
fn base32_decode_nopad(input: &str) -> Result<Vec<u8>, SecretDecodeError> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;

    for ch in input.chars() {
        if ch == '=' {
            // Tolerate trailing padding even though the spec is "no pad".
            continue;
        }
        let up = ch.to_ascii_uppercase();
        let val = ALPHABET
            .iter()
            .position(|&a| a as char == up)
            .ok_or(SecretDecodeError::InvalidChar(ch))? as u32;
        buffer = (buffer << 5) | val;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    // Reject non-canonical encodings: any leftover sub-byte bits MUST be zero.
    // A canonical 32-byte secret is 52 base32 chars = 260 bits = 256 used + 4
    // trailing zero bits; accepting non-zero trailing bits would let multiple
    // distinct strings decode to the same secret (malleable input). (gap-swarm #8)
    if bits > 0 && (buffer & ((1u32 << bits) - 1)) != 0 {
        return Err(SecretDecodeError::NonCanonical(bits));
    }
    Ok(out)
}

/// Decode a base32-no-pad QR `k=` field into a 32-byte [`UnitSecret`].
pub fn unit_secret_from_base32(input: &str) -> Result<UnitSecret, SecretDecodeError> {
    let bytes = base32_decode_nopad(input)?;
    if bytes.len() != 32 {
        return Err(SecretDecodeError::WrongLength(bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(UnitSecret(arr))
}

/// A 32-byte unit secret loaded from QR provisioning.
///
/// - Zeroizes its backing bytes on drop (manual volatile-style overwrite; no
///   external `zeroize` dependency needed for a 32-byte fixed array).
/// - Never logs its contents: the [`std::fmt::Debug`] impl is redacted.
#[derive(Clone)]
pub struct UnitSecret([u8; 32]);

impl UnitSecret {
    /// Construct from a 32-byte array.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        UnitSecret(bytes)
    }

    /// Borrow the raw 32 secret bytes for HMAC keying.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for UnitSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redacted — never print key material into logs or panic messages.
        f.write_str("UnitSecret(<redacted 32 bytes>)")
    }
}

impl Drop for UnitSecret {
    fn drop(&mut self) {
        // Best-effort scrub. `write_volatile` is not strictly required for a
        // stack/heap array of this size, but it prevents the optimizer from
        // eliding the overwrite of soon-to-be-freed key material.
        for b in self.0.iter_mut() {
            unsafe {
                std::ptr::write_volatile(b, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The 32 known secret bytes used for every KAT in this crate: 0x00..=0x1f.
    fn kat_secret() -> [u8; 32] {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = i as u8;
        }
        s
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn bridge_secret_and_signature_helpers_never_panic_on_arbitrary_input(
            secret in proptest::collection::vec(any::<u8>(), 0..128),
            device_id in ".{0,128}",
            miner_mac in ".{0,128}",
            url in ".{0,512}",
            expected_sha in ".{0,128}",
            body in proptest::collection::vec(any::<u8>(), 0..2048),
            ws_path in ".{0,128}",
            ws_key in ".{0,128}",
            base32 in ".{0,128}",
            ts in any::<u64>()
        ) {
            let _ = pair_hmac(&secret, &device_id, &miner_mac, ts);
            let _ = ota_sig(&secret, &body);
            let _ = ota_pull_sig(&secret, &url, &expected_sha);
            let _ = ws_sig(&secret, &ws_path, &ws_key);
            let _ = heartbeat_sig(&secret, ts, &body);
            let _ = unit_secret_from_base32(&base32);
        }
    }

    #[test]
    fn base32_rejects_non_canonical_trailing_bits() {
        // 52 'A's = 260 zero bits = 32 zero bytes + 4 trailing ZERO bits → canonical.
        let canonical = "A".repeat(52);
        assert_eq!(
            base32_decode_nopad(&canonical),
            Ok(vec![0u8; 32]),
            "canonical all-zero secret must decode to 32 zero bytes"
        );
        // Lowercase tolerance preserved.
        assert_eq!(
            base32_decode_nopad(&canonical.to_lowercase()),
            Ok(vec![0u8; 32])
        );
        // Same length, but the final char (B = 00001) sets a normally-discarded
        // trailing bit → a distinct string that would otherwise decode to the
        // SAME secret. Reject it (fail clearly at provisioning, not as an opaque
        // 401 later). (gap-swarm #8)
        let non_canonical = format!("{}B", "A".repeat(51));
        assert!(
            matches!(
                base32_decode_nopad(&non_canonical),
                Err(SecretDecodeError::NonCanonical(_))
            ),
            "non-canonical trailing bits must be rejected, got {:?}",
            base32_decode_nopad(&non_canonical)
        );
    }

    #[test]
    fn pair_hmac_golden_vector() {
        // KAT inputs from the task spec; golden digest computed with the
        // reference HMAC-SHA256 (Python `hmac`/`hashlib`) over the same key
        // and message, then pinned here. Re-verified by this Rust impl.
        let secret = kat_secret();
        let got = pair_hmac(&secret, "dcentos-test", "AA:BB:CC:DD:EE:FF", 1735689600);
        assert_eq!(
            got,
            "1d89d20f5de8e6241a4bc3b76bd4b993ebf48f452a2344d640c6fbcab42f0e58"
        );
        // Always lowercase, always 64 hex chars.
        assert_eq!(got.len(), 64);
        assert_eq!(got, got.to_lowercase());
    }

    #[test]
    fn pair_hmac_ts_is_decimal_ascii_no_padding() {
        // ts must serialize as plain decimal (no leading zeros), matching the
        // firmware's snprintf("%PRId64"). Distinct ts -> distinct digest.
        let secret = kat_secret();
        let a = pair_hmac(&secret, "d", "M", 1);
        let b = pair_hmac(&secret, "d", "M", 10);
        assert_ne!(a, b);
    }

    #[test]
    fn ota_sig_golden_vector() {
        let secret = kat_secret();
        let got = ota_sig(&secret, b"DCENT-OTA-IMAGE-BODY-EXAMPLE");
        assert_eq!(
            got,
            "87b2ef3b9e2414b683c0eb79c4763a57b311f7e8ef968aa219fb5ec0097065d6"
        );
    }

    #[test]
    fn ota_pull_sig_golden_vector() {
        let secret = kat_secret();
        let got = ota_pull_sig(
            &secret,
            "https://releases.d-central.tech/firmware/dcent-pack-0.2.0.bin",
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899",
        );
        assert_eq!(
            got,
            "b509534013c2d5b6e1aafd6f8798634f21e536625b1b5093c6b93db353a44808"
        );
    }

    #[test]
    fn ota_pull_sig_differs_from_ota_sig() {
        // Guard against accidentally reusing the upload signer for pull mode.
        let secret = kat_secret();
        let url = "https://example.com/fw.bin";
        let sha = "00".repeat(32);
        let pull = ota_pull_sig(&secret, url, &sha);
        // ota_sig over the same string-as-body would produce a different tag.
        let upload_lookalike = ota_sig(&secret, format!("{}:{}", url, sha).as_bytes());
        assert_ne!(pull, upload_lookalike);
    }

    #[test]
    fn ws_sig_golden_vector() {
        let secret = kat_secret();
        let got = ws_sig(
            &secret,
            "/api/v1/dashboard/live",
            "dGhlIHNhbXBsZSBub25jZQ==",
        );
        assert_eq!(
            got,
            "78c80130f2ed507e915c5ec9f8235a0b4db490e154f7d67694c9f4b80f06488d"
        );
    }

    // --- Change-A heartbeat signer KAT (frozen cross-language vector) ---------
    //
    // The EXACT wire body bytes from the frozen vector
    // (`dcent-expansion-pack/tools/heartbeat_hmac_kat.py :: KAT_BODY` +
    // `docs/V0.2_DCENTOS_CHANGES.md`). Raw byte-string so the inner `"` chars
    // are literal — this is the on-the-wire JSON, not a re-serialization.
    const HEARTBEAT_KAT_BODY: &[u8] = br#"{"device_id":"dcent-kat","hashrate_ths":21.3,"shares_accepted":12044,"shares_rejected":7,"best_difficulty":"184.2M","block_height":873221,"fan_speed_rpm":5400}"#;
    const HEARTBEAT_KAT_TS: u64 = 1_747_000_000;
    const HEARTBEAT_KAT_BODY_SHA: &str =
        "cdc2ab066f2bda8f010590fd9a55139571fe95d24793ac8e71e31a103a602f72";
    const HEARTBEAT_KAT_SIG: &str =
        "988aa10aa3af68e219cf0749ad45b85c8d07dc4559fec8547672871cd15a4f81";

    #[test]
    fn heartbeat_sig_reproduces_frozen_cross_language_kat() {
        // secret = bytes 0x00..=0x1F.
        let secret: Vec<u8> = (0u8..32).collect();
        // Intermediate body-sha must match the frozen value the C host test and
        // the Python reference signer both assert.
        assert_eq!(
            lower_hex(&Sha256::digest(HEARTBEAT_KAT_BODY)),
            HEARTBEAT_KAT_BODY_SHA,
            "body sha256 must match the frozen KAT"
        );
        // Full signature — the byte-for-byte anchor the signer and the bridge
        // verifier both bind to. If this drifts from the C/Python side, ONE of
        // the three fails (which is the whole point of the KAT).
        assert_eq!(
            heartbeat_sig(&secret, HEARTBEAT_KAT_TS, HEARTBEAT_KAT_BODY),
            HEARTBEAT_KAT_SIG
        );
    }

    #[test]
    fn heartbeat_sig_is_body_bound() {
        // Flipping one body byte must change the signature (replay resistance).
        let secret: Vec<u8> = (0u8..32).collect();
        let mut evil = HEARTBEAT_KAT_BODY.to_vec();
        evil[20] ^= 0x01;
        assert_ne!(
            heartbeat_sig(&secret, HEARTBEAT_KAT_TS, &evil),
            HEARTBEAT_KAT_SIG
        );
    }

    #[test]
    fn heartbeat_sig_is_ts_bound() {
        // A different ts must change the signature (freshness anchor).
        let secret: Vec<u8> = (0u8..32).collect();
        assert_ne!(
            heartbeat_sig(&secret, HEARTBEAT_KAT_TS + 1, HEARTBEAT_KAT_BODY),
            HEARTBEAT_KAT_SIG
        );
    }

    #[test]
    fn base32_decode_round_trip() {
        // base32-no-pad of 0x00..=0x1f, generated by the reference encoder.
        let b32 = "AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPQ";
        let secret = unit_secret_from_base32(b32).expect("valid base32");
        assert_eq!(secret.as_bytes(), &kat_secret());
    }

    #[test]
    fn base32_decode_tolerates_lowercase() {
        let b32 = "aaaqeayeaudaocajbifqydiob4ibceqtcqkrmfyydenbwha5dypq";
        let secret = unit_secret_from_base32(b32).expect("valid base32 lowercase");
        assert_eq!(secret.as_bytes(), &kat_secret());
    }

    #[test]
    fn base32_decode_rejects_wrong_length() {
        let err = unit_secret_from_base32("AAAA").unwrap_err();
        matches!(err, SecretDecodeError::WrongLength(_));
    }

    #[test]
    fn base32_decode_rejects_bad_char() {
        // '1', '8', '9', '0' are not in the RFC 4648 base32 alphabet.
        let err = unit_secret_from_base32("AAAA1AAA").unwrap_err();
        assert_eq!(err, SecretDecodeError::InvalidChar('1'));
    }

    #[test]
    fn unit_secret_debug_is_redacted() {
        let s = UnitSecret::from_bytes(kat_secret());
        let dbg = format!("{:?}", s);
        assert!(dbg.contains("redacted"));
        // Make sure no raw byte value leaked into the debug string.
        assert!(!dbg.contains("31")); // 0x1f = 31 would appear if it printed bytes
    }
}
