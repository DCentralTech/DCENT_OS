//! SV2 pool-authority-key parsing and the explicit insecure transport opt-out.
//!
//! # Why this module exists
//!
//! Stratum V2 transport security has two halves:
//!
//! 1. **Confidentiality / integrity** — the `Noise_NX_Secp256k1+EllSwift
//!    _ChaChaPoly_SHA256` handshake in [`super::noise`]. This is *always* on
//!    for the mining channel today (the client unconditionally runs the
//!    handshake before any SV2 frame). There is no plaintext mining path.
//! 2. **Pool authentication** — the server's `SIGNATURE_NOISE_MESSAGE`
//!    certificate is a BIP340 Schnorr signature over the server's static
//!    Noise key. Verifying it against the **pool authority public key**
//!    is what stops an active man-in-the-middle: without authority-key
//!    pinning, an attacker who can intercept TCP can complete a *valid*
//!    Noise handshake with their own keys and silently steal the miner's
//!    hashrate. The SV2 spec is explicit: *"Authority-key pinning is
//!    mandatory for safety."*
//!
//! Before this module, certificate verification was **TOFU-only**
//! (`pool_authority_key` was always `None`) and the JD/Template-Distribution
//! sessions ran in **cleartext**. This module closes both gaps:
//!
//! - [`parse_authority_key_from_sv2_url`] extracts the optional pinned
//!   authority key from the SV2 URL exactly as the spec encodes it:
//!   `stratum2+tcp://host:port/<base58check(version_le_u16 || pubkey32)>`.
//! - [`sv2_insecure_no_noise`] is the single, loudly-logged escape hatch.
//!   Secure Noise is the default; this only returns `true` when the
//!   operator explicitly sets `DCENT_SV2_INSECURE_NO_NOISE=1`.
//!
//! # Spec citations
//!
//! - Noise pattern / cipher suite: Stratum V2 spec §4 "Protocol Security"
//!   (stratumprotocol.org/specification) + SRI `noise_sv2` crate —
//!   `Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256`, 2 handshake messages
//!   (`-> e` / `<- e, ee, s, es`).
//! - `SIGNATURE_NOISE_MESSAGE`: SV2 spec §4.5.2 — `version: U16`,
//!   `valid_from: U32`, `not_valid_after: U32`, `signature: B0_64`
//!   (BIP340 Schnorr over `version || valid_from || not_valid_after ||
//!   server_static_pubkey`).
//! - Authority-key URL encoding: SV2 spec §4.1 +
//!   protocols/ §5.4 — base58check of
//!   `[0x01, 0x00]` (2-byte LE version prefix) || 32-byte authority pubkey.
//!
//! This module brings in no new crate dependency: Base58Check is decoded
//! with a self-contained implementation over the already-present `sha2`.

use sha2::{Digest, Sha256};

/// Process-wide serialization guard for tests that mutate the global
/// `DCENT_SV2_*` environment. `std::env::set_var` is process-global and
/// cargo runs `#[test]`s on multiple threads; any test that sets one of
/// these vars MUST hold this lock for the duration so a concurrent test
/// never observes a torn value.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Environment variable that, when set to a truthy value, disables the SV2
/// Noise handshake entirely (cleartext SV2). **Insecure** — only for lab
/// MITM/regression testing against a mock pool you control.
pub const ENV_INSECURE_NO_NOISE: &str = "DCENT_SV2_INSECURE_NO_NOISE";

/// Environment variable that, when truthy, forces the SV2 Noise handshake
/// even for a loopback Template Provider (disables the trusted-local-TP
/// cleartext convenience). A hardening knob — and the switch the JD Noise
/// integration test uses to exercise the encrypted TP path.
pub const ENV_TP_REQUIRE_NOISE: &str = "DCENT_SV2_TP_REQUIRE_NOISE";

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// `true` ⇒ require Noise even for a loopback Template Provider.
/// Default (unset) keeps the documented local-TP cleartext convenience.
pub fn sv2_tp_require_noise() -> bool {
    env_truthy(ENV_TP_REQUIRE_NOISE)
}

/// Returns `true` only when the operator has explicitly opted out of the
/// secure Noise transport via `DCENT_SV2_INSECURE_NO_NOISE`.
///
/// Secure Noise is the **default**. This is the single, audited escape
/// hatch. It emits a loud `tracing::error!` every time it returns `true`
/// so an accidentally-set env var is impossible to miss in logs.
///
/// Accepted truthy values: `1`, `true`, `yes`, `on` (case-insensitive).
/// Anything else (including unset) → secure (returns `false`).
pub fn sv2_insecure_no_noise() -> bool {
    match std::env::var(ENV_INSECURE_NO_NOISE) {
        Ok(v) => {
            let on = matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
            if on {
                tracing::error!(
                    env = ENV_INSECURE_NO_NOISE,
                    "*** SV2 NOISE DISABLED — CLEARTEXT TRANSPORT *** \
                     This is INSECURE: an active network attacker can read \
                     and rewrite mining traffic and steal hashrate. Only valid \
                     against a mock pool you fully control. Unset \
                     DCENT_SV2_INSECURE_NO_NOISE for production."
                );
            }
            on
        }
        Err(_) => false,
    }
}

