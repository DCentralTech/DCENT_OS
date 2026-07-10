//! Power management for BitAxe boards.
//!
//! Ported from ESP-Miner's vcore.c, power.c, TPS546.c, DS4432U.c, INA260.c.
//!
//! BitAxe boards use up to three different power ICs depending on the model:
//! - **TPS546** (most models): PMBus digital DC-DC converter for ASIC core voltage
//! - **DS4432U** (older boards): I2C DAC for analog voltage control
//! - **INA260** (some boards): Current/voltage/power monitor
//!
//! The [`PowerManager`] provides a unified interface that auto-detects which
//! ICs are present and routes calls accordingly.

use crate::board::{BitAxeModel, BoardConfig};
use crate::cml_escalation::{advance_cml_window, CmlEvent, WINDOW_MS};
use crate::i2c::{I2cBus, I2cError};
use log::*;
use serde::{Deserialize, Serialize};

// ===========================================================================
// PMBus utilities
// ===========================================================================
//
// The pure PMBus Linear11 / ULINEAR16 conversions now live in the host-pure
// `power_convert` module (this `power` module is espidf-gated, so they had zero
// host coverage there). Re-export them so existing `crate::power::pmbus_*` call
// sites (e.g. `temp.rs`) keep resolving and the unqualified calls below still work.
pub use crate::power_convert::{
    f32_to_pmbus_linear11, f32_to_pmbus_ulinear16, pmbus_linear11_to_f32, pmbus_ulinear16_to_f32,
};

// ===========================================================================
// Error types
// ===========================================================================

/// Errors from power management operations
#[derive(Debug)]
pub enum PowerError {
    /// I2C communication error
    I2cError(I2cError),
    /// Requested voltage exceeds safety limits
    VoltageOutOfRange {
        requested_mv: u16,
        min_mv: u16,
        max_mv: u16,
    },
    /// No voltage regulator detected
    NoRegulatorFound,
    /// The regulator has no software-off command; callers must cut the
    /// board-level buck-enable GPIO to remove ASIC power.
    RequiresBuckCut(String),
    /// TPS546 reported a fault condition. `status_word` carries the raw
    /// PMBus STATUS_WORD for diagnostics/logging only. `status_word = 0`
    /// means the source was a non-PMBus regulator fault (e.g. enable
    /// readback timeout).
    ///
    /// ACTUAL CONSUMER BEHAVIOR (do not over-promise): the `main.rs`
    /// supervisor treats ANY `RegulatorFault` as an immediate, unconditional
    /// `fail_closed_power_off` — there is no recoverable-vs-hard split, no
    /// cooldown, and no auto re-enable today. The per-bit "recoverable
    /// (6/11/14/15) vs hard (2/3/4/5)" classification is RESERVED for a
    /// future hardware-validated recovery FSM and is not parsed by any caller
    /// (see `docs/PUBLIC_BETA_READINESS_REPORT.md`). Conservative hard-kill is
    /// the correct, known-working beta posture.
    RegulatorFault { status_word: u16, msg: String },
    /// Power IC initialization failed
    InitFailed(String),
}

impl core::fmt::Display for PowerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::I2cError(e) => write!(f, "Power I2C error: {}", e),
            Self::VoltageOutOfRange {
                requested_mv,
                min_mv,
                max_mv,
            } => write!(
                f,
                "Voltage {} mV out of range [{}, {}] mV",
                requested_mv, min_mv, max_mv
            ),
            Self::NoRegulatorFound => write!(f, "No voltage regulator found on I2C bus"),
            Self::RequiresBuckCut(msg) => write!(f, "Buck GPIO cut required: {}", msg),
            Self::RegulatorFault { msg, .. } => write!(f, "Regulator fault: {}", msg),
            Self::InitFailed(msg) => write!(f, "Power init failed: {}", msg),
        }
    }
}

impl std::error::Error for PowerError {}

impl From<I2cError> for PowerError {
    fn from(e: I2cError) -> Self {
        Self::I2cError(e)
    }
}

// ===========================================================================
// Power telemetry
// ===========================================================================

/// Complete power telemetry snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PowerTelemetry {
    /// Output voltage in millivolts
    pub voltage_mv: f32,
    /// Output current in milliamps
    pub current_ma: f32,
    /// Power consumption in watts
    pub power_w: f32,
    /// Input (supply) voltage in millivolts
    pub input_voltage_mv: f32,
    /// Voltage regulator temperature in degrees C
    pub vreg_temp_c: f32,
}

// ===========================================================================
// TPS546 driver
// ===========================================================================

/// TPS546 PMBus digital DC-DC converter driver.
///
/// The TPS546 is the primary voltage regulator on most BitAxe boards.
/// It provides:
/// - Programmable output voltage via VOUT_COMMAND
/// - Current monitoring via READ_IOUT
/// - Voltage monitoring via READ_VOUT
/// - Temperature monitoring via READ_TEMPERATURE
/// - Overcurrent/overvoltage/overtemperature protection
///
/// Communication is via PMBus (I2C-based) at 400 kHz.
pub struct Tps546 {
    /// I2C address (typically 0x24)
    addr: u8,
    /// VOUT_MODE exponent for ULINEAR16 conversions
    vout_exponent: i8,
    /// Number of voltage domains (1 for single-ASIC, 3 for Hex with 2 ASICs per domain)
    voltage_domains: u16,
    /// Whether this board expects a 12V input supply.
    expects_12v_input: bool,
    /// Some GT variants latch isolated CML faults on unsupported/transient PMBus
    /// traffic. Treat those as recoverable unless detail bytes indicate a real
    /// internal regulator fault.
    tolerate_isolated_cml: bool,
    /// CML faults observed within the current 60s window. This two-strike
    /// window is genuinely protective: a single transient CML/PMBus glitch is
    /// TOLERATED (silent CLEAR_FAULTS, returns Ok — no false kill), but a
    /// SECOND CML in the same window escalates to a `RegulatorFault` so the
    /// `main.rs` supervisor performs an immediate fail-closed power-off
    /// instead of clearing the fault forever while the chips silently hang.
    /// (There is no soft "mining_paused → cooldown → re-enable" FSM today;
    /// escalation goes straight to the conservative hard-kill.)
    cml_fault_count: u8,
    /// Wall-clock-ish ms (esp_timer_get_time / 1000) at which the current CML
    /// counting window started. 0 = no window active.
    cml_window_start_ms: u64,
}

/// TPS546 default I2C address (PMBus)
pub const TPS546_ADDR: u8 = 0x24;

// PMBus command codes used with TPS546
mod pmbus {
    /// Read output voltage (ULINEAR16 format)
    pub const READ_VOUT: u8 = 0x8B;
    /// Read output current (Linear11 format)
    pub const READ_IOUT: u8 = 0x8C;
    /// Read junction temperature (Linear11 format)
    pub const READ_TEMPERATURE_1: u8 = 0x8D;
    /// Read input voltage (Linear11 format)
    pub const READ_VIN: u8 = 0x88;
    /// Set output voltage (ULINEAR16 format)
    pub const VOUT_COMMAND: u8 = 0x21;
    /// Read VOUT format/exponent
    pub const VOUT_MODE: u8 = 0x20;
    /// Read status word (fault flags)
    pub const STATUS_WORD: u8 = 0x79;
    /// Read communication/memory/logic fault details
    pub const STATUS_CML: u8 = 0x7E;
    /// Read device status byte
    pub const STATUS_BYTE: u8 = 0x78;
    /// Turn output on/off
    pub const OPERATION: u8 = 0x01;
    /// Enable output
    pub const OPERATION_ON: u8 = 0x80;
    /// Disable output (immediate off)
    pub const OPERATION_OFF: u8 = 0x00;
    /// Set VIN_ON threshold (Linear11)
    pub const VIN_ON: u8 = 0x35;
    /// Set VIN_OFF threshold (Linear11)
    pub const VIN_OFF: u8 = 0x36;
    /// Set VIN undervoltage warning (Linear11)
    pub const VIN_UV_WARN_LIMIT: u8 = 0x58;
    /// Set VIN overvoltage fault (Linear11)
    pub const VIN_OV_FAULT_LIMIT: u8 = 0x55;
    /// Set output overcurrent warning (Linear11)
    pub const IOUT_OC_WARN_LIMIT: u8 = 0x4A;
    /// Set output overcurrent fault (Linear11)
    pub const IOUT_OC_FAULT_LIMIT: u8 = 0x46;
    /// Set output voltage minimum (ULINEAR16)
    pub const VOUT_MIN: u8 = 0x2B;
    /// Set output voltage maximum (ULINEAR16)
    pub const VOUT_MAX: u8 = 0x24;
    /// Scale loop gain
    pub const VOUT_SCALE_LOOP: u8 = 0x29;
    /// Multi-phase stacking configuration (MFR_SPECIFIC_21)
    pub const MFR_SPECIFIC_21: u8 = 0xE5;
    /// Synchronization / phase configuration (MFR_SPECIFIC_32)
    pub const MFR_SPECIFIC_32: u8 = 0xF0;
    /// Compensation configuration (MFR_SPECIFIC_12, 5 bytes)
    pub const MFR_SPECIFIC_12: u8 = 0xDC;

