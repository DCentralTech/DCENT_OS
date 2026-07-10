//! ADC abstraction for DC bus voltage and current monitoring.
//!
//! Provides a unified interface for reading DC bus voltage regardless of
//! the underlying hardware: INA226 I2C power monitor, sysfs IIO ADC,
//! or simulated values for testing.
//!
//! Used by the off-grid controller to monitor battery voltage and
//! trigger frequency curtailment when voltage drops.

use serde::{Deserialize, Serialize};

use crate::i2c::I2cBus;
use crate::ina226::{Ina226, Ina226Config};
use crate::Result;

/// ADC reading from the DC bus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdcReading {
    /// DC bus voltage in volts (e.g., 48.2V for a 48V battery bank).
    pub voltage_v: f32,
    /// DC bus current in amps (positive = load, 0 if not measured).
    pub current_a: f32,
    /// Computed power in watts (voltage × current, or 0 if current not measured).
    pub power_w: f32,
}

/// ADC backend configuration (from TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdcBackendConfig {
    /// TI INA226 I2C power monitor (recommended for off-grid).
    Ina226 {
        #[serde(default = "default_i2c_bus")]
        i2c_bus: u8,
        #[serde(default = "default_ina226_addr")]
        i2c_addr: u8,
        #[serde(default = "default_shunt_mohm")]
        shunt_mohm: u16,
        /// External voltage divider ratio (1.0 = direct, 4.0 = 4:1 divider for >36V).
        #[serde(default = "default_divider")]
        voltage_divider: f32,
    },
    /// Linux IIO sysfs ADC (Amlogic SARADC, Zynq XADC external channels).
    Sysfs {
        /// Path to voltage raw value (e.g., "/sys/bus/iio/devices/iio:device0/in_voltage0_raw").
        voltage_path: String,
        /// ADC reference voltage in volts.
        #[serde(default = "default_vref")]
        vref: f32,
        /// ADC resolution in bits.
        #[serde(default = "default_adc_bits")]
        bits: u8,
        /// External voltage divider ratio.
        #[serde(default = "default_divider")]
        voltage_divider: f32,
    },
    /// Simulated ADC for testing without hardware.
    Simulated {
        voltage_v: f32,
        #[serde(default)]
        current_a: f32,
    },
}

fn default_i2c_bus() -> u8 {
    0
}
fn default_ina226_addr() -> u8 {
    0x40
}
fn default_shunt_mohm() -> u16 {
    10
}
fn default_divider() -> f32 {
    1.0
}
fn default_vref() -> f32 {
    1.8
}
fn default_adc_bits() -> u8 {
    12
}

impl Default for AdcBackendConfig {
    fn default() -> Self {
        AdcBackendConfig::Simulated {
            voltage_v: 52.0,
            current_a: 0.0,
        }
    }
}

/// Trait for reading DC bus voltage/current.
pub trait VoltageSource: Send + Sync {
    /// Read the current DC bus voltage and optionally current/power.
    fn read(&mut self) -> Result<AdcReading>;

    /// Human-readable backend name for dashboard display.
    fn source_name(&self) -> &str;

    /// Whether this source provides real current measurement.
    fn has_current(&self) -> bool {
        false
    }
}

/// INA226-based voltage/current source.
pub struct Ina226Source {
    ina: Ina226,
    i2c: I2cBus,
    voltage_divider: f32,
    configured: bool,
}

impl Ina226Source {
    pub fn open(i2c_bus: u8, addr: u8, shunt_mohm: u16, voltage_divider: f32) -> Result<Self> {
        let i2c = I2cBus::open(i2c_bus)?;
        let config = Ina226Config {
            i2c_addr: addr,
            shunt_resistor_mohm: shunt_mohm,
            max_current_a: 50.0,
        };
        let ina = Ina226::new(config);

        let mut source = Self {
            ina,
            i2c,
            voltage_divider,
            configured: false,
        };

        // Probe and configure
        if source.ina.probe(&mut source.i2c) {
            source.ina.configure(&mut source.i2c)?;
            source.configured = true;
        } else {
            tracing::warn!(
                addr = format_args!("0x{:02X}", addr),
                bus = i2c_bus,
                "INA226 not found — voltage readings will be unavailable"
            );
        }

        Ok(source)
    }
}

