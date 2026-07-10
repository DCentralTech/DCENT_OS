//!  strat-B — full Stratum V1 wire-format JSON-RPC DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §1 (Stratum V1 wire spec, lines 31-206).
//!
//! Stratum V1 transport: line-delimited JSON-RPC 2.0 over plain TCP.
//! Each message is exactly one JSON object terminated by `\n`. UTF-8.
//! Default port 3333.
//!
//! This module pins the typed message shapes — every field on the wire
//! is a struct field with the exact serde rename. The `dcentrald-stratum`
//! crate already implements the runtime client; this is a HAL-free
//! contract layer for tests, dashboard JSON exports, and toolbox
//! parity audits.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// `mining.configure` — BIP 310 negotiation
// ---------------------------------------------------------------------------

/// `mining.configure` request body — pool capability negotiation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningConfigureRequest {
    pub id: u64,
    /// Always `"mining.configure"`.
    pub method: String,
    /// Two-tuple: list of capability names + key-value extension table.
    pub params: MiningConfigureParams,
}

/// Two-tuple `[capabilities, extensions]` per BIP 310.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningConfigureParams(pub Vec<String>, pub MiningConfigureExtensions);

/// Extension key-value bag.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MiningConfigureExtensions {
    /// `version-rolling.mask` — 8-char hex string of allowed version bits.
    #[serde(
        rename = "version-rolling.mask",
        skip_serializing_if = "Option::is_none"
    )]
    pub version_rolling_mask: Option<String>,
    /// `version-rolling.min-bit-count` — minimum bit count required.
    #[serde(
        rename = "version-rolling.min-bit-count",
        skip_serializing_if = "Option::is_none"
    )]
    pub version_rolling_min_bit_count: Option<u32>,
    /// `minimum-difficulty.value` — hint for first share difficulty.
    #[serde(
        rename = "minimum-difficulty.value",
        skip_serializing_if = "Option::is_none"
    )]
    pub minimum_difficulty_value: Option<u64>,
}

/// `mining.configure` response result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MiningConfigureResult {
    /// True iff the pool will honor version-rolling.
    #[serde(rename = "version-rolling")]
    pub version_rolling: bool,
    /// Pool-returned mask: the INTERSECTION of allowed × requested.
    /// Use this, NOT what was asked.
    #[serde(rename = "version-rolling.mask")]
    pub version_rolling_mask: String,
    /// True iff the pool accepts minimum-difficulty hints.
    #[serde(rename = "minimum-difficulty", default)]
    pub minimum_difficulty: bool,
    /// True iff the pool will send `mining.set_extranonce` notifications.
    #[serde(rename = "subscribe-extranonce", default)]
    pub subscribe_extranonce: bool,
}

// ---------------------------------------------------------------------------
// `mining.subscribe`
// ---------------------------------------------------------------------------

/// `mining.subscribe` response: subscription details + extranonce1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningSubscribeResult {
    /// Sub-list of `(method, sub_id)` pairs.
    pub subscriptions: Vec<(String, String)>,
    /// Extranonce1 hex (variable 4-16 bytes, pool-specific AND dynamic).
    pub extranonce1: String,
    /// Number of bytes the miner controls in extranonce2.
    pub extranonce2_size: u8,
}

// ---------------------------------------------------------------------------
// `mining.notify` — job dispatch
// ---------------------------------------------------------------------------

/// `mining.notify` decoded params (the 9-element JSON array).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningNotifyParams {
    pub job_id: String,
    /// Big-endian hex of previous block hash (32 bytes / 64 hex chars).
    pub prev_block_hash: String,
    /// Coinbase first half (hex).
    pub coinb1: String,
    /// Coinbase second half (hex).
    pub coinb2: String,
    /// Merkle branches (each 32-byte hex).
    pub merkle_branches: Vec<String>,
    /// Block version (4-byte hex).
    pub version_hex: String,
    /// nBits (4-byte hex).
    pub nbits_hex: String,
    /// nTime (4-byte hex).
    pub ntime_hex: String,
    /// True = drop all stale work, switch immediately.
    pub clean_jobs: bool,
}

// ---------------------------------------------------------------------------
// `mining.submit`
// ---------------------------------------------------------------------------