    // ── Pass-5 audit additions: full upstream init coverage ──────────────────
    /// Phase routing for multi-TPS546 stacks (broadcast vs single-phase ops).
    pub const PHASE: u8 = 0x04;
    /// VOUT overvoltage fault threshold (ULINEAR16).
    pub const VOUT_OV_FAULT_LIMIT: u8 = 0x40;
    /// VOUT overvoltage warning threshold (ULINEAR16).
    pub const VOUT_OV_WARN_LIMIT: u8 = 0x42;
    /// VOUT undervoltage warning threshold (ULINEAR16).
    pub const VOUT_UV_WARN_LIMIT: u8 = 0x43;
    /// VOUT undervoltage fault threshold (ULINEAR16).
    pub const VOUT_UV_FAULT_LIMIT: u8 = 0x44;
    /// VOUT margin-high (test/diagnostic high rail, ULINEAR16).
    pub const VOUT_MARGIN_HIGH: u8 = 0x25;
    /// VOUT margin-low (test/diagnostic low rail, ULINEAR16).
    pub const VOUT_MARGIN_LOW: u8 = 0x26;
    /// Switching frequency (Linear11, kHz).
    pub const FREQUENCY_SWITCH: u8 = 0x33;
    /// Soft-start delay before ramp begins (Linear11, ms).
    pub const TON_DELAY: u8 = 0x60;
    /// Soft-start ramp time (Linear11, ms).
    pub const TON_RISE: u8 = 0x61;
    /// VIN overvoltage fault response policy (single byte).
    pub const VIN_OV_FAULT_RESPONSE: u8 = 0x5F;
    /// IOUT overcurrent fault response policy (single byte).
    pub const IOUT_OC_FAULT_RESPONSE: u8 = 0x47;
    /// Over-temperature fault response policy (single byte).
    pub const OT_FAULT_RESPONSE: u8 = 0x50;
    /// TPS546 die over-temp warning threshold (Linear11, °C).
    pub const OT_WARN_LIMIT: u8 = 0x51;
    /// TPS546 die over-temp fault threshold (Linear11, °C).
    pub const OT_FAULT_LIMIT: u8 = 0x4F;
    /// Detail status registers (read on fault for diagnostic snapshot).
    pub const STATUS_VOUT: u8 = 0x7A;
    pub const STATUS_IOUT: u8 = 0x7B;
    pub const STATUS_INPUT: u8 = 0x7C;
    pub const STATUS_TEMPERATURE: u8 = 0x7D;
    pub const STATUS_OTHER: u8 = 0x7F;
    pub const STATUS_MFR_SPECIFIC: u8 = 0x80;
    /// IC device ID block-read register (6 bytes).
    pub const IC_DEVICE_ID: u8 = 0xAD;
}

/// TPS546 initialization parameters (per board family).
///
/// Ported from ESP-Miner's TPS546_CONFIG struct.
#[derive(Debug, Clone)]
pub struct Tps546Config {
    /// Input voltage turn-on threshold (volts)
    pub vin_on: f32,
    /// Input voltage turn-off threshold (volts)
    pub vin_off: f32,
    /// Input undervoltage warning limit (volts, 0 = disabled)
    pub vin_uv_warn: f32,
    /// Input overvoltage fault limit (volts)
    pub vin_ov_fault: f32,
    /// Output voltage minimum (volts)
    pub vout_min: f32,
    /// Output voltage maximum (volts)
    pub vout_max: f32,
    /// Default output voltage (volts)
    pub vout_default: f32,
    /// Output overcurrent warning (amps)
    pub iout_oc_warn: f32,
    /// Output overcurrent fault (amps)
    pub iout_oc_fault: f32,
    /// Loop gain scaling factor
    pub scale_loop: f32,
    /// Multi-phase stack configuration (0x0000 = single, 0x0001 = 2-phase slave)
    pub stack_config: u16,
    /// Sync configuration (0x10 = SYNC disabled, 0xD0 = auto-detect SYNC)
    pub sync_config: u8,
    /// Compensation loop config (5 bytes, GT-specific; empty = skip)
    pub compensation: Option<[u8; 5]>,
    /// Allow isolated CML faults to be treated as recoverable.
    pub tolerate_isolated_cml: bool,

    // ── Pass-5 audit additions (full upstream init coverage) ─────────────
    /// VOUT_OV_FAULT_LIMIT as a ratio of vout_default (typ. 1.25).
    pub vout_ov_fault_ratio: f32,
    /// VOUT_OV_WARN_LIMIT as a ratio of vout_default (typ. 1.16).
    pub vout_ov_warn_ratio: f32,
    /// VOUT_UV_WARN_LIMIT as a ratio of vout_default (typ. 0.90).
    pub vout_uv_warn_ratio: f32,
    /// VOUT_UV_FAULT_LIMIT as a ratio of vout_default (typ. 0.75).
    pub vout_uv_fault_ratio: f32,
    /// VOUT_MARGIN_HIGH as a ratio of vout_default (typ. 1.10).
    pub vout_margin_high_ratio: f32,
    /// VOUT_MARGIN_LOW as a ratio of vout_default (typ. 0.90).
    pub vout_margin_low_ratio: f32,
    /// TPS546 switching frequency in kHz (650 for GT/Hex/single per ESP-Miner).
    pub switch_freq_khz: u16,
    /// Soft-start ramp time in ms (3 ms per ESP-Miner; 0 = leave POR default).
    pub ton_rise_ms: u8,
    /// TPS546 die over-temp warn threshold (°C, 0 = leave POR default).
    pub ot_warn_c: i16,
    /// TPS546 die over-temp fault threshold (°C, 0 = leave POR default).
    pub ot_fault_c: i16,
    /// PMBUS_PHASE register value (0x00 = single-phase / not applicable;
    /// 0xFF = broadcast OPERATION/VOUT to all phases on multi-TPS546 stacks).
    pub phase_register: u8,
}

impl Tps546Config {
    /// Default config for single-ASIC boards (Max, Ultra, Supra, Gamma).
    ///
    /// These boards run from 5V USB-C or barrel jack with a single
    /// voltage domain at ~1.2V output.
    pub fn single_asic() -> Self {
        Self {
            vin_on: 4.8,
            vin_off: 4.5,
            vin_uv_warn: 0.0,
            vin_ov_fault: 6.5,
            vout_min: 1.0,
            vout_max: 2.0,
            vout_default: 1.2,
            iout_oc_warn: 25.0,
            iout_oc_fault: 30.0,
            scale_loop: 0.25,
            stack_config: 0x0000,
            sync_config: 0x10,
            compensation: None,
            tolerate_isolated_cml: false,
            vout_ov_fault_ratio: 1.25,
            vout_ov_warn_ratio: 1.16,
            vout_uv_warn_ratio: 0.90,
            vout_uv_fault_ratio: 0.75,
            vout_margin_high_ratio: 1.10,
            vout_margin_low_ratio: 0.90,
            switch_freq_khz: 650,
            ton_rise_ms: 3,
            ot_warn_c: 105,
            ot_fault_c: 145,
            phase_register: 0x00,
        }
    }

    /// Config for Hex boards (HexUltra, HexSupra).
    ///
    /// Hex boards run from 12V with 6 ASICs in 3 series pairs (2 chips per
    /// voltage domain). The TPS546 outputs ~3.6V total (3 x 1.2V per domain).
    /// SCALE_LOOP is 0.125 (half of single-ASIC 0.25) because the output
    /// capacitance is higher and the load is distributed across 3 domains.
    ///
    /// Input protection: VIN_ON=11.5V prevents operation on 5V USB supplies.
    /// If powered by 5V, the TPS546 never enables — no damage, but no mining.
    pub fn hex() -> Self {
        Self {
            vin_on: 11.5,
            vin_off: 11.0,
            vin_uv_warn: 11.0,
            vin_ov_fault: 14.0,
            vout_min: 2.5,
            vout_max: 4.5,
            vout_default: 3.6,
            iout_oc_warn: 25.0,
            iout_oc_fault: 30.0,
            scale_loop: 0.125,
            stack_config: 0x0000, // Single module
            sync_config: 0x10,    // SYNC disabled
            compensation: None,
            tolerate_isolated_cml: false,
            vout_ov_fault_ratio: 1.25,
            vout_ov_warn_ratio: 1.16,
            vout_uv_warn_ratio: 0.90,
            vout_uv_fault_ratio: 0.75,
            vout_margin_high_ratio: 1.10,
            vout_margin_low_ratio: 0.90,
            switch_freq_khz: 650,
            ton_rise_ms: 3,
            ot_warn_c: 105,
            ot_fault_c: 145,
            phase_register: 0x00,
        }
    }

    /// Config for Gamma Turbo / GT (2x BM1370, 12V input, 2x TPS546 in parallel).
    ///
    /// Two hardware revisions exist:
    ///   * **v800 (strapped)**: TPS546 multi-phase is configured via HW strap
    ///     pins. Writing the MFR_SPECIFIC_21/32/12 registers is redundant and
    ///     can I2C-time-out (corrupts bus). STACK_CONFIG reads back as 0x0001.
    ///   * **v801 (strapless)**: No HW straps. Software MUST write
    ///     STACK_CONFIG=0x0001, SYNC_CONFIG=0xD0, COMPENSATION=[0x12 0x34 0x42
    ///     0x21 0x04] to put the two phases into proper 2-phase operation —
    ///     without it the phases don't share current and VOUT_OV trips at
    ///     ~25 s full-load. STACK_CONFIG reads back as 0x0000.
    ///
    /// We set the strapless values here; at init time `check_needs_mfr_config`
    /// reads the live STACK_CONFIG and skips the writes on strapped (v800)
    /// boards. This auto-adapts to either hardware revision.
    ///
    /// Values copied verbatim from ESP-Miner's upstream
    /// `TPS546_CONFIG_GAMMATURBO` (PR #1478 "801 Strapless working").
    pub fn gamma_turbo() -> Self {
        Self {
            vin_on: 11.0,
            vin_off: 10.5,
            vin_uv_warn: 11.0,
            vin_ov_fault: 14.0,
            vout_min: 1.0,
            vout_max: 3.0,
            vout_default: 1.2,
            iout_oc_warn: 50.0,
            iout_oc_fault: 55.0,
            scale_loop: 0.25,
            stack_config: 0x0001, // 2 modules (one-slave, 2-phase)
            sync_config: 0xD0,    // Enable auto-detect SYNC
            compensation: Some([0x12, 0x34, 0x42, 0x21, 0x04]),
            tolerate_isolated_cml: true,
            vout_ov_fault_ratio: 1.25,
            vout_ov_warn_ratio: 1.16,
            vout_uv_warn_ratio: 0.90,
            vout_uv_fault_ratio: 0.75,
            vout_margin_high_ratio: 1.10,
            vout_margin_low_ratio: 0.90,
            switch_freq_khz: 650,
            ton_rise_ms: 3,
            ot_warn_c: 105,
            ot_fault_c: 145,
            // Multi-TPS546 stack on GT — broadcast OPERATION/VOUT_COMMAND
            // to all phases. Only effective on strapless v801 path.
            phase_register: 0xFF,
        }
    }
}

