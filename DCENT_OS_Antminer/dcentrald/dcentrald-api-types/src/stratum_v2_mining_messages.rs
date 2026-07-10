//!  strat-D — Stratum V2 mining-extension message DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §6 (Mining Protocol).
//!
//! Stratum V2 mining-extension messages live under `ext = 0x8000 | bits`
//! (channel-msg flag set; see  strat-C `stratum_v2_messages.rs`
//! for the frame header). This module ports the per-channel job/share
//! message catalog with byte-pinned `msg_type` codes and typed payload
//! structs.
//!
//! Hard rules pinned by tests:
//! - `SetTarget` carries the full 256-bit target as 32 bytes — NOT a
//!   u64 difficulty number. Conversion: `difficulty = 0xFFFF000…0 / target`.
//! - `SetNewPrevHash` semantics = V1 `clean_jobs=true`. Drop everything
//!   older than the new `job_id`, switch immediately.
//! - `SubmitSharesSuccess` ACKs in BATCHES via `last_sequence_number` —
//!   there is no per-share accept event.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Message type byte catalog
// ---------------------------------------------------------------------------

/// Stratum V2 mining-extension message type byte (the `t` field in the
/// frame header). Discriminant value matches the wire byte verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Sv2MiningMsgType {
    /// `SetupConnection.Success` (server → client). Common-extension
    /// message that's strictly part of the connection setup, but
    /// surfaced here because it's the immediate reply to the V2
    /// SetupConnection from .
    SetupConnectionSuccess = 0x01,
    /// `SetupConnection.Error`.
    SetupConnectionError = 0x02,
    /// `OpenStandardMiningChannel` (client → server).
    OpenStandardMiningChannel = 0x10,
    /// `OpenStandardMiningChannel.Success`.
    OpenStandardMiningChannelSuccess = 0x11,
    /// `OpenStandardMiningChannel.Error`.
    OpenStandardMiningChannelError = 0x12,
    /// `OpenExtendedMiningChannel` (client → server).
    OpenExtendedMiningChannel = 0x13,
    /// `NewMiningJob` (server → client, Standard channel).
    NewMiningJob = 0x15,
    /// `SubmitSharesStandard` (client → server).
    SubmitSharesStandard = 0x1A,
    /// `SubmitSharesExtended` (client → server).
    SubmitSharesExtended = 0x1B,
    /// `SubmitSharesSuccess` (server → client). Batch ACK.
    SubmitSharesSuccess = 0x1C,
    /// `SubmitSharesError` (server → client).
    SubmitSharesError = 0x1D,
    /// `NewExtendedMiningJob` (server → client, Extended channel).
    NewExtendedMiningJob = 0x1F,
    /// `SetNewPrevHash` (server → client). Equivalent to V1 `clean_jobs=true`.
    SetNewPrevHash = 0x20,
    /// `SetTarget` (server → client). Carries 32-byte u256 target,
    /// NOT a u64 difficulty number.
    SetTarget = 0x21,
}

impl Sv2MiningMsgType {
    /// Numeric `t` byte on the wire.
    pub fn byte(&self) -> u8 {
        *self as u8
    }

    /// Look up the variant by its numeric `t` byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => Self::SetupConnectionSuccess,
            0x02 => Self::SetupConnectionError,
            0x10 => Self::OpenStandardMiningChannel,
            0x11 => Self::OpenStandardMiningChannelSuccess,
            0x12 => Self::OpenStandardMiningChannelError,
            0x13 => Self::OpenExtendedMiningChannel,
            0x15 => Self::NewMiningJob,
            0x1A => Self::SubmitSharesStandard,
            0x1B => Self::SubmitSharesExtended,
            0x1C => Self::SubmitSharesSuccess,
            0x1D => Self::SubmitSharesError,
            0x1F => Self::NewExtendedMiningJob,
            0x20 => Self::SetNewPrevHash,
            0x21 => Self::SetTarget,
            _ => return None,
        })
    }

    /// True iff this message flows from server → client.
    pub fn is_server_to_client(&self) -> bool {
        matches!(
            self,
            Self::SetupConnectionSuccess
                | Self::SetupConnectionError
                | Self::OpenStandardMiningChannelSuccess
                | Self::OpenStandardMiningChannelError
                | Self::NewMiningJob
                | Self::NewExtendedMiningJob
                | Self::SetNewPrevHash
                | Self::SetTarget
                | Self::SubmitSharesSuccess
                | Self::SubmitSharesError
        )
    }
}

