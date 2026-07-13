//! PIC1704 reflash auto-detection wrapper (W15.B3).
//!
//! Models how a future authorized PIC1704 reflash executor could route to:
//!
//! - [`super::programmer_stock`] (Ghidra-extracted stock bmminer
//!   protocol — PRIMARY since W15.B), or
//! - [`super::programmer_v2`] (W4 V2 theoretical/inferred REG_CMD
//!   0x10-0x15 framed protocol — alternative research model).
//!
//! Both backends require the `recovery-tool` Cargo feature. No shipped package
//! enables it, and the diagnostic-only `pic-recovery` package does not execute
//! this decision. A future transport requires separate controller-recovery
//! authority.
//!
//! ## Routing decision
//!
//! The canonical routing strategy is "try stock, fall back to V2":
//!
//! 1. Send a stock SEEK packet (8 bytes, leading 0x55 magic).
//! 2. Wait 300 ms (`STOCK_INTER_PHASE_MS`).
//! 3. Read 2 bytes.
//! 4. If `[0x01, 0x01]` ([`super::programmer_stock::ACK_SEEK`]) → choose
//!    [`Pic1704Protocol::Stock`].
//! 5. Else (NACK, timeout, or unexpected ACK) → choose
//!    [`Pic1704Protocol::V2Custom`].
//!
//! This module retains DECISION LOGIC only; no shipped I²C probe or mutation
//! transport consumes it. Keeping the routing decision host-safe lets unit
//! tests pin the historical policy without requiring a bus. Any future use
//! must be mediated by the controller-recovery authority architecture.

#![cfg(feature = "recovery-tool")]

/// Which PIC1704 reflash wire protocol to use against the bricked PIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pic1704Protocol {
    /// Ghidra-extracted stock bmminer protocol (W15.B PRIMARY).
    ///
    /// Wire characteristics:
    /// - Magic byte `0x55` first
    /// - 8-byte SEEK + 6-byte ERASE + 22-byte WRITE phase 1 + 6-byte
    ///   WRITE phase 2
    /// - Additive-sum checksum (NOT CRC)
    /// - Two-phase write (data + commit)
    /// - 300 ms inter-phase wait
    /// - ACK signatures: `[0x01,0x01]` SEEK / `[0x04,0x01]` ERASE /
    ///   `[0x02,0x01]` WRITE phase 1 / `[0x05,0x01]` WRITE phase 2
    ///
    /// This is the protocol stock bmminer ships against the chips that
    /// arrive from Bitmain in the field, so it's the "default best
    /// guess" against a freshly-bricked PIC1704.
    Stock,

    /// W4 V2 theoretical/inferred protocol (W14.C).
    ///
    /// Wire characteristics:
    /// - REG_CMD 0x10-0x15 (no magic prefix)
    /// - 24-bit LE address packing
    /// - CRC-ITU-T V.41 checksum (poly 0x1021)
    /// - Single-phase write (`0x12 + count + data`)
    ///
    /// Research model selected when a stock-protocol fixture does not match —
    /// for example, a bootloader image that speaks the dsPIC33EP-derived
    /// protocol. No shipped CLI exposes this selection.
    V2Custom,
}

impl Pic1704Protocol {
    /// Short label for logging / audit lines.
    pub fn label(self) -> &'static str {
        match self {
            Pic1704Protocol::Stock => "stock",
            Pic1704Protocol::V2Custom => "w4v2",
        }
    }
}

/// Auto-detect routing decision based on the result of a stock SEEK
/// probe.
///
/// Pass `Some([b0, b1])` if the I²C read returned 2 bytes; `None` if
/// the read failed (NACK, EIO, short read, or timeout). The decision:
///
/// - `Some([0x01, 0x01])` ([`super::programmer_stock::ACK_SEEK`]) →
///   [`Pic1704Protocol::Stock`]
/// - any other byte pattern OR `None` → [`Pic1704Protocol::V2Custom`]
///
/// The rationale: the stock SEEK packet has a benign leading byte
/// (`0x55`) that does NOT collide with any V2 REG_CMD ordinal, so a
/// chip that speaks V2 (or a totally different protocol) will simply
/// not return the stock ACK signature, and we cleanly fall back. This
/// is asymmetric: V2 callers MUST still pre-read REG_VERSION via the
/// V2 collision guard before any subsequent V2 transaction (sending
/// FP_SEEK 0x10 to an app-mode chip is a silent overvolt risk).
pub fn route_by_seek_ack(stock_seek_ack: Option<[u8; 2]>) -> Pic1704Protocol {
    match stock_seek_ack {
        Some(bytes) if bytes == super::programmer_stock::ACK_SEEK => Pic1704Protocol::Stock,
        _ => Pic1704Protocol::V2Custom,
    }
}

