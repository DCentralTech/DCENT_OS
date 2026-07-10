//! Stock FPGA I2C interface for PIC communication.
//!
//! On stock Bitmain firmware, PIC I2C is handled entirely by the FPGA via the
//! IIC_COMMAND register at byte offset 0x030. There are NO /dev/i2c-* devices
//! used. The FPGA internally handles the I2C bit-banging to the PIC on each
//! hash board.
//!
//! ## Protocol (from bitmaintech/bmminer-mix, driver-btm-c5.h / driver-btm-c5.c)
//!
//! **Single register at byte offset 0x030** (word offset 0x0C).
//!
//! ### Write format (bit 31 must be CLEAR):
//! ```text
//! Bit 31      = 0 (must be clear when writing)
//! Bit 25      = IIC_READ (0=write to PIC, 1=read from PIC)
//! Bit 24      = IIC_REG_ADDR_VALID (unused for S9 PIC protocol)
//! Bits [23:20] = IIC_ADDR_HIGH_4_BIT = 0x0A (fixed for S9)
//! Bits [19:16] = IIC_CHAIN_NUMBER (physical chain: 5, 6, 7, or 8)
//! Bits [15:8]  = IIC_REG_ADDR (unused for S9 PIC, set to 0)
//! Bits [7:0]   = data byte
//! ```
//!
//! ### Completion polling:
//! Read register, check bit 31. When set = done. Response data in bits[7:0].
//!
//! ### S9 PIC command protocol:
//! PIC commands are multi-byte sequences. Each byte is a separate register
//! write+poll cycle. Every command starts with preamble 0x55, 0xAA, then
//! the command byte, then optional data byte(s), then optional read-back.
//!
//! ### NACK detection:
//! When no PIC responds, the FPGA returns bit 31 set with data=0x03.
//!
//! ### Source:
//! - github.com/bitmaintech/bmminer-mix — driver-btm-c5.h, driver-btm-c5.c

use crate::stock_fpga::{StockFpga, REG_IIC_COMMAND};
use crate::{HalError, Result};

// ---------------------------------------------------------------------------
// IIC_COMMAND bit field definitions (from bitmaintech/bmminer-mix)
// ---------------------------------------------------------------------------

/// IIC_READ bit — set for read operations, clear for write.
const IIC_READ: u32 = 1 << 25;

/// IIC_REG_ADDR_VALID bit — not used for S9 PIC protocol.
#[allow(dead_code)]
const IIC_REG_ADDR_VALID: u32 = 1 << 24;

/// IIC_ADDR_HIGH_4_BIT — fixed 0x0A for S9 (PIC address family).
/// For T9_18: 0x04 instead.
const IIC_ADDR_HIGH_S9: u32 = 0x0A << 20;

/// Completion flag — bit 31 set means FPGA finished the I2C transaction.
const IIC_DONE: u32 = 0x8000_0000;

/// NACK response — when no PIC ACKs, FPGA returns data=0x03.
#[allow(dead_code)]
const IIC_NACK_DATA: u8 = 0x03;

/// Encode chain number into bits [19:16].
#[inline]
fn iic_chain_number(chain: u8) -> u32 {
    ((chain as u32) & 0x0F) << 16
}

// ---------------------------------------------------------------------------
// Polling constants (matching bmminer: 1ms delay, ~3-4 retries)
// ---------------------------------------------------------------------------

/// Maximum number of polls waiting for I2C completion.
/// bmminer uses 3-4 retries. We use 100 for safety on slow PIC responses.
const IIC_POLL_MAX: u32 = 100;

/// Delay between polls (milliseconds). bmminer uses usleep(1000) = 1ms.
const IIC_POLL_DELAY_MS: u64 = 1;

// ---------------------------------------------------------------------------
// PIC command constants (from bitmaintech/bmminer-mix driver-btm-c5.h)
// ---------------------------------------------------------------------------

