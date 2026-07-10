//! EMC2103 fan controller + dual external-diode temperature sensor driver.
//!
//! The EMC2103 is a single-PWM fan controller with tachometer feedback and two
//! external temperature-diode channels, used on the BitAxe GT / Gamma Turbo
//! board ("801", 2x BM1370 — the highest-power BitAxe) at I2C address 0x2E.
//!
//! Ported from ESP-Miner EMC2103.c / EMC2103.h. This driver replaces the inline
//! free-functions + raw register pokes that previously lived in `main.rs`.
//!
//! # Decode semantics (DCENT vs ESP-Miner)
//!
//! DCENT's temperature/RPM decode is an intentionally SIMPLIFIED / range-CLAMPED
//! variant of ESP-Miner's:
//! - Temperature is treated as UNSIGNED and range-gated `0 < reading < 0x400`
//!   (outside that band -> `None`). ESP-Miner sign-extends; DCENT does not.
//! - RPM is range-gated `0 < reading < 0x1FFF` (outside -> 0 RPM).
//!
//! These exact formulas are pinned by host tests in the `decode` submodule. Do
//! NOT "correct" them to the upstream signed/sign-extended version: GT temp/RPM
//! readings would change.
//!
//! # File layout (single source of truth)
//!
//! This file has NO top-level `use` so it re-includes cleanly (via `#[path]`)
//! into the no-`log` host-test crate `dcentaxe-core`, where only the pure
//! `decode` submodule compiles and its `#[cfg(test)]` tests run. The
//! `espidf_impl` module (the I2C-backed `Emc2103` struct) is target-gated to the
//! ESP-IDF firmware target and is compiled in `dcentaxe-hal` for firmware.

/// Pure, host-testable decode + register-map layer (no ESP-IDF / `log` / I2C
/// deps). This is the only part of the driver compiled into `dcentaxe-core` for
/// host unit tests.
pub mod decode {
    /// EMC2103 default I2C address.
    pub const EMC2103_ADDR: u8 = 0x2E;

    /// Internal temperature sensor — high byte.
    pub const REG_INTERNAL_TEMP_MSB: u8 = 0x00;
    /// Internal temperature sensor — low byte.
    pub const REG_INTERNAL_TEMP_LSB: u8 = 0x01;
    /// External diode 1 temperature — high byte.
    pub const REG_EXTERNAL_TEMP1_MSB: u8 = 0x02;
    /// External diode 1 temperature — low byte.
    pub const REG_EXTERNAL_TEMP1_LSB: u8 = 0x03;
    /// External diode 2 temperature — high byte.
    pub const REG_EXTERNAL_TEMP2_MSB: u8 = 0x04;
    /// External diode 2 temperature — low byte.
    pub const REG_EXTERNAL_TEMP2_LSB: u8 = 0x05;

    /// External diode 1 ideality-factor configuration.
    pub const REG_EXTERNAL_DIODE1_IDEALITY: u8 = 0x11;
    /// External diode 2 ideality-factor configuration.
    pub const REG_EXTERNAL_DIODE2_IDEALITY: u8 = 0x12;
    /// External diode 1 beta-compensation configuration.
    pub const REG_EXTERNAL_DIODE1_BETA: u8 = 0x14;
    /// External diode 2 beta-compensation configuration.
    pub const REG_EXTERNAL_DIODE2_BETA: u8 = 0x15;

    /// Configuration register 1 (cleared to 0x00 at init).
    pub const REG_CONFIGURATION1: u8 = 0x20;
    /// PWM configuration register (cleared to 0x00 at init -> direct PWM).
    pub const REG_PWM_CONFIG: u8 = 0x2A;
    /// Fan-setting (PWM duty) register.
    pub const REG_FAN_SETTING: u8 = 0x40;
    /// Tachometer reading — low byte.
    pub const REG_TACH_LSB: u8 = 0x4F;
    /// Tachometer reading — high byte.
    pub const REG_TACH_MSB: u8 = 0x4E;