impl Tps546 {
    /// Initialize the TPS546 voltage regulator.
    ///
    /// Probes the I2C bus, reads the VOUT_MODE exponent, and configures
    /// protection limits according to the provided configuration.
    pub fn new(
        i2c: &mut I2cBus,
        addr: u8,
        config: &Tps546Config,
        voltage_domains: u16,
        expects_12v_input: bool,
    ) -> Result<Self, PowerError> {
        // Verify device is present
        if !i2c.probe(addr) {
            return Err(PowerError::NoRegulatorFound);
        }

        // Read VOUT_MODE to get the exponent for ULINEAR16 conversions
        let vout_mode = i2c.read_reg_u8(addr, pmbus::VOUT_MODE)?;
        // VOUT_MODE bits[4:0] = signed exponent (two's complement, 5 bits)
        let exponent_raw = vout_mode & 0x1F;
        let vout_exponent = if exponent_raw > 15 {
            exponent_raw as i8 - 32
        } else {
            exponent_raw as i8
        };

        info!(
            "TPS546 at 0x{:02x}: VOUT_MODE=0x{:02x}, exponent={}",
            addr, vout_mode, vout_exponent
        );

        let mut tps = Self {
            addr,
            vout_exponent,
            voltage_domains,
            expects_12v_input,
            tolerate_isolated_cml: config.tolerate_isolated_cml,
            cml_fault_count: 0,
            cml_window_start_ms: 0,
        };

        // XPSAFE-2 (default-off feature): ARM the HAL fault-limit write guard
        // BEFORE configuring limits, so the legitimate `configure_limits` writes
        // below still go through, then LATCH it after init so any later stray
        // write to a protection-limit register is refused. Cross-pollinated from
        // DCENT_OS's per-bus EEPROM write denylist. No-op unless the
        // `tps546-fault-limit-guard` feature is enabled — field-proven boards are
        // byte-for-byte unchanged.
        #[cfg(feature = "tps546-fault-limit-guard")]
        i2c.arm_tps546_fault_limit_guard();

        // Turn off output first while configuring
        i2c.write_reg_u8(addr, pmbus::OPERATION, pmbus::OPERATION_OFF)?;

        // Configure ON_OFF_CONFIG so TPS546 responds to OPERATION commands
        // Bits: PU(0x10) | CMD(0x08) | POLARITY(0x02) | DELAY(0x01) = 0x1B
        i2c.write_reg_u8(addr, 0x02, 0x1B)?; // PMBUS_ON_OFF_CONFIG = 0x02
        info!("TPS546: ON_OFF_CONFIG set to 0x1B");

        // Clear any latched faults from previous run
        let _ = i2c.write(addr, &[0x03]); // PMBUS_CLEAR_FAULTS
        std::thread::sleep(std::time::Duration::from_millis(10));
        info!("TPS546: Faults cleared");

        // Configure protection limits
        tps.configure_limits(i2c, config)?;

        // XPSAFE-2: init's legitimate fault-limit writes are done — LATCH the
        // guard so the protection registers are now read-only for the rest of the
        // session. The runtime voltage path (VOUT_COMMAND/OPERATION) is unaffected.
        #[cfg(feature = "tps546-fault-limit-guard")]
        i2c.latch_tps546_fault_limit_guard();

        info!(
            "TPS546 initialized: VIN_ON={:.1}V, VOUT_DEFAULT={:.1}V, domains={}",
            config.vin_on, config.vout_default, voltage_domains
        );

        Ok(tps)
    }

    /// Initialize with default address (0x24).
    pub fn new_default(
        i2c: &mut I2cBus,
        config: &Tps546Config,
        voltage_domains: u16,
        expects_12v_input: bool,
    ) -> Result<Self, PowerError> {
        Self::new(i2c, TPS546_ADDR, config, voltage_domains, expects_12v_input)
    }

