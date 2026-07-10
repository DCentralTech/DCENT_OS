//! EMC2302 dual fan controller driver.
//!
//! The EMC2302 is a 2-channel PWM fan controller with tachometer feedback,
//! used on BitAxe Hex boards (302, 303, 701, 702) for dual-fan cooling.
//!
//! Key features:
//! - 2 independent PWM fan channels with separate tachometer inputs
//! - I2C interface at default address 0x2F
//! - Configurable fan speed ranges (500/1000/2000/4000 RPM)
//! - No built-in temperature sensor (paired with TMP1075 on Hex boards)
//!
//! Ported from ESP-Miner EMC2302.c
//!
//! IMPORTANT: EMC2302 TACH register naming is counterintuitive.
//! Register 0x3E = TACH Reading HIGH Byte (FxTR[12:5])
//! Register 0x3F = TACH Reading LOW Byte  (FxTR[4:0] in bits [7:3])
//! The HIGH byte must be read FIRST to latch the LOW byte for atomic readout.
//! ESP-Miner header incorrectly labels 0x3E as "LSB" - we use the correct
//! datasheet names (HIGH/LOW) to avoid confusion.

use crate::i2c::{I2cBus, I2cError};
use log::*;

/// EMC2302 default I2C address
pub const EMC2302_ADDR: u8 = 0x2F;

const REG_CONFIG: u8 = 0x20;
const REG_STATUS: u8 = 0x24;
const REG_STALL_STATUS: u8 = 0x25;
const REG_SPIN_STATUS: u8 = 0x26;
const REG_DRIVE_FAIL_STATUS: u8 = 0x27;
const REG_PWM_POLARITY: u8 = 0x2A;
const REG_PWM_OUTPUT: u8 = 0x2B;

// FAN_CONFIG1 register bit layout:
// Bit 7:    EN_ALGO   - Enable RPM-based closed-loop control (0=direct PWM)
// Bits 6:5: RNG       - Range (00=500, 01=1000, 10=2000, 11=4000 RPM)
// Bits 4:3: EDG       - TACH edges per revolution
// Bits 2:0: UD        - Update time

/// Mask to clear range bits [6:5] in FAN_CONFIG1.
const CONFIG1_RANGE_MASK: u8 = 0x9F; // 1001_1111
/// Bit shift for range field in FAN_CONFIG1.
const CONFIG1_RANGE_SHIFT: u8 = 5;

// Fan 1 registers
const REG_FAN1_SETTING: u8 = 0x30;
const REG_FAN1_CONFIG1: u8 = 0x32;
const REG_FAN1_CONFIG2: u8 = 0x33;
const REG_FAN1_SPINUP: u8 = 0x36;
/// Fan 1 TACH Reading HIGH Byte - contains FxTR[12:5] (8 bits).
/// Must be read FIRST to latch the LOW byte.
const REG_TACH1_HIGH: u8 = 0x3E;
/// Fan 1 TACH Reading LOW Byte - contains FxTR[4:0] in bits [7:3].
const REG_TACH1_LOW: u8 = 0x3F;

// Fan 2 registers
const REG_FAN2_SETTING: u8 = 0x40;
const REG_FAN2_CONFIG1: u8 = 0x42;
const REG_FAN2_CONFIG2: u8 = 0x43;
const REG_FAN2_SPINUP: u8 = 0x46;
/// Fan 2 TACH Reading HIGH Byte - contains FxTR[12:5] (8 bits).
const REG_TACH2_HIGH: u8 = 0x4E;
/// Fan 2 TACH Reading LOW Byte - contains FxTR[4:0] in bits [7:3].
const REG_TACH2_LOW: u8 = 0x4F;

// Identification registers
const REG_WHOAMI: u8 = 0xFD;
const REG_MANUFACTURER_ID: u8 = 0xFE;
const REG_REVISION: u8 = 0xFF;

/// Fan speed measurement range.
/// Determines the TACH multiplier for RPM calculation.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum FanRange {
    Rpm500 = 0,
    Rpm1000 = 1,
    Rpm2000 = 2,
    Rpm4000 = 3,
}

/// TACH multipliers corresponding to each fan range setting
const TACH_MULTIPLIERS: [u8; 4] = [1, 2, 4, 8];

