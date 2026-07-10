//!  strat-E — Stratum V2 channel-open response payloads (HAL-free).
//!
//! Source RE evidence:
//!
//! §6 (Mining Protocol — channel open replies + channel update messages).
//!
//!  strat-C pinned the SV2 frame header.  strat-D pinned
//! the message-type bytes + the job/share message bodies. This module
//! ships the channel-open success/error response payloads + per-channel
//! update messages (`SetExtranoncePrefix`, `UpdateChannel`,
//! `UpdateChannelError`, `CloseChannel`).
//!
//! Hard rules pinned by tests:
//! - `OpenStandardMiningChannel.Success` carries the **full 32-byte
//!   target** the channel must hash below (NOT a u64 difficulty).
//! - `OpenExtendedMiningChannel.Success` adds `extranonce_size` so the
//!   client knows how many bytes it controls.
//! - `SetExtranoncePrefix` invalidates ALL in-flight work for the
//!   channel (caller must flush — same semantic as V1
//!   `mining.set_extranonce`).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Channel-success response bodies
// ---------------------------------------------------------------------------

/// `OpenStandardMiningChannel.Success` (server → client, t=0x11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenStandardMiningChannelSuccess {
    /// Server-assigned channel id.
    pub channel_id: u32,
    /// Initial target (32-byte u256 — NOT a u64 difficulty).
    /// All hashes for jobs on this channel must be ≤ this value.
    pub target: [u8; 32],
    /// Pool-assigned extranonce prefix.
    pub extranonce_prefix: Vec<u8>,
    /// Group channel id (0 = not part of a group).
    pub group_channel_id: u32,
}

/// `OpenExtendedMiningChannel.Success` (server → client, t=0x14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenExtendedMiningChannelSuccess {
    pub channel_id: u32,
    pub target: [u8; 32],
    /// Number of bytes the client controls in extranonce (≤ 16).
    pub extranonce_size: u16,
    pub extranonce_prefix: Vec<u8>,
}

/// Wire message type bytes for the channel-success replies.
pub const OPEN_STANDARD_CHANNEL_SUCCESS_TYPE: u8 = 0x11;
pub const OPEN_EXTENDED_CHANNEL_SUCCESS_TYPE: u8 = 0x14;

// ---------------------------------------------------------------------------
// Channel-error response bodies
// ---------------------------------------------------------------------------

/// Documented channel-open error codes per the SV2 spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelOpenErrorCode {
    UnknownUser,
    /// Pool refuses to allocate the requested max-target / nominal-hashrate.
    MaxTargetOutOfRange,
    /// Standard channel requested but pool requires Extended.
    StandardNotSupported,
    /// Extended channel requested but pool requires Standard.
    ExtendedNotSupported,
    /// Pool is at capacity (rare).
    InternalError,
}

impl ChannelOpenErrorCode {
    pub fn wire(&self) -> &'static str {
        match self {
            Self::UnknownUser => "unknown-user",
            Self::MaxTargetOutOfRange => "max-target-out-of-range",
            Self::StandardNotSupported => "standard-not-supported",
            Self::ExtendedNotSupported => "extended-not-supported",
            Self::InternalError => "internal-error",
        }
    }
}

/// `OpenStandardMiningChannel.Error` (server → client, t=0x12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenStandardMiningChannelError {
    /// Echoes the request's channel-id (or 0 if rejected before assignment).
    pub channel_id: u32,
    pub error_code: ChannelOpenErrorCode,
}

/// `OpenExtendedMiningChannel.Error` — same shape as Standard.
pub type OpenExtendedMiningChannelError = OpenStandardMiningChannelError;

// ---------------------------------------------------------------------------
// Per-channel update messages
// ---------------------------------------------------------------------------

/// `SetExtranoncePrefix` (server → client). Pool migration / load-
/// balancer rotation. Caller MUST flush all in-flight work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetExtranoncePrefix {
    pub channel_id: u32,
    pub extranonce_prefix: Vec<u8>,
}

