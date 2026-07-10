// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — dcentaxe-asic
//
// ASIC driver crate for BitAxe mining hardware.
// Rust port of ESP-Miner ASIC drivers (BM1366, BM1368, BM1370, BM1397).
//
// Each driver faithfully reproduces the init sequences, register writes,
// job packet construction, and response parsing from the original C code.

pub mod common;
pub mod crc;
pub mod pll;
pub mod serial;

pub mod bm1366;
pub mod bm1368;
pub mod bm1370;
pub mod bm1373;
pub mod bm1397;

// Research-only KF1950 (WhatsMiner K-series) driver. UNTESTED.
// Gated by `asic-kf1950` Cargo feature, default OFF.
#[cfg(feature = "asic-kf1950")]
pub mod kf1950;

#[cfg(test)]
mod test_utils;

// Re-export key types at crate root for convenience
pub use common::{AsicError, AsicModel, AsicResult, MiningJob, RegisterData, RegisterType};
pub use serial::SerialPort;

/// Core ASIC driver trait -- each chip variant implements this.
///
/// The driver manages the full lifecycle of an ASIC mining chain:
/// initialization, work dispatch, nonce collection, frequency control,
/// and telemetry reads.
pub trait AsicDriver: Send {
    /// Initialize the ASIC chain: detect chips, set addresses, configure registers.
    ///
    /// Performs the full init sequence (chip detection, register configuration,
    /// address assignment, frequency ramp-up).
    ///
    /// # Arguments
    /// * `frequency` - Target hash frequency in MHz
    /// * `chain_count` - Expected number of ASIC chips on the chain
    /// * `initial_difficulty` - Starting TicketMask difficulty. ESP-Miner reads this
    ///   from device config (PR #1594 / `bfc422a`). Caller should pass the last-known
    ///   pool difficulty (NVS cache) or 256.0 as a safe default. `set_difficulty()`
    ///   overrides this once the pool's `mining.set_difficulty` arrives.
    ///
    /// # Returns
    /// The actual number of chips detected, or an error.
    fn init(
        &mut self,
        frequency: f32,
        chain_count: u8,
        initial_difficulty: f64,
    ) -> Result<u8, AsicError>;

    /// Send a mining job to the ASIC chain.
    fn send_work(&mut self, job: &MiningJob) -> Result<(), AsicError>;

    /// Process UART responses -- returns nonces found and/or register data.
    ///
    /// Parses the raw UART response bytes and returns zero or more results.
    /// Each result is either a nonce (job response) or a register value.
    fn process_work(&mut self, rx_buf: &[u8]) -> Result<Vec<AsicResult>, AsicError>;

    /// Set hash frequency (MHz).
    fn set_frequency(&mut self, target_freq: f32) -> Result<(), AsicError>;

    /// Set version mask for AsicBoost / version rolling.
    /// (No-op on BM1397 which doesn't support it.)
    fn set_version_mask(&mut self, mask: u32) -> Result<(), AsicError>;

    /// Read all known registers from all chips.
    fn read_registers(&mut self) -> Result<Vec<RegisterData>, AsicError>;

    /// Get the number of detected chips.
    fn chip_count(&self) -> u8;

    /// Get current operating frequency (MHz).
    fn current_frequency(&self) -> f32;

    /// Read and process UART responses with timeout.
    /// Returns nonces found and/or register data.
    fn read_responses(&mut self, timeout_ms: u16) -> Result<Vec<AsicResult>, AsicError>;

    /// Update the ASIC difficulty mask (TicketMask register).
    /// Called when pool difficulty changes to reduce UART traffic
    /// by filtering nonces below pool difficulty in hardware.
    ///
    /// `difficulty` is `f64` to match Stratum V1 fractional
    /// `mining.set_difficulty` support (ESP-Miner PR #1594).
    fn set_difficulty(&mut self, difficulty: f64) -> Result<(), AsicError>;

    /// Switch ASIC UART to maximum baud rate for full-speed mining.
    /// Must be called after init() completes. Returns the new baud rate
    /// so the host UART can be reconfigured to match.
    fn set_max_baud(&mut self) -> Result<u32, AsicError>;
}

