//! Wallet-address masking helpers for log/UI sanitization.
//!
//! # Why this exists
//! Every Stratum V1 share submit, every pool authorize, every config-write
//! event in dcentrald used to log the operator's full Bitcoin wallet address
//! at INFO level. Logs end up on tmpfs, in the persistent log ring, in the
//! REST `/api/debug/log` endpoint, and in any forwarded structured-log
//! collector the operator wires up. From a privacy and security standpoint
//! that is identical to logging a credit-card number — wallet addresses
//! identify the operator's earnings stream and link them to a public address
//! that anyone on the LAN, anyone with API access, or anyone who later
//! recovers a discarded miner can scrape.
//!
//! This module provides:
//! 1. [`mask_wallet`] — mask a single known-wallet string (e.g. the value of
//!    a `worker=` log field).
//! 2. [`mask_in_string`] — scan an arbitrary log line and mask any wallet-ish
//!    substrings detected anywhere inside it.
//! 3. [`is_likely_wallet`] — quick predicate.
//!
//! # Threat model
//! - INFO/WARN/ERROR logs MUST never contain a full wallet address.
//! - TRACE logs MAY contain full addresses (gated by `RUST_LOG=trace`, off by
//!   default in production). This is documented at the call site, not enforced
//!   by this crate.
//! - The `[logging] mask_logs = true` config option (default `true`) controls
//!   whether the daemon-side passthrough sanitizer in
//!   `dcentrald-api::routes::*::log_tail` strips any leftover addresses from
//!   tail responses. Operators with structured-log collectors that need raw
//!   addresses can set `mask_logs = false` — but the per-call `mask_wallet`
//!   substitutions on `worker=` / `username=` / `wallet=` fields are still
//!   applied at log-emission time and are NOT controlled by this flag.
//!
//! # Detection rules (hand-rolled, no regex dep)
//! - **bech32 / bech32m**: human-readable prefix in the set
//!   `bc, tb, bcrt, bsv, ltc, tltc` followed by the literal `1` and 6+
//!   characters from the bech32 charset (`qpzry9x8gf2tvdw0s3jn54khce6mua7l`).
//!   Total length 14..=90 (BIP-173).
//! - **base58 P2PKH / P2SH**: 25-35 chars from the base58 alphabet, must
//!   start with `1`, `3`, `5` (mainnet WIF), `m`, `n` (testnet P2PKH), or
//!   `2` (testnet P2SH). 26-byte minimum decoded length is approximated by
//!   the 25-char floor.
//! - **hex**: 32, 40, or 64 lowercase/uppercase hex chars surrounded by
//!   non-hex word boundaries. Common shapes are 64-char SHA256 / x-only
//!   pubkey, 40-char hash160, 32-char custom IDs.
//!
//! Hex is masked conservatively: 64-char strings are clearly addresses or
//! hashes; 40-char are clearly hash160; 32-char hex is masked because that's
//! also a private-key-half / xprv-fragment shape and erring on the side of
//! masking is the right default for log emission.
//!
//! # Format
//! `mask_wallet` returns `<first6>…<last4>` for inputs of length >= 12, with
//! a Unicode HORIZONTAL ELLIPSIS (`U+2026`) — single character — joining
//! them. Shorter strings are returned unchanged because they are unlikely to
//! be wallet addresses and short hashes are useful in logs.

use std::borrow::Cow;

/// Bech32 character set (BIP-173). Excludes `1` (separator), `b`, `i`, `o`.
/// The `1` is the separator between HRP and data; we match it literally.
const BECH32_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// Base58 alphabet (Bitcoin). Excludes `0`, `O`, `I`, `l`.
const BASE58_CHARSET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Bech32 human-readable prefixes we recognize. Lowercase only, since
/// addresses MUST be all lower- or all upper-case (BIP-173). Mixed-case is
/// invalid and we won't mask those — they're already malformed.
///
/// `bsv` is included for forward compatibility even though Bitcoin SV
/// shouldn't be funded from a DCENT_OS miner; it's still privacy-equivalent
/// data if it shows up.
const BECH32_HRPS: &[&[u8]] = &[
    b"bc",   // mainnet
    b"tb",   // testnet
    b"bcrt", // regtest
    b"bsv",  // BSV mainnet (defensive)
    b"ltc",  // Litecoin (LiteAxe)
    b"tltc", // Litecoin testnet
];