/// Errors from EMC2302 operations
#[derive(Debug)]
pub enum Emc2302Error {
    /// I2C communication error
    I2c(I2cError),
    /// Device not found on bus
    NotFound,
}
impl core::fmt::Display for Emc2302Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::I2c(e) => write!(f, "EMC2302 I2C error: {}", e),
            Self::NotFound => write!(f, "EMC2302 not found at 0x{:02X}", EMC2302_ADDR),
        }
    }
}

impl std::error::Error for Emc2302Error {}

impl From<I2cError> for Emc2302Error {
    fn from(e: I2cError) -> Self {
        Self::I2c(e)
    }
}

/// EMC2302 dual-channel fan controller.
///
/// Controls two independent fans via I2C PWM with tachometer feedback.
/// Both fans are always set to the same speed (matching ESP-Miner behavior),
/// but individual RPM readings are available per channel.
pub struct Emc2302 {
    addr: u8,
    fan1_multiplier: u8,
    fan2_multiplier: u8,
}

impl Emc2302 {
    /// Initialize the EMC2302 fan controller.
    ///
    /// Probes the I2C bus, reads identification registers, and configures
    /// both fan channels for 500 RPM minimum range (matching ESP-Miner).
    pub fn new(i2c: &mut I2cBus, addr: u8) -> Result<Self, Emc2302Error> {
        if !i2c.probe(addr) {
            return Err(Emc2302Error::NotFound);
        }

        // Read identification
        if let Ok(whoami) = i2c.read_reg_u8(addr, REG_WHOAMI) {
            if let Ok(mfr) = i2c.read_reg_u8(addr, REG_MANUFACTURER_ID) {
                info!(
                    "EMC2302 at 0x{:02X}: WHOAMI=0x{:02X}, MFR=0x{:02X}",
                    addr, whoami, mfr
                );
            }
        }

        let mut emc = Self {
            addr,
            fan1_multiplier: 1,
            fan2_multiplier: 1,
        };

        // Set both fans to 500 RPM minimum range (matches ESP-Miner init)
        emc.set_fan_range(i2c, REG_FAN1_CONFIG1, FanRange::Rpm500, true)?;
        emc.set_fan_range(i2c, REG_FAN2_CONFIG1, FanRange::Rpm500, false)?;

        info!("EMC2302 initialized: dual fan, 500 RPM range");
        Ok(emc)
    }

    /// Initialize with default address (0x2F).
    pub fn new_default(i2c: &mut I2cBus) -> Result<Self, Emc2302Error> {
        Self::new(i2c, EMC2302_ADDR)
    }

    /// Set the fan range for a specific fan channel.
    ///
    /// Reads the current FAN_CONFIG1 register, modifies the range bits [6:5],
    /// writes back, and updates the tach multiplier.
    ///
    /// FAN_CONFIG1 layout (EMC2302 datasheet Table 6-4):
    ///   Bit 7:    EN_ALGO (closed-loop enable)
    ///   Bits 6:5: RNG (range: 00=500, 01=1000, 10=2000, 11=4000 RPM)
    ///   Bits 4:3: EDG (edges per revolution)
    ///   Bits 2:0: UD (update time)
    fn set_fan_range(
        &mut self,
        i2c: &mut I2cBus,
        config_reg: u8,
        range: FanRange,
        is_fan1: bool,
    ) -> Result<(), Emc2302Error> {
        let mut config = i2c.read_reg_u8(self.addr, config_reg)?;
        // Range is in bits [6:5] of FAN_CONFIG1 (EMC2302 datasheet)
        config = (config & CONFIG1_RANGE_MASK) | ((range as u8 & 0x03) << CONFIG1_RANGE_SHIFT);
        i2c.write_reg_u8(self.addr, config_reg, config)?;

        let multiplier = TACH_MULTIPLIERS[range as usize];
        if is_fan1 {
            self.fan1_multiplier = multiplier;
        } else {
            self.fan2_multiplier = multiplier;
        }
        Ok(())
    }

    /// Whether the operator has set the explicit unsafe lab bypass that allows a
    /// true-zero (full-stop) fan command on a powered board.
    ///
    /// XPSAFE-4: COMPILE-TIME ONLY — delegates to the single-source-of-truth
    /// `safety::lab_safety_bypass_enabled`, which mirrors
    /// `main.rs::unsafe_lab_safety_bypass_enabled`. The previous runtime
    /// `std::env::var` arm was dead on the ESP32 (no process environment) and
    /// could make the HAL fan floor and the supervisor DISAGREE about whether the
    /// bypass is active; it is removed so every safety layer reads the SAME gate.
    fn lab_safety_bypass() -> bool {
        crate::safety::lab_safety_bypass_enabled()
    }

