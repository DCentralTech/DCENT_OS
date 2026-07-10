//! Instant troubleshooting tools.
//!
//! Diagnostic tools that return results immediately:
//! - Network test (DNS, gateway, pool connectivity, Stratum handshake)
//! - PSU probe (PMBus readings: VIN, VOUT, IOUT, temperature, faults)
//! - FPGA status (per-chain register dump with decoded fields)
//! - ASIC comm test (GetAddress broadcast, count responses, CRC errors)
//! - I2C scan (bus scan with device identification)

use serde::{Deserialize, Serialize};

/// Network diagnostic test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkTest {
    /// DNS resolution test passed.
    pub dns: bool,
    /// Default gateway reachable.
    pub gateway: bool,
    /// Pool TCP connection successful.
    pub pool_reachable: bool,
    /// Round-trip latency to pool in milliseconds.
    pub latency_ms: u32,
    /// Stratum handshake completed successfully.
    pub stratum_connected: bool,
    /// Error message (if any test failed).
    pub error: Option<String>,
}

/// PSU probe result (PMBus readings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsuProbe {
    /// Whether a PMBus PSU was detected.
    pub detected: bool,
    /// Input voltage (V).
    pub vin_v: f32,
    /// Output voltage (V).
    pub vout_v: f32,
    /// Output current (A).
    pub iout_a: f32,
    /// Input power (W).
    pub pin_w: f32,
    /// Output power (W).
    pub pout_w: f32,
    /// Calculated efficiency (%).
    pub efficiency_pct: f32,
    /// PSU internal temperature (C).
    pub temp_c: f32,
    /// PSU fan speed (RPM, if reported).
    pub fan_rpm: Option<u32>,
    /// Active fault codes.
    pub faults: Vec<String>,
    /// Raw PMBus status word.
    pub status_word: Option<u16>,
}

/// Per-chain FPGA status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpgaStatus {
    /// Chain ID.
    pub chain_id: u8,
    /// FPGA IP core version (hex).
    pub version: String,
    /// Control register value (decoded).
    pub ctrl_reg: u32,
    /// Chain enabled.
    pub enabled: bool,
    /// BM139X mode active.
    pub bm139x_mode: bool,
    /// Current baud rate divisor.
    pub baud_reg: u32,
    /// Calculated baud rate.
    pub baud_rate: u32,
    /// CRC error count.
    pub error_count: u32,
    /// CMD TX FIFO empty.
    pub cmd_tx_empty: bool,
    /// CMD RX FIFO empty.
    pub cmd_rx_empty: bool,
    /// Work TX FIFO empty.
    pub work_tx_empty: bool,
    /// Work RX FIFO empty (no pending nonces).
    pub work_rx_empty: bool,
}

/// ASIC communication test result (per chain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsicCommTest {
    /// Chain ID.
    pub chain_id: u8,
    /// Number of chips that responded.
    pub chip_count: u8,
    /// Chip type detected (e.g., "BM1387").
    pub chip_type: String,
    /// Chip ID hex (e.g., "0x1387").
    pub chip_id: String,
    /// CRC errors during test.
    pub crc_errors: u32,
    /// Response time in milliseconds.
    pub response_time_ms: u32,
    /// Whether communication was successful.
    pub success: bool,
    /// Error message (if failed).
    pub error: Option<String>,
}

/// I2C bus scan result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2cScan {
    /// List of devices found on the bus.
    pub devices: Vec<I2cDevice>,
}

/// A single I2C device found during scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2cDevice {
    /// 7-bit I2C address.
    pub addr: u8,
    /// Address in hex format (e.g., "0x55").
    pub addr_hex: String,
    /// Identified device type.
    pub device_type: String,
    /// Human-readable description.
    pub description: String,
}

impl I2cDevice {
    /// Identify a known device at the given I2C address.
    pub fn identify(addr: u8) -> Self {
        let (device_type, description) = match addr {
            // NOTE: platform-agnostic identification. On S9 (am1) 0x55-0x57 are
            // PIC16F1704 voltage controllers; on am2 hashboards 0x50-0x57 are
            // write-protected serial EEPROMs (HAL write-denylist) — NOT PICs.
            // The description disambiguates so an am2 operator isn't misled into
            // treating these as voltage targets. (gap-swarm HAL-safety #10)
            0x55 => (
                "PIC16F1704",
                "Chain 6 (J6) voltage controller (S9) / write-protected EEPROM (am2)",
            ),
            0x56 => (
                "PIC16F1704",
                "Chain 7 (J7) voltage controller (S9) / write-protected EEPROM (am2)",
            ),
            0x57 => (
                "PIC16F1704",
                "Chain 8 (J8) voltage controller (S9) / write-protected EEPROM (am2)",
            ),
            0x48..=0x4F => ("TMP75", "Temperature sensor"),
            0x50..=0x57 => ("EEPROM", "Serial EEPROM (24C02 or similar)"),
            _ => ("Unknown", "Unknown device"),
        };

        Self {
            addr,
            addr_hex: format!("0x{:02X}", addr),
            device_type: device_type.to_string(),
            description: description.to_string(),
        }
    }
}
