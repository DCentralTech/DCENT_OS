//! ASIC chip driver subsystem for dcentrald.
//!
//! Provides the ChipDriver trait for universal hash board compatibility,
//! the BM13xx wire protocol implementation, PIC microcontroller control,
//! and per-chip-family driver implementations.
//!
//! The ChipDriver trait is the central abstraction that makes Universal Hash
//! Board Compatibility possible. Each ASIC chip family implements this trait
//! with its specific initialization sequence, register values, job format,
//! and nonce decoding.
//!
//! Modules:
//! - `protocol` - BM13xx wire protocol (CRC5, CRC16, framing)
//! - `chain`    - Chain struct for FIFO access and command encoding
//! - `pic`      - PIC16F1704 microcontroller protocol (S9 I2C voltage control)
//! - `dspic`    - dsPIC33EP16GS202 voltage controller (S17/S19 framed I2C protocol)
//! - `pic1704`  - PIC1704 short-form-register voltage controller (CV1835 / AM335x BB / Amlogic S19j Pro)
//! - `drivers`  - Per-chip driver implementations (BM1387, BM1397, etc.)
//! - `bm1362`   - BM1362 cold-boot orchestration (W2.5, byte-sequence-tested)
//! - `bm1387`   - BM1387 protocol reference catalog (W11.10, RE2 §8.1 / §4.1) — reference only
//! - `bm1393`   - BM1393 protocol reference catalog (W11.10, RE2 §8.3 / §8.5) — reference only

// ASIC drivers intentionally retain reverse-engineered register constants,
// alternate firmware frame builders, and gated scaffold paths ahead of live
// platform promotion. Removing them to satisfy dead-code/doc-format lints
// would lose evidence; runtime safety gates still decide whether they can run.
#![allow(
    dead_code,
    clippy::doc_lazy_continuation,
    clippy::empty_line_after_doc_comments
)]

pub mod bm1362;
pub mod bm1387;
pub mod bm1393;
pub mod chain;
pub mod drivers;
pub mod dspic;
pub mod hw_err_tracker;
pub mod pic;
pub mod pic1704;
pub mod protocol;
pub mod uart_trans;

use thiserror::Error;

/// ASIC subsystem error type.
#[derive(Debug, Error)]
pub enum AsicError {
    /// HAL-level error (UIO, I2C, GPIO).
    #[error("HAL error: {0}")]
    Hal(#[from] dcentrald_hal::HalError),

    /// Chip not found in driver registry.
    #[error("unknown chip ID: 0x{chip_id:04X}")]
    UnknownChip { chip_id: u16 },

    /// No chips detected on chain.
    #[error("no chips detected on chain {chain_id}")]
    NoChipsDetected { chain_id: u8 },

    /// PIC communication failure.
    #[error("PIC error on addr 0x{addr:02X}: {detail}")]
    Pic { addr: u8, detail: String },

    /// CRC mismatch in chip response.
    #[error("CRC error on chain {chain_id}: expected 0x{expected:02X}, got 0x{actual:02X}")]
    CrcMismatch {
        chain_id: u8,
        expected: u8,
        actual: u8,
    },

    /// FIFO timeout (no response within expected time).
    #[error("FIFO timeout on chain {chain_id}: {detail}")]
    FifoTimeout { chain_id: u8, detail: String },

    /// GetAddress returned bytes, but the complete FPGA enumeration window did
    /// not satisfy the integrity contract required to select one ASIC driver.
    #[error("enumeration integrity failure on chain {chain_id}: {reason:?}")]
    EnumerationIntegrity {
        chain_id: u8,
        reason: crate::chain::EnumerationIdentityIneligibility,
    },

    /// Chip initialization failed.
    #[error("chip init failed on chain {chain_id}: {detail}")]
    InitFailed { chain_id: u8, detail: String },

    /// Invalid frequency or voltage parameter.
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
}

pub type Result<T> = std::result::Result<T, AsicError>;

pub use hw_err_tracker::{
    EwmaState, HwErrTracker, DEFAULT_ALPHA as HW_ERR_DEFAULT_ALPHA,
    DEFAULT_THRESHOLD as HW_ERR_DEFAULT_THRESHOLD,
};

#[cfg(test)]
mod mock_chain_mini_soak {
    #[derive(Debug, Clone, Copy)]
    struct MockNonce {
        value: u32,
        hw_error: bool,
        temp_c: f64,
    }

    #[derive(Debug)]
    struct MockChain {
        seed: u32,
        tick: u32,
        temp_c: f64,
        hw_error_every: u32,
    }

    impl MockChain {
        fn new(seed: u32, hw_error_every: u32) -> Self {
            Self {
                seed,
                tick: 0,
                temp_c: 58.0,
                hw_error_every,
            }
        }

        fn next_nonce(&mut self) -> MockNonce {
            self.tick = self.tick.saturating_add(1);
            self.seed = self
                .seed
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            self.temp_c += 0.05;
            let hw_error = self.hw_error_every > 0 && self.tick % self.hw_error_every == 0;
            MockNonce {
                value: self.seed,
                hw_error,
                temp_c: self.temp_c,
            }
        }
    }

    #[derive(Debug, Default)]
    struct MiniSoakResult {
        accepted_shares: u32,
        hw_errors: u32,
        max_temp_c: f64,
        failover_triggered: bool,
        ota_preflight_ok: bool,
    }

    fn mock_pool_accepts(nonce: MockNonce) -> bool {
        !nonce.hw_error && nonce.value != 0
    }

    fn run_mini_soak(
        chain: &mut MockChain,
        ticks: u32,
        failover_error_limit: u32,
    ) -> MiniSoakResult {
        let mut result = MiniSoakResult::default();
        let mut consecutive_errors = 0u32;

        for _ in 0..ticks {
            let nonce = chain.next_nonce();
            result.max_temp_c = result.max_temp_c.max(nonce.temp_c);

            if nonce.hw_error {
                result.hw_errors = result.hw_errors.saturating_add(1);
                consecutive_errors = consecutive_errors.saturating_add(1);
            } else {
                consecutive_errors = 0;
            }

            if consecutive_errors >= failover_error_limit {
                result.failover_triggered = true;
                break;
            }

            if mock_pool_accepts(nonce) {
                result.accepted_shares = result.accepted_shares.saturating_add(1);
            }
        }

        result.ota_preflight_ok =
            result.accepted_shares > 0 && result.max_temp_c < 95.0 && !result.failover_triggered;
        result
    }

    #[test]
    fn deterministic_mock_chain_mini_soak_covers_share_failover_and_ota_preflight() {
        let mut healthy_chain = MockChain::new(0xDCE0_0001, 29);
        let healthy = run_mini_soak(&mut healthy_chain, 240, 3);
        assert!(healthy.accepted_shares > 200);
        assert!(healthy.hw_errors > 0);
        assert!(!healthy.failover_triggered);
        assert!(healthy.ota_preflight_ok);
        assert!(healthy.max_temp_c < 95.0);

        let mut failing_chain = MockChain::new(0xDCE0_0001, 1);
        let failing = run_mini_soak(&mut failing_chain, 240, 3);
        assert!(failing.failover_triggered);
        assert!(!failing.ota_preflight_ok);
        assert_eq!(failing.hw_errors, 3);
    }
}
