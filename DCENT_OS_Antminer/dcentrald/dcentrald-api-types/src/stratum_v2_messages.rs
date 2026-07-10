//!  strat-C — Stratum V2 wire-spec DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §2 (lines 210-285).
//!
//! Stratum V2 is binary, length-prefixed, and **encrypted at the
//! application layer** (NEVER over TLS — the spec forbids 443). Two
//! layers:
//!
//! 1. **Noise NX handshake** (`Noise_NX_25519_ChaChaPoly_BLAKE2s`) —
//!    3 plaintext messages on the wire, then both sides derive
//!    ChaCha20-Poly1305 cipher contexts.
//! 2. **Framing**: every frame is `[ext:u16 LE | msg_type:u8 |
//!    len:u24 LE | payload]`. After handshake, frames are AEAD
//!    encrypted in 65535-byte chunks with a 16-byte Poly1305 tag.
//!
//! HAZARD pinned by tests:
//! - **Standard channels exhaust the nonce search in ~2.5 s on an S9**
//!   (~13.5 TH/s) — the nonce + version-rolling search space is only
//!   ~2^48 hashes. Below 1 TH/s standard is fine; above it use Extended.
//!.
//! - **Authority-key pinning is MANDATORY**. A miner that skips
//!   verification can be MITM'd into mining for an attacker.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Defaults + constants
// ---------------------------------------------------------------------------

/// Canonical Stratum V2 default port. NEVER 443 (TLS forbidden by spec).
pub const DEFAULT_STRATUM_V2_PORT: u16 = 3336;

/// Bit 15 of the `ext` field flags a per-channel message
/// (`0x8000 | bits` = "channel-msg" flag).
pub const CHANNEL_MSG_FLAG: u16 = 0x8000;

/// Common-extension namespace (no channel routing).
pub const COMMON_EXTENSION: u16 = 0x0000;

/// Maximum payload length the u24 length field can carry (16 MiB).
/// In practice payloads stay under 8 KiB.
pub const MAX_PAYLOAD_LEN: u32 = 0x00FF_FFFF;

/// Hashrate ceiling at which Standard Channels start exhausting nonce
/// search. Above this, callers should switch to Extended Channels.
pub const STANDARD_CHANNEL_NONCE_EXHAUSTION_THS: f32 = 1.0;

// ---------------------------------------------------------------------------
// Noise NX handshake (3 plaintext messages on wire)
// ---------------------------------------------------------------------------

/// One step of the Noise NX handshake. The runtime adapter wires the
/// actual key exchange and AEAD setup in `dcentrald-stratum::v2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseHandshakeStep {
    /// Client → Server: ephemeral pubkey `e` (32 B X25519).
    ClientHello,
    /// Server → Client: `e, ee, s, es, SIGNATURE_NOISE_MESSAGE`
    /// (32 + 32 + 16 + sig blob).
    ServerHello,
    /// Client → Server: `s, se` (32 + 16 MAC).
    ClientFinish,
}

impl NoiseHandshakeStep {
    /// Index in the 3-step handshake sequence.
    pub fn order(&self) -> u8 {
        match self {
            Self::ClientHello => 0,
            Self::ServerHello => 1,
            Self::ClientFinish => 2,
        }
    }

    /// Pretty-printed direction.
    pub fn direction(&self) -> &'static str {
        match self {
            Self::ClientHello | Self::ClientFinish => "client_to_server",
            Self::ServerHello => "server_to_client",
        }
    }
}

/// Cipher suite name negotiated by Noise NX. Pinned because a runtime
/// that drifts to a different suite will silently fail handshake.
pub const NOISE_NX_CIPHER_SUITE: &str = "Noise_NX_25519_ChaChaPoly_BLAKE2s";

// ---------------------------------------------------------------------------
// Frame header (post-handshake plaintext shape)
// ---------------------------------------------------------------------------

/// SV2 frame header per RE doc lines 234-243.
/// Wire bytes: `[ext:u16 LE | msg_type:u8 | len:u24 LE]` (6 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sv2FrameHeader {
    /// Extension namespace. `0x0000` = common; `0x8000 | bits` = channel.
    pub ext: u16,
    /// Message type within the extension.
    pub msg_type: u8,
    /// Payload length (max 16 MiB).
    pub len: u32,
}