    /// Set fan speed for both channels (0-100%).
    ///
    /// Both fans are always set to the same speed, matching ESP-Miner behavior.
    /// The PWM register accepts 0-255 (0x00 = off, 0xFF = full speed).
    ///
    /// HALPWR-6: a HAL-level non-zero floor (`safety::FAN_FLOOR_PCT`, 20%) is
    /// enforced so any caller (autotuner, MCP `set_fan_speed`, space-heater
    /// logic) commanding below the floor — including a true-zero — does NOT stop
    /// the fans on a powered mining board. A genuine full-stop is only honored
    /// under the explicit `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` lab bypass. The
    /// floor only ever RAISES a too-low command; it never reduces one.
    pub fn set_fan_speed(&self, i2c: &mut I2cBus, percent: u8) -> Result<(), Emc2302Error> {
        let percent = percent.min(100);
        let floored = crate::safety::fan_duty_with_floor(
            percent,
            crate::safety::FAN_FLOOR_PCT,
            Self::lab_safety_bypass(),
        );
        let setting = crate::safety::pwm_byte_for_pct(floored);
        i2c.write_reg_u8(self.addr, REG_FAN1_SETTING, setting)?;
        i2c.write_reg_u8(self.addr, REG_FAN2_SETTING, setting)?;
        Ok(())
    }

    /// Set fan speed as a float (0.0 - 1.0), matching ESP-Miner API.
    ///
    /// HALPWR-6: same HAL non-zero fan floor as [`set_fan_speed`], applied in
    /// float space so the original `(255.0 * clamped) as u8` byte math is
    /// preserved EXACTLY for any request at or above the floor. A request below
    /// `safety::FAN_FLOOR_PCT/100` (including 0.0) is raised to the floor fraction
    /// unless the explicit `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` lab bypass is set.
    /// The floor only ever raises a too-low command; it never reduces one.
    pub fn set_fan_speed_float(&self, i2c: &mut I2cBus, percent: f32) -> Result<(), Emc2302Error> {
        let clamped = percent.clamp(0.0, 1.0);
        let floor_frac = crate::safety::FAN_FLOOR_PCT as f32 / 100.0;
        let floored = if Self::lab_safety_bypass() || clamped >= floor_frac {
            clamped
        } else {
            floor_frac
        };
        let setting = (255.0 * floored) as u8;
        i2c.write_reg_u8(self.addr, REG_FAN1_SETTING, setting)?;
        i2c.write_reg_u8(self.addr, REG_FAN2_SETTING, setting)?;
        Ok(())
    }

    /// Read fan 1 RPM from tachometer.
    ///
    /// TACH count formula: RPM = 3,932,160 * multiplier / tach_count
    /// Returns 0 if no fan detected (tach_count maxed or 0).
    pub fn get_fan1_rpm(&self, i2c: &mut I2cBus) -> Result<u32, Emc2302Error> {
        self.read_fan_rpm(i2c, REG_TACH1_HIGH, REG_TACH1_LOW, self.fan1_multiplier)
    }

