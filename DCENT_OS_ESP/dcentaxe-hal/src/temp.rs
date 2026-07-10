//! Temperature sensor drivers for BitAxe boards.
//!
//! BitAxe boards use an EMC2101 fan/temperature controller IC which provides:
//! - Internal die temperature sensor (board ambient)
//! - External diode temperature sensor (connected to ASIC thermal diode)
//! - Fan PWM output and tachometer input (handled by fan.rs instead)
//!
//! Some boards may not have an EMC2101 and rely on the TPS546 voltage
//! regulator's built-in temperature sensor instead.

use crate::i2c::{I2cBus, I2cError};
use crate::temp_decode::{decode_external_temp, EMC2101_STATUS_EXT_FAULT};
use log::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// EMC2101 registers and constants
// ---------------------------------------------------------------------------

/// EMC2101 default I2C address
pub const EMC2101_ADDR: u8 = 0x4C;

/// EMC2101 register: Internal temperature (read-only, 8-bit, degrees C)
const EMC2101_REG_INTERNAL_TEMP: u8 = 0x00;

/// EMC2101 register: External temperature high byte (read-only, integer part)
const EMC2101_REG_EXTERNAL_TEMP_MSB: u8 = 0x01;

/// EMC2101 register: External temperature low byte (read-only, fractional 3 bits)
const EMC2101_REG_EXTERNAL_TEMP_LSB: u8 = 0x10;

/// EMC2101 register: Status (read-only, fault flags)
const EMC2101_REG_STATUS: u8 = 0x02;

/// EMC2101 register: Configuration (R/W)
const EMC2101_REG_CONFIG: u8 = 0x03;

/// EMC2101 register: Conversion rate (R/W, 0-15, default=4 = 4 conv/sec)
const EMC2101_REG_CONVERSION_RATE: u8 = 0x04;

/// EMC2101 register: external diode ideality factor (R/W)
const EMC2101_REG_IDEALITY_FACTOR: u8 = 0x17;

/// EMC2101 register: external diode beta compensation (R/W)
const EMC2101_REG_BETA_COMPENSATION: u8 = 0x18;

/// EMC2101 register: Product ID (read-only, should be 0x16 or 0x28)
const EMC2101_REG_PRODUCT_ID: u8 = 0xFD;

/// EMC2101 register: Manufacturer ID (read-only, should be 0x5D for SMSC/Microchip)
const EMC2101_REG_MANUFACTURER_ID: u8 = 0xFE;

/// EMC2101 register: Fan configuration (R/W)
const EMC2101_FAN_CONFIG: u8 = 0x4A;

/// EMC2101 register: Fan PWM duty cycle setting (R/W, 0-63)
const EMC2101_REG_FAN_SETTING: u8 = 0x4C;

/// EMC2101 register: TACH LSB (read-only)
const EMC2101_REG_TACH_LSB: u8 = 0x46;

/// EMC2101 register: TACH MSB (read-only)
const EMC2101_REG_TACH_MSB: u8 = 0x47;

// EMC2101 status bit: External diode fault — `EMC2101_STATUS_EXT_FAULT` now
// lives in the pure `temp_decode` module (single source of truth) and is
// imported above; the bare-name references below resolve via that `use`.

// ---------------------------------------------------------------------------
// Temperature data types
// ---------------------------------------------------------------------------

/// Temperature readings from the board sensors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemperatureReading {
    /// Internal sensor temperature (board ambient, degrees C)
    pub board_temp_c: f32,
    /// External diode temperature (ASIC junction, degrees C)
    /// None if the external sensor is not connected or faulted
    pub chip_temp_c: Option<f32>,
}

/// Errors from temperature sensor operations
#[derive(Debug)]
pub enum TempError {
    /// I2C communication error
    I2cError(I2cError),
    /// Sensor not found on the I2C bus
    SensorNotFound,
    /// External diode sensor is faulted (open or short)
    ExternalDiodeFault,
    /// Temperature reading is out of valid range
    ReadingOutOfRange(f32),
}