/// PIC command preamble byte 1.
const PIC_COMMAND_1: u8 = 0x55;

/// PIC command preamble byte 2.
const PIC_COMMAND_2: u8 = 0xAA;

/// PIC command codes (S9 PIC16F1704 app mode).
pub mod pic_cmd {
    pub const SET_PIC_FLASH_POINTER: u8 = 0x01;
    pub const SEND_DATA_TO_IIC: u8 = 0x02;
    pub const READ_DATA_FROM_IIC: u8 = 0x03;
    pub const ERASE_IIC_FLASH: u8 = 0x04;
    pub const WRITE_DATA_INTO_PIC: u8 = 0x05;
    pub const JUMP_FROM_LOADER_TO_APP: u8 = 0x06;
    pub const RESET_PIC: u8 = 0x07;
    pub const GET_PIC_FLASH_POINTER: u8 = 0x08;
    pub const ERASE_PIC_APP_PROGRAM: u8 = 0x09;
    pub const SET_VOLTAGE: u8 = 0x10;
    pub const SET_VOLTAGE_TIME: u8 = 0x11;
    pub const SET_HASH_BOARD_ID: u8 = 0x12;
    pub const GET_HASH_BOARD_ID: u8 = 0x13;
    pub const SET_HOST_MAC_ADDRESS: u8 = 0x14;
    pub const ENABLE_VOLTAGE: u8 = 0x15;
    pub const SEND_HEART_BEAT: u8 = 0x16;
    pub const GET_PIC_SOFTWARE_VERSION: u8 = 0x17;
    pub const GET_VOLTAGE: u8 = 0x18;
    pub const GET_DATE: u8 = 0x19;
    pub const GET_WHICH_MAC: u8 = 0x20;
    pub const GET_MAC: u8 = 0x21;
    pub const WR_TEMP_OFFSET_VALUE: u8 = 0x22;
    pub const RD_TEMP_OFFSET_VALUE: u8 = 0x23;
}

// ---------------------------------------------------------------------------
// Low-level register I/O (matches bmminer set_pic_iic exactly)
// ---------------------------------------------------------------------------

/// Core register write + poll. Matches bmminer's `set_pic_iic()`.
///
/// Writes a 32-bit value to IIC_COMMAND with bit 31 cleared, then polls
/// until bit 31 is set (FPGA done). Returns the response data byte (bits[7:0]).
fn set_pic_iic(fpga: &StockFpga, data: u32) -> Result<u8> {
    // Write with bit 31 CLEARED (bmminer does: `data & 0x7FFFFFFF`)
    fpga.write_reg(REG_IIC_COMMAND, data & 0x7FFF_FFFF);

    // Poll for completion (bit 31 set)
    for i in 0..IIC_POLL_MAX {
        let ret = fpga.read_reg(REG_IIC_COMMAND);
        if ret & IIC_DONE != 0 {
            let response = (ret & 0xFF) as u8;
            tracing::trace!(
                written = format_args!("0x{:08X}", data & 0x7FFF_FFFF),
                response_reg = format_args!("0x{:08X}", ret),
                response_data = format_args!("0x{:02X}", response),
                polls = i + 1,
                "IIC_COMMAND done"
            );
            return Ok(response);
        }
        std::thread::sleep(std::time::Duration::from_millis(IIC_POLL_DELAY_MS));
    }

    // Timeout
    let last = fpga.read_reg(REG_IIC_COMMAND);
    tracing::warn!(
        written = format_args!("0x{:08X}", data & 0x7FFF_FFFF),
        last = format_args!("0x{:08X}", last),
        "IIC_COMMAND timeout after {} polls",
        IIC_POLL_MAX,
    );
    Err(HalError::I2c {
        bus: 0,
        addr: 0,
        detail: format!(
            "IIC_COMMAND timeout after {}ms, last=0x{:08X}",
            IIC_POLL_MAX * IIC_POLL_DELAY_MS as u32,
            last,
        ),
    })
}

