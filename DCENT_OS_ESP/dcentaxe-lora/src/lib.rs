// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — dcentaxe-lora
//
// SX1262 sub-GHz LoRa driver + the lightweight "DCENT mesh" stack for the
// DCENT_axe board line. Every DCENT_axe board (BM1397 single → Hex) carries an
// onboard SX1262 on its OWN dedicated SPI bus (NOT the BAP/J4 header), so a
// UART-mode BAP accessory can never kill the radio. See the fork plan:
//    §4.4
//    §1
//
//
// SCAFFOLD STATUS (LORA-0): this crate is a clean-room driver + protocol
// SKELETON. It is a workspace member but is **NOT** depended on by the
// `dcentaxe` binary, registers **no** `/mcp` tools, adds **no** URI handlers,
// and is gated behind a default-OFF Cargo feature so it never enters a SKU
// image until a board explicitly selects it. Wiring it into the daemon (own
// FreeRTOS task, MCP registration, dashboard panel) is a deliberate follow-up —
// a non-functional "LoRa enabled" control would be a lying UI. See README.md.
//
// HOST-TEST STORY: the driver logic is written against the abstract [`SpiBus`]
// and [`GpioPin`] traits, so every byte-level command can be exercised on the
// dev machine with a mock bus — no ESP32 required. The real ESP-IDF SPI3/HSPI +
// GPIO implementation lives in [`esp_hal`], gated behind the `esp-idf` feature
// so host `cargo test` stays pure-Rust and fast (mirrors the dcentaxe-bap
// crate's `esp-idf`-feature split).

pub mod auth;
pub mod config;
pub mod duty;
pub mod flood;
pub mod gate;
pub mod mcp;
pub mod mesh;
pub mod relay;
pub mod sx1262;

/// Phase-2 Meshtastic-compatible interop: protobuf `Data`/`User`/`Position`,
/// shared-PSK AES-CTR channel crypto, the 16-byte packet header, the modem PHY,
/// and the managed rebroadcast router. Feature-gated (`meshtastic-interop`) so
/// the cheap entry board that never joins a Meshtastic mesh pays nothing —
/// including the AES crates, which are pulled in only by this feature.
#[cfg(feature = "meshtastic-interop")]
pub mod meshtastic;

/// Real ESP32-S3 SPI3/HSPI + GPIO transport. Only compiled when the firmware
/// selects the radio (`--features esp-idf`); excluded from host test builds.
#[cfg(feature = "esp-idf")]
pub mod esp_hal;

pub use auth::{MeshAuthenticator, ReplayGuard};
pub use config::MeshConfig;
pub use duty::{DutyCycle, ModulationParams};
pub use flood::{RebroadcastPlanner, RelayRole, RxAction, SuppressReason};
pub use gate::{AutotunerMode, CommandGate, ControlLimits, GateOutcome, GateReject, MeshControl};
pub use mesh::{MeshFrame, MeshHealth, MeshKind, NodeId, Peer, PeerTable, DEFAULT_TTL, MAX_TTL};
pub use relay::{Enqueued, RelayCache, TxPriority, TxQueue};
pub use sx1262::{CadExitMode, IrqStatus, LoRaPacketConfig, RampTime, Region, Sx1262, TcxoVoltage};

#[cfg(feature = "meshtastic-interop")]
pub use meshtastic::{Channel, MeshtasticPhyConfig, MeshtasticRouter, ModemPreset};

/// Errors surfaced by the radio driver + transport traits.
///
/// Kept deliberately small and `Clone`-able so host tests can assert on the
/// variant. The `Transport`/`Gpio` variants carry an owned string because the
/// concrete HAL error types differ between the host mock and esp-idf-hal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoraError {
    /// SPI transaction failed at the transport layer.
    Transport(String),
    /// A GPIO read/drive failed.
    Gpio(String),
    /// BUSY stayed asserted past the bounded poll budget — the SX1262 never
    /// reported ready. Almost always a wiring / TCXO / power fault on real HW.
    BusyTimeout,
    /// A command/response had an unexpected length or shape.
    Protocol(String),
    /// A value was outside the radio's legal range (frequency, power, …).
    OutOfRange(String),
    /// A control frame arrived over the air without valid owner auth and was
    /// refused (see [`mesh`] — air-gap control must pass the same owner-auth as
    /// the MCP owner-control path).
    Unauthorized,
}

impl core::fmt::Display for LoraError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoraError::Transport(s) => write!(f, "spi transport error: {s}"),
            LoraError::Gpio(s) => write!(f, "gpio error: {s}"),
            LoraError::BusyTimeout => write!(f, "sx1262 BUSY timeout"),
            LoraError::Protocol(s) => write!(f, "lora protocol error: {s}"),
            LoraError::OutOfRange(s) => write!(f, "value out of range: {s}"),
            LoraError::Unauthorized => write!(f, "unauthorized air-gap control"),
        }
    }
}

impl std::error::Error for LoraError {}