/// Explicit-override entry point for `--pic1704-protocol=<auto|stock|w4v2>`.
///
/// Returns `Some(p)` for `stock` / `w4v2` (explicit), `None` for `auto`
/// (caller must then run [`route_by_seek_ack`] against a real probe).
pub fn parse_protocol_override(s: &str) -> Result<Option<Pic1704Protocol>, String> {
    match s {
        "auto" => Ok(None),
        "stock" => Ok(Some(Pic1704Protocol::Stock)),
        "w4v2" => Ok(Some(Pic1704Protocol::V2Custom)),
        other => Err(format!(
            "unknown --pic1704-protocol {:?}; expected one of: auto, stock, w4v2",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::programmer_stock::{ACK_ERASE, ACK_SEEK, ACK_WRITE_PHASE1};
    use super::*;

    #[test]
    fn auto_detect_picks_stock_on_canonical_ack() {
        // The canonical SEEK ACK [0x01, 0x01] must route to Stock.
        assert_eq!(route_by_seek_ack(Some(ACK_SEEK)), Pic1704Protocol::Stock);
        assert_eq!(
            route_by_seek_ack(Some([0x01, 0x01])),
            Pic1704Protocol::Stock
        );
    }

    #[test]
    fn auto_detect_falls_back_to_v2_on_nack() {
        // None = NACK / EIO / short read → fall back to V2.
        assert_eq!(route_by_seek_ack(None), Pic1704Protocol::V2Custom);
    }

    #[test]
    fn auto_detect_falls_back_to_v2_on_unexpected_ack() {
        // ACK_ERASE [0x04, 0x01] is the wrong phase ACK signature for
        // a SEEK probe — fall back to V2 instead of trusting it.
        assert_eq!(
            route_by_seek_ack(Some(ACK_ERASE)),
            Pic1704Protocol::V2Custom
        );
        // WRITE phase 1 ACK [0x02, 0x01] also wrong for SEEK probe.
        assert_eq!(
            route_by_seek_ack(Some(ACK_WRITE_PHASE1)),
            Pic1704Protocol::V2Custom
        );
        // 0xFF, 0xFF (typical SDA pulled high on no-response) → V2.
        assert_eq!(
            route_by_seek_ack(Some([0xFF, 0xFF])),
            Pic1704Protocol::V2Custom
        );
        // Random pattern → V2.
        assert_eq!(
            route_by_seek_ack(Some([0xCA, 0xFE])),
            Pic1704Protocol::V2Custom
        );
        // Reversed bytes → V2 (catches a future endianness flip bug).
        assert_eq!(
            route_by_seek_ack(Some([0x01, 0x00])),
            Pic1704Protocol::V2Custom
        );
        assert_eq!(
            route_by_seek_ack(Some([0x00, 0x01])),
            Pic1704Protocol::V2Custom
        );
    }

    #[test]
    fn protocol_variants_pinned_for_logging() {
        assert_eq!(Pic1704Protocol::Stock.label(), "stock");
        assert_eq!(Pic1704Protocol::V2Custom.label(), "w4v2");
    }

    #[test]
    fn parse_protocol_override_known_values() {
        assert_eq!(parse_protocol_override("auto").unwrap(), None);
        assert_eq!(
            parse_protocol_override("stock").unwrap(),
            Some(Pic1704Protocol::Stock)
        );
        assert_eq!(
            parse_protocol_override("w4v2").unwrap(),
            Some(Pic1704Protocol::V2Custom)
        );
    }

    #[test]
    fn parse_protocol_override_rejects_garbage() {
        assert!(parse_protocol_override("").is_err());
        assert!(parse_protocol_override("STOCK").is_err()); // case-sensitive
        assert!(parse_protocol_override("stock-bmminer").is_err());
        assert!(parse_protocol_override("ghidra").is_err());
    }

    #[test]
    fn protocol_variants_distinct() {
        // Sanity guard for future enum extension.
        assert_ne!(Pic1704Protocol::Stock, Pic1704Protocol::V2Custom);
    }

    #[test]
    fn parse_protocol_override_round_trip_via_label() {
        // Round-trip: parse(label) → Some(variant), label round-trip.
        for variant in [Pic1704Protocol::Stock, Pic1704Protocol::V2Custom] {
            let label = variant.label();
            let parsed = parse_protocol_override(label).unwrap();
            assert_eq!(parsed, Some(variant), "round-trip for {}", label);
        }
    }
}