impl Sv2FrameHeader {
    /// Wire size of the header (6 bytes).
    pub const WIRE_SIZE: usize = 6;

    /// True iff the frame is routed per-channel (bit 15 set).
    pub fn is_channel_message(&self) -> bool {
        (self.ext & CHANNEL_MSG_FLAG) != 0
    }

    /// Encode the header into 6 wire bytes (LE).
    pub fn encode(&self) -> [u8; 6] {
        let ext = self.ext.to_le_bytes();
        let len = self.len.to_le_bytes();
        // u24 LE = first 3 bytes of u32 LE.
        [ext[0], ext[1], self.msg_type, len[0], len[1], len[2]]
    }

    /// Decode 6 wire bytes back into the typed header. Returns `None` if
    /// the slice is too short.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 6 {
            return None;
        }
        let ext = u16::from_le_bytes([bytes[0], bytes[1]]);
        let msg_type = bytes[2];
        let len = u32::from_le_bytes([bytes[3], bytes[4], bytes[5], 0]);
        Some(Self { ext, msg_type, len })
    }
}

// ---------------------------------------------------------------------------
// SetupConnection (t=0x00 in the common extension)
// ---------------------------------------------------------------------------

/// Top-level protocol role for `SetupConnection`. RE doc line 248.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Sv2Protocol {
    Mining = 0,
    JobDeclaration = 1,
    TemplateDistribution = 2,
}

impl Sv2Protocol {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
    pub fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Mining,
            1 => Self::JobDeclaration,
            2 => Self::TemplateDistribution,
            _ => return None,
        })
    }
}

/// `SetupConnection` flags for the Mining role per RE doc line 253-256.
pub mod setup_flags {
    /// Bit 0 — REQUIRES_STANDARD_JOB.
    pub const REQUIRES_STANDARD_JOB: u32 = 0b0000_0001;
    /// Bit 1 — REQUIRES_WORK_SELECTION.
    pub const REQUIRES_WORK_SELECTION: u32 = 0b0000_0010;
    /// Bit 2 — REQUIRES_VERSION_ROLLING.
    pub const REQUIRES_VERSION_ROLLING: u32 = 0b0000_0100;
}

/// Common-extension `SetupConnection` message (`t=0x00`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupConnection {
    pub protocol: Sv2Protocol,
    pub min_version: u16,
    pub max_version: u16,
    pub flags: u32,
    pub endpoint_host: String,
    pub endpoint_port: u16,
    pub vendor: String,
    pub hardware_version: String,
    pub firmware: String,
    pub device_id: String,
}

impl SetupConnection {
    /// Common-extension message type for `SetupConnection`.
    pub const MSG_TYPE: u8 = 0x00;

    /// True iff the client requires standard-channel jobs.
    pub fn requires_standard_job(&self) -> bool {
        (self.flags & setup_flags::REQUIRES_STANDARD_JOB) != 0
    }

    /// True iff the client requires version-rolling.
    pub fn requires_version_rolling(&self) -> bool {
        (self.flags & setup_flags::REQUIRES_VERSION_ROLLING) != 0
    }
}

// ---------------------------------------------------------------------------
// Mining channel types
// ---------------------------------------------------------------------------

/// Mining channel kind per RE doc lines 259-283.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sv2ChannelType {
    /// Pool computes the full job. Single 80-byte header. Nonce search
    /// exhausts in ~2.5 s on an S9 — see HAZARD.
    Standard,
    /// Client controls extranonce (≤ 16 B), builds own coinbase. The
    /// "cgminer-equivalent" channel.
    Extended,
    /// Multiple Standard channels share one logical control channel.
    Group,
}

impl Sv2ChannelType {
    /// Mining-extension `OpenXChannel` message type byte.
    pub fn open_msg_type(&self) -> u8 {
        match self {
            Self::Standard => 0x10,
            Self::Extended => 0x13,
            // Group channels open as Standard with `group_id != 0`,
            // not via a distinct message type. Fall through to 0x10.
            Self::Group => 0x10,
        }
    }