impl core::fmt::Display for TempError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::I2cError(e) => write!(f, "Temp sensor I2C error: {}", e),
            Self::SensorNotFound => write!(f, "Temperature sensor not found"),
            Self::ExternalDiodeFault => write!(f, "External temperature diode fault"),
            Self::ReadingOutOfRange(t) => write!(f, "Temperature reading out of range: {:.1}C", t),
        }
    }
}

impl std::error::Error for TempError {}

impl From<I2cError> for TempError {
    fn from(e: I2cError) -> Self {
        Self::I2cError(e)
    }
}

// ---------------------------------------------------------------------------
// Temperature sensor trait
// ---------------------------------------------------------------------------

/// Trait for temperature sensor implementations.
///
/// Different BitAxe boards may use different temperature sensing methods.
/// This trait provides a unified interface.
pub trait TempSensor {
    /// Read the board/ambient temperature in degrees Celsius.
    fn get_board_temp(&mut self) -> Result<f32, TempError>;

    /// Read the ASIC chip temperature in degrees Celsius.
    ///
    /// Returns None if no external sensor is connected to the ASIC.
    fn get_chip_temp(&mut self) -> Result<Option<f32>, TempError>;

    /// Read all available temperatures at once.
    fn get_all(&mut self) -> Result<TemperatureReading, TempError> {
        Ok(TemperatureReading {
            board_temp_c: self.get_board_temp()?,
            chip_temp_c: self.get_chip_temp()?,
        })
    }
}

// ---------------------------------------------------------------------------
// EMC2101 driver
// ---------------------------------------------------------------------------

/// EMC2101 temperature sensor and fan controller IC.
///
/// The EMC2101 is a combined temperature sensor and fan controller commonly
/// used on BitAxe boards. It provides:
/// - Internal temperature sensor (board ambient, +/- 2C accuracy)
/// - External diode temperature input (ASIC thermal diode, +/- 1C accuracy)
/// - Programmable conversion rate (1-32 conversions/second)
///
/// Only the temperature sensing functions are implemented here; fan control
/// is handled by the dedicated [`FanController`] using the ESP32 LEDC PWM
/// directly (which gives us more precise control than routing through the EMC2101).
pub struct Emc2101 {
    /// I2C address (typically 0x4C)
    addr: u8,
    /// Whether the external diode sensor is connected and working
    external_diode_ok: bool,
}

impl Emc2101 {
    /// Initialize the EMC2101 temperature sensor.
    ///
    /// Probes the I2C bus for the device, verifies the manufacturer ID,
    /// and configures the conversion rate.
    pub fn new(i2c: &mut I2cBus, addr: u8) -> Result<Self, TempError> {
        // Check if device is present
        if !i2c.probe(addr) {
            return Err(TempError::SensorNotFound);
        }

        // Verify manufacturer ID
        let mfr_id = i2c.read_reg_u8(addr, EMC2101_REG_MANUFACTURER_ID)?;
        if mfr_id != 0x5D {
            warn!(
                "EMC2101 unexpected manufacturer ID: 0x{:02x} (expected 0x5D)",
                mfr_id
            );
        }

        let product_id = i2c.read_reg_u8(addr, EMC2101_REG_PRODUCT_ID)?;
        info!(
            "EMC2101 detected: manufacturer=0x{:02x}, product=0x{:02x}",
            mfr_id, product_id
        );

        // Set conversion rate to 8 conversions/second (register value 0x07)
        // This gives us a new reading every 125 ms
        i2c.write_reg_u8(addr, EMC2101_REG_CONVERSION_RATE, 0x07)?;

        // Check if external diode is connected
        let status = i2c.read_reg_u8(addr, EMC2101_REG_STATUS)?;
        let external_diode_ok = (status & EMC2101_STATUS_EXT_FAULT) == 0;

        if !external_diode_ok {
            warn!("EMC2101: External temperature diode fault detected");
        } else {
            info!("EMC2101: External temperature diode OK");
        }

        Ok(Self {
            addr,
            external_diode_ok,
        })
    }

    /// Initialize with default address (0x4C).
    pub fn new_default(i2c: &mut I2cBus) -> Result<Self, TempError> {
        Self::new(i2c, EMC2101_ADDR)
    }

