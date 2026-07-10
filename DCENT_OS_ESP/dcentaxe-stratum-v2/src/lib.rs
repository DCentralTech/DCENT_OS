//! Stratum V2 client for DCENT_axe
//!
//! Implements the Mining Protocol (client-side) with Noise Framework encryption.
//! Designed for ESP32-S3 with ~20KB RAM budget.
//!
//! Architecture:
//!   1. Noise_NX handshake (secp256k1 + ChaChaPoly + SHA256)
//!   2. SV2 binary framing (extension_type, msg_type, payload_length, payload)
//!   3. Standard Mining Channel messages
//!   4. Job Declaration (optional, for solo miners)
//!
//! Status: functional standard-channel client. Job declaration is not implemented.

pub mod channel;
pub mod client;
pub mod framing;
pub mod noise;
pub mod types;

/// Stratum V2 protocol version
pub const SV2_PROTOCOL_VERSION: u16 = 2;

/// Maximum message payload size (16KB — fits in ESP32 RAM)
pub const MAX_MESSAGE_SIZE: usize = 16384;

/// SV2 connection state
#[derive(Debug, Clone, PartialEq)]
pub enum Sv2State {
    /// Not connected
    Disconnected,
    /// TCP connected, performing Noise handshake
    Handshaking,
    /// Noise handshake complete, setting up mining channel
    Authenticated,
    /// Mining channel open, ready for work
    Mining,
    /// Error state
    Error(String),
}

/// Check if SV2 is compiled in and available to callers.
pub fn is_available() -> bool {
    true
}