/// Supported ASIC model metadata
impl AsicModel {
    /// Default operating frequency for this ASIC model (MHz)
    pub fn default_frequency(&self) -> f32 {
        match self {
            Self::BM1366 => 485.0,
            Self::BM1368 => 490.0,
            Self::BM1370 => 525.0,
            Self::BM1373 => 550.0, // PROJECTED — verify on hardware
            Self::BM1397 => 400.0,
            // KF1950: PLL formula not RE'd; the upstream fork hardcodes
            // pll_n=0x80 regardless of target. 400 MHz is a safe placeholder.
            #[cfg(feature = "asic-kf1950")]
            Self::KF1950 => 400.0,
            // Avalon A3197 nominal ~500 MHz; CPM table covers 99-600 MHz in
            // ~1.5 MHz steps (293 entries) per AVALON_ASIC_PROTOCOL.md §8.
            #[cfg(feature = "asic-avalon")]
            Self::Avalon => 500.0,
        }
    }

    /// Maximum safe frequency for this ASIC model (MHz)
    pub fn max_frequency(&self) -> f32 {
        match self {
            Self::BM1366 => 600.0,
            Self::BM1368 => 600.0,
            Self::BM1370 => 650.0,
            Self::BM1373 => 700.0, // PROJECTED — verify on hardware
            Self::BM1397 => 500.0,
            // KF1950: M30S/M30S+ class — stock WhatsMiner runs ~600-700 MHz
            // per chip. Conservative bound until verified.
            #[cfg(feature = "asic-kf1950")]
            Self::KF1950 => 600.0,
            // Avalon CPM-table top entry per AVALON_ASIC_PROTOCOL.md §8.
            #[cfg(feature = "asic-avalon")]
            Self::Avalon => 600.0,
        }
    }

    /// Minimum operating frequency (MHz)
    pub fn min_frequency(&self) -> f32 {
        match self {
            Self::BM1366 => 100.0,
            Self::BM1368 => 100.0,
            Self::BM1370 => 100.0,
            Self::BM1373 => 100.0, // PROJECTED
            Self::BM1397 => 50.0,
            #[cfg(feature = "asic-kf1950")]
            Self::KF1950 => 100.0,
            // Avalon CPM-table bottom entry (~99 MHz, rounded up).
            #[cfg(feature = "asic-avalon")]
            Self::Avalon => 100.0,
        }
    }

    /// Expected chip ID register value for detection
    pub fn expected_chip_id(&self) -> u16 {
        match self {
            Self::BM1366 => 0x1366,
            Self::BM1368 => 0x1368,
            Self::BM1370 => 0x1370,
            Self::BM1373 => 0x1373,
            Self::BM1397 => 0x1397,
            #[cfg(feature = "asic-kf1950")]
            Self::KF1950 => 0x1950,
            // Avalon AVA_P_DETECT response carries DNA/version, not a 16-bit
            // chip-ID register. Placeholder to satisfy the trait — actual
            // identity is parsed by `AvalonShimDriver::init` from the
            // AVA_P_ACKDETECT payload.
            #[cfg(feature = "asic-avalon")]
            Self::Avalon => 0x3197,
        }
    }

    /// ASIC response size in bytes (BM1397=9, KF1950=11, others=11)
    pub fn response_size(&self) -> usize {
        match self {
            Self::BM1397 => 9,
            _ => 11,
        }
    }
}

/// Create a driver for the specified ASIC model.
///
/// Note: `AsicModel::Avalon` is **not** constructible via this factory — the
/// Avalon shim driver lives in `projects/dcentaxe-avalon/dcentaxe-nano3s-asic`
/// and `DCENT_OS_AvalonMiner/dcentrald/dcentrald-avalon-asic`, and owns a
/// SysV-msgq transport instead of a `SerialPort`. Construct it directly via
/// `AvalonShimDriver::open_default()` from those crates.
pub fn create_driver(model: AsicModel, serial_port: SerialPort) -> Box<dyn AsicDriver> {
    match model {
        AsicModel::BM1366 => Box::new(bm1366::BM1366::new(serial_port)),
        AsicModel::BM1368 => Box::new(bm1368::BM1368::new(serial_port)),
        AsicModel::BM1370 => Box::new(bm1370::BM1370::new(serial_port)),
        AsicModel::BM1373 => Box::new(bm1373::BM1373::new(serial_port)),
        AsicModel::BM1397 => Box::new(bm1397::BM1397::new(serial_port)),
        #[cfg(feature = "asic-kf1950")]
        AsicModel::KF1950 => Box::new(kf1950::Kf1950::new(serial_port)),
        #[cfg(feature = "asic-avalon")]
        AsicModel::Avalon => panic!(
            "AsicModel::Avalon cannot be constructed via create_driver — \
             use AvalonShimDriver::open_default() from dcentaxe-nano3s-asic \
             or dcentrald-avalon-asic instead. See dcentaxe-asic/src/lib.rs \
             docstring on `create_driver` for context."
        ),
    }
}