    /// Apply the board-specific external diode ideality factor.
    pub fn set_ideality_factor(&self, i2c: &mut I2cBus, ideality: u8) -> Result<(), TempError> {
        i2c.write_reg_u8(self.addr, EMC2101_REG_IDEALITY_FACTOR, ideality)?;
        Ok(())
    }

    /// Apply the board-specific external diode beta compensation.
    pub fn set_beta_compensation(&self, i2c: &mut I2cBus, beta: u8) -> Result<(), TempError> {
        i2c.write_reg_u8(self.addr, EMC2101_REG_BETA_COMPENSATION, beta)?;
        Ok(())
    }

    /// Read the internal (board) temperature.
    ///
    /// The internal sensor measures the EMC2101's own die temperature,
    /// which approximates the PCB ambient temperature near the IC.
    /// Resolution: 1 degree C, range: 0 to 127 C.
    pub fn read_internal_temp(&self, i2c: &mut I2cBus) -> Result<f32, TempError> {
        let raw = i2c.read_reg_u8(self.addr, EMC2101_REG_INTERNAL_TEMP)?;
        // Internal temp is an unsigned 8-bit value in degrees C
        Ok(raw as f32)
    }

    /// Read the external diode temperature (ASIC junction temperature).
    ///
    /// The external sensor uses a thermal diode connected to the ASIC die.
    /// Resolution: 0.125 degrees C (11-bit), range: -64 to +191 C.
    ///
    /// Returns None if the external diode is faulted or disconnected.
    pub fn read_external_temp(&mut self, i2c: &mut I2cBus) -> Result<Option<f32>, TempError> {
        // Re-check fault status
        let status = i2c.read_reg_u8(self.addr, EMC2101_REG_STATUS)?;
        self.external_diode_ok = (status & EMC2101_STATUS_EXT_FAULT) == 0;

        if !self.external_diode_ok {
            return Ok(None);
        }

        // Read high byte (integer part, signed 8-bit)
        let msb = i2c.read_reg_u8(self.addr, EMC2101_REG_EXTERNAL_TEMP_MSB)?;
        // Read low byte (fractional part in upper 3 bits: bits[7:5])
        let lsb = i2c.read_reg_u8(self.addr, EMC2101_REG_EXTERNAL_TEMP_LSB)?;

        // HALT-3: a non-fault reading at or above 127C is REAL danger and is
        // returned as Some(temp) so it reaches max_temp and the 105C emergency
        // path; only the negative open-circuit sentinel (< -10C, e.g. MSB=0x80
        // decoding to -128C) is dropped as unavailable. Availability is gated
        // solely on the STATUS external-fault bit, already checked above; the
        // pure fn re-checks it harmlessly (this branch is only reached when not
        // faulted) and is the surface the host tests exercise.
        Ok(decode_external_temp(status, msb, lsb))
    }

    /// Check if the external diode sensor is functioning.
    pub fn external_diode_ok(&self) -> bool {
        self.external_diode_ok
    }

    /// Initialize EMC2101 fan control.
    ///
    /// Configures the EMC2101 as a direct-PWM fan controller:
    /// - TACH input enabled for RPM reading
    /// - Fan driver enabled in direct-setting mode
    /// - PWM frequency ~22.5 kHz
    pub fn init_fan(&self, i2c: &mut I2cBus) -> Result<(), TempError> {
        // Set TACH input (register 0x03, bit 2)
        i2c.write_reg_u8(self.addr, EMC2101_REG_CONFIG, 0x04)?;

        // Fan config register 0x4A:
        //   Bit 6: EN=0 (CLK select, don't invert)
        //   Bit 5: Direct Setting Mode
        //   Bit 1:0: PWM frequency range (0b11 = ~22.5 kHz)
        // Value 0b00100011 = 0x23 — matches ESP-Miner
        i2c.write_reg_u8(self.addr, EMC2101_FAN_CONFIG, 0b00100011)?;

        info!("EMC2101: Fan controller initialized (direct PWM mode)");
        Ok(())
    }