/// Abstract full-duplex SPI transaction carrying ONE NSS-framed exchange.
///
/// The SX1262 is an SPI mode-0, MSB-first device whose every command is framed
/// by NSS: assert NSS, clock the opcode + parameters out on MOSI while the chip
/// simultaneously clocks status/response bytes back on MISO, then release NSS.
///
/// `transfer` performs exactly that in place: on entry `buf` holds the bytes to
/// send (opcode followed by params / NOP placeholders); on return `buf` holds
/// the bytes received during the same clocks. The implementation owns NSS
/// assert/release (one call == one transaction) and MUST NOT poll BUSY — BUSY
/// handling lives in the driver via [`GpioPin`] so it is host-testable.
///
/// This is intentionally the `embedded-hal` `SpiBus::transfer_in_place` shape so
/// the esp-idf-hal impl is a thin adapter and a host mock is trivial.
pub trait SpiBus {
    fn transfer(&mut self, buf: &mut [u8]) -> Result<(), LoraError>;
}

/// Abstract single GPIO line. Used for the three SX1262 control pins the driver
/// must touch directly: BUSY (input, polled), DIO1 (input, IRQ line), and
/// NRESET (output, active-low reset).
///
/// Input pins use [`is_high`](GpioPin::is_high); the NRESET output pin uses
/// [`set_low`](GpioPin::set_low)/[`set_high`](GpioPin::set_high). A read-only
/// pin impl may return [`LoraError::Gpio`] from the setters (the driver never
/// drives BUSY/DIO1), and an output-only pin may do the same for `is_high`.
pub trait GpioPin {
    /// Read the line. `true` == logic high.
    fn is_high(&self) -> Result<bool, LoraError>;
    /// Drive the line high.
    fn set_high(&mut self) -> Result<(), LoraError>;
    /// Drive the line low.
    fn set_low(&mut self) -> Result<(), LoraError>;
}

#[cfg(test)]
pub(crate) use mock::{MockPin, MockSpi};

#[cfg(test)]
mod mock {
    //! Shared host-test doubles for [`SpiBus`] / [`GpioPin`]. Exercised by the
    //! per-module unit tests; not compiled into firmware.
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records every byte written (MOSI transcript) and replays a queued MISO
    /// response per `transfer`. If no canned response is queued the bus echoes
    /// zeros (length-preserving), which is the common "command, don't care about
    /// the reply" case.
    pub struct MockSpi {
        /// Flat concatenation of every MOSI buffer seen, in call order.
        pub mosi: RefCell<Vec<u8>>,
        /// One queued MISO buffer per upcoming `transfer`. Each must match the
        /// transfer length.
        pub miso: RefCell<VecDeque<Vec<u8>>>,
        pub fail_next: RefCell<bool>,
    }

    impl MockSpi {
        pub fn new() -> Self {
            Self {
                mosi: RefCell::new(Vec::new()),
                miso: RefCell::new(VecDeque::new()),
                fail_next: RefCell::new(false),
            }
        }
        /// Queue a MISO response for an upcoming transfer of the same length.
        pub fn queue_miso(&self, bytes: &[u8]) {
            self.miso.borrow_mut().push_back(bytes.to_vec());
        }
    }

    impl SpiBus for MockSpi {
        fn transfer(&mut self, buf: &mut [u8]) -> Result<(), LoraError> {
            if *self.fail_next.borrow() {
                *self.fail_next.borrow_mut() = false;
                return Err(LoraError::Transport("injected".into()));
            }
            self.mosi.borrow_mut().extend_from_slice(buf);
            if let Some(resp) = self.miso.borrow_mut().pop_front() {
                assert_eq!(resp.len(), buf.len(), "queued MISO length must match");
                buf.copy_from_slice(&resp);
            } else {
                for b in buf.iter_mut() {
                    *b = 0;
                }
            }
            Ok(())
        }
    }

    /// A GPIO whose `is_high()` plays back a scripted sequence (e.g. BUSY high
    /// for N polls then low) and which records drive transitions for NRESET.
    pub struct MockPin {
        reads: RefCell<VecDeque<bool>>,
        /// Value returned once `reads` is exhausted.
        steady: bool,
        pub drives: RefCell<Vec<bool>>,
    }

    impl MockPin {
        /// A pin that is always at `level`.
        pub fn level(level: bool) -> Self {
            Self {
                reads: RefCell::new(VecDeque::new()),
                steady: level,
                drives: RefCell::new(Vec::new()),
            }
        }
        /// A pin that yields each value in `seq` then holds `steady`.
        pub fn sequence(seq: &[bool], steady: bool) -> Self {
            Self {
                reads: RefCell::new(seq.iter().copied().collect()),
                steady,
                drives: RefCell::new(Vec::new()),
            }
        }
    }

    impl GpioPin for MockPin {
        fn is_high(&self) -> Result<bool, LoraError> {
            Ok(self.reads.borrow_mut().pop_front().unwrap_or(self.steady))
        }
        fn set_high(&mut self) -> Result<(), LoraError> {
            self.drives.borrow_mut().push(true);
            Ok(())
        }
        fn set_low(&mut self) -> Result<(), LoraError> {
            self.drives.borrow_mut().push(false);
            Ok(())
        }
    }

    #[test]
    fn mock_spi_echoes_zero_without_canned_response() {
        let mut spi = MockSpi::new();
        let mut buf = [0xAA, 0xBB, 0xCC];
        spi.transfer(&mut buf).unwrap();
        assert_eq!(buf, [0, 0, 0]);
        assert_eq!(&*spi.mosi.borrow(), &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn mock_pin_sequence_then_steady() {
        let p = MockPin::sequence(&[true, true, false], true);
        assert!(p.is_high().unwrap());
        assert!(p.is_high().unwrap());
        assert!(!p.is_high().unwrap());
        assert!(p.is_high().unwrap(), "falls back to steady");
    }
}