impl VoltageSource for Ina226Source {
    fn read(&mut self) -> Result<AdcReading> {
        if !self.configured {
            return Err(crate::HalError::I2c {
                bus: 0,
                addr: 0x40,
                detail: "INA226 not configured — sensor not detected on I2C bus".into(),
            });
        }
        let reading = self.ina.read(&mut self.i2c)?;
        Ok(AdcReading {
            voltage_v: reading.bus_voltage_v * self.voltage_divider,
            current_a: reading.current_a,
            power_w: reading.power_w * self.voltage_divider,
        })
    }

    fn source_name(&self) -> &str {
        "INA226"
    }
    fn has_current(&self) -> bool {
        self.configured
    }
}

/// Sysfs IIO ADC voltage source (voltage only, no current).
pub struct SysfsSource {
    voltage_path: String,
    scale: f32, // (vref / 2^bits) * voltage_divider
}

impl SysfsSource {
    pub fn new(voltage_path: String, vref: f32, bits: u8, voltage_divider: f32) -> Self {
        // Clamp bits to 0-24 to prevent overflow in 1 << bits (u32 max shift is 31)
        let safe_bits = bits.min(24);
        let scale = (vref / (1u32 << safe_bits) as f32) * voltage_divider;
        Self {
            voltage_path,
            scale,
        }
    }
}

impl VoltageSource for SysfsSource {
    fn read(&mut self) -> Result<AdcReading> {
        let raw_str = std::fs::read_to_string(&self.voltage_path).map_err(|e| {
            crate::HalError::DeviceOpen {
                path: self.voltage_path.clone(),
                source: e,
            }
        })?;
        let raw: u32 = raw_str.trim().parse().unwrap_or(0);
        Ok(AdcReading {
            voltage_v: raw as f32 * self.scale,
            current_a: 0.0,
            power_w: 0.0,
        })
    }

    fn source_name(&self) -> &str {
        "Sysfs ADC"
    }
}

/// Simulated voltage source for testing.
pub struct SimulatedSource {
    voltage_v: f32,
    current_a: f32,
}

impl SimulatedSource {
    pub fn new(voltage_v: f32, current_a: f32) -> Self {
        Self {
            voltage_v,
            current_a,
        }
    }
}

impl VoltageSource for SimulatedSource {
    fn read(&mut self) -> Result<AdcReading> {
        Ok(AdcReading {
            voltage_v: self.voltage_v,
            current_a: self.current_a,
            power_w: self.voltage_v * self.current_a,
        })
    }

    fn source_name(&self) -> &str {
        "Simulated"
    }
    fn has_current(&self) -> bool {
        self.current_a > 0.0
    }
}

/// Create a VoltageSource from configuration.
pub fn create_voltage_source(config: &AdcBackendConfig) -> Result<Box<dyn VoltageSource>> {
    match config {
        AdcBackendConfig::Ina226 {
            i2c_bus,
            i2c_addr,
            shunt_mohm,
            voltage_divider,
        } => Ok(Box::new(Ina226Source::open(
            *i2c_bus,
            *i2c_addr,
            *shunt_mohm,
            *voltage_divider,
        )?)),
        AdcBackendConfig::Sysfs {
            voltage_path,
            vref,
            bits,
            voltage_divider,
        } => Ok(Box::new(SysfsSource::new(
            voltage_path.clone(),
            *vref,
            *bits,
            *voltage_divider,
        ))),
        AdcBackendConfig::Simulated {
            voltage_v,
            current_a,
        } => Ok(Box::new(SimulatedSource::new(*voltage_v, *current_a))),
    }
}