    /// Set fan speed via EMC2101 (0-100%).
    ///
    /// The EMC2101 FAN_SETTING register (0x4C) accepts values 0-63,
    /// where 0 = off and 63 = 100%.
    ///
    /// HALPWR-6: a HAL-level non-zero floor (`safety::FAN_FLOOR_PCT`, 20%) is
    /// enforced — identical to `Emc2302::set_fan_speed` — so any caller
    /// (autotuner, MCP `set_fan_speed`, space-heater logic) commanding below the
    /// floor, including a true-zero, does NOT stop the fan on a powered mining
    /// board (the Gamma / BM1370 public target uses this EMC2101 path). A genuine
    /// full-stop is only honored under the explicit
    /// `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` lab bypass. The floor only ever
    /// RAISES a too-low command; it never reduces one. The emergency full-cooling
    /// path is separate (`safety::emc2101_panic_duty`) and is NOT floor-clamped.
    pub fn set_fan_speed(&self, i2c: &mut I2cBus, percent: u8) -> Result<(), TempError> {
        let floored = crate::safety::fan_duty_with_floor(
            percent,
            crate::safety::FAN_FLOOR_PCT,
            crate::safety::lab_safety_bypass_enabled(),
        );
        let duty = crate::safety::emc2101_duty_for_pct(floored);
        i2c.write_reg_u8(self.addr, EMC2101_REG_FAN_SETTING, duty)?;
        Ok(())
    }

    /// Read fan RPM from the EMC2101 TACH registers.
    ///
    /// The EMC2101 measures fan speed via the TACH pin. The TACH count
    /// is an inverse frequency measurement: RPM = 5400000 / tach_count.
    /// A tach_count of 0xFFFF means no fan detected.
    pub fn read_fan_rpm(&self, i2c: &mut I2cBus) -> Result<u32, TempError> {
        let lsb = i2c.read_reg_u8(self.addr, EMC2101_REG_TACH_LSB)? as u16;
        let msb = i2c.read_reg_u8(self.addr, EMC2101_REG_TACH_MSB)? as u16;
        let tach_count = (msb << 8) | lsb;
        if tach_count == 0 || tach_count == 0xFFFF {
            return Ok(0);
        }
        Ok(5_400_000 / tach_count as u32)
    }
}

// HALT-4: the previous `impl TempSensor for Emc2101` was a dormant trap — both
// methods hard-coded `Err(TempError::SensorNotFound)` (the device needs the
// shared `&mut I2cBus`, which the trait signature can't pass), so any future
// generic thermal-supervisor reaching for the trait would wrongly conclude the
// sensor is absent and either block mining or feed a false "no thermal data"
// to the THERMAL-BLIND path. It was never called (main.rs uses the concrete
// `read_internal_temp()` / `read_external_temp()` directly), so removing the
// impl is behavior-preserving today and deletes the trap. The `TempSensor`
// trait stays defined for sensors whose read path needs no external bus
// handle; an EMC2101 impl that always reports the device missing must not ship.

// ---------------------------------------------------------------------------
// TMP1075 driver (used on Hex boards for inlet/outlet temperature)
// ---------------------------------------------------------------------------

/// TMP1075 default I2C addresses on Hex boards.
///
/// Hex boards use two TMP1075 sensors at 0x4A and 0x4B
/// (matching ESP-Miner board 302/303/701/702 configuration).
/// Sensor 0 (0x4A): primary chip temperature
/// Sensor 1 (0x4B): secondary chip temperature
pub const TMP1075_ADDR_PRIMARY: u8 = 0x4A;
pub const TMP1075_ADDR_SECONDARY: u8 = 0x4B;

/// TMP1075 register: Temperature (read-only, 16-bit, 2's complement)
const TMP1075_REG_TEMP: u8 = 0x00;
/// TMP1075 register: Configuration (R/W)
const TMP1075_REG_CONFIG: u8 = 0x01;
/// TMP1075 register: Device ID (read-only, should be 0x0075)
const TMP1075_REG_DEVICE_ID: u8 = 0x0F;