    /// Configure all protection limits on the TPS546.
    fn configure_limits(&self, i2c: &mut I2cBus, config: &Tps546Config) -> Result<(), PowerError> {
        // PMBus writes need inter-command delays — the TPS546 clock-stretches
        // and can timeout if commands are sent back-to-back too fast.
        // ESP-Miner's TPS546 init takes ~400ms for 20+ register writes.
        use std::thread;
        use std::time::Duration;
        const PMBUS_DELAY: Duration = Duration::from_millis(5);

        // VIN thresholds (Linear11 format)
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VIN_ON,
            f32_to_pmbus_linear11(config.vin_on),
        )
        .map_err(|e| {
            error!("TPS546: VIN_ON write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VIN_OFF,
            f32_to_pmbus_linear11(config.vin_off),
        )
        .map_err(|e| {
            error!("TPS546: VIN_OFF write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);

        if config.vin_uv_warn > 0.0 {
            i2c.write_reg_u16_le(
                self.addr,
                pmbus::VIN_UV_WARN_LIMIT,
                f32_to_pmbus_linear11(config.vin_uv_warn),
            )
            .map_err(|e| {
                error!("TPS546: VIN_UV_WARN write failed: {}", e);
                e
            })?;
            thread::sleep(PMBUS_DELAY);
        }

        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VIN_OV_FAULT_LIMIT,
            f32_to_pmbus_linear11(config.vin_ov_fault),
        )
        .map_err(|e| {
            error!("TPS546: VIN_OV_FAULT write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);

        // VOUT limits (ULINEAR16 format)
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VOUT_MIN,
            f32_to_pmbus_ulinear16(config.vout_min, self.vout_exponent),
        )
        .map_err(|e| {
            error!("TPS546: VOUT_MIN write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VOUT_MAX,
            f32_to_pmbus_ulinear16(config.vout_max, self.vout_exponent),
        )
        .map_err(|e| {
            error!("TPS546: VOUT_MAX write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);

        // ── Pass-5 audit: VOUT fault/warn thresholds ─────────────────────
        // POR defaults are tight (~115% / ~80%); ESP-Miner widens to
        // 1.25× / 1.16× / 0.90× / 0.75× of vout_default. Without these,
        // GT load transients can overshoot the stock VOUT_OV threshold and
        // trigger STATUS_WORD=0x8223 (root cause of intermittent reboot
        // loops on Gamma Turbo). All non-fatal: warn-and-continue on NACK.
        let vout_thresholds: &[(u8, &str, f32)] = &[
            (
                pmbus::VOUT_OV_FAULT_LIMIT,
                "VOUT_OV_FAULT",
                config.vout_ov_fault_ratio,
            ),
            (
                pmbus::VOUT_OV_WARN_LIMIT,
                "VOUT_OV_WARN",
                config.vout_ov_warn_ratio,
            ),
            (
                pmbus::VOUT_UV_WARN_LIMIT,
                "VOUT_UV_WARN",
                config.vout_uv_warn_ratio,
            ),
            (
                pmbus::VOUT_UV_FAULT_LIMIT,
                "VOUT_UV_FAULT",
                config.vout_uv_fault_ratio,
            ),
            (
                pmbus::VOUT_MARGIN_HIGH,
                "VOUT_MARGIN_H",
                config.vout_margin_high_ratio,
            ),
            (
                pmbus::VOUT_MARGIN_LOW,
                "VOUT_MARGIN_L",
                config.vout_margin_low_ratio,
            ),
        ];
        for (reg, name, ratio) in vout_thresholds {
            if *ratio <= 0.0 {
                continue;
            }
            let raw = f32_to_pmbus_ulinear16(config.vout_default * ratio, self.vout_exponent);
            match i2c.write_reg_u16_le(self.addr, *reg, raw) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                }
                Err(e) => warn!("TPS546: {} write failed: {}", name, e),
            }
        }

        // ── Pass-5 audit: fault response policies ─────────────────────────
        // Hiccup-retry on VIN_OV (0xB7), latch-off on IOUT_OC (0xC0),
        // retry-after-cool on OT (0xFF). Matches ESP-Miner upstream.
        let fault_responses: &[(u8, &str, u8)] = &[
            (pmbus::VIN_OV_FAULT_RESPONSE, "VIN_OV_FAULT_RESP", 0xB7),
            (pmbus::IOUT_OC_FAULT_RESPONSE, "IOUT_OC_FAULT_RESP", 0xC0),
            (pmbus::OT_FAULT_RESPONSE, "OT_FAULT_RESP", 0xFF),
        ];
        for (reg, name, val) in fault_responses {
            match i2c.write_reg_u8(self.addr, *reg, *val) {
                Ok(()) => thread::sleep(PMBUS_DELAY),
                Err(e) => warn!("TPS546: {} write failed: {}", name, e),
            }
        }

        // ── Pass-5 audit: TPS546 die over-temp thresholds ───────────────
        if config.ot_warn_c > 0 {
            match i2c.write_reg_u16_le(
                self.addr,
                pmbus::OT_WARN_LIMIT,
                f32_to_pmbus_linear11(config.ot_warn_c as f32),
            ) {
                Ok(()) => thread::sleep(PMBUS_DELAY),
                Err(e) => warn!("TPS546: OT_WARN_LIMIT write failed: {}", e),
            }
        }
        if config.ot_fault_c > 0 {
            match i2c.write_reg_u16_le(
                self.addr,
                pmbus::OT_FAULT_LIMIT,
                f32_to_pmbus_linear11(config.ot_fault_c as f32),
            ) {
                Ok(()) => thread::sleep(PMBUS_DELAY),
                Err(e) => warn!("TPS546: OT_FAULT_LIMIT write failed: {}", e),
            }
        }

        // Output current limits (Linear11 format)
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::IOUT_OC_WARN_LIMIT,
            f32_to_pmbus_linear11(config.iout_oc_warn),
        )
        .map_err(|e| {
            error!("TPS546: IOUT_OC_WARN write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::IOUT_OC_FAULT_LIMIT,
            f32_to_pmbus_linear11(config.iout_oc_fault),
        )
        .map_err(|e| {
            error!("TPS546: IOUT_OC_FAULT write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);

        // Scale loop gain
        i2c.write_reg_u16_le(
            self.addr,
            pmbus::VOUT_SCALE_LOOP,
            f32_to_pmbus_linear11(config.scale_loop),
        )
        .map_err(|e| {
            error!("TPS546: VOUT_SCALE_LOOP write failed: {}", e);
            e
        })?;
        thread::sleep(PMBUS_DELAY);

        // ── Pass-5 audit: switching frequency ─────────────────────────────
        // GT external L/C is tuned for 650 kHz. PMBus default is 500 kHz —
        // wrong frequency increases ripple and is a co-factor in VOUT_OV.
        if config.switch_freq_khz > 0 {
            match i2c.write_reg_u16_le(
                self.addr,
                pmbus::FREQUENCY_SWITCH,
                f32_to_pmbus_linear11(config.switch_freq_khz as f32),
            ) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                    info!("TPS546: FREQUENCY_SWITCH = {} kHz", config.switch_freq_khz);
                }
                Err(e) => warn!("TPS546: FREQUENCY_SWITCH write failed: {}", e),
            }
        }

        // ── Pass-5 audit: soft-start ramp time ────────────────────────────
        // 3 ms ramp avoids overshoot at enable on multi-phase GT (POR=2 ms).
        if config.ton_rise_ms > 0 {
            match i2c.write_reg_u16_le(
                self.addr,
                pmbus::TON_RISE,
                f32_to_pmbus_linear11(config.ton_rise_ms as f32),
            ) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                    info!("TPS546: TON_RISE = {} ms", config.ton_rise_ms);
                }
                Err(e) => warn!("TPS546: TON_RISE write failed: {}", e),
            }
            match i2c.write_reg_u16_le(self.addr, pmbus::TON_DELAY, f32_to_pmbus_linear11(0.0)) {
                Ok(()) => thread::sleep(PMBUS_DELAY),
                Err(e) => warn!("TPS546: TON_DELAY write failed: {}", e),
            }
        }

        // Multi-phase MFR-register configuration. GT has two hardware revs:
        //   * v800 "strapped": phases configured via HW strap pins; writing
        //     MFR regs is redundant and can I2C-time-out on the GT silicon.
        //   * v801 "strapless": MFR regs MUST be written to get 2-phase op;
        //     without them a VOUT_OV fault fires at ~25 s full load.
        // We read STACK_CONFIG first: 0x0001 → strapped, skip writes;
        // 0x0000 → strapless, write the full multi-phase config.
        let mut needs_mfr_writes = config.stack_config != 0x0000
            || config.sync_config != 0x10
            || config.compensation.is_some();
        if needs_mfr_writes {
            match i2c.read_reg_u16_le(self.addr, pmbus::MFR_SPECIFIC_21) {
                Ok(current) => {
                    info!("TPS546: live STACK_CONFIG=0x{:04x}", current);
                    if current == config.stack_config && current != 0x0000 {
                        info!(
                            "TPS546: strapped board detected (STACK_CONFIG matches) — skipping MFR writes"
                        );
                        needs_mfr_writes = false;
                    } else {
                        info!("TPS546: strapless board detected — writing STACK/SYNC/COMPENSATION");
                    }
                }
                Err(e) => {
                    warn!(
                        "TPS546: STACK_CONFIG read failed ({}) — skipping MFR writes to avoid bus corruption",
                        e
                    );
                    needs_mfr_writes = false;
                }
            }
        }

        if needs_mfr_writes && config.stack_config != 0x0000 {
            match i2c.write_reg_u16_le(self.addr, pmbus::MFR_SPECIFIC_21, config.stack_config) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                    info!("TPS546: STACK_CONFIG=0x{:04x} written", config.stack_config);
                }
                Err(e) => {
                    warn!("TPS546: STACK_CONFIG write failed: {}", e);
                }
            }
        }

        if needs_mfr_writes && config.sync_config != 0x10 {
            match i2c.write_reg_u8(self.addr, pmbus::MFR_SPECIFIC_32, config.sync_config) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                    info!("TPS546: SYNC_CONFIG=0x{:02x} written", config.sync_config);
                }
                Err(e) => {
                    warn!("TPS546: SYNC_CONFIG write failed: {}", e);
                }
            }
        }

        if needs_mfr_writes {
            if let Some(comp) = &config.compensation {
                let mut buf = [0u8; 6];
                buf[0] = pmbus::MFR_SPECIFIC_12;
                buf[1..6].copy_from_slice(comp);
                match i2c.write(self.addr, &buf) {
                    Ok(()) => {
                        thread::sleep(PMBUS_DELAY);
                        info!("TPS546: COMPENSATION written ({} bytes)", comp.len());
                    }
                    Err(e) => {
                        warn!("TPS546: COMPENSATION write failed: {}", e);
                    }
                }
            }
        }

        // ── Pass-5 audit: PHASE register for multi-TPS546 stacks ──────────
        // 0xFF broadcasts OPERATION/VOUT_COMMAND to all phases on strapless
        // GT v801. Skipped on strapped v800 (would NACK). Single-phase
        // boards leave at 0x00.
        if needs_mfr_writes && config.phase_register != 0x00 {
            match i2c.write_reg_u8(self.addr, pmbus::PHASE, config.phase_register) {
                Ok(()) => {
                    thread::sleep(PMBUS_DELAY);
                    info!("TPS546: PHASE=0x{:02x} written", config.phase_register);
                }
                Err(e) => warn!("TPS546: PHASE write failed: {}", e),
            }
        }

        // Set default output voltage
        self.set_vout_raw(i2c, config.vout_default)?;

        Ok(())
    }

    /// Set the output voltage in volts (total across all voltage domains).
    ///
    /// For single-ASIC boards, this sets the per-ASIC voltage directly.
    /// For Hex boards, the per-ASIC voltage is total_voltage / voltage_domains.
    fn set_vout_raw(&self, i2c: &mut I2cBus, voltage_v: f32) -> Result<(), PowerError> {
        let raw = f32_to_pmbus_ulinear16(voltage_v, self.vout_exponent);
        i2c.write_reg_u16_le(self.addr, pmbus::VOUT_COMMAND, raw)?;
        Ok(())
    }

    /// Set the ASIC core voltage in millivolts.
    ///
    /// This is the per-ASIC voltage. For Hex boards with series-connected
    /// ASICs, the actual regulator output is voltage_mv * voltage_domains.
    ///
    /// Example: set_voltage_mv(1220) on a Hex board (3 domains):
    ///   TPS546 output = 1.22V * 3 = 3.66V
    ///   Each domain pair runs at 1.22V across 2 series ASICs
    pub fn set_voltage_mv(&self, i2c: &mut I2cBus, voltage_mv: u16) -> Result<(), PowerError> {
        // HALPWR-3 defense-in-depth: `voltage_mv` is the PER-ASIC core voltage
        // (the total rail is voltage_mv * voltage_domains). No real BitAxe board
        // runs a per-ASIC core above ~1.55 V, so refuse (fail-closed) any per-ASIC
        // request above the absolute chip-safe driver ceiling regardless of
        // caller, even though `PowerManager::set_voltage`'s board clamp normally
        // runs first. This never raises the board clamp — it only makes the raw
        // driver self-protecting. (The TPS546's own VOUT_MAX register is the other
        // line of defense; this guard is in addition to it.)
        if !crate::safety::voltage_within_driver_ceiling(
            voltage_mv,
            crate::safety::DRIVER_VOLTAGE_CEILING_MV,
        ) {
            error!(
                "TPS546: per-ASIC voltage {} mV exceeds driver ceiling {} mV — refusing",
                voltage_mv,
                crate::safety::DRIVER_VOLTAGE_CEILING_MV
            );
            return Err(PowerError::VoltageOutOfRange {
                requested_mv: voltage_mv,
                max_mv: crate::safety::DRIVER_VOLTAGE_CEILING_MV,
                min_mv: 0,
            });
        }

        let per_asic_v = voltage_mv as f32 / 1000.0;
        let total_v = per_asic_v * self.voltage_domains as f32;

        if self.voltage_domains > 1 {
            info!(
                "TPS546: Setting {} mV per domain -> {:.3}V total ({} domains)",
                voltage_mv, total_v, self.voltage_domains
            );
        } else {
            info!(
                "TPS546: Setting voltage to {} mV ({:.3}V)",
                voltage_mv, total_v
            );
        }

        self.set_vout_raw(i2c, total_v)
    }

    /// Read the actual output voltage in volts.
    pub fn get_vout(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_le(self.addr, pmbus::READ_VOUT)?;
        Ok(pmbus_ulinear16_to_f32(raw, self.vout_exponent))
    }

    /// Read the per-ASIC voltage in millivolts.
    pub fn get_voltage_mv(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let total_v = self.get_vout(i2c)?;
        let per_asic_v = total_v / self.voltage_domains as f32;
        Ok(per_asic_v * 1000.0)
    }

    /// Read the output current in amps.
    pub fn get_iout(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_le(self.addr, pmbus::READ_IOUT)?;
        Ok(pmbus_linear11_to_f32(raw))
    }

    /// Read the output current in milliamps.
    pub fn get_current_ma(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        Ok(self.get_iout(i2c)? * 1000.0)
    }

    /// Read the input (supply) voltage in volts.
    pub fn get_vin(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_le(self.addr, pmbus::READ_VIN)?;
        Ok(pmbus_linear11_to_f32(raw))
    }

    /// Read the input voltage in millivolts.
    pub fn get_input_voltage_mv(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        Ok(self.get_vin(i2c)? * 1000.0)
    }

    /// Read the regulator junction temperature in degrees C.
    pub fn get_temperature(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_le(self.addr, pmbus::READ_TEMPERATURE_1)?;
        Ok(pmbus_linear11_to_f32(raw))
    }

    /// Enable the TPS546 output.
    ///
    /// Reads VIN before enabling and warns if the input voltage is too low
    /// (e.g., Hex or GT board on 5V USB instead of 12V). The TPS546's VIN_ON
    /// threshold will prevent output anyway, but this gives a clear log
    /// message for debugging.
    pub fn enable(&self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        // Clear any latched faults before enabling
        let _ = i2c.write(self.addr, &[0x03]); // CLEAR_FAULTS
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Pre-check: read VIN to detect wrong power supply
        match i2c.read_reg_u16_le(self.addr, pmbus::READ_VIN) {
            Ok(raw) => {
                let vin = pmbus_linear11_to_f32(raw);
                info!("TPS546: VIN = {:.2}V", vin);
                if self.expects_12v_input && vin < 10.0 {
                    error!(
                        "TPS546: VIN={:.1}V too low for 12V board! \
                         Need 12V supply (VIN_ON threshold is around 11V). 5V USB will NOT work.",
                        vin
                    );
                    return Err(PowerError::InitFailed(format!(
                        "Input voltage {:.1}V too low for 12V board (need 12V, VIN_ON threshold is around 11V)",
                        vin
                    )));
                } else if vin < 4.0 {
                    warn!(
                        "TPS546: VIN={:.1}V — critically low. Check power supply.",
                        vin
                    );
                }
            }
            Err(_) => warn!("TPS546: Could not read VIN (device may not be powered)"),
        }

        i2c.write_reg_u8(self.addr, pmbus::OPERATION, pmbus::OPERATION_ON)?;
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Read STATUS_WORD to check for faults
        match i2c.read_reg_u16_le(self.addr, pmbus::STATUS_WORD) {
            Ok(status) => {
                if status & 0xFF00 != 0 {
                    warn!(
                        "TPS546: STATUS_WORD after enable: 0x{:04X} (faults present!)",
                        status
                    );
                    // Decode specific Hex-relevant faults
                    if status & (1 << 3) != 0 {
                        error!("TPS546: VIN undervoltage! Check the 12V supply for Hex/GT boards.");
                    }
                } else {
                    info!("TPS546: Output ENABLED (STATUS=0x{:04X})", status);
                }
            }
            Err(_) => info!("TPS546: Output ENABLED (status read failed)"),
        }

        // Read actual VOUT to verify
        match i2c.read_reg_u16_le(self.addr, pmbus::READ_VOUT) {
            Ok(raw) => {
                let vout = pmbus_ulinear16_to_f32(raw, self.vout_exponent);
                if self.voltage_domains > 1 {
                    info!(
                        "TPS546: Actual VOUT = {:.3}V ({:.0}mV per domain, {} domains)",
                        vout,
                        vout / self.voltage_domains as f32 * 1000.0,
                        self.voltage_domains
                    );
                } else {
                    info!("TPS546: Actual VOUT = {:.3}V", vout);
                }

                // HALPWR-7: compare READ_VOUT against the commanded setpoint. A
                // rail that enables but sits well below command (failed phase,
                // marginal supply, ASIC soft-short) used to return Ok silently;
                // surface it as a SOFT warning so first-boot diagnosis sees a
                // sagging rail instead of mining on it. Under-volt is benign for
                // the silicon, so we do NOT fail enable() — the per-domain-scaled
                // tolerance decision is the pure `safety::vout_reached_setpoint`.
                if let Ok(cmd_raw) = i2c.read_reg_u16_le(self.addr, pmbus::VOUT_COMMAND) {
                    let cmd_v = pmbus_ulinear16_to_f32(cmd_raw, self.vout_exponent);
                    if !crate::safety::vout_reached_setpoint(
                        vout,
                        cmd_v,
                        self.voltage_domains,
                        crate::safety::VOUT_SETTLE_TOL_PER_DOMAIN_MV,
                    ) {
                        warn!(
                            "TPS546: rail did not reach setpoint after enable — READ_VOUT={:.3}V vs VOUT_CMD={:.3}V (Δ={:.0}mV, {} domains). Check supply/phase before relying on hashrate.",
                            vout,
                            cmd_v,
                            (vout - cmd_v).abs() * 1000.0,
                            self.voltage_domains
                        );
                    }
                }
            }
            Err(_) => {}
        }

        Ok(())
    }

    /// Disable the TPS546 output (immediate shutdown).
    pub fn disable(&self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        i2c.write_reg_u8(self.addr, pmbus::OPERATION, pmbus::OPERATION_OFF)?;
        info!("TPS546: Output DISABLED");
        Ok(())
    }

    /// RESERVED: clear any latched fault bits via PMBus CLEAR_FAULTS (single
    /// byte 0x03) and return the re-read STATUS_WORD. Intended for a future
    /// hardware-validated recovery FSM (clear → cooldown → re-enable at a
    /// reduced setpoint) and is NOT consumed by the current supervisor, which
    /// performs an immediate fail-closed power-off on any fault. Kept because
    /// it is correct, harmless, and the building block such an FSM would need.
    pub fn clear_faults(&self, i2c: &mut I2cBus) -> Result<u16, PowerError> {
        let _ = i2c.write(self.addr, &[0x03]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let status = i2c.read_reg_u16_le(self.addr, pmbus::STATUS_WORD)?;
        info!("TPS546: CLEAR_FAULTS → STATUS_WORD=0x{:04x}", status);
        Ok(status)
    }

    /// Diagnose the input power supply.
    ///
    /// Reads VIN and reports whether the supply voltage matches expectations
    /// for the board type. Hex and GammaTurbo boards need 12V; GammaDuo and most
    /// single-ASIC boards use 5V.
    /// Returns the input voltage in volts, or an error if the supply is wrong.
    pub fn diagnose_input(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let vin = self.get_vin(i2c)?;

        if self.expects_12v_input {
            // Hex and GammaTurbo-class boards expect 12V.
            if vin < 10.0 {
                error!(
                    "TPS546: VIN={:.1}V — board requires 12V supply! \
                     5V USB will not work. VIN_ON threshold is around 11V.",
                    vin
                );
                return Err(PowerError::InitFailed(format!(
                    "VIN={:.1}V too low for 12V board (need 12V)",
                    vin
                )));
            } else if vin > 14.0 {
                error!(
                    "TPS546: VIN={:.1}V exceeds OV_FAULT limit (14V)! Check PSU.",
                    vin
                );
                return Err(PowerError::RegulatorFault {
                    status_word: 0,
                    msg: format!("VIN={:.1}V exceeds 14V OV fault", vin),
                });
            }
            info!("TPS546: VIN={:.2}V (12V supply OK)", vin);
        } else {
            // Single-ASIC board expects 5V
            if vin < 4.0 {
                warn!("TPS546: VIN={:.1}V — low. Check USB-C or barrel jack.", vin);
            } else if vin > 6.5 {
                error!("TPS546: VIN={:.1}V exceeds OV_FAULT limit (6.5V)!", vin);
            } else {
                info!("TPS546: VIN={:.2}V (5V supply OK)", vin);
            }
        }

        Ok(vin)
    }

    /// Check the TPS546 status register for fault conditions.
    ///
    /// Returns Ok(()) if no faults, or a descriptive error string.
    pub fn check_fault(&mut self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        let status = i2c.read_reg_u16_le(self.addr, pmbus::STATUS_WORD)?;

        if status == 0 {
            // Age the CML window by wall-clock. A clean poll INSIDE the 60s
            // window must keep the strike alive (HALPWR-1); only a fully-elapsed
            // window forgets it. Old code zeroed the strike on every clean
            // STATUS_WORD, and because the 1st-strike CLEAR_FAULTS guaranteed the
            // next poll read clean, the 2nd strike could never land.
            let now_ms = unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64 / 1000;
            let dec = advance_cml_window(
                CmlEvent::Clean,
                now_ms,
                self.cml_window_start_ms,
                self.cml_fault_count,
                WINDOW_MS,
            );
            self.cml_fault_count = dec.new_count;
            self.cml_window_start_ms = dec.new_window_start_ms;
            return Ok(());
        }

        // Gamma Turbo boards can latch isolated CML/OTHER status for unsupported
        // or transient PMBus transactions without an actual electrical trip.
        // Preserve real protection shutdowns, but clear these recoverable alerts.
        let isolated_cml = self.tolerate_isolated_cml && (status & !(0x0002 | 0x0200)) == 0;
        if isolated_cml {
            let cml = i2c.read_reg_u8(self.addr, pmbus::STATUS_CML).unwrap_or(0);
            let fatal_cml = (cml & 0x18) != 0; // MEM or PROC
            if !fatal_cml {
                let mut cml_bits = Vec::new();
                if cml & 0x80 != 0 {
                    cml_bits.push("INVALID_COMMAND");
                }
                if cml & 0x40 != 0 {
                    cml_bits.push("INVALID_DATA");
                }
                if cml & 0x20 != 0 {
                    cml_bits.push("PEC_ERROR");
                }
                if cml & 0x02 != 0 {
                    cml_bits.push("COMM_ERROR");
                }
                if cml_bits.is_empty() {
                    cml_bits.push("UNKNOWN_CML");
                }
                warn!(
                    "TPS546 recoverable GT CML fault: STATUS_WORD=0x{:04x}, STATUS_CML=0x{:02x} ({})",
                    status,
                    cml,
                    cml_bits.join(", ")
                );

                // Two-strike escalation: first CML → silent CLEAR_FAULTS as before
                // (tolerate one transient glitch — no false kill). Second CML
                // inside the 60s window → return Err so the main.rs supervisor
                // performs an immediate fail-closed power-off, before the chips
                // wedge silently and trip a C-side abort that bypasses the Rust
                // panic_hook. (Conservative hard-kill; no soft recovery FSM.)
                let now_ms = unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64 / 1000;
                let dec = advance_cml_window(
                    CmlEvent::Cml,
                    now_ms,
                    self.cml_window_start_ms,
                    self.cml_fault_count,
                    WINDOW_MS,
                );
                self.cml_fault_count = dec.new_count;
                self.cml_window_start_ms = dec.new_window_start_ms;

                let _ = i2c.write(self.addr, &[0x03]);
                std::thread::sleep(std::time::Duration::from_millis(10));

                if dec.escalate {
                    let count = dec.strikes;
                    warn!(
                        "TPS546 CML fault repeated ({}x in 60s) — escalating to fail-closed power-off",
                        count
                    );
                    return Err(PowerError::RegulatorFault {
                        status_word: status,
                        msg: format!(
                            "repeated CML fault (count={}, cml=0x{:02x}, bits={})",
                            count,
                            cml,
                            cml_bits.join(", ")
                        ),
                    });
                }

                return Ok(());
            }
        }

        // Phantom-VOUT_OV-with-CML pattern (live-observed 2026-04-25 on GT):
        // STATUS_WORD=0x8223 reports CML+VOUT_OV+VOUT but the snapshot shows
        // READ_VOUT=1.146V vs VOUT_CMD=1.150V — well within spec. The VOUT_OV
        // bit is asserted as a side effect of the same PMBus glitch that set
        // CML INVALID_DATA, NOT an actual overvoltage event. This detection is
        // PROTECTIVE: without it a transient phantom-OV would be reported as a
        // real fault and trigger an UNNECESSARY fail-closed power-off on a
        // healthy miner. When the rail is verified within tolerance we clear it
        // as recoverable (return Ok, no kill); only a repeated/persistent
        // phantom-OV escalates to the conservative hard-kill (see below).
        let has_cml_bit = (status & (1 << 1)) != 0;
        let has_vout_ov_bit = (status & (1 << 5)) != 0;
        if self.tolerate_isolated_cml && has_cml_bit && has_vout_ov_bit {
            // Verify the rail is actually within tolerance (±100 mV of cmd).
            // If true → phantom OV; treat as the same two-strike CML pattern.
            let cmd_raw = i2c
                .read_reg_u16_le(self.addr, pmbus::VOUT_COMMAND)
                .unwrap_or(0);
            let cmd_v = pmbus_ulinear16_to_f32(cmd_raw, self.vout_exponent);
            let read_raw = i2c
                .read_reg_u16_le(self.addr, pmbus::READ_VOUT)
                .unwrap_or(0);
            let read_v = pmbus_ulinear16_to_f32(read_raw, self.vout_exponent);
            // Per-domain compare; for stacked GT the read is the stack total
            // but the cmd is also stack-total so the diff math holds.
            let phantom = cmd_v > 0.0 && read_v > 0.0 && (read_v - cmd_v).abs() < 0.100;
            if phantom {
                let cml = i2c.read_reg_u8(self.addr, pmbus::STATUS_CML).unwrap_or(0);
                warn!(
                    "TPS546 phantom VOUT_OV-with-CML: STATUS_WORD=0x{:04x} CML=0x{:02x} \
                     READ_VOUT={:.3}V vs VOUT_CMD={:.3}V (Δ={:.0}mV) — clearing as recoverable",
                    status,
                    cml,
                    read_v,
                    cmd_v,
                    (read_v - cmd_v).abs() * 1000.0
                );

                // Same two-strike escalation as isolated CML.
                let now_ms = unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64 / 1000;
                let dec = advance_cml_window(
                    CmlEvent::Cml,
                    now_ms,
                    self.cml_window_start_ms,
                    self.cml_fault_count,
                    WINDOW_MS,
                );
                self.cml_fault_count = dec.new_count;
                self.cml_window_start_ms = dec.new_window_start_ms;

                let _ = i2c.write(self.addr, &[0x03]); // CLEAR_FAULTS
                std::thread::sleep(std::time::Duration::from_millis(10));

                if dec.escalate {
                    let count = dec.strikes;
                    warn!(
                        "TPS546 phantom-OV+CML repeated ({}x in 60s) — escalating to fail-closed power-off",
                        count
                    );
                    // A repeated phantom-OV in the window is no longer a benign
                    // one-off glitch — escalate to the conservative fail-closed
                    // power-off (any RegulatorFault hard-kills in main.rs). Carry
                    // the RAW STATUS_WORD for diagnostics; no status_word masking
                    // is done because no consumer classifies it (the historical
                    // RECOVERABLE_MASK/HARD_MASK FSM was never implemented).
                    return Err(PowerError::RegulatorFault {
                        status_word: status,
                        msg: format!(
                            "phantom VOUT_OV with CML repeated (count={}, cml=0x{:02x}, status=0x{:04x})",
                            count, cml, status
                        ),
                    });
                }

                return Ok(());
            }
            // Not phantom — rail actually out of tolerance. Fall through to the
            // hard-fault path so the operator/recovery sees the real overvoltage.
            warn!(
                "TPS546 VOUT_OV with CML and rail out of tolerance: \
                 READ_VOUT={:.3}V vs VOUT_CMD={:.3}V — treating as REAL overvoltage",
                read_v, cmd_v
            );
        }

        let mut faults = Vec::new();

        if status & (1 << 1) != 0 {
            faults.push("CML (communication fault)");
        }
        if status & (1 << 2) != 0 {
            faults.push("TEMPERATURE (over-temperature)");
        }
        if status & (1 << 3) != 0 {
            faults.push("VIN_UV (input undervoltage)");
        }
        if status & (1 << 4) != 0 {
            faults.push("IOUT_OC (output overcurrent)");
        }
        if status & (1 << 5) != 0 {
            faults.push("VOUT_OV (output overvoltage)");
        }
        if status & (1 << 6) != 0 {
            faults.push("OFF (output is off)");
        }
        if status & (1 << 7) != 0 {
            faults.push("BUSY");
        }
        if status & (1 << 11) != 0 {
            faults.push("POWER_GOOD# (power not good)");
        }
        if status & (1 << 13) != 0 {
            faults.push("INPUT (input fault/warning)");
        }
        if status & (1 << 14) != 0 {
            faults.push("IOUT/POUT (current/power warning)");
        }
        if status & (1 << 15) != 0 {
            faults.push("VOUT (output voltage warning)");
        }

        if !faults.is_empty() {
            let msg = format!("STATUS_WORD=0x{:04x}: {}", status, faults.join(", "));
            warn!("TPS546 fault: {}", msg);
            // Diagnostic snapshot — read all detail registers + live readings
            // so the operator can root-cause the fault without re-running.
            self.snapshot_status(i2c, status);
            return Err(PowerError::RegulatorFault {
                status_word: status,
                msg,
            });
        }

        Ok(())
    }

    /// Read all TPS546 detail status bytes plus live VOUT/VIN/IOUT/TEMP and
    /// VOUT_COMMAND, emit one structured warn-level log line. Called from
    /// `check_fault` on any non-zero STATUS_WORD that isn't tolerated as
    /// isolated CML. Mirrors ESP-Miner's `TPS546_log_snapshot()`.
    pub fn snapshot_status(&self, i2c: &mut I2cBus, status_word: u16) {
        let s_vout = i2c.read_reg_u8(self.addr, pmbus::STATUS_VOUT).unwrap_or(0);
        let s_iout = i2c.read_reg_u8(self.addr, pmbus::STATUS_IOUT).unwrap_or(0);
        let s_input = i2c.read_reg_u8(self.addr, pmbus::STATUS_INPUT).unwrap_or(0);
        let s_temp = i2c
            .read_reg_u8(self.addr, pmbus::STATUS_TEMPERATURE)
            .unwrap_or(0);
        let s_cml = i2c.read_reg_u8(self.addr, pmbus::STATUS_CML).unwrap_or(0);
        let s_other = i2c.read_reg_u8(self.addr, pmbus::STATUS_OTHER).unwrap_or(0);
        let s_mfr = i2c
            .read_reg_u8(self.addr, pmbus::STATUS_MFR_SPECIFIC)
            .unwrap_or(0);
        let vout_cmd = i2c
            .read_reg_u16_le(self.addr, pmbus::VOUT_COMMAND)
            .map(|raw| pmbus_ulinear16_to_f32(raw, self.vout_exponent))
            .unwrap_or(0.0);
        let read_vout = i2c
            .read_reg_u16_le(self.addr, pmbus::READ_VOUT)
            .map(|raw| pmbus_ulinear16_to_f32(raw, self.vout_exponent))
            .unwrap_or(0.0);
        let read_vin = i2c
            .read_reg_u16_le(self.addr, pmbus::READ_VIN)
            .map(pmbus_linear11_to_f32)
            .unwrap_or(0.0);
        let read_iout = i2c
            .read_reg_u16_le(self.addr, pmbus::READ_IOUT)
            .map(pmbus_linear11_to_f32)
            .unwrap_or(0.0);
        let read_temp = i2c
            .read_reg_u16_le(self.addr, pmbus::READ_TEMPERATURE_1)
            .map(pmbus_linear11_to_f32)
            .unwrap_or(0.0);
        warn!(
            "TPS546 snapshot: STATUS_WORD=0x{:04x} VOUT=0x{:02x} IOUT=0x{:02x} INPUT=0x{:02x} TEMP=0x{:02x} CML=0x{:02x} OTHER=0x{:02x} MFR=0x{:02x} | VOUT_CMD={:.3}V READ_VOUT={:.3}V READ_VIN={:.2}V READ_IOUT={:.2}A READ_TEMP={:.1}C",
            status_word, s_vout, s_iout, s_input, s_temp, s_cml, s_other, s_mfr,
            vout_cmd, read_vout, read_vin, read_iout, read_temp
        );
    }
}

// ===========================================================================
// DS4432U driver
// ===========================================================================

/// DS4432U I2C DAC driver for voltage control (older BitAxe boards).
///
/// The DS4432U is a dual-output I2C current DAC. On BitAxe, one output
/// is connected to the voltage regulator's feedback network to adjust
/// the output voltage.
pub struct Ds4432u {
    /// I2C address (typically 0x48)
    addr: u8,
}

/// DS4432U default I2C address
pub const DS4432U_ADDR: u8 = 0x48;

/// DS4432U register: Output 0 current sink/source
const DS4432U_REG_OUT0: u8 = 0xF8;

/// DS4432U register: Output 1 current sink/source
#[allow(dead_code)]
const DS4432U_REG_OUT1: u8 = 0xF9;

// DS4432U transfer-function constants (VREF / R_FB_TOP / R_FB_BOT / R_FS / IFS)
// and the `change → reg` math now live in the host-pure `power_convert` module
// (`ds4432u_dac_code`), so `set_voltage_mv` below calls it for a byte-identical,
// host-tested regulator write.

impl Ds4432u {
    /// Initialize the DS4432U DAC.
    pub fn new(i2c: &mut I2cBus, addr: u8) -> Result<Self, PowerError> {
        if !i2c.probe(addr) {
            return Err(PowerError::NoRegulatorFound);
        }
        info!("DS4432U detected at 0x{:02x}", addr);
        Ok(Self { addr })
    }

    /// Initialize with default address (0x48).
    pub fn new_default(i2c: &mut I2cBus) -> Result<Self, PowerError> {
        Self::new(i2c, DS4432U_ADDR)
    }

    /// Set the output voltage in millivolts.
    ///
    /// Calculates the required DAC code based on the feedback network
    /// and writes it to the DS4432U output register.
    pub fn set_voltage_mv(&self, i2c: &mut I2cBus, voltage_mv: u16) -> Result<(), PowerError> {
        // HALPWR-3 defense-in-depth + the ESP-Miner DS4432U+ transfer function are
        // now in the host-pure `power_convert::ds4432u_dac_code`, which returns
        // `None` for exactly the conditions this guard previously rejected
        // (`vout < 0.0` defensive, or `voltage_mv > DRIVER_VOLTAGE_CEILING_MV`).
        // The upstream `PowerManager::set_voltage` board clamp normally runs first
        // and is tighter on every real board; this driver ceiling only ever makes
        // the driver MORE conservative (these boards cannot disable the rail over
        // I2C, so over-volt recovery is hard). Byte-identical to the prior inline
        // math: `reg = ceil(change) as u8`, source bit set when `Vout > VREF`.
        let reg = match crate::power_convert::ds4432u_dac_code(
            voltage_mv,
            crate::safety::DRIVER_VOLTAGE_CEILING_MV,
        ) {
            Some(code) => code,
            None => {
                return Err(PowerError::VoltageOutOfRange {
                    requested_mv: voltage_mv,
                    max_mv: crate::safety::DRIVER_VOLTAGE_CEILING_MV,
                    min_mv: 0,
                });
            }
        };

        info!(
            "DS4432U: Setting voltage to {} mV (code=0x{:02x})",
            voltage_mv, reg
        );

        i2c.write(self.addr, &[DS4432U_REG_OUT0, reg])?;
        Ok(())
    }
}

// ===========================================================================
// INA260 driver
// ===========================================================================

/// INA260 current/voltage/power monitor driver.
///
/// The INA260 is a high-accuracy, zero-drift current/voltage/power monitor
/// with an integrated shunt resistor. It provides direct digital readout
/// of current, voltage, and power without external calibration.
pub struct Ina260 {
    /// I2C address (typically 0x40)
    addr: u8,
}

/// INA260 default I2C address
pub const INA260_ADDR: u8 = 0x40;

/// INA260 register addresses
mod ina260_regs {
    /// Configuration register (R/W)
    pub const CONFIG: u8 = 0x00;
    /// Current measurement result (read-only)
    pub const CURRENT: u8 = 0x01;
    /// Bus voltage measurement (read-only)
    pub const BUS_VOLTAGE: u8 = 0x02;
    /// Power measurement (read-only)
    pub const POWER: u8 = 0x03;
    /// Manufacturer ID (should be 0x5449 = "TI")
    pub const MANUFACTURER_ID: u8 = 0xFE;
    /// Die ID (should be 0x2270)
    pub const DIE_ID: u8 = 0xFF;
}

impl Ina260 {
    /// Initialize the INA260 power monitor.
    pub fn new(i2c: &mut I2cBus, addr: u8) -> Result<Self, PowerError> {
        if !i2c.probe(addr) {
            return Err(PowerError::NoRegulatorFound);
        }

        // Verify manufacturer ID
        let mfr_id = i2c.read_reg_u16_be(addr, ina260_regs::MANUFACTURER_ID)?;
        if mfr_id != 0x5449 {
            warn!(
                "INA260 unexpected manufacturer ID: 0x{:04x} (expected 0x5449)",
                mfr_id
            );
        }

        let die_id = i2c.read_reg_u16_be(addr, ina260_regs::DIE_ID)?;
        info!(
            "INA260 detected at 0x{:02x}: MFR=0x{:04x}, DIE=0x{:04x}",
            addr, mfr_id, die_id
        );

        // Configure: continuous current + voltage, 1.1ms conversion, 4x averaging
        // Config = 0b_0110_0010_0010_0111 = 0x6227
        let config: u16 = 0x6227;
        i2c.write_reg_u16_be(addr, ina260_regs::CONFIG, config)?;

        Ok(Self { addr })
    }

    /// Initialize with default address (0x40).
    pub fn new_default(i2c: &mut I2cBus) -> Result<Self, PowerError> {
        Self::new(i2c, INA260_ADDR)
    }

    /// Read the bus voltage in millivolts.
    ///
    /// LSB = 1.25 mV, range 0-36V.
    pub fn get_voltage_mv(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_be(self.addr, ina260_regs::BUS_VOLTAGE)?;
        Ok(raw as f32 * 1.25)
    }

    /// Read the current in milliamps.
    ///
    /// LSB = 1.25 mA, signed 16-bit two's complement.
    pub fn get_current_ma(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_be(self.addr, ina260_regs::CURRENT)?;
        let signed = raw as i16;
        Ok(signed as f32 * 1.25)
    }

    /// Read the power in milliwatts.
    ///
    /// LSB = 10 mW.
    pub fn get_power_mw(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        let raw = i2c.read_reg_u16_be(self.addr, ina260_regs::POWER)?;
        Ok(raw as f32 * 10.0)
    }
}

// ===========================================================================
// Unified power manager
// ===========================================================================

/// Which power IC type is being used for voltage regulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerIcType {
    /// TPS546 PMBus digital DC-DC converter (most common)
    Tps546,
    /// DS4432U I2C DAC (older boards)
    Ds4432u,
}

/// Unified power management interface.
///
/// Auto-detects which power ICs are present on the board and provides
/// a single interface for voltage control and power monitoring.
///
/// Safety: voltage changes are **rejected (fail-closed)** when outside the
/// board's configured `[min_voltage_mv, max_voltage_mv]` range — the output is
/// left unchanged and `set_voltage` returns `VoltageOutOfRange`. (Fail-closed
/// reject, not saturate/clamp, is the correct behavior for a mining voltage
/// rail.) This is a hardware safety measure — never remove the range check.
pub struct PowerManager {
    /// Detected voltage regulator type
    regulator: PowerIcType,
    /// TPS546 driver (if present)
    tps546: Option<Tps546>,
    /// DS4432U driver (if present)
    ds4432u: Option<Ds4432u>,
    /// INA260 power monitor (if present)
    ina260: Option<Ina260>,
    /// Board voltage safety limits
    min_voltage_mv: u16,
    max_voltage_mv: u16,
    /// Number of voltage domains
    voltage_domains: u16,
    /// Power offset for board-level losses not measured by the regulator
    power_offset_w: f32,
    /// Best-known target voltage for regulators without direct readback.
    last_set_voltage_mv: u16,
}

impl PowerManager {
    /// Initialize the power management system.
    ///
    /// Scans the I2C bus to detect which power ICs are present, initializes
    /// them with the appropriate configuration for the board model, and
    /// configures safety limits.
    pub fn new(i2c: &mut I2cBus, config: &BoardConfig) -> Result<Self, PowerError> {
        info!("Initializing power management for {}", config.model.name());

        let mut tps546 = None;
        let mut ds4432u = None;
        let mut ina260 = None;
        let mut regulator = PowerIcType::Tps546;

        // Detect TPS546
        if i2c.probe(TPS546_ADDR) {
            info!("TPS546 detected at 0x{:02x}", TPS546_ADDR);

            let tps_config = if config.model.is_hex() {
                Tps546Config::hex()
            } else if config.model == BitAxeModel::GammaTurbo {
                Tps546Config::gamma_turbo() // Gamma Turbo runs on 12V, not 5V
            } else {
                Tps546Config::single_asic()
            };

            tps546 = Some(Tps546::new(
                i2c,
                TPS546_ADDR,
                &tps_config,
                config.voltage_domains,
                config.model.is_hex() || config.model == BitAxeModel::GammaTurbo,
            )?);
            regulator = PowerIcType::Tps546;
        }

        // Detect DS4432U (only if no TPS546 found)
        if tps546.is_none() && i2c.probe(DS4432U_ADDR) {
            info!("DS4432U detected at 0x{:02x}", DS4432U_ADDR);
            ds4432u = Some(Ds4432u::new(i2c, DS4432U_ADDR)?);
            regulator = PowerIcType::Ds4432u;
        }

        // Detect INA260 (independent power monitor, may coexist with either regulator)
        if i2c.probe(INA260_ADDR) {
            info!("INA260 detected at 0x{:02x}", INA260_ADDR);
            ina260 = Some(Ina260::new(i2c, INA260_ADDR)?);
        }

        // Ensure we found at least one voltage regulator
        if tps546.is_none() && ds4432u.is_none() {
            return Err(PowerError::NoRegulatorFound);
        }

        info!(
            "Power manager initialized: regulator={:?}, INA260={}, limits=[{}-{}] mV",
            regulator,
            ina260.is_some(),
            config.min_voltage_mv,
            config.max_voltage_mv,
        );

        Ok(Self {
            regulator,
            tps546,
            ds4432u,
            ina260,
            min_voltage_mv: config.min_voltage_mv,
            max_voltage_mv: config.max_voltage_mv,
            voltage_domains: config.voltage_domains,
            power_offset_w: config.power_offset_w,
            last_set_voltage_mv: config.default_voltage_mv,
        })
    }

    /// Create a no-op PowerManager for boards with fixed voltage (no I2C regulator).
    pub fn null() -> Self {
        info!("PowerManager: null (fixed voltage, no I2C regulator)");
        Self {
            regulator: PowerIcType::Tps546, // placeholder — never used
            tps546: None,
            ds4432u: None,
            ina260: None,
            min_voltage_mv: 0,
            max_voltage_mv: 0,
            voltage_domains: 1,
            power_offset_w: 0.0,
            last_set_voltage_mv: 0,
        }
    }

    /// Set the ASIC core voltage in millivolts.
    ///
    /// **Safety**: The requested voltage is **rejected (fail-closed)** if it
    /// falls outside the board's configured `[min_voltage_mv, max_voltage_mv]`
    /// range — the output is left unchanged and this returns
    /// `PowerError::VoltageOutOfRange`. It is NOT clamped/saturated to the
    /// boundary; a caller must inspect the returned `Result` and not assume the
    /// rail moved. (Fail-closed reject is the correct behavior for a mining
    /// voltage rail.)
    ///
    /// Setting voltage to 0 disables the output (TPS546 only).
    pub fn set_voltage(&mut self, i2c: &mut I2cBus, voltage_mv: u16) -> Result<(), PowerError> {
        // Allow voltage_mv == 0 as "disable output"
        if voltage_mv != 0 && (voltage_mv < self.min_voltage_mv || voltage_mv > self.max_voltage_mv)
        {
            return Err(PowerError::VoltageOutOfRange {
                requested_mv: voltage_mv,
                min_mv: self.min_voltage_mv,
                max_mv: self.max_voltage_mv,
            });
        }

        info!("Setting core voltage to {} mV", voltage_mv);

        match self.regulator {
            PowerIcType::Tps546 => {
                if let Some(ref tps) = self.tps546 {
                    if voltage_mv == 0 {
                        tps.disable(i2c)?;
                    } else {
                        tps.set_voltage_mv(i2c, voltage_mv)?;
                        tps.enable(i2c)?; // Must send OPERATION_ON to start output
                    }
                }
            }
            PowerIcType::Ds4432u => {
                if voltage_mv == 0 {
                    return Err(PowerError::RequiresBuckCut(
                        "DS4432U cannot disable ASIC rails over I2C".to_string(),
                    ));
                }
                if let Some(ref ds) = self.ds4432u {
                    ds.set_voltage_mv(i2c, voltage_mv)?;
                }
            }
        }

        self.last_set_voltage_mv = voltage_mv;

        Ok(())
    }

    /// Read the current ASIC core voltage in millivolts.
    pub fn get_voltage_mv(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        match self.regulator {
            PowerIcType::Tps546 => {
                if let Some(ref tps) = self.tps546 {
                    return tps.get_voltage_mv(i2c);
                }
            }
            PowerIcType::Ds4432u => {
                // DS4432U does not expose a true core-voltage readback. Return the
                // last successful setpoint instead of the INA260 input-rail voltage.
                return Ok(self.last_set_voltage_mv as f32);
            }
        }
        Ok(0.0)
    }

    /// Read the output current in milliamps.
    pub fn get_current_ma(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.get_current_ma(i2c);
        }
        if let Some(ref ina) = self.ina260 {
            return Ok(ina.get_current_ma(i2c)?);
        }
        Ok(0.0)
    }

    /// Read the total power consumption in watts.
    ///
    /// For TPS546, this calculates Vout * Iout and adds the board-level
    /// power offset (ESP32, fans, etc. not measured by the regulator).
    /// For INA260, uses the built-in power measurement.
    pub fn get_power_w(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            let vout = tps.get_vout(i2c)?;
            let iout = tps.get_iout(i2c)?;
            let regulator_power = vout * iout;
            return Ok(regulator_power + self.power_offset_w);
        }
        if let Some(ref ina) = self.ina260 {
            return Ok(ina.get_power_mw(i2c)? / 1000.0);
        }
        Ok(0.0)
    }

    /// Pass-5 audit: combined power + current accessor that issues a SINGLE
    /// READ_IOUT (and READ_VOUT for power calc) instead of doing them twice
    /// when the caller wants both. Mirrors ESP-Miner PR #1641 fix.
    /// Returns (power_w, current_a).
    pub fn get_output(&self, i2c: &mut I2cBus) -> Result<(f32, f32), PowerError> {
        if let Some(ref tps) = self.tps546 {
            let vout = tps.get_vout(i2c)?;
            let iout = tps.get_iout(i2c)?;
            let power_w = vout * iout + self.power_offset_w;
            return Ok((power_w, iout));
        }
        if let Some(ref ina) = self.ina260 {
            let power_w = ina.get_power_mw(i2c)? / 1000.0;
            let current_a = ina.get_current_ma(i2c)? / 1000.0;
            return Ok((power_w, current_a));
        }
        Ok((0.0, 0.0))
    }

    /// Read the input (supply) voltage in millivolts.
    pub fn get_input_voltage_mv(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.get_input_voltage_mv(i2c);
        }
        if let Some(ref ina) = self.ina260 {
            return Ok(ina.get_voltage_mv(i2c)?);
        }
        Ok(0.0)
    }

    /// Read the voltage regulator temperature in degrees C.
    pub fn get_vreg_temp(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.get_temperature(i2c);
        }
        Ok(0.0)
    }

