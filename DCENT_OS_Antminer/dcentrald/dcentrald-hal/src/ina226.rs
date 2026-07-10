//! TI INA226 high-side current/voltage power monitor driver.
//!
//! The INA226 measures bus voltage (0-36V) and shunt voltage (±81.92 mV)
//! via a precision shunt resistor, providing current and power readings.
//! Communication via standard I2C (no custom framing like Bitmain PSU).
//!
//! Used for real power measurement in Direct DC / Off-Grid mode.
//! Per hash board: 1 mΩ shunt + INA226 at 0x40/0x41/0x42 on the existing I2C bus.
//!
//! Register map (from TI INA226 datasheet, SBOS547A):
//!   0x00  Configuration    RW  16-bit  Averaging, conversion time, mode
//!   0x01  Shunt Voltage    R   16-bit  Shunt voltage measurement (2.5 µV LSB)
//!   0x02  Bus Voltage      R   16-bit  Bus voltage measurement (1.25 mV LSB)
//!   0x03  Power            R   16-bit  Power = current × bus_voltage (25 mW LSB default)
//!   0x04  Current          R   16-bit  Calibrated current (LSB set by calibration register)
//!   0x05  Calibration      RW  16-bit  Sets current LSB scaling
//!   0xFE  Manufacturer ID  R   16-bit  0x5449 ("TI")
//!   0xFF  Die ID           R   16-bit  0x2260

use crate::i2c::I2cBus;
use crate::Result;

// INA226 register addresses
const REG_CONFIGURATION: u8 = 0x00;
const REG_SHUNT_VOLTAGE: u8 = 0x01;
const REG_BUS_VOLTAGE: u8 = 0x02;
#[allow(dead_code)]
const REG_POWER: u8 = 0x03;
#[allow(dead_code)]
const REG_CURRENT: u8 = 0x04;
const REG_CALIBRATION: u8 = 0x05;
const REG_MANUFACTURER_ID: u8 = 0xFE;

/// Expected manufacturer ID for TI INA226.
const MANUFACTURER_ID_TI: u16 = 0x5449;

/// Default configuration: 16 averages, 1.1ms conversion, continuous shunt+bus.
const DEFAULT_CONFIG: u16 = 0x4527;

/// INA226 measurement reading.
#[derive(Debug, Clone, Default)]
pub struct Ina226Reading {
    /// Bus voltage in volts (0-36V range, 1.25 mV resolution).
    pub bus_voltage_v: f32,
    /// Shunt voltage in millivolts (±81.92 mV range, 2.5 µV resolution).
    pub shunt_voltage_mv: f32,
    /// Calculated current in amps (from shunt voltage / shunt resistance).
    pub current_a: f32,
    /// Calculated power in watts (bus_voltage × current).
    pub power_w: f32,
}

/// INA226 configuration.
#[derive(Debug, Clone)]
pub struct Ina226Config {
    /// I2C address (0x40-0x4F, set by A0/A1 pins).
    pub i2c_addr: u8,
    /// Shunt resistor value in milliohms (e.g., 1 for 1 mΩ, 10 for 10 mΩ).
    pub shunt_resistor_mohm: u16,
    /// Maximum expected current in amps (for calibration register).
    pub max_current_a: f32,
}

impl Default for Ina226Config {
    fn default() -> Self {
        Self {
            i2c_addr: 0x40,
            shunt_resistor_mohm: 10,
            max_current_a: 50.0,
        }
    }
}

/// INA226 power monitor instance.
pub struct Ina226 {
    config: Ina226Config,
    /// Shunt resistance in ohms (derived from config).
    shunt_ohms: f32,
    /// Current LSB in amps (set by calibration).
    current_lsb: f32,
}

impl Ina226 {
    /// Create a new INA226 instance with the given configuration.
    pub fn new(config: Ina226Config) -> Self {
        let shunt_ohms = config.shunt_resistor_mohm as f32 / 1000.0;
        // Current LSB = max_current / 2^15 (INA226 is 15-bit signed)
        let current_lsb = config.max_current_a / 32768.0;
        Self {
            config,
            shunt_ohms,
            current_lsb,
        }
    }

    /// Probe for INA226 at the configured address.
    /// Returns true if manufacturer ID matches TI (0x5449).
    pub fn probe(&self, i2c: &mut I2cBus) -> bool {
        match self.read_register(i2c, REG_MANUFACTURER_ID) {
            Ok(id) => {
                let is_ti = id == MANUFACTURER_ID_TI;
                if is_ti {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.config.i2c_addr),
                        "INA226 detected (TI manufacturer ID 0x5449)"
                    );
                }
                is_ti
            }
            Err(_) => false,
        }
    }

    /// Configure the INA226 (set averaging, conversion time, calibration).
    /// Call once after probe returns true.
    pub fn configure(&self, i2c: &mut I2cBus) -> Result<()> {
        // Set configuration: 16 averages, 1.1ms shunt+bus conversion, continuous mode
        self.write_register(i2c, REG_CONFIGURATION, DEFAULT_CONFIG)?;

        // Calculate and set calibration register
        // CAL = 0.00512 / (current_lsb × R_shunt)
        let cal = (0.00512 / (self.current_lsb * self.shunt_ohms)) as u16;
        self.write_register(i2c, REG_CALIBRATION, cal)?;

        tracing::info!(
            addr = format_args!("0x{:02X}", self.config.i2c_addr),
            shunt_mohm = self.config.shunt_resistor_mohm,
            cal,
            current_lsb_ua = format_args!("{:.1}", self.current_lsb * 1e6),
            "INA226 configured"
        );
        Ok(())
    }

    /// Read all measurements from INA226.
    pub fn read(&self, i2c: &mut I2cBus) -> Result<Ina226Reading> {
        let bus_raw = self.read_register(i2c, REG_BUS_VOLTAGE)?;
        let shunt_raw = self.read_register(i2c, REG_SHUNT_VOLTAGE)? as i16;

        // Bus voltage: 1.25 mV per LSB
        let bus_voltage_v = bus_raw as f32 * 1.25e-3;

        // Shunt voltage: 2.5 µV per LSB (signed)
        let shunt_voltage_mv = shunt_raw as f32 * 2.5e-3;

        // Current from shunt voltage and resistance
        let current_a = (shunt_voltage_mv / 1000.0) / self.shunt_ohms;

        // Power
        let power_w = bus_voltage_v * current_a;

        Ok(Ina226Reading {
            bus_voltage_v,
            shunt_voltage_mv,
            current_a: current_a.abs(),
            power_w: power_w.abs(),
        })
    }

    /// Read bus voltage only (faster, single register).
    pub fn read_bus_voltage(&self, i2c: &mut I2cBus) -> Result<f32> {
        let raw = self.read_register(i2c, REG_BUS_VOLTAGE)?;
        Ok(raw as f32 * 1.25e-3)
    }

    fn read_register(&self, i2c: &mut I2cBus, reg: u8) -> Result<u16> {
        i2c.set_slave(self.config.i2c_addr)?;
        let mut buf = [0u8; 2];
        i2c.write(&[reg])?;
        std::thread::sleep(std::time::Duration::from_micros(500));
        i2c.read(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    fn write_register(&self, i2c: &mut I2cBus, reg: u8, value: u16) -> Result<()> {
        i2c.set_slave(self.config.i2c_addr)?;
        let bytes = value.to_be_bytes();
        i2c.write(&[reg, bytes[0], bytes[1]])?;
        std::thread::sleep(std::time::Duration::from_micros(500));
        Ok(())
    }
}