/// TMP1075 digital temperature sensor (TI).
///
/// Used on BitAxe Hex boards for inlet/outlet temperature monitoring.
/// 12-bit resolution (0.0625°C), ±1°C accuracy from -25°C to +100°C.
/// Two sensors per Hex board: one for air inlet, one for air outlet.
pub struct Tmp1075 {
    /// I2C address
    addr: u8,
    /// Sensor location label (for logging)
    label: &'static str,
}

impl Tmp1075 {
    /// Initialize a TMP1075 temperature sensor.
    ///
    /// Probes the I2C bus and optionally verifies the device ID.
    pub fn new(i2c: &mut I2cBus, addr: u8, label: &'static str) -> Result<Self, TempError> {
        if !i2c.probe(addr) {
            return Err(TempError::SensorNotFound);
        }

        // Try to read device ID (optional verification)
        if let Ok(id) = i2c.read_reg_u16_be(addr, TMP1075_REG_DEVICE_ID) {
            info!(
                "TMP1075 {} at 0x{:02X}: device_id=0x{:04X}",
                label, addr, id
            );
        } else {
            info!(
                "TMP1075 {} at 0x{:02X}: detected (no device ID)",
                label, addr
            );
        }

        // Set to continuous conversion mode, 12-bit resolution (default config)
        // Config register: continuous mode, 12-bit, 27.5ms conversion
        let _ = i2c.write_reg_u16_be(addr, TMP1075_REG_CONFIG, 0x0000);

        Ok(Self { addr, label })
    }

    /// Initialize primary temperature sensor (0x4A).
    pub fn new_primary(i2c: &mut I2cBus) -> Result<Self, TempError> {
        Self::new(i2c, TMP1075_ADDR_PRIMARY, "chip1")
    }

    /// Initialize secondary temperature sensor (0x4B).
    pub fn new_secondary(i2c: &mut I2cBus) -> Result<Self, TempError> {
        Self::new(i2c, TMP1075_ADDR_SECONDARY, "chip2")
    }

    /// Read temperature in degrees Celsius.
    ///
    /// TMP1075 temperature register is 12-bit, 2's complement, left-justified
    /// in a 16-bit word. Resolution: 0.0625°C per LSB.
    pub fn read_temp(&self, i2c: &mut I2cBus) -> Result<f32, TempError> {
        let raw = i2c.read_reg_u16_be(self.addr, TMP1075_REG_TEMP)?;

        // Temperature is in the upper 12 bits, 2's complement
        // Shift right by 4, then multiply by 0.0625
        let raw_signed = (raw as i16) >> 4;
        let temp = raw_signed as f32 * 0.0625;

        if temp < -40.0 || temp > 200.0 {
            warn!("TMP1075 {} temp out of range: {:.1}C", self.label, temp);
            return Err(TempError::ReadingOutOfRange(temp));
        }

        Ok(temp)
    }

    /// Get the sensor label.
    pub fn label(&self) -> &'static str {
        self.label
    }
}

// ---------------------------------------------------------------------------
// TPS546 temperature (via power module)
// ---------------------------------------------------------------------------

/// Read temperature from the TPS546 voltage regulator.
///
/// The TPS546 has a built-in temperature sensor accessible via PMBus.
/// This is not the ASIC temperature — it measures the regulator die,
/// which is near the ASIC but typically 10-20C cooler.
///
/// This function is provided as a fallback for boards without an EMC2101.
/// The actual TPS546 temperature reading is implemented in [`crate::power::Tps546`].
pub fn tps546_get_temperature(i2c: &mut I2cBus, addr: u8) -> Result<f32, TempError> {
    // PMBus READ_TEMPERATURE_1 command (0x8D)
    const PMBUS_READ_TEMPERATURE_1: u8 = 0x8D;

    let data = i2c.write_read(addr, &[PMBUS_READ_TEMPERATURE_1], 2)?;
    let raw = u16::from_le_bytes([data[0], data[1]]);

    // PMBus Linear11 format: 5-bit exponent + 11-bit mantissa
    let temp = crate::power::pmbus_linear11_to_f32(raw);

    Ok(temp)
}
