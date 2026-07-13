// SPDX-License-Identifier: GPL-3.0-or-later
//! "Bitcoin ON mesh" (Phase 3) — the sovereign no-internet Bitcoin ticker.
//!
//! A **gateway** node (one with internet, `MeshConfig.is_gateway`) fetches the
//! Bitcoin network snapshot from mempool.space and originates it as a
//! [`NetInfo`](crate::mesh::NetInfo) frame. Every off-grid node on the mesh
//! **relays it for free** (the Phase-1 managed flood in [`crate::flood`] carries
//! any [`MeshKind`](crate::mesh::MeshKind), so no new relay logic is needed) and
//! **displays it with no Wi-Fi** — block height, price, difficulty and fee reach
//! a plugged-in-but-offline miner over LoRa (the dcent-ticker crossover).
//!
//! This module is the pure, host-tested logic AROUND that frame:
//!   * [`BitcoinTracker`] — the freshest snapshot heard, with staleness so an
//!     off-grid node never shows hour-old data as live.
//!   * ticker formatters (price with separators, compact difficulty, Moscow time,
//!     a one-line ticker usable as a Meshtastic text broadcast).
//!   * mempool.space response parsers (so the gateway's fetch path is testable
//!     without a network — the crate carries NO runtime JSON dependency, so the
//!     scanner below is a small, robust extractor for mempool's flat responses).
//!
//! The ONLY non-pure part — the actual HTTPS GET — is a thin esp-idf wrapper in
//! the binary's radio task (documented seam; the crate stays host-testable).

use crate::mesh::NetInfo;
use serde::{Deserialize, Serialize};

/// Satoshis per whole coin.
pub const SATS_PER_BTC: f64 = 100_000_000.0;

/// A received [`NetInfo`] older than this (by local receive time) is considered
/// STALE — the ticker shows it greyed/aged rather than as live truth.
pub const NETINFO_STALE_S: u64 = 30 * 60;

/// Default gateway re-fetch/re-beacon cadence (seconds). Infrequent by design;
/// the region duty governor is the hard airtime bound on top of this.
pub const GATEWAY_BEACON_INTERVAL_S: u64 = 10 * 60;

// ───────────────────────────────────────────────────────────────────────────
// Tracker
// ───────────────────────────────────────────────────────────────────────────

/// Tracks the freshest Bitcoin network snapshot heard over the mesh, for the
/// dashboard/MCP ticker. Clock-free: the caller passes a monotonic-ish
/// `now_unix`; freshness is measured from LOCAL receive time (robust to a skew
/// between the gateway's clock and an off-grid consumer's clock).
#[derive(Debug, Clone, Default)]
pub struct BitcoinTracker {
    latest: Option<NetInfo>,
    /// Local time we accepted the current `latest` (drives staleness).
    received_at_unix: u64,
}

impl BitcoinTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update from a received [`NetInfo`]. Accepted only if its gateway
    /// `timestamp_unix` is strictly newer than what we hold — so an OLD relayed
    /// copy (or a rebroadcast of the same snapshot) can never overwrite fresher
    /// data. Returns `true` when the snapshot was accepted.
    pub fn observe(&mut self, info: &NetInfo, now_unix: u64) -> bool {
        let newer = self
            .latest
            .as_ref()
            .map(|cur| info.timestamp_unix > cur.timestamp_unix)
            .unwrap_or(true);
        if newer {
            self.latest = Some(info.clone());
            self.received_at_unix = now_unix;
            true
        } else {
            false
        }
    }

    /// The freshest snapshot held, if any.
    pub fn latest(&self) -> Option<&NetInfo> {
        self.latest.as_ref()
    }

    /// Seconds since this node LOCALLY received the current snapshot.
    pub fn received_age_s(&self, now_unix: u64) -> Option<u64> {
        self.latest
            .as_ref()
            .map(|_| now_unix.saturating_sub(self.received_at_unix))
    }

    /// `true` when a snapshot is held AND was received within [`NETINFO_STALE_S`].
    pub fn is_fresh(&self, now_unix: u64) -> bool {
        self.received_age_s(now_unix)
            .map(|a| a <= NETINFO_STALE_S)
            .unwrap_or(false)
    }

    /// A serde snapshot for the dashboard/MCP ticker surface.
    pub fn view(&self, now_unix: u64) -> BitcoinTickerView {
        match &self.latest {
            Some(n) => BitcoinTickerView {
                present: true,
                fresh: self.is_fresh(now_unix),
                block_height: n.block_height,
                price_usd: n.price_usd,
                difficulty: n.difficulty,
                fee_fastest_sat_vb: n.fee_fastest_sat_vb,
                moscow_time: moscow_time(n.price_usd),
                data_timestamp_unix: n.timestamp_unix,
                received_age_s: self.received_age_s(now_unix).unwrap_or(0),
            },
            None => BitcoinTickerView::default(),
        }
    }
}