/// Write a single data byte to PIC via FPGA I2C.
///
/// This is ONE I2C byte transaction. PIC commands require MULTIPLE calls
/// (preamble + command + data).
///
/// Equivalent to bmminer's `write_pic_iic(false, false, 0, chain, data)`.
fn write_pic_iic_byte(fpga: &StockFpga, chain: u8, data: u8) -> Result<u8> {
    let value = IIC_ADDR_HIGH_S9 | iic_chain_number(chain) | (data as u32);
    set_pic_iic(fpga, value)
}

/// Read a single byte from PIC via FPGA I2C.
///
/// Sets the IIC_READ bit (bit 25) to request a read from the PIC.
/// Response data is in bits[7:0] of the completion register value.
///
/// Equivalent to bmminer's `write_pic_iic(true, false, 0, chain, 0)`.
fn read_pic_iic_byte(fpga: &StockFpga, chain: u8) -> Result<u8> {
    let value = IIC_READ | IIC_ADDR_HIGH_S9 | iic_chain_number(chain);
    set_pic_iic(fpga, value)
}

/// Send PIC command preamble: 0x55, 0xAA.
///
/// Every PIC command starts with this 2-byte preamble. Each byte is a
/// separate register write+poll cycle.
fn send_pic_preamble(fpga: &StockFpga, chain: u8) -> Result<()> {
    write_pic_iic_byte(fpga, chain, PIC_COMMAND_1)?;
    write_pic_iic_byte(fpga, chain, PIC_COMMAND_2)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// StockFpgaI2c — high-level PIC interface
// ---------------------------------------------------------------------------

/// Stock FPGA I2C interface for PIC communication.
///
/// Provides high-level PIC operations (heartbeat, voltage, version) over the
/// stock Bitmain FPGA's IIC_COMMAND register. This replaces the Linux /dev/i2c-*
/// and AXI IIC approaches used by BraiinsOS.
///
/// **Important**: Methods take a `chain` number (5-8 for S9), NOT a 7-bit I2C
/// address. The FPGA routes to the correct physical I2C bus based on chain number.
/// The IIC_ADDR_HIGH_4_BIT (0x0A) is fixed for all S9 chains.
///
/// # Usage
///
/// ```ignore
/// let fpga = StockFpga::open()?;
/// let i2c = StockFpgaI2c::new(&fpga);
///
/// // Chain numbers 6, 7, 8 for a typical S9 with 3 hash boards
/// i2c.send_heartbeat(6)?;
/// let version = i2c.get_pic_version(7)?;
/// i2c.set_voltage(8, 57)?; // ~9.1V
/// i2c.enable_voltage(8)?;
/// ```
pub struct StockFpgaI2c<'a> {
    fpga: &'a StockFpga,
}

impl<'a> StockFpgaI2c<'a> {
    /// Create a new I2C interface backed by the stock FPGA.
    pub fn new(fpga: &'a StockFpga) -> Self {
        Self { fpga }
    }

