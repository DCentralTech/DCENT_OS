//! I2C bus driver for BitAxe peripheral communication.
//!
//! The BitAxe uses a single I2C bus (master mode, 400 kHz) connected to:
//! - TPS546 voltage regulator (PMBus, address 0x24)
//! - DS4432U DAC (older boards, address 0x48)
//! - INA260 power monitor (some boards, address 0x40)
//! - EMC2101 fan/temp controller (address 0x4C)
//!
//! This module wraps `esp_idf_hal::i2c::I2cDriver` with convenience methods
//! for common I2C operations including combined write-read transactions.

use crate::tps546_guard::GuardState;
use esp_idf_hal::gpio::{InputPin, OutputPin};
use esp_idf_hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_hal::units::Hertz;
use log::*;

/// Default I2C bus frequency (400 kHz — Fast Mode)
pub const DEFAULT_I2C_FREQ_HZ: u32 = 400_000;

/// Default I2C operation timeout in milliseconds
const I2C_TIMEOUT_MS: u32 = 100;

/// Errors from I2C operations
#[derive(Debug)]
pub enum I2cError {
    /// Failed to initialize the I2C peripheral
    InitFailed(String),
    /// Write operation failed (device NACK or bus error)
    WriteFailed { addr: u8, detail: String },
    /// Read operation failed
    ReadFailed { addr: u8, detail: String },
    /// Combined write-read operation failed
    WriteReadFailed { addr: u8, detail: String },
    /// No device acknowledged at the given address
    DeviceNotFound(u8),
}

impl core::fmt::Display for I2cError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InitFailed(msg) => write!(f, "I2C init failed: {}", msg),
            Self::WriteFailed { addr, detail } => {
                write!(f, "I2C write to 0x{:02x} failed: {}", addr, detail)
            }
            Self::ReadFailed { addr, detail } => {
                write!(f, "I2C read from 0x{:02x} failed: {}", addr, detail)
            }
            Self::WriteReadFailed { addr, detail } => {
                write!(f, "I2C write-read 0x{:02x} failed: {}", addr, detail)
            }
            Self::DeviceNotFound(addr) => write!(f, "No I2C device at 0x{:02x}", addr),
        }
    }
}

impl std::error::Error for I2cError {}

/// I2C bus driver wrapping the ESP-IDF I2C master.
///
/// Provides a shared bus interface for all I2C peripherals on the BitAxe.
/// Thread safety note: I2C operations are not internally synchronized —
/// callers must ensure exclusive access if shared across tasks.
pub struct I2cBus<'d> {
    driver: I2cDriver<'d>,
    freq_hz: u32,
    /// Count of consecutive I2C errors — triggers bus recovery at threshold.
    consecutive_errors: u32,
    /// XPSAFE-2: opt-in, latch-after-init write-protect for the TPS546
    /// fault-limit / fault-response registers (the last-line hardware
    /// protection). Disarmed by default, so a board that never arms it keeps
    /// its exact pre-XPSAFE-2 behavior. Cross-pollinated from DCENT_OS's HAL
    /// EEPROM write denylist. See `crate::tps546_guard`.
    tps546_guard: GuardState,
}