    /// Read fan 2 RPM from tachometer.
    pub fn get_fan2_rpm(&self, i2c: &mut I2cBus) -> Result<u32, Emc2302Error> {
        self.read_fan_rpm(i2c, REG_TACH2_HIGH, REG_TACH2_LOW, self.fan2_multiplier)
    }
    /// Internal: read RPM from a TACH register pair.
    ///
    /// EMC2302 TACH register layout (Microchip DS20005296):
    ///   HIGH byte (lower address, e.g. 0x3E): FxTR[12:5] -- 8 bits
    ///   LOW byte  (higher address, e.g. 0x3F): FxTR[4:0] in bits [7:3], unused [2:0]
    ///
    /// The HIGH byte MUST be read first -- this latches the LOW byte for
    /// atomic readout (EMC2302 datasheet Section 6.14).
    ///
    /// tach_counter = (HIGH << 5) | (LOW >> 3)
    /// RPM = 3,932,160 * multiplier / tach_counter
    ///
    /// NOTE: ESP-Miner header incorrectly labels 0x3E as "TACH1_LSB" and
    /// 0x3F as "TACH1_MSB". The datasheet is clear: 0x3E = HIGH byte.
    /// ESP-Miner code works correctly because it reads from 0x3E as a
    /// 2-byte sequential read (data[0]=0x3E=HIGH, data[1]=0x3F=LOW) and
    /// uses (data[0]<<5)|(data[1]>>3), which is the correct formula.
    fn read_fan_rpm(
        &self,
        i2c: &mut I2cBus,
        high_reg: u8,
        low_reg: u8,
        multiplier: u8,
    ) -> Result<u32, Emc2302Error> {
        // Read HIGH byte first -- this latches LOW byte for atomic readout
        let high = i2c.read_reg_u8(self.addr, high_reg)?;
        let low = i2c.read_reg_u8(self.addr, low_reg)?;

        // Check for "no fan" sentinel: HIGH=0xFF means tach counter maxed out,
        // indicating no tach signal (fan disconnected or not spinning).
        if high == 0xFF {
            return Ok(0);
        }

        // Reconstruct 13-bit TACH counter from the two bytes
        let tach_counter = ((high as u16) << 5) | ((low as u16) >> 3);
        if tach_counter == 0 {
            return Ok(0);
        }

        let rpm = 3_932_160u32 * multiplier as u32 / tach_counter as u32;

        // Clamp to u16 range like ESP-Miner
        Ok(rpm.min(u16::MAX as u32))
    }