/// The dashboard/MCP-facing ticker view (serialized camelCase to match the
/// `/api/system/info` convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BitcoinTickerView {
    /// A snapshot is held.
    pub present: bool,
    /// The held snapshot is within the freshness window.
    pub fresh: bool,
    pub block_height: i64,
    pub price_usd: f64,
    pub difficulty: f64,
    pub fee_fastest_sat_vb: u32,
    /// Sats per USD ("Moscow time").
    pub moscow_time: u64,
    /// Gateway fetch time of the held snapshot.
    pub data_timestamp_unix: u64,
    /// Seconds since this node received it.
    pub received_age_s: u64,
}

// ───────────────────────────────────────────────────────────────────────────
// Gateway helpers
// ───────────────────────────────────────────────────────────────────────────

/// Whether a gateway should re-fetch + re-beacon now, given its last beacon time.
pub fn gateway_should_beacon(last_beacon_unix: u64, now_unix: u64, interval_s: u64) -> bool {
    now_unix.saturating_sub(last_beacon_unix) >= interval_s
}

/// Assemble a [`NetInfo`] from fetched primitives + the gateway's fetch time.
pub fn build_netinfo(
    block_height: i64,
    difficulty: f64,
    price_usd: f64,
    fee_fastest_sat_vb: u32,
    timestamp_unix: u64,
) -> NetInfo {
    NetInfo {
        block_height,
        difficulty,
        price_usd,
        fee_fastest_sat_vb,
        timestamp_unix,
    }
}