impl<'d> I2cBus<'d> {
    /// Initialize the I2C bus in master mode.
    ///
    /// # Arguments
    /// * `i2c` - I2C peripheral instance (typically I2C0)
    /// * `sda` - SDA pin (GPIO47 on most BitAxe boards)
    /// * `scl` - SCL pin (GPIO48 on most BitAxe boards)
    /// * `freq_hz` - Bus frequency in Hz (400000 for Fast Mode)
    pub fn new<I2C: esp_idf_hal::i2c::I2c + 'd>(
        i2c: I2C,
        sda: impl InputPin + OutputPin + 'd,
        scl: impl InputPin + OutputPin + 'd,
        freq_hz: u32,
    ) -> Result<Self, I2cError> {
        let config = I2cConfig::new().baudrate(Hertz(freq_hz));

        let driver = I2cDriver::new(i2c, sda, scl, &config)
            .map_err(|e| I2cError::InitFailed(format!("{:?}", e)))?;

        info!("I2C bus initialized at {} Hz", freq_hz);

        Ok(Self {
            driver,
            freq_hz,
            consecutive_errors: 0,
            tps546_guard: GuardState::default(),
        })
    }

    /// Initialize the I2C bus at the default 400 kHz frequency.
    pub fn new_default<I2C: esp_idf_hal::i2c::I2c + 'd>(
        i2c: I2C,
        sda: impl InputPin + OutputPin + 'd,
        scl: impl InputPin + OutputPin + 'd,
    ) -> Result<Self, I2cError> {
        Self::new(i2c, sda, scl, DEFAULT_I2C_FREQ_HZ)
    }

    /// Attempt I2C bus recovery after a run of consecutive errors.
    ///
    /// HALT-7: this driver is the esp-idf-hal **legacy** `I2cDriver` (port-based
    /// `i2c_driver`), not the new `i2c_master` bus handle, so the
    /// `i2c_master_bus_reset()` call the old comment referenced does NOT apply
    /// here (it takes an `i2c_master_bus_handle_t` the legacy driver never
    /// creates). What the legacy driver *can* do is reset the TX/RX FIFOs on the
    /// **driver's own port** (previously hard-coded to `0`), which clears a
    /// stuck transaction's queued bytes. A FIFO flush does NOT clock SCL, so it
    /// cannot by itself release a slave that is physically holding SDA low — for
    /// that, the bus must be re-created (a `STOP`/re-init driving SCL). We log
    /// the distinction (invoked vs. confirmed-cleared) so a genuinely wedged
    /// device shows up instead of looking recovered.
    ///
    /// Returns `true` if, after the FIFO reset, a lightweight probe confirms the
    /// bus is responsive again (cleared); `false` if recovery was invoked but the
    /// bus still looks wedged (caller/telemetry should treat the device as down).
    pub fn bus_recovery(&mut self) -> bool {
        let port = self.driver.port(); // derive from the driver, never hard-code
        warn!(
            "I2C: bus recovery INVOKED on port {} after {} consecutive errors \
             (FIFO flush; legacy driver cannot clock SCL to free an SDA-stuck slave)",
            port, self.consecutive_errors
        );
        // Reset the TX/RX FIFOs on the driver's actual port to clear any stuck
        // transaction state queued in the peripheral.
        unsafe {
            esp_idf_svc::sys::i2c_reset_tx_fifo(port);
            esp_idf_svc::sys::i2c_reset_rx_fifo(port);
        }
        self.consecutive_errors = 0;

        // Probe a benign, always-present anchor to see whether the bus actually
        // recovered. We probe the EMC2101 fan/temp controller address (0x4C);
        // an ACK means SDA/SCL are free again. A bare-address probe drives a
        // START + address + STOP, which also nudges SCL.
        let cleared = self.probe(0x4C);
        if cleared {
            info!(
                "I2C: bus recovery CLEARED on port {} (probe ACK after FIFO reset)",
                port
            );
        } else {
            warn!(
                "I2C: bus recovery on port {} did NOT clear — device likely still \
                 holding SDA low; a driver re-create (SCL toggle) is required to \
                 fully release it",
                port
            );
        }
        cleared
    }

    /// XPSAFE-2: opt into the TPS546 fault-limit write guard.
    ///
    /// Called by the platform power-init path BEFORE the legitimate
    /// `configure_limits` pass. Arming alone does NOT start blocking — the guard
    /// only enforces after `latch_tps546_fault_limit_guard()` is called at the
    /// end of init, so the init writes themselves still go through.
    ///
    /// This is the BitAxe analog of DCENT_OS `I2cBus::set_write_denylist`, but
    /// opt-in + latch-after-init because the protected registers MUST be writable
    /// during init (BitAxe has no separate EEPROM to deny outright). Default-OFF:
    /// a board that never calls this keeps its exact prior behavior.
    pub fn arm_tps546_fault_limit_guard(&mut self) {
        self.tps546_guard.arm();
        info!(
            "I2C: TPS546 fault-limit write guard ARMED (will enforce after init latch); \
             reads always allowed, only protected-register writes are gated (XPSAFE-2)"
        );
    }

    /// XPSAFE-2: begin enforcing the TPS546 fault-limit write guard.
    ///
    /// Call once at the END of power init, after `configure_limits` has written
    /// the protection thresholds. No-op if the guard was never armed. After this,
    /// any write targeting a TPS546 protection-limit / fault-response register is
    /// refused and counted; reads and normal `VOUT_COMMAND`/`OPERATION` writes are
    /// unaffected.
    pub fn latch_tps546_fault_limit_guard(&mut self) {
        self.tps546_guard.latch();
        if self.tps546_guard.enforcing() {
            info!("I2C: TPS546 fault-limit write guard LATCHED — protection registers are now read-only (XPSAFE-2)");
        }
    }

    /// Whether the TPS546 fault-limit guard is currently enforcing writes.
    pub fn tps546_guard_enforcing(&self) -> bool {
        self.tps546_guard.enforcing()
    }

    /// Number of protected TPS546 register writes refused since arm. Surfaced so
    /// a latent bug hammering a fault limit is visible instead of silent.
    pub fn tps546_guard_blocked_count(&self) -> u64 {
        self.tps546_guard.blocked_count
    }

    /// Refuse a guarded write: bump the counter, log loudly, and return the
    /// standard HAL write error. Mirrors DCENT_OS `I2cBus::refuse_write`.
    fn refuse_tps546_write(&mut self, addr: u8, register: u8) -> I2cError {
        self.tps546_guard.record_block();
        let n = self.tps546_guard.blocked_count;
        error!(
            "I2C write REFUSED to TPS546 0x{:02x} register 0x{:02x} — protection-limit \
             register is write-locked after power init (XPSAFE-2). Reads are still \
             allowed; only writes to the OV/OC/OT/UV fault thresholds + fault-response \
             policy bytes are blocked. blocked_count={}. If a new feature legitimately \
             needs to re-tighten a fault limit, route it through power init (re-arm/latch), \
             do NOT remove the guard.",
            addr, register, n
        );
        I2cError::WriteFailed {
            addr,
            detail: format!(
                "write to register 0x{:02x} refused (TPS546 fault-limit guard; reads still \
                 allowed). protection-limit register. blocked_count={}",
                register, n
            ),
        }
    }

    /// Write data to an I2C device.
    ///
    /// Sends the address byte (with write bit) followed by the data bytes.
    /// Returns an error if the device NACKs the address or any data byte.
    /// After 3 consecutive errors, performs bus recovery and retries once.
    ///
    /// XPSAFE-2: if the TPS546 fault-limit guard is enforcing and `data[0]`
    /// (the PMBus register code) targets a protected TPS546 register, the write
    /// is refused before touching the bus. This is inert by default (the guard
    /// is disarmed unless the platform opts in) and never affects reads.
    ///
    /// # Arguments
    /// * `addr` - 7-bit I2C device address
    /// * `data` - Bytes to write (typically register address + value)
    pub fn write(&mut self, addr: u8, data: &[u8]) -> Result<(), I2cError> {
        // XPSAFE-2 guard: refuse protected-register writes once enforcing. The
        // register code is the first payload byte (`[reg, value..]`); an empty
        // payload (bare-address probe) has no register and is never protected.
        if let Some(&register) = data.first() {
            if self.tps546_guard.is_write_blocked(addr, register) {
                return Err(self.refuse_tps546_write(addr, register));
            }
        }
        match self.driver.write(addr, data, I2C_TIMEOUT_MS) {
            Ok(()) => {
                self.consecutive_errors = 0;
                Ok(())
            }
            Err(e) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= 3 {
                    self.bus_recovery();
                    // Retry once after recovery
                    self.driver.write(addr, data, I2C_TIMEOUT_MS).map_err(|e2| {
                        I2cError::WriteFailed {
                            addr,
                            detail: format!("{:?}", e2),
                        }
                    })
                } else {
                    Err(I2cError::WriteFailed {
                        addr,
                        detail: format!("{:?}", e),
                    })
                }
            }
        }
    }

    /// Read data from an I2C device.
    ///
    /// Sends the address byte (with read bit) and clocks in `len` bytes.
    /// After 3 consecutive errors, performs bus recovery and retries once.
    ///
    /// # Arguments
    /// * `addr` - 7-bit I2C device address
    /// * `len` - Number of bytes to read
    pub fn read(&mut self, addr: u8, len: usize) -> Result<Vec<u8>, I2cError> {
        let mut buf = vec![0u8; len];
        match self.driver.read(addr, &mut buf, I2C_TIMEOUT_MS) {
            Ok(()) => {
                self.consecutive_errors = 0;
                Ok(buf)
            }
            Err(e) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= 3 {
                    self.bus_recovery();
                    // Retry once after recovery
                    let mut retry_buf = vec![0u8; len];
                    self.driver
                        .read(addr, &mut retry_buf, I2C_TIMEOUT_MS)
                        .map_err(|e2| I2cError::ReadFailed {
                            addr,
                            detail: format!("{:?}", e2),
                        })?;
                    Ok(retry_buf)
                } else {
                    Err(I2cError::ReadFailed {
                        addr,
                        detail: format!("{:?}", e),
                    })
                }
            }
        }
    }

    /// Combined write-then-read transaction (I2C repeated start).
    ///
    /// This is the most common I2C pattern: write a register address, then
    /// read back the register value without releasing the bus between operations.
    /// Every thermal / power / fan sensor on the board uses this path — a wedged
    /// device would otherwise hang the mining loop. Matches the retry semantics
    /// of `write()` / `read()` and ports ESP-Miner `i2c_transfer_with_retries()`
    /// (commit `e979fad`).
    ///
    /// # Arguments
    /// * `addr` - 7-bit I2C device address
    /// * `write_data` - Bytes to write (typically the register address)
    /// * `read_len` - Number of bytes to read back
    pub fn write_read(
        &mut self,
        addr: u8,
        write_data: &[u8],
        read_len: usize,
    ) -> Result<Vec<u8>, I2cError> {
        let mut buf = vec![0u8; read_len];
        match self
            .driver
            .write_read(addr, write_data, &mut buf, I2C_TIMEOUT_MS)
        {
            Ok(()) => {
                self.consecutive_errors = 0;
                Ok(buf)
            }
            Err(e) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= 3 {
                    self.bus_recovery();
                    // Retry once after recovery.
                    let mut retry_buf = vec![0u8; read_len];
                    self.driver
                        .write_read(addr, write_data, &mut retry_buf, I2C_TIMEOUT_MS)
                        .map_err(|e2| I2cError::WriteReadFailed {
                            addr,
                            detail: format!("{:?}", e2),
                        })?;
                    Ok(retry_buf)
                } else {
                    Err(I2cError::WriteReadFailed {
                        addr,
                        detail: format!("{:?}", e),
                    })
                }
            }
        }
    }

    /// Read a single byte from a register.
    ///
    /// Convenience method for the common pattern of writing a 1-byte register
    /// address and reading back 1 byte.
    pub fn read_reg_u8(&mut self, addr: u8, reg: u8) -> Result<u8, I2cError> {
        let data = self.write_read(addr, &[reg], 1)?;
        Ok(data[0])
    }

    /// Read a 16-bit big-endian value from a register.
    ///
    /// Used by PMBus devices (TPS546) and temperature sensors (EMC2101)
    /// which return 16-bit values in big-endian byte order.
    pub fn read_reg_u16_be(&mut self, addr: u8, reg: u8) -> Result<u16, I2cError> {
        let data = self.write_read(addr, &[reg], 2)?;
        Ok(u16::from_be_bytes([data[0], data[1]]))
    }

    /// Read a 16-bit little-endian value from a register.
    ///
    /// Some PMBus commands return data in little-endian format.
    pub fn read_reg_u16_le(&mut self, addr: u8, reg: u8) -> Result<u16, I2cError> {
        let data = self.write_read(addr, &[reg], 2)?;
        Ok(u16::from_le_bytes([data[0], data[1]]))
    }

    /// Write a single byte to a register.
    pub fn write_reg_u8(&mut self, addr: u8, reg: u8, value: u8) -> Result<(), I2cError> {
        self.write(addr, &[reg, value])
    }

    /// Write a 16-bit big-endian value to a register.
    pub fn write_reg_u16_be(&mut self, addr: u8, reg: u8, value: u16) -> Result<(), I2cError> {
        let bytes = value.to_be_bytes();
        self.write(addr, &[reg, bytes[0], bytes[1]])
    }

    /// Write a 16-bit little-endian value to a register.
    pub fn write_reg_u16_le(&mut self, addr: u8, reg: u8, value: u16) -> Result<(), I2cError> {
        let bytes = value.to_le_bytes();
        self.write(addr, &[reg, bytes[0], bytes[1]])
    }

    /// Probe an I2C address to check if a device is present.
    ///
    /// Sends just the address byte with write bit. If the device ACKs,
    /// it is present on the bus.
    pub fn probe(&mut self, addr: u8) -> bool {
        // Write zero bytes — just the address to see if device ACKs
        self.driver.write(addr, &[], I2C_TIMEOUT_MS).is_ok()
    }

    /// Scan the I2C bus and return all addresses that respond.
    ///
    /// Useful for hardware detection during startup — determines which
    /// power ICs and sensors are present on this board variant.
    pub fn scan(&mut self) -> Vec<u8> {
        let mut found = Vec::new();
        for addr in 0x08..=0x77 {
            if self.probe(addr) {
                info!("I2C device found at 0x{:02x}", addr);
                found.push(addr);
            }
        }
        if found.is_empty() {
            warn!("No I2C devices found on bus");
        } else {
            info!("I2C scan complete: {} device(s) found", found.len());
        }
        found
    }

    /// Get the configured bus frequency.
    pub fn freq_hz(&self) -> u32 {
        self.freq_hz
    }
}