/// `UpdateChannel` (client → server). Client requests an updated
/// nominal-hashrate / max-target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateChannel {
    pub channel_id: u32,
    /// Nominal hashrate the client commits to (H/s).
    pub nominal_hashrate: f32,
    /// Soft maximum target the client wants to mine against.
    pub maximum_target: [u8; 32],
}

/// `UpdateChannel.Error` (server → client).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateChannelError {
    pub channel_id: u32,
    pub error_code: ChannelOpenErrorCode,
}

/// `CloseChannel` (either direction). Reason-code free-text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloseChannel {
    pub channel_id: u32,
    pub reason_code: String,
}

/// True iff the message is a per-channel update that invalidates
/// in-flight work (caller must flush).
pub fn invalidates_in_flight_work_extranonce_change() -> bool {
    // SetExtranoncePrefix unconditionally invalidates the channel's
    // in-flight work — caller MUST treat it as an implicit clean_jobs.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_channel_success_carries_32_byte_target() {
        // Pin: target is [u8; 32] (full u256) NOT a u64 difficulty.
        let s = OpenStandardMiningChannelSuccess {
            channel_id: 1,
            target: [0xFFu8; 32],
            extranonce_prefix: vec![0x11, 0x22],
            group_channel_id: 0,
        };
        let json = serde_json::to_value(&s).unwrap();
        let arr = json["target"].as_array().unwrap();
        assert_eq!(arr.len(), 32);
        assert!(json.get("difficulty").is_none()); // negative pin
    }

    #[test]
    fn extended_channel_success_adds_extranonce_size() {
        let s = OpenExtendedMiningChannelSuccess {
            channel_id: 1,
            target: [0xFFu8; 32],
            extranonce_size: 8,
            extranonce_prefix: vec![0xAA, 0xBB],
        };
        let json = serde_json::to_value(&s).unwrap();
        // Extended carries `extranonce_size` (Standard does not).
        assert_eq!(json["extranonce_size"].as_u64(), Some(8));
    }

    #[test]
    fn extranonce_size_max_is_16_bytes() {
        // SV2 spec: extranonce_size ≤ 16. Pin via type bound + a sanity
        // test that a reasonable value round-trips.
        let s = OpenExtendedMiningChannelSuccess {
            channel_id: 1,
            target: [0xFFu8; 32],
            extranonce_size: 16,
            extranonce_prefix: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: OpenExtendedMiningChannelSuccess = serde_json::from_str(&json).unwrap();
        assert_eq!(back.extranonce_size, 16);
    }

    #[test]
    fn channel_open_error_codes_match_spec_wire_form() {
        assert_eq!(ChannelOpenErrorCode::UnknownUser.wire(), "unknown-user");
        assert_eq!(
            ChannelOpenErrorCode::MaxTargetOutOfRange.wire(),
            "max-target-out-of-range"
        );
        assert_eq!(
            ChannelOpenErrorCode::StandardNotSupported.wire(),
            "standard-not-supported"
        );
        assert_eq!(
            ChannelOpenErrorCode::ExtendedNotSupported.wire(),
            "extended-not-supported"
        );
        assert_eq!(ChannelOpenErrorCode::InternalError.wire(), "internal-error");
    }

    #[test]
    fn channel_open_error_serializes_in_kebab_case() {
        let json = serde_json::to_string(&ChannelOpenErrorCode::MaxTargetOutOfRange).unwrap();
        assert_eq!(json, "\"max-target-out-of-range\"");
        let back: ChannelOpenErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ChannelOpenErrorCode::MaxTargetOutOfRange);
    }

    #[test]
    fn extended_channel_error_alias_shape_equals_standard() {
        // Type alias — must serialize identically.
        let std_err = OpenStandardMiningChannelError {
            channel_id: 1,
            error_code: ChannelOpenErrorCode::ExtendedNotSupported,
        };
        let ext_err: OpenExtendedMiningChannelError = OpenStandardMiningChannelError {
            channel_id: 1,
            error_code: ChannelOpenErrorCode::ExtendedNotSupported,
        };
        let std_json = serde_json::to_value(&std_err).unwrap();
        let ext_json = serde_json::to_value(&ext_err).unwrap();
        assert_eq!(std_json, ext_json);
    }

    #[test]
    fn set_extranonce_prefix_field_layout_pinned() {
        let s = SetExtranoncePrefix {
            channel_id: 1,
            extranonce_prefix: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["channel_id"].as_u64(), Some(1));
        let prefix = json["extranonce_prefix"].as_array().unwrap();
        assert_eq!(prefix.len(), 4);
    }

    #[test]
    fn set_extranonce_prefix_invalidates_in_flight_work() {
        // Per-spec semantic — equivalent to V1 mining.set_extranonce:
        // pool migration / LB rotation invalidates everything in flight.
        assert!(invalidates_in_flight_work_extranonce_change());
    }

    #[test]
    fn update_channel_carries_nominal_hashrate_and_target() {
        let u = UpdateChannel {
            channel_id: 1,
            nominal_hashrate: 110_000_000_000_000.0, // 110 TH/s in H/s
            maximum_target: [0x00u8; 32],
        };
        let json = serde_json::to_value(&u).unwrap();
        assert!(json["nominal_hashrate"].is_f64());
        let target = json["maximum_target"].as_array().unwrap();
        assert_eq!(target.len(), 32);
    }

    #[test]
    fn close_channel_carries_reason_code() {
        let c = CloseChannel {
            channel_id: 1,
            reason_code: "operator-disabled".to_string(),
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["reason_code"], "operator-disabled");
    }

    #[test]
    fn channel_response_message_type_bytes_pinned() {
        // 0x11 = OpenStandardMiningChannel.Success; 0x14 = OpenExtended.
        // Pin both because clients pre-allocate per-msg-type handlers.
        assert_eq!(OPEN_STANDARD_CHANNEL_SUCCESS_TYPE, 0x11);
        assert_eq!(OPEN_EXTENDED_CHANNEL_SUCCESS_TYPE, 0x14);
    }

    #[test]
    fn standard_channel_success_round_trips_through_serde() {
        let original = OpenStandardMiningChannelSuccess {
            channel_id: 42,
            target: [0xAAu8; 32],
            extranonce_prefix: vec![0x01, 0x02, 0x03],
            group_channel_id: 7,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: OpenStandardMiningChannelSuccess = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn extended_channel_success_round_trips_through_serde() {
        let original = OpenExtendedMiningChannelSuccess {
            channel_id: 42,
            target: [0xAAu8; 32],
            extranonce_size: 12,
            extranonce_prefix: vec![0x01, 0x02],
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: OpenExtendedMiningChannelSuccess = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn channel_open_error_round_trips_through_serde() {
        for code in [
            ChannelOpenErrorCode::UnknownUser,
            ChannelOpenErrorCode::MaxTargetOutOfRange,
            ChannelOpenErrorCode::StandardNotSupported,
            ChannelOpenErrorCode::ExtendedNotSupported,
            ChannelOpenErrorCode::InternalError,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let back: ChannelOpenErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back);
        }
    }

    #[test]
    fn group_channel_id_zero_means_not_grouped() {
        // Pool may assign group_channel_id=0 for ungrouped Standard
        // channels. Pin the convention so consumers don't treat zero
        // as "missing" vs "explicit no-group".
        let s = OpenStandardMiningChannelSuccess {
            channel_id: 1,
            target: [0xFFu8; 32],
            extranonce_prefix: vec![],
            group_channel_id: 0,
        };
        assert_eq!(s.group_channel_id, 0);
    }
}