    /// Whether this board exposes a real voltage-regulator temperature sensor.
    ///
    /// `get_vreg_temp()` returns `0.0` when no TPS546 is present to preserve the
    /// legacy telemetry shape, but safety logic must not treat that as a valid
    /// temperature reading.
    pub fn has_vreg_temp_sensor(&self) -> bool {
        self.tps546.is_some()
    }

    /// Read all power telemetry in a single call.
    ///
    /// HALPWR-2: each field is read INDEPENDENTLY. A transient sub-read NACK on
    /// one register no longer aborts the whole snapshot (which made main.rs drop
    /// every power field for that tick, including the valid voltage/current it
    /// DID read). A failed sub-read becomes `f32::NAN` ("field unavailable", via
    /// the pure `safety::power_field_or_nan` decision) while the fields that
    /// succeeded survive — mirroring `snapshot_status()`'s per-register
    /// `.unwrap_or`. Consumers MUST treat NaN as "unavailable" (skip / hold
    /// last-good via `safety::power_field_available`) rather than a real reading.
    /// Returns `Ok` even when some fields are NaN; the per-field health is in the
    /// values themselves.
    pub fn get_telemetry(&self, i2c: &mut I2cBus) -> Result<PowerTelemetry, PowerError> {
        use crate::safety::power_field_or_nan;
        let voltage = self.get_voltage_mv(i2c);
        let current = self.get_current_ma(i2c);
        let power = self.get_power_w(i2c);
        let vin = self.get_input_voltage_mv(i2c);
        let vreg = self.get_vreg_temp(i2c);
        Ok(PowerTelemetry {
            voltage_mv: power_field_or_nan(voltage.is_ok(), *voltage.as_ref().unwrap_or(&0.0)),
            current_ma: power_field_or_nan(current.is_ok(), *current.as_ref().unwrap_or(&0.0)),
            power_w: power_field_or_nan(power.is_ok(), *power.as_ref().unwrap_or(&0.0)),
            input_voltage_mv: power_field_or_nan(vin.is_ok(), *vin.as_ref().unwrap_or(&0.0)),
            vreg_temp_c: power_field_or_nan(vreg.is_ok(), *vreg.as_ref().unwrap_or(&0.0)),
        })
    }