/// A pinned SV2 pool authority public key (x-only, 32 bytes, secp256k1).
///
/// When present, the client MUST verify the server's
/// `SIGNATURE_NOISE_MESSAGE` against this key and abort the handshake on
/// failure (no TOFU fallback). This is the MITM defense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolAuthorityKey(pub [u8; 32]);

impl PoolAuthorityKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Errors from parsing the authority key out of an SV2 URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityKeyError {
    /// The URL had no authority-key path component (`…:port` with no `/key`).
    /// This is *not* an error at the call site — TOFU is allowed when the
    /// operator did not pin a key — but it is surfaced so the caller can
    /// log the (insecure) TOFU posture.
    NotPresent,
    /// The path component was present but not valid base58check.
    InvalidBase58Check(String),
    /// Decoded payload had the wrong length or version prefix.
    InvalidPayload(String),
}

impl std::fmt::Display for AuthorityKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthorityKeyError::NotPresent => write!(f, "no authority key in SV2 URL"),
            AuthorityKeyError::InvalidBase58Check(e) => {
                write!(f, "invalid base58check authority key: {}", e)
            }
            AuthorityKeyError::InvalidPayload(e) => {
                write!(f, "invalid authority key payload: {}", e)
            }
        }
    }
}

/// Bitcoin Base58 alphabet (same alphabet SRI/`bs58` uses for SV2 keys).
const B58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode a Base58Check string into its payload (checksum stripped + verified).
///
/// Base58Check = base58( data || first4(sha256(sha256(data))) ). This is the
/// exact encoding `bs58::encode(..).with_check()` (used by SRI for SV2
/// authority keys) produces.
fn base58check_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Err("empty string".into());
    }
    // Base58 → big-endian byte vector.
    let mut bytes: Vec<u8> = vec![0];
    for ch in s.bytes() {
        let val = B58_ALPHABET
            .iter()
            .position(|&c| c == ch)
            .ok_or_else(|| format!("invalid base58 character: {:?}", ch as char))?
            as u32;
        let mut carry = val;
        for b in bytes.iter_mut() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    // Leading '1' chars are leading zero bytes.
    for ch in s.bytes() {
        if ch == b'1' {
            bytes.push(0);
        } else {
            break;
        }
    }
    bytes.reverse();

    if bytes.len() < 4 {
        return Err("decoded payload shorter than checksum".into());
    }
    let (payload, checksum) = bytes.split_at(bytes.len() - 4);
    let h1 = Sha256::digest(payload);
    let h2 = Sha256::digest(h1);
    if checksum != &h2[..4] {
        return Err("base58check checksum mismatch".into());
    }
    Ok(payload.to_vec())
}