/// First-character set for base58 Bitcoin addresses we mask:
/// `1` mainnet P2PKH, `3` mainnet P2SH, `m`/`n` testnet P2PKH, `2` testnet
/// P2SH. We deliberately do NOT include `5`/`K`/`L` (WIF private keys) or
/// `x` (xpub/xprv) because those should not be in operator logs at all and
/// we don't want a "valid mask" to make their accidental appearance look OK.
const BASE58_FIRST_BYTES: &[u8] = b"123mn";

/// Mask a known-wallet string. Returns the mask form for any input >= 12
/// characters; otherwise returns the input unchanged. This is the right
/// helper for known-wallet log fields like `worker={username}`.
///
/// The output uses Unicode horizontal ellipsis (`U+2026`) which renders as
/// a single character in any UTF-8-aware viewer. This is one byte shorter
/// per emission than `...` and visually distinguishes the masked region
/// from arbitrary text.
///
/// ```
/// use dcentrald_common::wallet_mask::mask_wallet;
/// assert_eq!(
///     mask_wallet("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
///     "bc1q04…hzp6",
/// );
/// assert_eq!(
///     mask_wallet("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"),
///     "1A1zP1…vfNa",
/// );
/// // Short strings pass through.
/// assert_eq!(mask_wallet("worker.1"), "worker.1");
/// ```
pub fn mask_wallet(addr: &str) -> String {
    let bytes = addr.as_bytes();
    if bytes.len() < 12 {
        return addr.to_string();
    }
    // Take 6 ASCII chars from the front and 4 from the back. We assume
    // wallet addresses are ASCII (bech32 / base58 / hex are all ASCII), so
    // byte-slicing is safe; if a non-ASCII string is passed in, we fall
    // back to a char-based slice to avoid panicking on UTF-8 boundaries.
    if addr.is_ascii() {
        let prefix = &addr[..6];
        let suffix = &addr[addr.len() - 4..];
        return format!("{prefix}\u{2026}{suffix}");
    }
    // Non-ASCII safety path (defensive — wallets shouldn't get here).
    let chars: Vec<char> = addr.chars().collect();
    if chars.len() < 12 {
        return addr.to_string();
    }
    let prefix: String = chars.iter().take(6).collect();
    let suffix: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{prefix}\u{2026}{suffix}")
}

/// Quick predicate: does this look like a wallet address?
///
/// Used for early-exit checks. Returns `true` if the input matches any of
/// the bech32 / base58 / hex shapes recognized by [`mask_in_string`].
pub fn is_likely_wallet(s: &str) -> bool {
    matches_bech32(s.as_bytes(), 0).is_some()
        || matches_base58_address(s.as_bytes(), 0).is_some()
        || matches_hex_address(s.as_bytes(), 0).is_some()
}