    /// True iff this channel kind is safe for the given measured
    /// hashrate. Standard exhausts nonce above ~1 TH/s.
    pub fn safe_for_hashrate_ths(&self, ths: f32) -> bool {
        match self {
            Self::Standard => ths <= STANDARD_CHANNEL_NONCE_EXHAUSTION_THS,
            Self::Extended | Self::Group => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_3336_not_443() {
        // RE doc HAZARD: NEVER 443 — TLS forbidden by spec.
        assert_eq!(DEFAULT_STRATUM_V2_PORT, 3336);
        assert_ne!(DEFAULT_STRATUM_V2_PORT, 443);
    }

    #[test]
    fn channel_msg_flag_at_bit_15() {
        assert_eq!(CHANNEL_MSG_FLAG, 0x8000);
        assert_eq!(CHANNEL_MSG_FLAG, 1u16 << 15);
    }

    #[test]
    fn noise_handshake_steps_in_canonical_order() {
        assert_eq!(NoiseHandshakeStep::ClientHello.order(), 0);
        assert_eq!(NoiseHandshakeStep::ServerHello.order(), 1);
        assert_eq!(NoiseHandshakeStep::ClientFinish.order(), 2);
    }

    #[test]
    fn noise_handshake_direction_alternates() {
        assert_eq!(
            NoiseHandshakeStep::ClientHello.direction(),
            "client_to_server"
        );
        assert_eq!(
            NoiseHandshakeStep::ServerHello.direction(),
            "server_to_client"
        );
        assert_eq!(
            NoiseHandshakeStep::ClientFinish.direction(),
            "client_to_server"
        );
    }

    #[test]
    fn noise_cipher_suite_pinned() {
        // A drift to e.g. AES-GCM would silently break handshake.
        assert_eq!(NOISE_NX_CIPHER_SUITE, "Noise_NX_25519_ChaChaPoly_BLAKE2s");
    }

    #[test]
    fn frame_header_is_six_bytes() {
        assert_eq!(Sv2FrameHeader::WIRE_SIZE, 6);
        let hdr = Sv2FrameHeader {
            ext: 0,
            msg_type: 0,
            len: 0,
        };
        assert_eq!(hdr.encode().len(), 6);
    }

    #[test]
    fn frame_header_encodes_little_endian() {
        let hdr = Sv2FrameHeader {
            ext: 0x8001,
            msg_type: 0x10,
            len: 0x123456,
        };
        let bytes = hdr.encode();
        // ext LE: 0x01, 0x80; msg_type: 0x10; len LE u24: 0x56, 0x34, 0x12.
        assert_eq!(bytes, [0x01, 0x80, 0x10, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn frame_header_decode_round_trips() {
        let original = Sv2FrameHeader {
            ext: 0x8001,
            msg_type: 0x13,
            len: 1024,
        };
        let bytes = original.encode();
        let decoded = Sv2FrameHeader::decode(&bytes).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn frame_header_decode_rejects_short_buffer() {
        assert!(Sv2FrameHeader::decode(&[]).is_none());
        assert!(Sv2FrameHeader::decode(&[0x00, 0x00, 0x00, 0x00, 0x00]).is_none());
    }

    #[test]
    fn channel_message_routing_via_bit_15() {
        let common = Sv2FrameHeader {
            ext: 0x0000,
            msg_type: 0x00,
            len: 0,
        };
        assert!(!common.is_channel_message());

        let channel = Sv2FrameHeader {
            ext: 0x8001,
            msg_type: 0x10,
            len: 0,
        };
        assert!(channel.is_channel_message());
    }

    #[test]
    fn setup_connection_flags_bit_positions_pinned() {
        // Per RE doc line 253-256.
        assert_eq!(setup_flags::REQUIRES_STANDARD_JOB, 0x01);
        assert_eq!(setup_flags::REQUIRES_WORK_SELECTION, 0x02);
        assert_eq!(setup_flags::REQUIRES_VERSION_ROLLING, 0x04);
    }

    #[test]
    fn setup_connection_helpers_match_flags() {
        let mut conn = SetupConnection {
            protocol: Sv2Protocol::Mining,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: "pool".into(),
            endpoint_port: 3336,
            vendor: "DCENT".into(),
            hardware_version: "0".into(),
            firmware: concat!("DCENT_OS/", env!("CARGO_PKG_VERSION")).into(),
            device_id: "test".into(),
        };
        assert!(!conn.requires_standard_job());
        assert!(!conn.requires_version_rolling());

        conn.flags = setup_flags::REQUIRES_STANDARD_JOB | setup_flags::REQUIRES_VERSION_ROLLING;
        assert!(conn.requires_standard_job());
        assert!(conn.requires_version_rolling());
    }

    #[test]
    fn setup_connection_msg_type_is_zero() {
        assert_eq!(SetupConnection::MSG_TYPE, 0x00);
    }

    #[test]
    fn protocol_byte_round_trips() {
        for p in [
            Sv2Protocol::Mining,
            Sv2Protocol::JobDeclaration,
            Sv2Protocol::TemplateDistribution,
        ] {
            let n = p.as_u8();
            assert_eq!(Sv2Protocol::from_u8(n), Some(p));
        }
        assert!(Sv2Protocol::from_u8(3).is_none());
        assert!(Sv2Protocol::from_u8(255).is_none());
    }

    #[test]
    fn channel_open_msg_types_match_re_doc() {
        // OpenStandardMiningChannel = t=0x10
        // OpenExtendedMiningChannel = t=0x13
        assert_eq!(Sv2ChannelType::Standard.open_msg_type(), 0x10);
        assert_eq!(Sv2ChannelType::Extended.open_msg_type(), 0x13);
    }

    #[test]
    fn standard_channel_unsafe_above_one_ths() {
        // RE doc HAZARD: standard exhausts nonce search ~2.5s on S9.
        assert!(Sv2ChannelType::Standard.safe_for_hashrate_ths(0.5));
        assert!(Sv2ChannelType::Standard.safe_for_hashrate_ths(1.0));
        assert!(!Sv2ChannelType::Standard.safe_for_hashrate_ths(13.5)); // S9
        assert!(!Sv2ChannelType::Standard.safe_for_hashrate_ths(110.0)); // S19j Pro
    }

    #[test]
    fn extended_channel_safe_at_any_hashrate() {
        for ths in [0.5_f32, 1.0, 13.5, 110.0, 250.0] {
            assert!(
                Sv2ChannelType::Extended.safe_for_hashrate_ths(ths),
                "extended must be safe at {} TH/s",
                ths
            );
        }
    }

    #[test]
    fn handshake_step_serializes_in_snake_case() {
        for (step, expected) in [
            (NoiseHandshakeStep::ClientHello, "\"client_hello\""),
            (NoiseHandshakeStep::ServerHello, "\"server_hello\""),
            (NoiseHandshakeStep::ClientFinish, "\"client_finish\""),
        ] {
            assert_eq!(serde_json::to_string(&step).unwrap(), expected);
        }
    }

    #[test]
    fn channel_type_round_trips_through_serde() {
        for ct in [
            Sv2ChannelType::Standard,
            Sv2ChannelType::Extended,
            Sv2ChannelType::Group,
        ] {
            let json = serde_json::to_string(&ct).unwrap();
            let back: Sv2ChannelType = serde_json::from_str(&json).unwrap();
            assert_eq!(ct, back);
        }
    }

    #[test]
    fn protocol_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&Sv2Protocol::Mining).unwrap(),
            "\"mining\""
        );
        assert_eq!(
            serde_json::to_string(&Sv2Protocol::JobDeclaration).unwrap(),
            "\"job_declaration\""
        );
        assert_eq!(
            serde_json::to_string(&Sv2Protocol::TemplateDistribution).unwrap(),
            "\"template_distribution\""
        );
    }

    #[test]
    fn max_payload_length_is_16_mib() {
        // u24 max = 0x00FFFFFF = 16 MiB - 1.
        assert_eq!(MAX_PAYLOAD_LEN, 0x00FF_FFFF);
        assert_eq!(MAX_PAYLOAD_LEN, (1u32 << 24) - 1);
    }
}