    /// Send a PIC heartbeat (prevents voltage watchdog shutdown).
    ///
    /// PIC watchdog timeout: ~1 minute (stock PIC), ~10s (BraiinsOS PIC).
    /// Call this every 5-30 seconds to keep hash boards powered.
    ///
    /// Protocol: 0x55, 0xAA, 0x16 (three write cycles, no read-back).
    pub fn send_heartbeat(&self, chain: u8) -> Result<()> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::SEND_HEART_BEAT)?;

        tracing::trace!(chain, "PIC heartbeat sent on chain {}", chain);
        Ok(())
    }

    /// Set PIC voltage DAC value.
    ///
    /// Protocol: 0x55, 0xAA, 0x10, <dac_value> + 100ms settling.
    ///
    /// # Arguments
    /// * `chain` - Physical chain number (5-8)
    /// * `dac_value` - DAC value (0=9.44V, 6=9.4V init, 57=9.10V, 255=7.94V)
    pub fn set_voltage(&self, chain: u8, dac_value: u8) -> Result<()> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::SET_VOLTAGE)?;
        write_pic_iic_byte(self.fpga, chain, dac_value)?;

        // 100ms settling time (matching bmminer's usleep(100000))
        std::thread::sleep(std::time::Duration::from_millis(100));

        tracing::debug!(
            chain,
            dac_value,
            "PIC voltage set to DAC={} on chain {}",
            dac_value,
            chain,
        );
        Ok(())
    }

    /// Enable PIC voltage output.
    ///
    /// Protocol: 0x55, 0xAA, 0x15, 0x01
    pub fn enable_voltage(&self, chain: u8, enable: bool) -> Result<()> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::ENABLE_VOLTAGE)?;
        write_pic_iic_byte(self.fpga, chain, if enable { 0x01 } else { 0x00 })?;

        tracing::debug!(
            chain,
            enable,
            "PIC voltage {} on chain {}",
            if enable { "enabled" } else { "disabled" },
            chain,
        );
        Ok(())
    }

    /// Read PIC voltage DAC value.
    ///
    /// Protocol: 0x55, 0xAA, 0x18, then read one byte.
    pub fn get_voltage(&self, chain: u8) -> Result<u8> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::GET_VOLTAGE)?;
        let dac = read_pic_iic_byte(self.fpga, chain)?;

        tracing::debug!(chain, dac, "PIC voltage DAC={} on chain {}", dac, chain,);
        Ok(dac)
    }

    /// Read PIC firmware version.
    ///
    /// Protocol: 0x55, 0xAA, 0x17, then read one byte.
    /// Returns 0x03 for legacy Braiins-derived PIC firmware, 0x56/0x5A/0x5E for stock Bitmain PICs.
    pub fn get_pic_version(&self, chain: u8) -> Result<u8> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::GET_PIC_SOFTWARE_VERSION)?;
        let version = read_pic_iic_byte(self.fpga, chain)?;

        tracing::debug!(
            chain,
            version = format_args!("0x{:02X}", version),
            "PIC version 0x{:02X} on chain {} (0x56/5A/5E=stock, 0x03=legacy Braiins-derived)",
            version,
            chain,
        );
        Ok(version)
    }

    /// Perform a raw I2C read (single byte) to detect PIC state.
    ///
    /// Returns 0xCC if PIC is in bootloader, 0x60 if in app mode.
    /// No preamble — just a single read cycle.
    pub fn raw_read(&self, chain: u8) -> Result<u8> {
        read_pic_iic_byte(self.fpga, chain)
    }

    /// Send JUMP command to transition PIC from bootloader to app mode.
    ///
    /// Protocol: 0x55, 0xAA, 0x06
    /// Only needed if raw_read() returns 0xCC (bootloader).
    /// Do NOT send if PIC is already in app mode (0x60) — it will break back into bootloader.
    pub fn jump_to_app(&self, chain: u8) -> Result<()> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::JUMP_FROM_LOADER_TO_APP)?;

        // Wait for PIC to transition
        std::thread::sleep(std::time::Duration::from_millis(500));

        tracing::info!(chain, "PIC JUMP to app mode sent on chain {}", chain);
        Ok(())
    }

    /// Reset PIC (forces reboot into bootloader).
    ///
    /// Protocol: 0x55, 0xAA, 0x07
    /// Only works on BraiinsOS PICs. Stock PICs ignore unknown commands.
    pub fn reset_pic(&self, chain: u8) -> Result<()> {
        send_pic_preamble(self.fpga, chain)?;
        write_pic_iic_byte(self.fpga, chain, pic_cmd::RESET_PIC)?;

        // Wait for PIC to reboot
        std::thread::sleep(std::time::Duration::from_millis(1000));

        tracing::info!(chain, "PIC RESET sent on chain {}", chain);
        Ok(())
    }
}