    /// Check for voltage regulator faults.
    pub fn check_fault(&mut self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        if let Some(ref mut tps) = self.tps546 {
            return tps.check_fault(i2c);
        }
        Ok(())
    }

    /// Enable the voltage regulator output.
    pub fn enable(&self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.enable(i2c);
        }
        Ok(())
    }

    /// Disable the voltage regulator output.
    pub fn disable(&self, i2c: &mut I2cBus) -> Result<(), PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.disable(i2c);
        }
        if self.ds4432u.is_some() {
            return Err(PowerError::RequiresBuckCut(
                "DS4432U boards require buck-enable GPIO off for power removal".to_string(),
            ));
        }
        Ok(())
    }

    /// RESERVED: clear latched TPS546 fault bits and read back STATUS_WORD.
    /// Returns 0 on non-TPS546 regulators (no-op). Provided for a future
    /// hardware-validated recovery FSM (clear after a cooldown, then re-enable
    /// if status==0 / escalate if still asserted); NOT consumed today — the
    /// supervisor performs an immediate fail-closed power-off on any fault.
    pub fn clear_faults(&self, i2c: &mut I2cBus) -> Result<u16, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.clear_faults(i2c);
        }
        Ok(0)
    }

    /// RESERVED: VR die temperature passthrough intended for a future
    /// hardware-validated recovery FSM (to decide whether to break a cooldown
    /// early). NOT consumed today; the supervisor reads VR temp via
    /// `get_vreg_temp` for telemetry and hard-kills on any fault.
    pub fn get_vr_temperature(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.get_temperature(i2c);
        }
        Err(PowerError::NoRegulatorFound)
    }

    /// Get the detected regulator type.
    pub fn regulator_type(&self) -> PowerIcType {
        self.regulator
    }

    /// Check if an INA260 power monitor is available.
    pub fn has_ina260(&self) -> bool {
        self.ina260.is_some()
    }

    /// Get the configured voltage limits.
    pub fn voltage_limits(&self) -> (u16, u16) {
        (self.min_voltage_mv, self.max_voltage_mv)
    }

    /// Diagnose the input power supply.
    ///
    /// For Hex boards, this checks that a 12V supply is connected.
    /// Returns the input voltage or an error if the supply is wrong.
    pub fn diagnose_input(&self, i2c: &mut I2cBus) -> Result<f32, PowerError> {
        if let Some(ref tps) = self.tps546 {
            return tps.diagnose_input(i2c);
        }
        // No TPS546 — try INA260 for basic voltage reading
        if let Some(ref ina) = self.ina260 {
            let mv = ina.get_voltage_mv(i2c)?;
            return Ok(mv / 1000.0);
        }
        Ok(0.0)
    }

    /// Get the number of voltage domains.
    pub fn voltage_domains(&self) -> u16 {
        self.voltage_domains
    }

    /// Get the power offset in watts.
    pub fn power_offset_w(&self) -> f32 {
        self.power_offset_w
    }
}