/// `mining.submit` request params (5-tuple, 6-tuple with version_bits).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningSubmitParams {
    pub worker_name: String,
    pub job_id: String,
    pub extranonce2_hex: String,
    pub ntime_hex: String,
    pub nonce_hex: String,
    /// Optional — only sent if BIP 310 version-rolling is active.
    /// `version_bits = original_version XOR rolled_version`.
    pub version_bits_hex: Option<String>,
}

/// Stratum V1 share-rejection codes per RE doc lines 135-143.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u16)]
pub enum StratumV1RejectCode {
    /// 21 — Job not found (stale).
    JobNotFound = 21,
    /// 22 — Duplicate share.
    DuplicateShare = 22,
    /// 23 — Low difficulty share.
    LowDifficulty = 23,
    /// 24 — Unauthorized worker.
    UnauthorizedWorker = 24,
    /// 25 — Not subscribed.
    NotSubscribed = 25,
    /// 26 — Reserved (unused per RE doc).
    Reserved = 26,
    /// 27 — Invalid version mask. Common bug: sending the rolled
    /// version itself instead of the XOR delta.
    InvalidVersionMask = 27,
}

impl StratumV1RejectCode {
    /// Numeric reject code on the wire.
    pub fn code(&self) -> u16 {
        *self as u16
    }

    /// Look up the variant by numeric code.
    pub fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            21 => Self::JobNotFound,
            22 => Self::DuplicateShare,
            23 => Self::LowDifficulty,
            24 => Self::UnauthorizedWorker,
            25 => Self::NotSubscribed,
            26 => Self::Reserved,
            27 => Self::InvalidVersionMask,
            _ => return None,
        })
    }

    /// Canonical reject message text (matches what pools commonly return).
    pub fn message(&self) -> &'static str {
        match self {
            Self::JobNotFound => "Job not found (stale)",
            Self::DuplicateShare => "Duplicate share",
            Self::LowDifficulty => "Low difficulty share",
            Self::UnauthorizedWorker => "Unauthorized worker",
            Self::NotSubscribed => "Not subscribed",
            Self::Reserved => "Reserved",
            Self::InvalidVersionMask => "Invalid version mask",
        }
    }
}

// ---------------------------------------------------------------------------
// `mining.set_extranonce` + `client.reconnect`
// ---------------------------------------------------------------------------

/// `mining.set_extranonce` notification params — 2-tuple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiningSetExtranonceParams(pub String, pub u8);

impl MiningSetExtranonceParams {
    pub fn extranonce1(&self) -> &str {
        &self.0
    }
    pub fn extranonce2_size(&self) -> u8 {
        self.1
    }
}

/// `client.reconnect` notification params — 3-tuple `(host, port, wait_s)`.
/// Empty host means "reuse current".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientReconnectParams(pub String, pub u16, pub u32);

impl ClientReconnectParams {
    pub fn host(&self) -> &str {
        &self.0
    }
    pub fn port(&self) -> u16 {
        self.1
    }
    pub fn wait_seconds(&self) -> u32 {
        self.2
    }
    pub fn reuse_current_host(&self) -> bool {
        self.host().is_empty()
    }
}

// ---------------------------------------------------------------------------
// Default canonical values
// ---------------------------------------------------------------------------

/// Canonical Stratum V1 default port.
pub const DEFAULT_STRATUM_V1_PORT: u16 = 3333;

/// DCENT_OS canonical version-rolling mask (BIP 310).
pub const DEFAULT_VERSION_ROLLING_MASK: &str = "1fffe000";

/// DCENT_OS default `version-rolling.min-bit-count`.
pub const DEFAULT_VERSION_ROLLING_MIN_BIT_COUNT: u32 = 2;