    /// One of the EMC2103's two external temperature-diode channels.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Emc2103Sensor {
        /// External diode 1 (registers 0x02 / 0x03).
        External1,
        /// External diode 2 (registers 0x04 / 0x05).
        External2,
    }

    impl Emc2103Sensor {
        /// Returns the `(msb_reg, lsb_reg)` register pair for this sensor.
        pub const fn regs(self) -> (u8, u8) {
            match self {
                Self::External1 => (REG_EXTERNAL_TEMP1_MSB, REG_EXTERNAL_TEMP1_LSB),
                Self::External2 => (REG_EXTERNAL_TEMP2_MSB, REG_EXTERNAL_TEMP2_LSB),
            }
        }
    }

    /// The sensor carrying the PRIMARY chip temperature, honoring the board's
    /// `temp_flip` wiring (GT board "801" = `temp_flip = true` => chip on
    /// External2 / 0x04-0x05). Mirrors ESP-Miner `EMC2103_get_external_temp`.
    pub const fn chip_sensor(temp_flip: bool) -> Emc2103Sensor {
        if temp_flip {
            Emc2103Sensor::External2
        } else {
            Emc2103Sensor::External1
        }
    }

    /// The sensor carrying the SECONDARY temperature (the other die on the GT
    /// board), honoring `temp_flip`. Mirrors ESP-Miner `EMC2103_get_external_temp2`.
    pub const fn secondary_sensor(temp_flip: bool) -> Emc2103Sensor {
        if temp_flip {
            Emc2103Sensor::External1
        } else {
            Emc2103Sensor::External2
        }
    }

    /// Decode a temperature register pair to degrees Celsius.
    ///
    /// EXACT DCENT formula (unsigned, range-gated `0 < reading < 0x400`):
    /// `reading = (msb << 8 | lsb) >> 5`, value = `reading / 8.0`. Returns
    /// `None` outside the in-range band (0 = no reading, >= 0x400 includes the
    /// 0x8000 diode-fault word).
    pub fn decode_temp(msb: u8, lsb: u8) -> Option<f32> {
        let reading = ((msb as u16) << 8 | lsb as u16) >> 5;
        if reading > 0 && reading < 0x400 {
            Some(reading as f32 / 8.0)
        } else {
            None
        }
    }

    /// Decode a tachometer register pair to RPM.
    ///
    /// EXACT DCENT formula (range-gated `0 < reading < 0x1FFF`):
    /// `reading = (lsb | (msb << 8)) >> 3`, RPM = `7_864_320 / reading`.
    /// Returns 0 outside the in-range band (0 = no reading, 0x1FFF = no-fan
    /// sentinel).
    pub fn decode_rpm(msb: u8, lsb: u8) -> u32 {
        let reading = ((lsb as u16) | ((msb as u16) << 8)) >> 3;
        if reading > 0 && reading < 0x1FFF {
            7_864_320 / reading as u32
        } else {
            0
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn decode_temp_raw_bytes_to_degc() {
            // reading=200 -> 25.0C
            assert_eq!(decode_temp(0x19, 0x00), Some(25.0));
            // reading=400 -> 50.0C
            assert_eq!(decode_temp(0x32, 0x00), Some(50.0));
            // reading=201 -> 25.125C (>>5 pulls lsb upper bits across the byte boundary)
            assert_eq!(decode_temp(0x19, 0x20), Some(25.125));
            // reading=0x3FF (in-range upper boundary) -> 127.875C
            assert_eq!(decode_temp(0x7F, 0xE0), Some(127.875));
        }

        #[test]
        fn decode_temp_out_of_range_is_none() {
            // reading=0 -> None
            assert_eq!(decode_temp(0x00, 0x00), None);
            // reading=0x400 (== the 0x8000 DIODE_FAULT word) -> None
            assert_eq!(decode_temp(0x80, 0x00), None);
        }

        #[test]
        fn decode_rpm_real_value() {
            // reading=1000 -> 7_864_320 / 1000 = 7864
            assert_eq!(decode_rpm(0x1F, 0x40), 7864);
        }

        #[test]
        fn decode_rpm_sentinels_and_boundaries() {
            // reading=0x1FFF is NOT < 0x1FFF -> 0 (no-fan sentinel)
            assert_eq!(decode_rpm(0xFF, 0xFF), 0);
            // reading=0 -> 0
            assert_eq!(decode_rpm(0x00, 0x00), 0);
            // reading=0x1FFE (in-range lower boundary) -> 7_864_320 / 8190 = 960
            assert_eq!(decode_rpm(0xFF, 0xF0), 960);
        }

        #[test]
        fn temp_flip_register_pair_selection() {
            // GT board (temp_flip=true) — must equal the current hard-coded
            // main.rs mapping: chip on 0x04/0x05, secondary on 0x02/0x03.
            assert_eq!(chip_sensor(true).regs(), (0x04, 0x05));
            assert_eq!(secondary_sensor(true).regs(), (0x02, 0x03));
            // Un-flipped board: chip on 0x02/0x03, secondary on 0x04/0x05.
            assert_eq!(chip_sensor(false).regs(), (0x02, 0x03));
            assert_eq!(secondary_sensor(false).regs(), (0x04, 0x05));
            // Direct sensor register mapping.
            assert_eq!(Emc2103Sensor::External1.regs(), (0x02, 0x03));
            assert_eq!(Emc2103Sensor::External2.regs(), (0x04, 0x05));
        }
    }
}