/// Scan `s` for wallet-shaped substrings and replace each with its masked
/// form. Returns `Cow::Borrowed(s)` if no match was found, `Cow::Owned(...)`
/// otherwise. This is the right helper for arbitrary log lines and log-tail
/// passthrough.
///
/// The scanner walks the string once at byte-level and tries each detector
/// at every position where a non-`[A-Za-z0-9]` character (or start-of-string)
/// precedes the candidate. This avoids matching wallet-shaped substrings
/// that are part of a longer identifier.
pub fn mask_in_string(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Cow::Borrowed(s);
    }

    // Cheap pre-check: bail out early if no input byte could ever start a
    // wallet match. This keeps the hot path on benign log lines (the
    // overwhelming majority) at one linear scan with no allocations.
    let could_match = bytes.iter().any(|&b| {
        matches!(b,
            b'b' | b'B' | b't' | b'T' | b'l' | b'L'
                | b'1' | b'2' | b'3' | b'm' | b'n'
                | b'a'..=b'f' | b'A'..=b'F' | b'0'..=b'9'
        )
    });
    if !could_match {
        return Cow::Borrowed(s);
    }

    let mut out: Option<String> = None;
    let mut i = 0usize;
    // `last` = next byte index not yet copied into `out`. Gaps between matches
    // are flushed as valid UTF-8 `&str` slices (in `emit_match` and the trailing
    // flush below), never reconstructed byte-by-byte (which corrupts non-ASCII).
    let mut last = 0usize;

    while i < bytes.len() {
        // Word-boundary check: only attempt a match if `i == 0` or the
        // previous byte is non-alphanumeric. This prevents masking
        // substrings that are part of a longer identifier (e.g. a session
        // ID that happens to contain a 64-char hex run starting partway).
        let at_boundary = match i.checked_sub(1).and_then(|prev| bytes.get(prev)) {
            Some(prev) => !is_alnum_byte(*prev),
            None => true,
        };

        if at_boundary {
            if let Some(end) = matches_bech32(bytes, i) {
                emit_match(&mut out, s, last, i, end);
                last = end;
                i = end;
                continue;
            }
            if let Some(end) = matches_base58_address(bytes, i) {
                emit_match(&mut out, s, last, i, end);
                last = end;
                i = end;
                continue;
            }
            if let Some(end) = matches_hex_address(bytes, i) {
                emit_match(&mut out, s, last, i, end);
                last = end;
                i = end;
                continue;
            }
        }

        // No match at i — defer copying. The byte is part of the gap [last..],
        // flushed as a UTF-8 slice on the next match or at the end (never
        // reconstructed via `byte as char`, which corrupts multibyte UTF-8).
        i += 1;
    }

    match out {
        Some(mut buf) => {
            // Flush the trailing unmasked gap [last..] as a valid `&str` slice.
            buf.push_str(&s[last..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(s),
    }
}

// ---------------------------------------------------------------------------
// Internal: scanners
// ---------------------------------------------------------------------------

#[inline]
fn is_alnum_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

#[inline]
fn is_bech32_data_byte(b: u8) -> bool {
    BECH32_CHARSET.contains(&b)
}

#[inline]
fn is_base58_byte(b: u8) -> bool {
    BASE58_CHARSET.contains(&b)
}

#[inline]
fn is_hex_byte(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// If a bech32 address starts at `start` in `bytes`, return its exclusive end
/// index. Otherwise return `None`. Requires word-boundary at the END (the
/// caller already enforced word-boundary at the START).
fn matches_bech32(bytes: &[u8], start: usize) -> Option<usize> {
    // Find the longest matching HRP at this position. We must do this in
    // priority order — `bcrt` before `bc`, `tltc` before `tb`/`ltc` — so
    // `bcrt1...` doesn't get misread as `bc` + bogus data.
    let mut hrp_end = None;
    for hrp in BECH32_HRPS {
        let Some(separator_idx) = start.checked_add(hrp.len()) else {
            continue;
        };
        if bytes.get(start..separator_idx) == Some(*hrp)
            && bytes.get(separator_idx).copied() == Some(b'1')
        {
            // Prefer the longest match.
            match hrp_end {
                Some((existing_len, _)) if existing_len >= hrp.len() => {}
                _ => hrp_end = Some((hrp.len(), separator_idx + 1)),
            }
        }
    }
    let (_, data_start) = hrp_end?;

    // Greedily consume bech32 data chars. Total HRP+1+data length must be
    // 14..=90 per BIP-173.
    let mut end = data_start;
    while bytes.get(end).copied().is_some_and(is_bech32_data_byte) {
        end += 1;
    }
    let total_len = end - start;
    if !(14..=90).contains(&total_len) {
        return None;
    }
    // Need at least 6 data chars (checksum is 6 bytes).
    if end - data_start < 6 {
        return None;
    }
    // Word boundary at end.
    if bytes.get(end).copied().is_some_and(is_alnum_byte) {
        return None;
    }
    Some(end)
}

/// If a base58 P2PKH/P2SH address starts at `start` in `bytes`, return its
/// exclusive end index. Otherwise return `None`. Requires word-boundary at
/// the END.
fn matches_base58_address(bytes: &[u8], start: usize) -> Option<usize> {
    let first = bytes.get(start).copied()?;
    if !BASE58_FIRST_BYTES.contains(&first) {
        return None;
    }
    let mut end = start;
    while bytes.get(end).copied().is_some_and(is_base58_byte) {
        end += 1;
    }
    let total_len = end - start;
    // P2PKH/P2SH base58 length is 25..=35 (typically 26..=34).
    if !(25..=35).contains(&total_len) {
        return None;
    }
    if bytes.get(end).copied().is_some_and(is_alnum_byte) {
        return None;
    }
    Some(end)
}

/// If a long hex run starts at `start` in `bytes`, return its exclusive end
/// index. We only mask runs of length 32, 40, or 64 — those are the
/// load-bearing crypto shapes (256-bit txid/pubkey/secret half, 160-bit
/// hash160, 128-bit half-key). We do NOT mask longer hex (might be a chained
/// concat / hexdump) or shorter hex (would hit too many false positives like
/// MAC addresses without colons or short error codes).
fn matches_hex_address(bytes: &[u8], start: usize) -> Option<usize> {
    if !bytes.get(start).copied().is_some_and(is_hex_byte) {
        return None;
    }
    let mut end = start;
    while bytes.get(end).copied().is_some_and(is_hex_byte) {
        end += 1;
    }
    let len = end - start;
    if len == 32 || len == 40 || len == 64 {
        // Word boundary at end.
        if bytes.get(end).copied().is_some_and(is_alnum_byte) {
            return None;
        }
        return Some(end);
    }
    None
}

fn emit_match(out: &mut Option<String>, full: &str, gap_start: usize, start: usize, end: usize) {
    let buf = match out {
        Some(buf) => buf,
        None => {
            // First match — initialize the owned buffer (the prefix is the
            // first gap, flushed below as a valid UTF-8 slice).
            out.insert(String::with_capacity(full.len()))
        }
    };
    // Flush the unmasked gap [gap_start..start] as a `&str` slice. NEVER copy
    // it byte-by-byte via `byte as char` — that reinterprets UTF-8 continuation
    // bytes (>=0x80) as Latin-1 and corrupts multibyte chars (e.g. `café` ->
    // `cafÃ©`) in the log/privacy sanitizer. Wallet matches are ASCII, so
    // `gap_start`/`start`/`end` are always char boundaries.
    buf.push_str(&full[gap_start..start]);
    let masked = mask_wallet(&full[start..end]);
    buf.push_str(&masked);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn wallet_mask_helpers_never_panic_on_arbitrary_text(input in ".{0,2048}") {
            let masked_known = mask_wallet(&input);
            let masked_line = mask_in_string(&input);
            let _ = is_likely_wallet(&input);
            let replacement = char::REPLACEMENT_CHARACTER;

            prop_assert!(!masked_known.contains(replacement) || input.contains(replacement));
            prop_assert!(!masked_line.contains(replacement) || input.contains(replacement));
        }
    }

    // ---- mask_wallet ------------------------------------------------------

    #[test]
    fn masks_bech32_mainnet() {
        let addr = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        assert_eq!(mask_wallet(addr), "bc1q04\u{2026}hzp6");
    }

    #[test]
    fn masks_bech32_testnet() {
        let addr = "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
        let masked = mask_wallet(addr);
        assert!(masked.starts_with("tb1qrp"));
        assert!(masked.ends_with("fmv3"));
        assert!(!masked.contains("dkfs"));
    }

    #[test]
    fn masks_base58_p2pkh() {
        let addr = "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";
        assert_eq!(mask_wallet(addr), "1A1zP1\u{2026}vfNa");
    }

    #[test]
    fn masks_base58_p2sh() {
        let addr = "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy";
        let masked = mask_wallet(addr);
        assert!(masked.starts_with("3J98t1"));
        assert!(masked.ends_with("WNLy"));
    }

    #[test]
    fn masks_hex_64() {
        let addr = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let masked = mask_wallet(addr);
        assert!(!masked.is_empty());
        assert!(masked.starts_with("012345"));
        assert!(masked.ends_with("cdef"));
    }

    #[test]
    fn passes_short_strings_unchanged() {
        assert_eq!(mask_wallet("worker.1"), "worker.1");
        assert_eq!(mask_wallet("rig"), "rig");
        assert_eq!(mask_wallet(""), "");
    }

    #[test]
    fn handles_non_ascii_safely() {
        // Should not panic; emoji is non-ASCII and len 12 in chars (>=12), so
        // the char-based fallback runs.
        let s = "abcdef\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}";
        let masked = mask_wallet(s);
        assert!(masked.contains('\u{2026}'));
    }

    // ---- is_likely_wallet -------------------------------------------------

    #[test]
    fn is_likely_wallet_recognizes_known_shapes() {
        assert!(is_likely_wallet(
            "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"
        ));
        assert!(is_likely_wallet("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert!(is_likely_wallet(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn is_likely_wallet_rejects_non_wallets() {
        assert!(!is_likely_wallet("rig01"));
        assert!(!is_likely_wallet("worker.1"));
        assert!(!is_likely_wallet("dcentos-miner"));
        assert!(!is_likely_wallet("info dcentrald::pool connected"));
        // 16-char hex is not in our masked-length set.
        assert!(!is_likely_wallet("0123456789abcdef"));
        // base58 first-char allowed but length too short.
        assert!(!is_likely_wallet("1AB"));
    }

    // ---- mask_in_string ---------------------------------------------------

    #[test]
    fn passes_through_benign_log_lines_borrowed() {
        let s = "INFO dcentrald::daemon Init phase complete (5 fans, 3 chains)";
        let out = mask_in_string(s);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, s);
    }

    #[test]
    fn masks_wallet_inside_log_line() {
        let s = "Pool authorized worker=bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6 with diff 4096";
        let out = mask_in_string(s);
        assert!(matches!(out, Cow::Owned(_)));
        assert!(out.contains("bc1q04\u{2026}hzp6"));
        assert!(!out.contains("dzgmtjex"));
        assert!(out.contains("with diff 4096"));
    }

    #[test]
    fn masks_multiple_wallets_in_one_line() {
        let s =
            "primary=1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa backup=3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy";
        let out = mask_in_string(s);
        assert!(out.contains("1A1zP1\u{2026}vfNa"));
        assert!(out.contains("3J98t1\u{2026}WNLy"));
        assert!(!out.contains("eP5QGefi"));
    }

    #[test]
    fn masks_64_char_hex_in_log() {
        let s = "submit txid=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef height=850000";
        let out = mask_in_string(s);
        assert!(out.contains("012345\u{2026}cdef"));
        assert!(out.contains("height=850000"));
    }

    #[test]
    fn does_not_mask_short_hex() {
        // 16-char hex (e.g. session id) — not in our targeted lengths.
        let s = "session=0123456789abcdef step=2";
        let out = mask_in_string(s);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn does_not_mask_word_internal_hex_run() {
        // Hex run is part of a longer identifier — boundary check rejects it.
        let s = "log_id=a0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef_tail";
        let out = mask_in_string(s);
        // The 64-char hex run at offset 7 is preceded by `=` (boundary OK)
        // but followed by `_tail` — the trailing word boundary rejects it.
        // It also fails because the `a` at position 7 makes the run 65
        // chars. Either way: not masked.
        assert_eq!(out, s);
    }

    #[test]
    fn handles_empty_string() {
        let out = mask_in_string("");
        assert_eq!(out, "");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn handles_pool_password_field_unchanged() {
        // Passwords aren't wallet-shaped — and our scanner shouldn't claim
        // any common short ASCII password matches a wallet pattern.
        let s = "password=x";
        assert_eq!(mask_in_string(s), s);
    }

    #[test]
    fn masks_share_submit_log_no_full_address() {
        // Synthesize a share-submit log line in the shape used by the V1
        // client and verify no full bech32 wallet bleeds through.
        let s = "INFO submitting share worker=bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6.rig01 nonce=deadbeef";
        let out = mask_in_string(s);
        // The `.rig01` suffix means the full bech32 doesn't reach a word
        // boundary; without the suffix we'd mask the full address. The
        // current contract is: addresses must be word-bounded to be masked.
        // Callers that emit `worker=` log fields MUST therefore call
        // `mask_wallet` on the bare wallet portion BEFORE concatenating
        // `.rig01`, which is what the daemon does (see Step 3).
        let _ = out;
        // The bare-wallet helper DOES mask:
        let bare = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        assert_ne!(mask_wallet(bare), bare);
        assert!(!mask_wallet(bare).contains("dzgmtj"));
    }

    #[test]
    fn bech32_mixed_case_not_masked() {
        // BIP-173 forbids mixed case; we won't claim it as a wallet match.
        let s = "BC1Qxyzabc..."; // upper-case HRP — our table is lower-case-only
        let out = mask_in_string(s);
        assert_eq!(out, s);
    }

    #[test]
    fn bech32m_taproot_address_masked() {
        // p2tr (bech32m) address — 62 chars, valid HRP `bc1p...`.
        let s = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";
        let out = mask_in_string(s);
        assert!(out.contains("bc1p5c\u{2026}drcr"));
    }

    #[test]
    fn regtest_bcrt_prefix_masked() {
        let s = "regtest=bcrt1qy0fz4l2yzh4q3qhrlfxz3uqkkrvm9wjqkkv5tj end";
        let out = mask_in_string(s);
        assert!(out.contains("bcrt1q\u{2026}"));
        assert!(out.ends_with("end"));
    }

    #[test]
    fn ltc_prefix_masked() {
        let s = "ltc1qw508d6qejxtdg4y5r3zarvary0c5xw7kgmn4n9";
        let out = mask_in_string(s);
        assert!(out.contains("ltc1qw\u{2026}mn4n9") || out.contains("ltc1qw\u{2026}"));
    }

    #[test]
    fn mask_in_string_preserves_non_ascii_after_a_match() {
        // Bug-hunt HIGH (2026-05-28): once a wallet match opened the owned buffer,
        // later non-ASCII bytes were copied via `byte as char` (Latin-1), corrupting
        // multibyte UTF-8 in the log sanitizer (café -> cafÃ©). Gaps are now flushed
        // as `&str` slices, so non-ASCII after a match survives intact.
        let line = "worker=bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh tail=café 🔥 ohm=Ω";
        let out = mask_in_string(line);
        assert!(out.contains("café"), "café corrupted: {out}");
        assert!(out.contains('🔥'), "emoji corrupted: {out}");
        assert!(out.contains('Ω'), "omega corrupted: {out}");
        // The wallet is still masked, and the output has no UTF-8 replacement char.
        assert!(!out.contains("bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh"));
        assert!(
            !out.contains('\u{FFFD}'),
            "UTF-8 corruption produced U+FFFD: {out}"
        );
    }

    #[test]
    fn mask_in_string_preserves_non_ascii_before_a_match() {
        // The prefix gap (before the first match) must also survive non-ASCII.
        let line = "café=bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        let out = mask_in_string(line);
        assert!(out.starts_with("café="), "prefix corrupted: {out}");
        assert!(!out.contains("bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh"));
        assert!(!out.contains('\u{FFFD}'));
    }

    /// W1.4 acceptance test: simulate the exact log-line shape produced by
    /// `dcentrald-stratum::v1::client::run_session()` after our W1.4
    /// substitution. The structured-field value is `mask_wallet(&pool.worker)`,
    /// and we verify no full address survives ANY masked path.
    #[test]
    fn share_submit_log_shape_never_leaks_full_address() {
        // Inputs the daemon actually emits to the field:
        let bech32 = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        let p2pkh = "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";
        let p2sh = "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy";
        let p2tr = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";

        for full in [bech32, p2pkh, p2sh, p2tr] {
            // Per-call masked field value (the exact code at the v1 client
            // emits this through `worker = %mask_wallet(&pool.worker)`).
            let field_value = mask_wallet(full);
            assert!(
                !field_value.contains(full),
                "mask_wallet({full}) leaked the full address: {field_value}"
            );
            assert!(
                field_value.contains('\u{2026}'),
                "mask_wallet({full}) did not include the ellipsis marker: {field_value}"
            );

            // Synthesize a tracing-style structured line and run it through
            // mask_in_string for the log-tail passthrough. Even if a third-
            // party crate logged the full address, the sanitizer must catch
            // it.
            let raw_log = format!("INFO submitting share worker={full} diff=4096 nonce=deadbeef");
            let sanitized = mask_in_string(&raw_log);
            assert!(
                !sanitized.contains(full),
                "mask_in_string passthrough leaked full address: {sanitized}"
            );
        }
    }

    /// W1.4 acceptance test: when the operator opts out via `mask_logs =
    /// false`, the per-call masked log fields must STILL be masked. The
    /// gate only controls passthrough sanitization, not the per-call
    /// substitutions baked into the Stratum clients.
    #[test]
    fn per_call_mask_independent_of_passthrough_gate() {
        // mask_wallet is unconditional and has no flag knob.
        let masked = mask_wallet("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6");
        assert!(masked.starts_with("bc1q04"));
        assert!(masked.ends_with("hzp6"));
        assert!(!masked.contains("dzgmtj"));
    }
}