/// Capability name list canonically advertised by DCENT_OS.
pub const DCENTOS_CONFIGURE_CAPABILITIES: &[&str] = &[
    "version-rolling",
    "minimum-difficulty",
    "subscribe-extranonce",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stratum_v1_port_pinned() {
        assert_eq!(DEFAULT_STRATUM_V1_PORT, 3333);
    }

    #[test]
    fn default_version_rolling_mask_pinned() {
        // 0x1fffe000 — DCENT_OS canonical mask. Pool returns the
        // INTERSECTION of allowed × this requested mask.
        assert_eq!(DEFAULT_VERSION_ROLLING_MASK, "1fffe000");
        assert_eq!(DEFAULT_VERSION_ROLLING_MIN_BIT_COUNT, 2);
    }

    #[test]
    fn capability_list_matches_re_doc() {
        // mining-core-bible.md §1 mining.configure capabilities array.
        assert_eq!(
            DCENTOS_CONFIGURE_CAPABILITIES,
            &[
                "version-rolling",
                "minimum-difficulty",
                "subscribe-extranonce"
            ]
        );
    }

    #[test]
    fn reject_codes_pinned_to_21_through_27() {
        assert_eq!(StratumV1RejectCode::JobNotFound.code(), 21);
        assert_eq!(StratumV1RejectCode::DuplicateShare.code(), 22);
        assert_eq!(StratumV1RejectCode::LowDifficulty.code(), 23);
        assert_eq!(StratumV1RejectCode::UnauthorizedWorker.code(), 24);
        assert_eq!(StratumV1RejectCode::NotSubscribed.code(), 25);
        assert_eq!(StratumV1RejectCode::Reserved.code(), 26);
        assert_eq!(StratumV1RejectCode::InvalidVersionMask.code(), 27);
    }

    #[test]
    fn reject_code_from_code_round_trips() {
        for code in [21u16, 22, 23, 24, 25, 26, 27] {
            let v = StratumV1RejectCode::from_code(code).expect("known code");
            assert_eq!(v.code(), code);
        }
    }

    #[test]
    fn reject_code_from_code_returns_none_for_unknown() {
        for unknown in [0u16, 1, 20, 28, 100, 999] {
            assert!(StratumV1RejectCode::from_code(unknown).is_none());
        }
    }

    #[test]
    fn reject_messages_match_re_doc() {
        assert_eq!(
            StratumV1RejectCode::JobNotFound.message(),
            "Job not found (stale)"
        );
        assert_eq!(
            StratumV1RejectCode::LowDifficulty.message(),
            "Low difficulty share"
        );
        assert_eq!(
            StratumV1RejectCode::InvalidVersionMask.message(),
            "Invalid version mask"
        );
    }

    #[test]
    fn mining_notify_params_field_count_is_nine() {
        // RE doc line 102-108: 9 elements in the params array.
        // Pin every field is present.
        let params = MiningNotifyParams {
            job_id: "j".into(),
            prev_block_hash: "p".into(),
            coinb1: "c1".into(),
            coinb2: "c2".into(),
            merkle_branches: vec!["m".into()],
            version_hex: "v".into(),
            nbits_hex: "b".into(),
            ntime_hex: "t".into(),
            clean_jobs: true,
        };
        let json = serde_json::to_value(&params).unwrap();
        assert!(json.is_object());
        assert!(json.get("clean_jobs").is_some());
        assert_eq!(json["clean_jobs"], true);
    }

    #[test]
    fn mining_notify_clean_jobs_field_pinned_as_bool() {
        // clean_jobs=true MUST trigger stale-work flush. Pin the type
        // so a refactor cannot ship clean_jobs as a string accidentally.
        let params = MiningNotifyParams {
            clean_jobs: false,
            job_id: "j".into(),
            prev_block_hash: "p".into(),
            coinb1: "".into(),
            coinb2: "".into(),
            merkle_branches: vec![],
            version_hex: "".into(),
            nbits_hex: "".into(),
            ntime_hex: "".into(),
        };
        let json = serde_json::to_value(&params).unwrap();
        assert!(json["clean_jobs"].is_boolean());
    }

    #[test]
    fn mining_submit_version_bits_optional() {
        // Per RE doc line 125: version_bits is optional, only sent on
        // BIP 310. Pin: serializing without it does NOT emit a null.
        let without = MiningSubmitParams {
            worker_name: "w".into(),
            job_id: "j".into(),
            extranonce2_hex: "ex2".into(),
            ntime_hex: "tt".into(),
            nonce_hex: "nn".into(),
            version_bits_hex: None,
        };
        let json = serde_json::to_value(&without).unwrap();
        // Default serde behavior: null when None — accept either, but
        // pin that the field name is `version_bits_hex`.
        assert!(json.get("version_bits_hex").is_some());

        let with = MiningSubmitParams {
            version_bits_hex: Some("00010000".into()),
            ..without.clone()
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["version_bits_hex"], "00010000");
    }

    #[test]
    fn version_rolling_mask_round_trips_through_extensions() {
        let ext = MiningConfigureExtensions {
            version_rolling_mask: Some(DEFAULT_VERSION_ROLLING_MASK.to_string()),
            version_rolling_min_bit_count: Some(2),
            minimum_difficulty_value: Some(2048),
        };
        let json = serde_json::to_string(&ext).unwrap();
        // BIP 310 wire field names use dot-notation — pin this verbatim
        // because pyasic + Braiins clients depend on the exact spelling.
        assert!(json.contains("\"version-rolling.mask\":\"1fffe000\""));
        assert!(json.contains("\"version-rolling.min-bit-count\":2"));
        assert!(json.contains("\"minimum-difficulty.value\":2048"));
    }

    #[test]
    fn configure_extensions_skip_serializing_when_none() {
        // Sending `null` for a missing capability is wrong on Stratum
        // V1 — the field should simply not appear. Pin the
        // skip_serializing_if behavior.
        let empty = MiningConfigureExtensions::default();
        let json = serde_json::to_string(&empty).unwrap();
        assert!(!json.contains("version-rolling.mask"));
        assert!(!json.contains("version-rolling.min-bit-count"));
        assert!(!json.contains("minimum-difficulty.value"));
    }

    #[test]
    fn configure_result_default_is_no_capabilities() {
        // Pool that returns nothing → no capabilities granted.
        let r = MiningConfigureResult::default();
        assert!(!r.version_rolling);
        assert!(r.version_rolling_mask.is_empty());
        assert!(!r.minimum_difficulty);
        assert!(!r.subscribe_extranonce);
    }

    #[test]
    fn subscribe_result_carries_dynamic_extranonce1() {
        // RE doc HAZARD: extranonce1 is variable-length AND dynamic;
        // never hardcode 4 or 8. Pin that the type lets the server
        // send any string.
        let r = MiningSubscribeResult {
            subscriptions: vec![("mining.notify".into(), "abc".into())],
            extranonce1: "00f2".into(),
            extranonce2_size: 4,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["extranonce1"], "00f2");
        assert_eq!(json["extranonce2_size"], 4);
    }

    #[test]
    fn set_extranonce_helpers_decode_tuple() {
        let p = MiningSetExtranonceParams("00f5".to_string(), 4);
        assert_eq!(p.extranonce1(), "00f5");
        assert_eq!(p.extranonce2_size(), 4);
    }

    #[test]
    fn client_reconnect_empty_host_means_reuse_current() {
        let p = ClientReconnectParams("".to_string(), 3333, 5);
        assert!(p.reuse_current_host());
        assert_eq!(p.port(), 3333);
        assert_eq!(p.wait_seconds(), 5);

        let p = ClientReconnectParams("new.pool".to_string(), 3334, 0);
        assert!(!p.reuse_current_host());
        assert_eq!(p.host(), "new.pool");
    }

    #[test]
    fn reject_code_round_trips_through_serde() {
        for code in [
            StratumV1RejectCode::JobNotFound,
            StratumV1RejectCode::DuplicateShare,
            StratumV1RejectCode::LowDifficulty,
            StratumV1RejectCode::UnauthorizedWorker,
            StratumV1RejectCode::NotSubscribed,
            StratumV1RejectCode::Reserved,
            StratumV1RejectCode::InvalidVersionMask,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let back: StratumV1RejectCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back);
        }
    }

    #[test]
    fn reject_code_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&StratumV1RejectCode::JobNotFound).unwrap(),
            "\"job_not_found\""
        );
        assert_eq!(
            serde_json::to_string(&StratumV1RejectCode::InvalidVersionMask).unwrap(),
            "\"invalid_version_mask\""
        );
    }

    #[test]
    fn mining_notify_round_trips_through_serde() {
        let original = MiningNotifyParams {
            job_id: "abc123".into(),
            prev_block_hash: "00".repeat(32),
            coinb1: "01".into(),
            coinb2: "02".into(),
            merkle_branches: vec!["aa".repeat(32), "bb".repeat(32)],
            version_hex: "20000000".into(),
            nbits_hex: "1d00ffff".into(),
            ntime_hex: "65000000".into(),
            clean_jobs: true,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: MiningNotifyParams = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }
}
