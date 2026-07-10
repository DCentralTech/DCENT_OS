//!  ipr-A — Bitmain IP Reporter protocol codec (HAL-free).
//!
//! Source RE evidence:
//!  (281 lines).
//! Decoded verbatim from the Bitmain `Antminer_firmware` repo source
//! at `monitor-ipsig.c`.
//!
//! ## Protocol summary
//!
//! - Miner sends UDP broadcast to **port 14235**, payload `"<IP>,<MAC>"`
//!   ASCII (e.g. `"203.0.113.50,AA:BB:CC:DD:EE:FF"`), no trailing newline.
//! - Miner listens on UDP/14236 for the desktop ACK.
//! - Desktop tool replies with the miner's MAC; matching MAC = success
//!   → miner emits `"OK"`, mismatch = `"FAILD"`.
//! - Triggered by a GPIO button held LOW for ~100 ms debounce.
//! - Cross-compatible with pyasic's `MinerListener` (which reads the same
//!   UDP/14235 broadcast).
//!
//! HAL-free: pure byte-serdes for the wire format. The runtime adapter
//! (in `dcentos-discovery` or future `dcentrald-asic` integration) opens
//! the UDP socket, asserts SO_BROADCAST, and uses these helpers to
//! encode/decode payloads.

use serde::{Deserialize, Serialize};

/// Canonical UDP port the miner broadcasts to.
pub const BROADCAST_PORT: u16 = 14235;

/// Canonical UDP port the miner listens on for the desktop ACK.
pub const MINER_LISTEN_PORT: u16 = 14236;

/// Successful-match ACK string (sent by the miner to the desktop tool).
pub const ACK_OK: &str = "OK";

/// Mismatch-MAC ACK string. Sic — typo from the original Bitmain
/// source ("FAILD" vs. "FAILED") preserved verbatim for compatibility.
pub const ACK_FAIL: &str = "FAILD";

/// One IP Reporter announcement payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IpReporterAnnouncement {
    /// IPv4 address as ASCII (e.g. `"203.0.113.50"`).
    pub ip: String,
    /// MAC address as ASCII (e.g. `"AA:BB:CC:DD:EE:FF"`). Always uppercase
    /// hex with `:` separators per the Bitmain source.
    pub mac: String,
}

impl IpReporterAnnouncement {
    /// Encode to the wire format `"<IP>,<MAC>"` (no trailing newline).
    pub fn encode(&self) -> String {
        format!("{},{}", self.ip, self.mac)
    }

    /// Decode a wire-format payload. Strips trailing whitespace before
    /// splitting (some clients add CRLF; the original protocol does not).
    pub fn decode(bytes: &[u8]) -> Result<Self, IpReporterError> {
        let s = std::str::from_utf8(bytes)
            .map_err(|_| IpReporterError::NotUtf8)?
            .trim_end();
        let mut parts = s.splitn(2, ',');
        let ip = parts
            .next()
            .ok_or(IpReporterError::Malformed)?
            .trim()
            .to_string();
        let mac = parts
            .next()
            .ok_or(IpReporterError::Malformed)?
            .trim()
            .to_string();
        if ip.is_empty() || mac.is_empty() {
            return Err(IpReporterError::Malformed);
        }
        // Sanity-check IP shape: must contain three dots.
        if ip.matches('.').count() != 3 {
            return Err(IpReporterError::InvalidIp { got: ip });
        }
        // Sanity-check MAC shape: must contain five colons.
        if mac.matches(':').count() != 5 {
            return Err(IpReporterError::InvalidMac { got: mac });
        }
        Ok(IpReporterAnnouncement { ip, mac })
    }
}

/// Decode error type for `IpReporterAnnouncement::decode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum IpReporterError {
    /// Payload is not valid UTF-8.
    NotUtf8,
    /// Payload structure is wrong (missing comma, empty fields, etc.).
    Malformed,
    /// IP doesn't have three dots.
    InvalidIp { got: String },
    /// MAC doesn't have five colons.
    InvalidMac { got: String },
}

/// Classify the desktop tool's reply. Returns:
/// - `ReplyVerdict::MacMatch` if the reply equals the miner's MAC.
/// - `ReplyVerdict::MacMismatch { expected, got }` otherwise.
pub fn classify_reply(my_mac: &str, reply_bytes: &[u8]) -> ReplyVerdict {
    let reply = match std::str::from_utf8(reply_bytes) {
        Ok(s) => s.trim(),
        Err(_) => return ReplyVerdict::Garbage,
    };
    // Bitmain source uses strncmp against the miner's MAC; we compare
    // case-insensitively because the desktop tool isn't strict.
    if reply.eq_ignore_ascii_case(my_mac) {
        ReplyVerdict::MacMatch
    } else {
        ReplyVerdict::MacMismatch {
            expected: my_mac.to_string(),
            got: reply.to_string(),
        }
    }
}