#[cfg(target_os = "espidf")]
mod espidf_impl {
    use super::decode::{self, Emc2103Sensor};
    use crate::i2c::{I2cBus, I2cError};

    /// Errors from EMC2103 operations.
    #[derive(Debug)]
    pub enum Emc2103Error {
        /// I2C communication error.
        I2c(I2cError),
        /// Device not found on bus.
        NotFound,
    }
    impl core::fmt::Display for Emc2103Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::I2c(e) => write!(f, "EMC2103 I2C error: {}", e),
                Self::NotFound => {
                    write!(f, "EMC2103 not found at 0x{:02X}", decode::EMC2103_ADDR)
                }
            }
        }
    }

    impl std::error::Error for Emc2103Error {}

    impl From<I2cError> for Emc2103Error {
        fn from(e: I2cError) -> Self {
            Self::I2c(e)
        }
    }

    /// EMC2103 single-PWM fan controller + dual external-diode temp sensor.
    ///
    /// Used on the BitAxe GT / Gamma Turbo board (2x BM1370). The `temp_flip`
    /// flag (from `BoardConfig::temp_flip`) selects which external diode carries
    /// the primary chip temperature vs the secondary die temperature.
    pub struct Emc2103 {
        addr: u8,
        temp_flip: bool,
    }

    impl Emc2103 {
        /// Construct a driver for the device at `addr` with the board's
        /// `temp_flip` wiring. Performs no I2C — the caller probes the bus and
        /// branches on the result (preserving the existing boot flow).
        pub fn new(addr: u8, temp_flip: bool) -> Self {
            Self { addr, temp_flip }
        }

        /// Initialize the controller: clear CONFIGURATION1 + PWM_CONFIG (direct
        /// PWM), set the fan duty, then best-effort program the external-diode
        /// ideality/beta when `ideality != 0`.
        ///
        /// The first three writes are checked (`?`), preserving the original
        /// `&&` short-circuit: a failure aborts init and reports `Err`. The
        /// ideality/beta writes are best-effort (ignored), matching the prior
        /// inline code exactly.
        pub fn init(
            &mut self,
            i2c: &mut I2cBus,
            duty: u8,
            ideality: u8,
            beta: u8,
        ) -> Result<(), Emc2103Error> {
            i2c.write_reg_u8(self.addr, decode::REG_CONFIGURATION1, 0x00)?;
            i2c.write_reg_u8(self.addr, decode::REG_PWM_CONFIG, 0x00)?;
            i2c.write_reg_u8(self.addr, decode::REG_FAN_SETTING, duty)?;
            if ideality != 0 {
                let _ = i2c.write_reg_u8(self.addr, decode::REG_EXTERNAL_DIODE1_IDEALITY, ideality);
                let _ = i2c.write_reg_u8(self.addr, decode::REG_EXTERNAL_DIODE2_IDEALITY, ideality);
                let _ = i2c.write_reg_u8(self.addr, decode::REG_EXTERNAL_DIODE1_BETA, beta);
                let _ = i2c.write_reg_u8(self.addr, decode::REG_EXTERNAL_DIODE2_BETA, beta);
            }
            Ok(())
        }

        /// Read a temperature from the given external-diode sensor, decoding via
        /// the pure host-tested formula. Returns `None` on I2C error or an
        /// out-of-range reading (MSB-first read order, matching the prior code).
        pub fn read_temp(&self, i2c: &mut I2cBus, sensor: Emc2103Sensor) -> Option<f32> {
            let (msb_reg, lsb_reg) = sensor.regs();
            let msb = i2c.read_reg_u8(self.addr, msb_reg).ok()?;
            let lsb = i2c.read_reg_u8(self.addr, lsb_reg).ok()?;
            decode::decode_temp(msb, lsb)
        }

        /// Read the PRIMARY chip temperature (honors `temp_flip`).
        pub fn read_chip_temp(&self, i2c: &mut I2cBus) -> Option<f32> {
            self.read_temp(i2c, decode::chip_sensor(self.temp_flip))
        }

        /// Read the SECONDARY die temperature (honors `temp_flip`).
        pub fn read_secondary_temp(&self, i2c: &mut I2cBus) -> Option<f32> {
            self.read_temp(i2c, decode::secondary_sensor(self.temp_flip))
        }

        /// Read fan RPM from the tachometer (LSB-first read order, matching the
        /// prior code). Returns 0 on I2C error or no-fan sentinel.
        pub fn read_rpm(&self, i2c: &mut I2cBus) -> u32 {
            let lsb = i2c
                .read_reg_u8(self.addr, decode::REG_TACH_LSB)
                .unwrap_or(0);
            let msb = i2c
                .read_reg_u8(self.addr, decode::REG_TACH_MSB)
                .unwrap_or(0);
            decode::decode_rpm(msb, lsb)
        }

        /// Set fan speed (0-100%). Maps percent to an 8-bit PWM duty and writes
        /// the fan-setting register.
        ///
        /// HALPWR-6: a HAL-level non-zero floor (`safety::FAN_FLOOR_PCT`, 20%) is
        /// enforced — identical to `Emc2302::set_fan_speed` — so any caller
        /// (autotuner, MCP `set_fan_speed`, space-heater logic) commanding below
        /// the floor, including a true-zero, does NOT stop the fan on a powered
        /// mining board (the GT / Gamma Turbo uses this EMC2103 path). A genuine
        /// full-stop is only honored under the explicit
        /// `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` lab bypass. The floor only ever
        /// RAISES a too-low command; it never reduces one.
        pub fn set_fan_speed(&self, i2c: &mut I2cBus, pct: u8) -> Result<(), Emc2103Error> {
            let floored = crate::safety::fan_duty_with_floor(
                pct,
                crate::safety::FAN_FLOOR_PCT,
                crate::safety::lab_safety_bypass_enabled(),
            );
            let duty = crate::safety::pwm_byte_for_pct(floored);
            i2c.write_reg_u8(self.addr, decode::REG_FAN_SETTING, duty)?;
            Ok(())
        }

        /// The board's `temp_flip` wiring flag (explicit accessor).
        pub fn temp_flip(&self) -> bool {
            self.temp_flip
        }
    }
}

#[cfg(target_os = "espidf")]
pub use espidf_impl::{Emc2103, Emc2103Error};