/// Build [`NetInfo`] with **network difficulty from compact nBits** (no HTTP).
///
/// Use this on the gateway when a stratum job (or mesh Tip) supplies `nbits`:
/// fills `difficulty` via [`difficulty_from_nbits`] instead of hardcoding `0.0`.
/// Price/fee may still be `0` until an HTTPS client supplies them (operator).
pub fn netinfo_from_stratum_nbits(
    block_height: i64,
    nbits: u32,
    price_usd: f64,
    fee_fastest_sat_vb: u32,
    timestamp_unix: u64,
) -> NetInfo {
    build_netinfo(
        block_height,
        difficulty_from_nbits(nbits),
        price_usd,
        fee_fastest_sat_vb,
        timestamp_unix,
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Formatters (ticker display + Meshtastic text bridge)
// ───────────────────────────────────────────────────────────────────────────

/// Sats per USD ("Moscow time") for a BTC/USD price. `0` for a non-positive price.
pub fn moscow_time(price_usd: f64) -> u64 {
    if price_usd <= 0.0 || !price_usd.is_finite() {
        return 0;
    }
    (SATS_PER_BTC / price_usd) as u64
}

/// Group an integer with thousands separators: `67890 → "67,890"`.
fn group_thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// Price with a `$` and thousands separators, e.g. `"$67,890"`.
pub fn format_price_usd(price: f64) -> String {
    if !price.is_finite() || price < 0.0 {
        return "$-".into();
    }
    format!("${}", group_thousands(price.round() as i64))
}

/// Compact difficulty, e.g. `110568428300952 → "110.57 T"`.
pub fn format_difficulty(d: f64) -> String {
    if !d.is_finite() || d <= 0.0 {
        return "-".into();
    }
    const UNITS: [(&str, f64); 6] = [
        ("E", 1e18),
        ("P", 1e15),
        ("T", 1e12),
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
    ];
    for (suffix, scale) in UNITS {
        if d >= scale {
            return format!("{:.2} {}", d / scale, suffix);
        }
    }
    format!("{d:.0}")
}

/// A one-line human ticker — usable directly as a Meshtastic text broadcast so a
/// gateway can push the ticker onto a stock Meshtastic mesh too (Phase-2 bridge).
pub fn ticker_line(info: &NetInfo) -> String {
    format!(
        "BTC #{} | {} | {} | {} sat/vB",
        info.block_height,
        format_price_usd(info.price_usd),
        format_difficulty(info.difficulty),
        info.fee_fastest_sat_vb
    )
}

// ───────────────────────────────────────────────────────────────────────────
// mempool.space response parsers (no runtime JSON dependency)
// ───────────────────────────────────────────────────────────────────────────

/// Extract the number for `"key"` from a flat JSON body. A pragmatic scanner for
/// mempool.space's flat responses — NOT a full JSON parser (the crate carries no
/// runtime JSON dep). Finds `"key"`, then the next `:`, then the leading number.
pub fn extract_json_number(body: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let key_pos = body.find(&needle)?;
    let after_key = &body[key_pos + needle.len()..];
    let colon = after_key.find(':')?;
    let rest = after_key[colon + 1..].trim_start();
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        .collect();
    num.parse::<f64>().ok()
}

/// Parse the plain-integer body of mempool.space `/api/blocks/tip/height`.
pub fn parse_tip_height(body: &str) -> Option<i64> {
    body.trim().parse::<i64>().ok()
}

/// USD price from `/api/v1/prices` (`{"time":…,"USD":67890,…}`).
pub fn parse_price_usd(body: &str) -> Option<f64> {
    extract_json_number(body, "USD").filter(|p| *p > 0.0)
}

/// Fastest recommended fee from `/api/v1/fees/recommended`
/// (`{"fastestFee":12,…}`).
pub fn parse_fee_fastest(body: &str) -> Option<u32> {
    extract_json_number(body, "fastestFee").map(|f| f.max(0.0) as u32)
}

/// Network difficulty from a block-tip JSON body (first `"difficulty":…`).
pub fn parse_difficulty(body: &str) -> Option<f64> {
    extract_json_number(body, "difficulty").filter(|d| *d > 0.0)
}

// ───────────────────────────────────────────────────────────────────────────
// Compact nBits → network difficulty (no HTTP)
// ───────────────────────────────────────────────────────────────────────────

/// Bitcoin difficulty-1 compact target (`nBits = 0x1d00ffff`).
pub const DIFF1_NBITS: u32 = 0x1d00_ffff;

/// Decode Bitcoin compact `nBits` into a 256-bit big-endian target.
///
/// Returns `None` for zero mantissa, negative compact form, or overflow past
/// 256 bits — never panics. Matches Bitcoin Core `SetCompact` fail-closed
/// semantics for the zero/negative cases we care about for difficulty math.
pub fn target_from_nbits(nbits: u32) -> Option<[u8; 32]> {
    let mantissa = nbits & 0x007f_ffff;
    let exponent = ((nbits >> 24) & 0xff) as i32;
    let negative = nbits & 0x0080_0000 != 0;
    if mantissa == 0 || negative {
        return None;
    }

    let mut target = [0u8; 32];
    if exponent <= 3 {
        let m = mantissa >> (8 * (3 - exponent));
        // Place the 3-byte mantissa at the low end (big-endian target).
        target[29] = ((m >> 16) & 0xff) as u8;
        target[30] = ((m >> 8) & 0xff) as u8;
        target[31] = (m & 0xff) as u8;
    } else {
        // Word size: mantissa is a 3-byte big-endian integer shifted left by
        // 8*(exponent-3) bits. In a 32-byte BE buffer that means the high byte
        // of the mantissa lands at index `32 - exponent`.
        let start = 32i32 - exponent;
        if start < 0 {
            // Overflow — larger than 256-bit; refuse.
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

/// Network difficulty from compact `nBits`: `difficulty_1_target / target`.
///
/// By definition `difficulty_from_nbits(0x1d00ffff) == 1.0`. Returns `0.0` for
/// non-decodable / zero targets (fail-closed for a NetInfo fill — never claim
/// a fabricated difficulty). Pure, host-testable; no HTTP.
pub fn difficulty_from_nbits(nbits: u32) -> f64 {
    let Some(target) = target_from_nbits(nbits) else {
        return 0.0;
    };
    let Some(diff1) = target_from_nbits(DIFF1_NBITS) else {
        return 0.0;
    };
    // target as f64 from the top non-zero bytes (same approach as hash_to_difficulty).
    let t = be_target_to_f64(&target);
    let d1 = be_target_to_f64(&diff1);
    if t <= 0.0 || !t.is_finite() || !d1.is_finite() {
        return 0.0;
    }
    let d = d1 / t;
    if d.is_finite() && d > 0.0 {
        d
    } else {
        0.0
    }
}

fn be_target_to_f64(t: &[u8; 32]) -> f64 {
    let leading = t.iter().take_while(|&&b| b == 0).count();
    if leading >= 32 {
        return 0.0;
    }
    let mut top: u64 = 0;
    let n = 8.min(32 - leading);
    for i in 0..n {
        top = (top << 8) | t[leading + i] as u64;
    }
    if n < 8 {
        top <<= (8 - n) * 8;
    }
    let shift = (32 - leading as i32 - 8) * 8;
    (top as f64) * (2.0_f64).powi(shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: u64) -> NetInfo {
        NetInfo {
            block_height: 901_234,
            difficulty: 110_568_428_300_952.0,
            price_usd: 67_890.0,
            fee_fastest_sat_vb: 12,
            timestamp_unix: ts,
        }
    }

    // ---- tracker ----

    #[test]
    fn tracker_accepts_newer_rejects_old_and_equal() {
        let mut t = BitcoinTracker::new();
        assert!(t.latest().is_none());
        assert!(t.observe(&sample(1000), 5000), "first is accepted");
        assert_eq!(t.latest().unwrap().timestamp_unix, 1000);
        // Same timestamp (a rebroadcast) → rejected, no-op.
        assert!(!t.observe(&sample(1000), 5001));
        // Older gateway timestamp → rejected.
        assert!(!t.observe(&sample(900), 5002));
        // Newer → accepted.
        assert!(t.observe(&sample(1100), 5003));
        assert_eq!(t.latest().unwrap().timestamp_unix, 1100);
    }

    #[test]
    fn tracker_freshness_uses_local_receive_time() {
        let mut t = BitcoinTracker::new();
        t.observe(&sample(1000), 10_000);
        assert!(t.is_fresh(10_000));
        assert!(t.is_fresh(10_000 + NETINFO_STALE_S));
        assert!(
            !t.is_fresh(10_000 + NETINFO_STALE_S + 1),
            "just past the window"
        );
        assert_eq!(t.received_age_s(10_600), Some(600));
        // Freshness is independent of the gateway timestamp being ancient — it's
        // about how long since WE heard it (clock-skew robust).
        let empty = BitcoinTracker::new();
        assert!(!empty.is_fresh(0));
    }

    #[test]
    fn tracker_view_is_populated() {
        let mut t = BitcoinTracker::new();
        t.observe(&sample(1000), 10_000);
        let v = t.view(10_120);
        assert!(v.present && v.fresh);
        assert_eq!(v.block_height, 901_234);
        assert_eq!(v.fee_fastest_sat_vb, 12);
        assert_eq!(v.received_age_s, 120);
        assert_eq!(v.moscow_time, moscow_time(67_890.0));
        // Empty tracker → default (absent) view.
        assert_eq!(BitcoinTracker::new().view(0), BitcoinTickerView::default());
    }

    // ---- gateway cadence ----

    #[test]
    fn gateway_beacon_cadence() {
        assert!(!gateway_should_beacon(1000, 1000 + 300, 600));
        assert!(gateway_should_beacon(1000, 1000 + 600, 600));
        assert!(gateway_should_beacon(1000, 1000 + 9999, 600));
        // First-ever beacon: with `last=0` and a real unix clock (now ≫ interval)
        // the gateway fires at boot rather than waiting a full interval…
        assert!(gateway_should_beacon(0, 1_800_000_000, 600));
        // …but not before `interval` has actually elapsed.
        assert!(!gateway_should_beacon(0, 10, 600));
    }

    // ---- formatters ----

    #[test]
    fn moscow_time_and_price() {
        assert_eq!(moscow_time(100_000.0), 1000);
        assert_eq!(moscow_time(0.0), 0);
        assert_eq!(moscow_time(-5.0), 0);
        assert_eq!(format_price_usd(67_890.0), "$67,890");
        assert_eq!(format_price_usd(1_234_567.0), "$1,234,567");
        assert_eq!(format_price_usd(100.0), "$100");
        assert_eq!(format_price_usd(f64::NAN), "$-");
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(group_thousands(-5000), "-5,000");
    }

    #[test]
    fn difficulty_compact() {
        assert_eq!(format_difficulty(110_568_428_300_952.0), "110.57 T");
        assert_eq!(format_difficulty(0.0), "-");
        assert_eq!(format_difficulty(-1.0), "-");
        assert_eq!(format_difficulty(5_000.0), "5.00 k");
    }

    #[test]
    fn ticker_line_is_readable() {
        let line = ticker_line(&sample(1000));
        assert_eq!(line, "BTC #901234 | $67,890 | 110.57 T | 12 sat/vB");
    }

    // ---- mempool.space parsers ----

    #[test]
    fn parse_tip_height_plain_int() {
        assert_eq!(parse_tip_height("901234\n"), Some(901_234));
        assert_eq!(parse_tip_height("  901234  "), Some(901_234));
        assert_eq!(parse_tip_height("not a number"), None);
    }

    #[test]
    fn parse_prices_response() {
        let body = r#"{"time":1700000000,"USD":67890,"EUR":62000,"GBP":54000}"#;
        assert_eq!(parse_price_usd(body), Some(67_890.0));
        // A float price also parses.
        assert_eq!(parse_price_usd(r#"{"USD":67890.42}"#), Some(67_890.42));
        // Missing / non-positive → None.
        assert_eq!(parse_price_usd(r#"{"EUR":62000}"#), None);
        assert_eq!(parse_price_usd(r#"{"USD":0}"#), None);
    }

    #[test]
    fn parse_recommended_fee_response() {
        let body = r#"{"fastestFee":12,"halfHourFee":8,"hourFee":5,"economyFee":3,"minimumFee":1}"#;
        assert_eq!(parse_fee_fastest(body), Some(12));
        assert_eq!(parse_fee_fastest(r#"{"halfHourFee":8}"#), None);
    }

    #[test]
    fn parse_difficulty_from_block_json() {
        // A trimmed block object as returned in a tip array element.
        let body = r#"{"id":"00..","height":901234,"difficulty":110568428300952.7,"nonce":42}"#;
        let d = parse_difficulty(body).unwrap();
        assert!((d - 110_568_428_300_952.7).abs() < 1.0);
    }

    #[test]
    fn extract_json_number_ignores_key_substrings_and_whitespace() {
        // `"fee"` must not match `"fees"`; whitespace after the colon is skipped.
        let body = r#"{"fees": 999, "fee" :  7 }"#;
        assert_eq!(extract_json_number(body, "fee"), Some(7.0));
        assert_eq!(extract_json_number(body, "fees"), Some(999.0));
        assert_eq!(extract_json_number(body, "absent"), None);
    }

    #[test]
    fn parsers_never_panic_on_garbage() {
        let mut state: u32 = 0xABCD_1234;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _ in 0..4000 {
            let len = (next() % 200) as usize;
            let s: String = (0..len).map(|_| (next() % 128) as u8 as char).collect();
            let _ = parse_tip_height(&s);
            let _ = parse_price_usd(&s);
            let _ = parse_fee_fastest(&s);
            let _ = parse_difficulty(&s);
            let _ = extract_json_number(&s, "USD");
            let _ = difficulty_from_nbits(next());
            let _ = target_from_nbits(next());
        }
    }

    // ---- nBits → difficulty (Phase-3 enhancement / no HTTP) ----

    #[test]
    fn difficulty_from_nbits_diff1_is_one() {
        let d = difficulty_from_nbits(DIFF1_NBITS);
        assert!(
            (d - 1.0).abs() < 1e-9,
            "0x1d00ffff must be difficulty 1.0, got {d}"
        );
    }

    #[test]
    fn netinfo_from_stratum_nbits_fills_diff1() {
        let n = netinfo_from_stratum_nbits(800_000, DIFF1_NBITS, 0.0, 0, 1_800_000_000);
        assert_eq!(n.block_height, 800_000);
        assert!((n.difficulty - 1.0).abs() < 1e-9);
        assert_eq!(n.price_usd, 0.0);
        assert_eq!(n.fee_fastest_sat_vb, 0);
        // Binary can replace the current `build_netinfo(h, 0.0, …)` call with this.
    }

    #[test]
    fn difficulty_from_nbits_known_pairs() {
        // Bitcoin Core GetDifficulty for nBits 0x1b0404cb (block ~100000 era):
        //   dDiff = 0xffff/0x0404cb * 256^(29-0x1b) ≈ 16307.42
        let d = difficulty_from_nbits(0x1b04_04cb);
        assert!(
            (d - 16_307.420_938).abs() < 0.1,
            "unexpected difficulty for 0x1b0404cb: {d}"
        );
        // Half mantissa at same exponent as diff1 → ~2× difficulty.
        let d2 = difficulty_from_nbits(0x1d00_7fff);
        assert!((d2 - 2.000_030_5).abs() < 0.01, "got {d2}");
    }

    #[test]
    fn target_from_nbits_diff1_shape() {
        let t = target_from_nbits(DIFF1_NBITS).unwrap();
        // 0x00ffff << 8*(0x1d-3) = 0x00ffff followed by 26 zero bytes, BE.
        // exponent 0x1d=29 → start index = 32-29 = 3 → bytes [3]=0x00,[4]=0xff,[5]=0xff
        assert_eq!(t[0], 0x00);
        assert_eq!(t[1], 0x00);
        assert_eq!(t[2], 0x00);
        assert_eq!(t[3], 0x00);
        assert_eq!(t[4], 0xff);
        assert_eq!(t[5], 0xff);
        assert!(t[6..].iter().all(|&b| b == 0));
    }

    #[test]
    fn target_from_nbits_refuses_zero_and_negative() {
        assert!(target_from_nbits(0).is_none());
        assert!(target_from_nbits(0x1d00_0000).is_none());
        // Negative compact form (bit 0x00800000).
        assert!(target_from_nbits(0x1d80_ffff).is_none());
        assert_eq!(difficulty_from_nbits(0), 0.0);
    }
}