/// Result of `classify_reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ReplyVerdict {
    /// Reply matches the miner's MAC — emit `OK`, light green LED.
    MacMatch,
    /// Reply doesn't match — emit `FAILD`, light red LED.
    MacMismatch { expected: String, got: String },
    /// Reply was non-UTF-8 garbage.
    Garbage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ports_match_bitmain_source() {
        // Verbatim from monitor-ipsig.c.
        assert_eq!(BROADCAST_PORT, 14235);
        assert_eq!(MINER_LISTEN_PORT, 14236);
    }

    #[test]
    fn ack_strings_preserve_bitmain_typo() {
        // "FAILD" is the original Bitmain typo — preserve for
        // wire-protocol compatibility with the desktop tool.
        assert_eq!(ACK_OK, "OK");
        assert_eq!(ACK_FAIL, "FAILD");
    }

    #[test]
    fn encode_produces_canonical_wire_form() {
        let a = IpReporterAnnouncement {
            ip: "203.0.113.50".to_string(),
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
        };
        assert_eq!(a.encode(), "203.0.113.50,AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn decode_round_trips_canonical_payload() {
        let a = IpReporterAnnouncement {
            ip: "203.0.113.36".to_string(),
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
        };
        let bytes = a.encode().into_bytes();
        let back = IpReporterAnnouncement::decode(&bytes).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn decode_strips_trailing_crlf() {
        // Some non-Bitmain clients add CRLF; the canonical protocol
        // does not, but decode should be tolerant.
        let bytes = b"203.0.113.50,AA:BB:CC:DD:EE:FF\r\n";
        let r = IpReporterAnnouncement::decode(bytes).unwrap();
        assert_eq!(r.ip, "203.0.113.50");
        assert_eq!(r.mac, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn decode_rejects_missing_comma() {
        let bytes = b"203.0.113.50AA:BB:CC:DD:EE:FF";
        let err = IpReporterAnnouncement::decode(bytes).unwrap_err();
        assert!(matches!(err, IpReporterError::Malformed));
    }

    #[test]
    fn decode_rejects_invalid_ip() {
        let bytes = b"not-an-ip,AA:BB:CC:DD:EE:FF";
        let err = IpReporterAnnouncement::decode(bytes).unwrap_err();
        match err {
            IpReporterError::InvalidIp { got } => assert_eq!(got, "not-an-ip"),
            _ => panic!("expected InvalidIp"),
        }
    }

    #[test]
    fn decode_rejects_invalid_mac() {
        let bytes = b"203.0.113.50,not-a-mac";
        let err = IpReporterAnnouncement::decode(bytes).unwrap_err();
        match err {
            IpReporterError::InvalidMac { got } => assert_eq!(got, "not-a-mac"),
            _ => panic!("expected InvalidMac"),
        }
    }

    #[test]
    fn decode_rejects_empty_payload() {
        let err = IpReporterAnnouncement::decode(b"").unwrap_err();
        assert!(matches!(err, IpReporterError::Malformed));
    }

    #[test]
    fn decode_rejects_invalid_utf8() {
        let bytes = &[0xFF, 0xFE, 0xFD];
        let err = IpReporterAnnouncement::decode(bytes).unwrap_err();
        assert_eq!(err, IpReporterError::NotUtf8);
    }

    #[test]
    fn classify_reply_returns_mac_match_on_exact_mac() {
        let v = classify_reply("AA:BB:CC:DD:EE:FF", b"AA:BB:CC:DD:EE:FF");
        assert_eq!(v, ReplyVerdict::MacMatch);
    }

    #[test]
    fn classify_reply_returns_mac_match_case_insensitive() {
        // Bitmain's original strncmp is case-sensitive but real-world
        // desktop tools sometimes lowercase the MAC; tolerate it.
        let v = classify_reply("AA:BB:CC:DD:EE:FF", b"aa:bb:cc:dd:ee:ff");
        assert_eq!(v, ReplyVerdict::MacMatch);
    }

    #[test]
    fn classify_reply_returns_mismatch_on_wrong_mac() {
        let v = classify_reply("AA:BB:CC:DD:EE:FF", b"11:22:33:44:55:66");
        match v {
            ReplyVerdict::MacMismatch { expected, got } => {
                assert_eq!(expected, "AA:BB:CC:DD:EE:FF");
                assert_eq!(got, "11:22:33:44:55:66");
            }
            _ => panic!("expected MacMismatch"),
        }
    }

    #[test]
    fn classify_reply_returns_garbage_on_invalid_utf8() {
        let v = classify_reply("AA:BB:CC:DD:EE:FF", &[0xFF]);
        assert_eq!(v, ReplyVerdict::Garbage);
    }

    #[test]
    fn announcement_round_trips_through_serde() {
        let a = IpReporterAnnouncement {
            ip: "203.0.113.36".to_string(),
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: IpReporterAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn ip_reporter_error_round_trips_through_serde() {
        let e = IpReporterError::InvalidMac {
            got: "bad".to_string(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: IpReporterError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
        assert!(json.contains("\"error\":\"invalid_mac\""));
    }
}