/// Extract the optional pinned pool authority key from an SV2 URL.
///
/// Per the SV2 spec the authority key travels in the URL path:
///
/// ```text
/// stratum2+tcp://pool.example.com:34254/<base58check( [0x01,0x00] || pubkey32 )>
/// ```
///
/// The 2-byte LE version prefix is `0x0001` (`[0x01, 0x00]`). A trailing
/// `/` with no key, or no `/` at all, yields [`AuthorityKeyError::NotPresent`]
/// (TOFU posture — the caller decides whether to warn or refuse).
///
/// This intentionally does *not* go through `url_validator::validate_sv2
/// _pool_url` (which strips/forbids the path) — it parses the path
/// component itself so the existing host:port validator is untouched.
pub fn parse_authority_key_from_sv2_url(url: &str) -> Result<PoolAuthorityKey, AuthorityKeyError> {
    // Strip the scheme so the next '/' is unambiguously the path separator.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);

    let path = match after_scheme.split_once('/') {
        Some((_authority, path)) => path,
        None => return Err(AuthorityKeyError::NotPresent),
    };
    // Take only the first path segment; ignore any query/fragment.
    let key_str = path.split(['/', '?', '#']).next().unwrap_or("").trim();
    if key_str.is_empty() {
        return Err(AuthorityKeyError::NotPresent);
    }

    let payload = base58check_decode(key_str).map_err(AuthorityKeyError::InvalidBase58Check)?;

    // Expected: 2-byte LE version prefix (0x0001) + 32-byte x-only pubkey.
    if payload.len() != 34 {
        return Err(AuthorityKeyError::InvalidPayload(format!(
            "expected 34 bytes (2 version + 32 key), got {}",
            payload.len()
        )));
    }
    let version = u16::from_le_bytes([payload[0], payload[1]]);
    if version != 1 {
        return Err(AuthorityKeyError::InvalidPayload(format!(
            "unsupported authority-key version {} (expected 1)",
            version
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&payload[2..34]);
    Ok(PoolAuthorityKey(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-encode a payload as Base58Check so the round-trip is testable
    /// without pulling in the `bs58` crate.
    fn base58check_encode(payload: &[u8]) -> String {
        let h1 = Sha256::digest(payload);
        let h2 = Sha256::digest(h1);
        let mut data = payload.to_vec();
        data.extend_from_slice(&h2[..4]);

        // Count leading zeros (→ leading '1's).
        let zeros = data.iter().take_while(|&&b| b == 0).count();
        let mut num = data.clone();
        let mut out: Vec<u8> = Vec::new();
        // Big-endian base conversion to base58.
        let mut start = 0;
        while start < num.len() {
            let mut remainder = 0u32;
            let mut all_zero = true;
            for b in num.iter_mut().skip(start) {
                let acc = (remainder << 8) | (*b as u32);
                *b = (acc / 58) as u8;
                remainder = acc % 58;
                if *b != 0 && all_zero {
                    all_zero = false;
                }
            }
            out.push(B58_ALPHABET[remainder as usize]);
            if all_zero {
                // advance start past leading zero digits
                while start < num.len() && num[start] == 0 {
                    start += 1;
                }
            }
        }
        for _ in 0..zeros {
            out.push(b'1');
        }
        out.reverse();
        String::from_utf8(out).unwrap()
    }

    fn make_url_with_key(key: [u8; 32]) -> String {
        let mut payload = vec![0x01u8, 0x00]; // version 1, LE
        payload.extend_from_slice(&key);
        format!(
            "stratum2+tcp://pool.example.com:34254/{}",
            base58check_encode(&payload)
        )
    }

    #[test]
    fn roundtrips_pinned_authority_key() {
        let key = [0x7Au8; 32];
        let url = make_url_with_key(key);
        let parsed = parse_authority_key_from_sv2_url(&url).unwrap();
        assert_eq!(parsed.0, key);
    }

    #[test]
    fn no_path_is_not_present() {
        let e =
            parse_authority_key_from_sv2_url("stratum2+tcp://pool.example.com:3336").unwrap_err();
        assert_eq!(e, AuthorityKeyError::NotPresent);
    }

    #[test]
    fn trailing_slash_no_key_is_not_present() {
        let e =
            parse_authority_key_from_sv2_url("stratum2+tcp://pool.example.com:3336/").unwrap_err();
        assert_eq!(e, AuthorityKeyError::NotPresent);
    }

    #[test]
    fn corrupt_base58_is_rejected() {
        let e =
            parse_authority_key_from_sv2_url("stratum2+tcp://pool.example.com:3336/not-valid-0OIl")
                .unwrap_err();
        assert!(matches!(e, AuthorityKeyError::InvalidBase58Check(_)));
    }

    #[test]
    fn checksum_tamper_is_rejected() {
        let key = [0x33u8; 32];
        let url = make_url_with_key(key);
        // Flip the last base58 char → checksum break.
        let mut chars: Vec<char> = url.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        let e = parse_authority_key_from_sv2_url(&tampered).unwrap_err();
        assert!(matches!(e, AuthorityKeyError::InvalidBase58Check(_)));
    }

    #[test]
    fn wrong_version_prefix_is_rejected() {
        let mut payload = vec![0x09u8, 0x00]; // version 9
        payload.extend_from_slice(&[0x11u8; 32]);
        let url = format!("stratum2+tcp://p:3336/{}", base58check_encode(&payload));
        let e = parse_authority_key_from_sv2_url(&url).unwrap_err();
        assert!(matches!(e, AuthorityKeyError::InvalidPayload(_)));
    }

    #[test]
    fn wrong_length_payload_is_rejected() {
        let mut payload = vec![0x01u8, 0x00];
        payload.extend_from_slice(&[0x22u8; 16]); // too short
        let url = format!("stratum2+tcp://p:3336/{}", base58check_encode(&payload));
        let e = parse_authority_key_from_sv2_url(&url).unwrap_err();
        assert!(matches!(e, AuthorityKeyError::InvalidPayload(_)));
    }

    // Env-var-driven tests are consolidated into a single `#[test]` so the
    // process-global `DCENT_SV2_INSECURE_NO_NOISE` is never observed by a
    // concurrently-running test (cargo runs `#[test]`s on multiple threads).
    #[test]
    fn insecure_flag_default_off_and_explicit_opt_in() {
        let _g = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Default (unset) → secure.
        std::env::remove_var(ENV_INSECURE_NO_NOISE);
        assert!(!sv2_insecure_no_noise(), "unset must be secure");

        // Explicit truthy values → insecure.
        for v in ["1", "true", "TRUE", "yes", "On"] {
            std::env::set_var(ENV_INSECURE_NO_NOISE, v);
            assert!(sv2_insecure_no_noise(), "{v} must opt out");
        }

        // Non-truthy / garbage → still secure (fail-closed).
        for v in ["0", "false", "no", "garbage", ""] {
            std::env::set_var(ENV_INSECURE_NO_NOISE, v);
            assert!(!sv2_insecure_no_noise(), "{v:?} must stay secure");
        }

        std::env::remove_var(ENV_INSECURE_NO_NOISE);
        assert!(!sv2_insecure_no_noise());
    }
}