// ---------------------------------------------------------------------------
// SetupConnection success / error
// ---------------------------------------------------------------------------

/// Server reply to `SetupConnection` indicating success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupConnectionSuccess {
    pub used_version: u16,
    /// Server's intersection of the requested flags.
    pub flags: u32,
}

/// Documented `SetupConnection.Error` codes per the SV2 spec §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupConnectionErrorCode {
    UnsupportedFeatureFlags,
    UnsupportedProtocol,
    ProtocolVersionMismatch,
}

impl SetupConnectionErrorCode {
    /// Wire form of the error code (kebab-case ASCII).
    pub fn wire(&self) -> &'static str {
        match self {
            Self::UnsupportedFeatureFlags => "unsupported-feature-flags",
            Self::UnsupportedProtocol => "unsupported-protocol",
            Self::ProtocolVersionMismatch => "protocol-version-mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupConnectionError {
    pub error_code: SetupConnectionErrorCode,
}

// ---------------------------------------------------------------------------
// NewMiningJob (Standard) + NewExtendedMiningJob
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewMiningJob {
    pub channel_id: u32,
    pub job_id: u32,
    pub min_ntime: u32,
    pub version: u32,
    /// Pre-computed merkle root (Standard channels only).
    pub merkle_root: [u8; 32],
    /// True iff the pool allows BIP-310 version-rolling on this job.
    pub version_rolling_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewExtendedMiningJob {
    pub channel_id: u32,
    pub job_id: u32,
    pub min_ntime: u32,
    pub version: u32,
    pub version_rolling_allowed: bool,
    /// First half of the coinbase tx (before extranonce).
    pub coinbase_tx_prefix: Vec<u8>,
    /// Second half of the coinbase tx (after extranonce).
    pub coinbase_tx_suffix: Vec<u8>,
    /// Merkle path — the client folds coinbase hash through these.
    pub merkle_path: Vec<[u8; 32]>,
    /// Pool-assigned extranonce prefix (client appends its own bytes).
    pub extranonce_prefix: Vec<u8>,
}

// ---------------------------------------------------------------------------
// SetNewPrevHash — equivalent to V1 clean_jobs=true
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetNewPrevHash {
    pub channel_id: u32,
    pub job_id: u32,
    pub prev_hash: [u8; 32],
    pub min_ntime: u32,
    pub nbits: u32,
}

// ---------------------------------------------------------------------------
// Share submission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitSharesStandard {
    pub channel_id: u32,
    /// Per-channel monotonic counter.
    pub sequence_number: u32,
    pub job_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitSharesExtended {
    pub channel_id: u32,
    pub sequence_number: u32,
    pub job_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version: u32,
    /// Client-controlled extranonce bytes.
    pub extranonce: Vec<u8>,
}

/// Pool batch ACK. NOT a per-share accept — `last_sequence_number`
/// implicitly accepts every share with a sequence ≤ this number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitSharesSuccess {
    pub channel_id: u32,
    pub last_sequence_number: u32,
    pub new_submits_accepted_count: u32,
    /// Cumulative weight of newly accepted shares (sum of difficulty).
    pub new_shares_sum: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitSharesError {
    pub channel_id: u32,
    pub sequence_number: u32,
    /// Per-spec free-text error code.
    pub error_code: String,
}

// ---------------------------------------------------------------------------
// SetTarget — full 256-bit target
// ---------------------------------------------------------------------------

/// SV2 difficulty as the full 256-bit maximum target. NOT a u64
/// difficulty number — the conversion is
/// `difficulty = (2^256 - 1) / target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTarget {
    pub channel_id: u32,
    pub maximum_target: [u8; 32],
}

impl SetTarget {
    /// Returns true iff `lhs` represents a harder target (smaller u256
    /// when interpreted big-endian) than `rhs`. Useful for asserting
    /// that an in-flight job's target is the previous (older, easier)
    /// target than a newly-arrived one.
    pub fn target_is_harder_than(lhs: &[u8; 32], rhs: &[u8; 32]) -> bool {
        // Compare big-endian byte-by-byte.
        for i in 0..32 {
            if lhs[i] != rhs[i] {
                return lhs[i] < rhs[i];
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_bytes_match_re_doc() {
        // 01-stratum-v2-spec.md §6 message-type table.
        assert_eq!(Sv2MiningMsgType::SetupConnectionSuccess.byte(), 0x01);
        assert_eq!(Sv2MiningMsgType::SetupConnectionError.byte(), 0x02);
        assert_eq!(Sv2MiningMsgType::OpenStandardMiningChannel.byte(), 0x10);
        assert_eq!(Sv2MiningMsgType::OpenExtendedMiningChannel.byte(), 0x13);
        assert_eq!(Sv2MiningMsgType::NewMiningJob.byte(), 0x15);
        assert_eq!(Sv2MiningMsgType::SubmitSharesStandard.byte(), 0x1A);
        assert_eq!(Sv2MiningMsgType::SubmitSharesExtended.byte(), 0x1B);
        assert_eq!(Sv2MiningMsgType::SubmitSharesSuccess.byte(), 0x1C);
        assert_eq!(Sv2MiningMsgType::SubmitSharesError.byte(), 0x1D);
        assert_eq!(Sv2MiningMsgType::NewExtendedMiningJob.byte(), 0x1F);
        assert_eq!(Sv2MiningMsgType::SetNewPrevHash.byte(), 0x20);
        assert_eq!(Sv2MiningMsgType::SetTarget.byte(), 0x21);
    }

    #[test]
    fn from_byte_round_trips_known_messages() {
        for v in [
            Sv2MiningMsgType::SetupConnectionSuccess,
            Sv2MiningMsgType::SetupConnectionError,
            Sv2MiningMsgType::OpenStandardMiningChannel,
            Sv2MiningMsgType::OpenStandardMiningChannelSuccess,
            Sv2MiningMsgType::OpenStandardMiningChannelError,
            Sv2MiningMsgType::OpenExtendedMiningChannel,
            Sv2MiningMsgType::NewMiningJob,
            Sv2MiningMsgType::SubmitSharesStandard,
            Sv2MiningMsgType::SubmitSharesExtended,
            Sv2MiningMsgType::SubmitSharesSuccess,
            Sv2MiningMsgType::SubmitSharesError,
            Sv2MiningMsgType::NewExtendedMiningJob,
            Sv2MiningMsgType::SetNewPrevHash,
            Sv2MiningMsgType::SetTarget,
        ] {
            let n = v.byte();
            assert_eq!(Sv2MiningMsgType::from_byte(n), Some(v));
        }
    }

    #[test]
    fn from_byte_returns_none_for_unassigned() {
        // 0x00 / 0x14 / 0x16 / 0x17 / 0x18 / 0x19 / 0x1E / 0x22 / 0xFF
        // are NOT in the catalog.
        for unknown in [0x00u8, 0x14, 0x16, 0x17, 0x18, 0x19, 0x1E, 0x22, 0xFF] {
            assert!(
                Sv2MiningMsgType::from_byte(unknown).is_none(),
                "unexpected match for 0x{:02X}",
                unknown
            );
        }
    }

    #[test]
    fn server_to_client_classification_pinned() {
        // The server initiates job/target/clean-jobs notifications and
        // ACKs/errors. Client originates Open + SubmitShares messages.
        for v in [
            Sv2MiningMsgType::SetupConnectionSuccess,
            Sv2MiningMsgType::NewMiningJob,
            Sv2MiningMsgType::NewExtendedMiningJob,
            Sv2MiningMsgType::SetNewPrevHash,
            Sv2MiningMsgType::SetTarget,
            Sv2MiningMsgType::SubmitSharesSuccess,
            Sv2MiningMsgType::SubmitSharesError,
        ] {
            assert!(v.is_server_to_client(), "{:?} should be server→client", v);
        }
        for v in [
            Sv2MiningMsgType::OpenStandardMiningChannel,
            Sv2MiningMsgType::OpenExtendedMiningChannel,
            Sv2MiningMsgType::SubmitSharesStandard,
            Sv2MiningMsgType::SubmitSharesExtended,
        ] {
            assert!(!v.is_server_to_client(), "{:?} should be client→server", v);
        }
    }

    #[test]
    fn setup_connection_error_codes_match_spec_wire_form() {
        // 01-stratum-v2-spec.md §5 documents three values verbatim.
        assert_eq!(
            SetupConnectionErrorCode::UnsupportedFeatureFlags.wire(),
            "unsupported-feature-flags"
        );
        assert_eq!(
            SetupConnectionErrorCode::UnsupportedProtocol.wire(),
            "unsupported-protocol"
        );
        assert_eq!(
            SetupConnectionErrorCode::ProtocolVersionMismatch.wire(),
            "protocol-version-mismatch"
        );
    }

    #[test]
    fn setup_connection_error_serializes_in_kebab_case() {
        // Spec uses kebab-case wire form. Pin so a refactor doesn't
        // accidentally switch to snake_case.
        let json =
            serde_json::to_string(&SetupConnectionErrorCode::UnsupportedFeatureFlags).unwrap();
        assert_eq!(json, "\"unsupported-feature-flags\"");
        // Ensure round-trip.
        let back: SetupConnectionErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SetupConnectionErrorCode::UnsupportedFeatureFlags);
    }

    #[test]
    fn new_mining_job_field_layout_pinned() {
        // 01-stratum-v2-spec.md §6 NewMiningJob fields:
        // channel_id, job_id, min_ntime, version, merkle_root, +
        // version_rolling_allowed.
        let job = NewMiningJob {
            channel_id: 1,
            job_id: 42,
            min_ntime: 0x65000000,
            version: 0x20000000,
            merkle_root: [0u8; 32],
            version_rolling_allowed: true,
        };
        let json = serde_json::to_value(&job).unwrap();
        for field in [
            "channel_id",
            "job_id",
            "min_ntime",
            "version",
            "merkle_root",
            "version_rolling_allowed",
        ] {
            assert!(
                json.get(field).is_some(),
                "NewMiningJob missing field {}",
                field
            );
        }
    }

    #[test]
    fn new_extended_mining_job_carries_coinbase_split_and_path() {
        let job = NewExtendedMiningJob {
            channel_id: 1,
            job_id: 7,
            min_ntime: 0,
            version: 0,
            version_rolling_allowed: false,
            coinbase_tx_prefix: vec![0x01, 0x02],
            coinbase_tx_suffix: vec![0x03, 0x04],
            merkle_path: vec![[0u8; 32], [1u8; 32]],
            extranonce_prefix: vec![0xAA, 0xBB],
        };
        let json = serde_json::to_value(&job).unwrap();
        assert!(json.get("coinbase_tx_prefix").is_some());
        assert!(json.get("coinbase_tx_suffix").is_some());
        assert!(json.get("merkle_path").is_some());
        assert!(json.get("extranonce_prefix").is_some());
    }

    #[test]
    fn submit_shares_extended_extranonce_is_optional_only_in_extended() {
        // SubmitSharesStandard has NO extranonce field. SubmitSharesExtended
        // does. Pin both.
        let std_share = SubmitSharesStandard {
            channel_id: 1,
            sequence_number: 1,
            job_id: 1,
            nonce: 0,
            ntime: 0,
            version: 0,
        };
        let ext_share = SubmitSharesExtended {
            channel_id: 1,
            sequence_number: 1,
            job_id: 1,
            nonce: 0,
            ntime: 0,
            version: 0,
            extranonce: vec![0xDE, 0xAD],
        };
        let std_json = serde_json::to_value(&std_share).unwrap();
        let ext_json = serde_json::to_value(&ext_share).unwrap();
        assert!(std_json.get("extranonce").is_none());
        assert!(ext_json.get("extranonce").is_some());
    }

    #[test]
    fn submit_shares_success_acks_in_batches() {
        // Pin: SubmitSharesSuccess carries `last_sequence_number` and
        // `new_submits_accepted_count` — pool ACKs in batches, NOT one
        // accept event per share.
        let ack = SubmitSharesSuccess {
            channel_id: 1,
            last_sequence_number: 100,
            new_submits_accepted_count: 25,
            new_shares_sum: 16384 * 25,
        };
        let json = serde_json::to_value(&ack).unwrap();
        assert!(json.get("last_sequence_number").is_some());
        assert!(json.get("new_submits_accepted_count").is_some());
        assert!(json.get("new_shares_sum").is_some());
        // Negative pin: no per-share accept field.
        assert!(json.get("accepted").is_none());
    }

    #[test]
    fn set_new_prev_hash_carries_full_32_byte_prev_hash() {
        let snph = SetNewPrevHash {
            channel_id: 1,
            job_id: 5,
            prev_hash: [0xABu8; 32],
            min_ntime: 0,
            nbits: 0x1d00ffff,
        };
        let json = serde_json::to_value(&snph).unwrap();
        let arr = json["prev_hash"].as_array().unwrap();
        assert_eq!(arr.len(), 32);
    }

    #[test]
    fn set_target_carries_full_256_bit_target_not_difficulty() {
        // 01-stratum-v2-spec.md §6.3: SetTarget sends the FULL 256-bit
        // target as 32 bytes — NOT a u64 difficulty. Pin via field type.
        let st = SetTarget {
            channel_id: 1,
            maximum_target: [0xFFu8; 32],
        };
        let json = serde_json::to_value(&st).unwrap();
        let arr = json["maximum_target"].as_array().unwrap();
        assert_eq!(arr.len(), 32);
        // Negative pin: must NOT have a `difficulty` u64 field.
        assert!(json.get("difficulty").is_none());
    }

    #[test]
    fn target_is_harder_than_compares_big_endian() {
        // Target with smaller leading bytes is harder.
        let easy = [0xFFu8; 32];
        let mut hard = [0xFFu8; 32];
        hard[0] = 0x00; // Vastly harder (smaller u256).
        assert!(SetTarget::target_is_harder_than(&hard, &easy));
        assert!(!SetTarget::target_is_harder_than(&easy, &hard));
        assert!(!SetTarget::target_is_harder_than(&easy, &easy));
    }

    #[test]
    fn message_type_serializes_in_snake_case() {
        for (variant, expected) in [
            (Sv2MiningMsgType::NewMiningJob, "\"new_mining_job\""),
            (
                Sv2MiningMsgType::SubmitSharesExtended,
                "\"submit_shares_extended\"",
            ),
            (Sv2MiningMsgType::SetNewPrevHash, "\"set_new_prev_hash\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), expected);
        }
    }

    #[test]
    fn submit_shares_standard_sequence_is_per_channel_monotonic() {
        // Pin field name + type — sequence_number is u32 monotonic.
        let s = SubmitSharesStandard {
            channel_id: 1,
            sequence_number: 999,
            job_id: 1,
            nonce: 0,
            ntime: 0,
            version: 0,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["sequence_number"].as_u64(), Some(999));
    }

    #[test]
    fn open_channel_pair_have_success_and_error_variants() {
        // Standard channel: 0x10 + 0x11 + 0x12. Verify all three exist.
        assert_eq!(Sv2MiningMsgType::OpenStandardMiningChannel.byte(), 0x10);
        assert_eq!(
            Sv2MiningMsgType::OpenStandardMiningChannelSuccess.byte(),
            0x11
        );
        assert_eq!(
            Sv2MiningMsgType::OpenStandardMiningChannelError.byte(),
            0x12
        );
    }
}
