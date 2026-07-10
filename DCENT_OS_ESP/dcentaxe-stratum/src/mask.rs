// DCENT_axe Stratum — pool worker / URL read-surface masking
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// B-ESP-10 (iteration 2): the pool `worker` is the operator's FULL BTC payout
// address on V1 solo, and a pool URL can embed `user:pass@` credentials. EVERY
// read surface that emits these (HTTP fields, MCP resources, *stratum event
// details*, *console logs*, *the OLED*) must route through these helpers.
//
// `dcentaxe-stratum` builds stratum EVENT DETAIL strings (`StratumEventRecord`)
// and emits client console logs. Those flow verbatim to the HTTP/MCP read
// surfaces (`recent_events`, `primary_failback_detail`, `last_reconnect_cause`).
// The binary's `crate::shared::{sanitize_pool_url, mask_wallet}` is NOT reachable
// from this crate, so these are byte-identical copies of the canonical helpers in
// `dcentaxe/src/shared.rs` (which themselves mirror the Antminer
// `dcentrald_stratum::pool_api::sanitize_pool_url` /
// `dcentrald_common::wallet_mask::mask_wallet`). Keep all three byte-identical so
// axe (binary), axe-stratum (events/logs) and Antminer redact pools the same way.
//
// READ-ONLY: never call these on a config WRITE/edit path; the wire protocol
// (`mining.authorize` params) MUST keep the real worker/url.

/// Mask a pool worker (operator BTC payout address) for read surfaces.
/// `<first6>…<last4>` for inputs ≥ 12 bytes; shorter strings pass through.
pub fn mask_wallet(addr: &str) -> String {
    let bytes = addr.as_bytes();
    if bytes.len() < 12 {
        return addr.to_string();
    }
    // Wallet addresses are ASCII (bech32 / base58 / hex), so byte-slicing 6 from
    // the front and 4 from the back is safe; fall back to a char-based slice for
    // any non-ASCII input to avoid panicking on a UTF-8 boundary.
    if addr.is_ascii() {
        let prefix = &addr[..6];
        let suffix = &addr[addr.len() - 4..];
        return format!("{prefix}\u{2026}{suffix}");
    }
    let chars: Vec<char> = addr.chars().collect();
    if chars.len() < 12 {
        return addr.to_string();
    }
    let prefix: String = chars.iter().take(6).collect();
    let suffix: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{prefix}\u{2026}{suffix}")
}

/// Strip the `user:pass@` authority from a pool URL for read surfaces, keeping
/// `scheme://host[:port][/path]`. A URL without `://` passes through trimmed.
pub fn sanitize_pool_url(url: &str) -> String {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(parts) => parts,
        None => return trimmed.to_string(),
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(authority_end);
    let authority = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    format!("{scheme}://{authority}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_wallet_redacts_btc_payout_address() {
        // bech32 solo payout address → <first6>…<last4>
        let addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        let masked = mask_wallet(addr);
        assert_eq!(masked, "bc1qar\u{2026}5mdq");
        assert!(!masked.contains("0srrr7xfkvy5l643lydnw9re59gtzz"));
    }

    #[test]
    fn mask_wallet_passes_through_short_worker() {
        assert_eq!(mask_wallet("worker"), "worker");
        assert_eq!(mask_wallet("dcentaxe"), "dcentaxe");
        // Exactly 11 bytes still passes through (< 12 threshold).
        assert_eq!(mask_wallet("12345678901"), "12345678901");
    }

    #[test]
    fn mask_wallet_masks_at_twelve_bytes() {
        assert_eq!(mask_wallet("123456789012"), "123456\u{2026}9012");
    }

    #[test]
    fn mask_wallet_handles_non_ascii_without_panicking() {
        // 12 chars but multibyte → char-based slice path, no UTF-8 boundary panic.
        let masked = mask_wallet("αβγδεζηθικλμ");
        assert_eq!(masked, "αβγδεζ\u{2026}ικλμ");
    }

    #[test]
    fn sanitize_pool_url_strips_user_pass_authority() {
        assert_eq!(
            sanitize_pool_url("stratum+tcp://user:pass@pool.example.com:3333"),
            "stratum+tcp://pool.example.com:3333"
        );
    }

    #[test]
    fn sanitize_pool_url_keeps_host_and_path_visible() {
        // Host MUST stay visible (ops diagnostics); only creds are stripped.
        assert_eq!(
            sanitize_pool_url("stratum+tcp://creds:secret@host:21496/path"),
            "stratum+tcp://host:21496/path"
        );
        assert_eq!(
            sanitize_pool_url("stratum+tcp://public-pool.io:21496"),
            "stratum+tcp://public-pool.io:21496"
        );
    }

    #[test]
    fn sanitize_pool_url_passes_through_bare_host() {
        assert_eq!(sanitize_pool_url("public-pool.io"), "public-pool.io");
        assert_eq!(sanitize_pool_url("  solo.ckpool.org  "), "solo.ckpool.org");
    }
}