    /// Read stall/spin/drive-fail status for diagnostics.
    pub fn read_status(&self, i2c: &mut I2cBus) -> Result<FanStatus, Emc2302Error> {
        let stall = i2c.read_reg_u8(self.addr, REG_STALL_STATUS)?;
        let spin = i2c.read_reg_u8(self.addr, REG_SPIN_STATUS)?;
        let drive_fail = i2c.read_reg_u8(self.addr, REG_DRIVE_FAIL_STATUS)?;

        Ok(FanStatus {
            fan1_stall: (stall & 0x01) != 0,
            fan2_stall: (stall & 0x02) != 0,
            fan1_spin_fail: (spin & 0x01) != 0,
            fan2_spin_fail: (spin & 0x02) != 0,
            fan1_drive_fail: (drive_fail & 0x01) != 0,
            fan2_drive_fail: (drive_fail & 0x02) != 0,
        })
    }
    /// Dump diagnostic registers for both fans (spinup config, min drive, config1/2).
    pub fn dump_diagnostics(&self, i2c: &mut I2cBus) {
        // Fan 1 registers
        let f1_setting = i2c.read_reg_u8(self.addr, REG_FAN1_SETTING).unwrap_or(0);
        let f1_config1 = i2c.read_reg_u8(self.addr, REG_FAN1_CONFIG1).unwrap_or(0);
        let f1_config2 = i2c.read_reg_u8(self.addr, REG_FAN1_CONFIG2).unwrap_or(0);
        let f1_spinup = i2c.read_reg_u8(self.addr, REG_FAN1_SPINUP).unwrap_or(0);
        let f1_min_drive = i2c.read_reg_u8(self.addr, 0x38).unwrap_or(0); // FAN1_MIN_DRIVE
                                                                          // Fan 2 registers
        let f2_setting = i2c.read_reg_u8(self.addr, REG_FAN2_SETTING).unwrap_or(0);
        let f2_config1 = i2c.read_reg_u8(self.addr, REG_FAN2_CONFIG1).unwrap_or(0);
        let f2_config2 = i2c.read_reg_u8(self.addr, REG_FAN2_CONFIG2).unwrap_or(0);
        let f2_spinup = i2c.read_reg_u8(self.addr, REG_FAN2_SPINUP).unwrap_or(0);
        let f2_min_drive = i2c.read_reg_u8(self.addr, 0x48).unwrap_or(0); // FAN2_MIN_DRIVE

        // Global config registers
        let config = i2c.read_reg_u8(self.addr, REG_CONFIG).unwrap_or(0);
        let pwm_output = i2c.read_reg_u8(self.addr, REG_PWM_OUTPUT).unwrap_or(0);
        let pwm_polarity = i2c.read_reg_u8(self.addr, REG_PWM_POLARITY).unwrap_or(0);
        let pwm_base = i2c.read_reg_u8(self.addr, 0x2D).unwrap_or(0); // PWM_BASEF123
        let f1_divide = i2c.read_reg_u8(self.addr, 0x31).unwrap_or(0); // PWM1_DIVIDE
        let f2_divide = i2c.read_reg_u8(self.addr, 0x41).unwrap_or(0); // PWM2_DIVIDE

        let pwm_type = if pwm_output & 0x03 == 0x03 {
            "push-pull"
        } else if pwm_output == 0 {
            "open-drain"
        } else {
            "mixed"
        };
        info!("EMC2302 Global: config=0x{:02X} pwm_out=0x{:02X}({}) polarity=0x{:02X} base_freq=0x{:02X}",
            config, pwm_output, pwm_type, pwm_polarity, pwm_base);

        // Decode FAN_CONFIG1 fields for diagnostics
        let decode_cfg1 = |cfg: u8| -> (bool, u16, u8, u8) {
            let en_algo = (cfg >> 7) & 1 != 0;
            let range_bits = (cfg >> 5) & 0x03;
            let range_rpm: u16 = match range_bits {
                0 => 500,
                1 => 1000,
                2 => 2000,
                _ => 4000,
            };
            let edges = (cfg >> 3) & 0x03;
            let update = cfg & 0x07;
            (en_algo, range_rpm, edges, update)
        };

        let (f1_algo, f1_range, f1_edges, f1_update) = decode_cfg1(f1_config1);
        let (f2_algo, f2_range, f2_edges, f2_update) = decode_cfg1(f2_config1);

        info!("EMC2302 Fan1: setting=0x{:02X} cfg1=0x{:02X}(algo={} range={}RPM edges={} upd={}) cfg2=0x{:02X} spinup=0x{:02X} min_drive=0x{:02X} divide=0x{:02X}",
            f1_setting, f1_config1, f1_algo, f1_range, f1_edges, f1_update,
            f1_config2, f1_spinup, f1_min_drive, f1_divide);
        info!("EMC2302 Fan2: setting=0x{:02X} cfg1=0x{:02X}(algo={} range={}RPM edges={} upd={}) cfg2=0x{:02X} spinup=0x{:02X} min_drive=0x{:02X} divide=0x{:02X}",
            f2_setting, f2_config1, f2_algo, f2_range, f2_edges, f2_update,
            f2_config2, f2_spinup, f2_min_drive, f2_divide);

        // Read and display raw TACH registers for debugging
        let t1_high = i2c.read_reg_u8(self.addr, REG_TACH1_HIGH).unwrap_or(0);
        let t1_low = i2c.read_reg_u8(self.addr, REG_TACH1_LOW).unwrap_or(0);
        let t2_high = i2c.read_reg_u8(self.addr, REG_TACH2_HIGH).unwrap_or(0);
        let t2_low = i2c.read_reg_u8(self.addr, REG_TACH2_LOW).unwrap_or(0);
        let t1_count = ((t1_high as u16) << 5) | ((t1_low as u16) >> 3);
        let t2_count = ((t2_high as u16) << 5) | ((t2_low as u16) >> 3);
        info!(
            "EMC2302 TACH1: high=0x{:02X} low=0x{:02X} count={} (0x{:04X})",
            t1_high, t1_low, t1_count, t1_count
        );
        info!(
            "EMC2302 TACH2: high=0x{:02X} low=0x{:02X} count={} (0x{:04X})",
            t2_high, t2_low, t2_count, t2_count
        );
    }
}

/// Diagnostic status for both fan channels.
#[derive(Debug, Clone)]
pub struct FanStatus {
    pub fan1_stall: bool,
    pub fan2_stall: bool,
    pub fan1_spin_fail: bool,
    pub fan2_spin_fail: bool,
    pub fan1_drive_fail: bool,
    pub fan2_drive_fail: bool,
}

impl FanStatus {
    /// Returns true if any fan has a fault condition.
    pub fn any_fault(&self) -> bool {
        self.fan1_stall
            || self.fan2_stall
            || self.fan1_spin_fail
            || self.fan2_spin_fail
            || self.fan1_drive_fail
            || self.fan2_drive_fail
    }
}
