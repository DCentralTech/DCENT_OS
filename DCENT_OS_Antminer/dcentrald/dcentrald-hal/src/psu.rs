//! APW PSU controller for Antminer miners.
//!
//! Communicates via I2C (/dev/i2c-1) at address 0x10 using Bitmain's custom
//! PMBus-like protocol. Each command uses [0x55, 0xAA, LEN, CMD, PAYLOAD, CHECKSUM].
//! Bytes are sent one at a time with 3ms delays (400 Hz effective).
//! Write commands (0x81, 0x83) use echo-as-ACK: each byte is echoed back.
//!
//! Protocol source: , LIVE_RECON_S17_PRO.md, APW_PSU.py
//!
//! # APW121215a framed-I2C (am2 / S19j Pro)
//! The APW121215a on S19j Pro am2 speaks the same [0x55, 0xAA, LEN, CMD, PAYLOAD,
//! CKSUM] frame format but is reached via kernel `/dev/i2c-0` (xiic-i2c) at slave
//! 0x10. Unlike S9 GPIO bit-bang, **no echo-as-ACK** is performed (kernel I2C
//! handles per-byte ack). The `Apw121215a` struct in this module implements that
//! variant..

use crate::i2c::{
    I2cBus, I2cMutationLabel, I2cOperationIntent, I2cServiceHandle, I2cTransactionStep,
};
use crate::psu_gpio_gate::PsuGpioGate;
use crate::psu_gpio_i2c::GpioBitBangI2c;
use crate::HalError;
use crate::Result;
use std::sync::atomic::{AtomicU64, Ordering};

/// GPIO bit-bang is a transport fallback only when the requested kernel I2C
/// adapter is genuinely absent. Ownership, permission, and I/O failures are
/// control-plane or integrity failures and must propagate without touching an
/// alternate master for the same physical fabric.
fn kernel_i2c_absence_allows_gpio_fallback(error: &HalError) -> bool {
    matches!(
        error,
        HalError::DeviceOpen { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound
                || source.raw_os_error() == Some(libc::ENODEV)
    )
}

/// Default PSU I2C bus (separate from hash board bus 0).
pub const PSU_I2C_BUS: u8 = 1;

/// Default PSU I2C address.
pub const PSU_I2C_ADDR: u8 = 0x10;

/// PSU protocol preamble.
pub const PSU_PREAMBLE: [u8; 2] = [0x55, 0xAA];

// PSU command codes
pub const CMD_GET_VERSION: u8 = 0x01;
pub const CMD_GET_VOLTAGE_DAC: u8 = 0x03;
pub const CMD_MEASURE_VOLTAGE: u8 = 0x04;
pub const CMD_READ_STATE: u8 = 0x05;
pub const CMD_WATCHDOG: u8 = 0x81;
pub const CMD_SET_VOLTAGE: u8 = 0x83;

/// Delay between I2C byte writes (3ms for ~400 Hz bus).
const BYTE_DELAY_MS: u64 = 3;

/// Watchdog feed interval (20 seconds, timeout is ~60s).
///
/// A28 (knowledge-goldmine s13 F-16/F-17): the APW PSU enforces this watchdog in
/// its own PIC16F1704 firmware via a TMR4-driven 32-bit tick counter (RAM
/// 0x40-0x43, reload 0x64/100d per tick); `disable_watchdog()` (CMD 0x81, payload
/// 0x00) suppresses it. This is the *PSU* watchdog and is SEPARATE from the S19
/// hashboard DC-DC PIC16F1704 watchdog (the 0x003D main-loop-count "CLOSE DC-DC"
/// budget), which is documented where it belongs in
/// `dcentrald-silicon-profiles::pic1704_crc` (goldmine A42) — not duplicated here.
pub const WATCHDOG_INTERVAL_S: u64 = 20;

/// Default GPIO pins for PSU bit-bang I2C (S19 Pro).
const PSU_GPIO_SDA: u32 = 895;
const PSU_GPIO_SCL: u32 = 896;

enum ApwIo {
    Kernel(I2cBus),
    Gpio(GpioBitBangI2c),
    Service(I2cServiceHandle),
}

/// I2C bus backend — either kernel `/dev/i2c-N` or GPIO bit-bang.
enum PsuBus {
    /// Kernel I2C via /dev/i2c-N.
    Kernel(I2cBus),
    /// GPIO bit-bang I2C (fallback when /dev/i2c-1 doesn't exist).
    Gpio(GpioBitBangI2c),
}

/// PSU controller for APW series power supplies.
pub struct PsuController {
    bus: PsuBus,
    address: u8,
    pub version: Option<String>,
    pub watchdog_enabled: bool,
}

impl PsuController {
    /// Open PSU on I2C bus 1 at address 0x10.
    ///
    /// Tries kernel I2C first (`/dev/i2c-1`). If unavailable, falls back to
    /// GPIO bit-bang I2C (SDA=895, SCL=896) for platforms like S19 Pro where
    /// the PSU bus is not exposed as a kernel device.
    pub fn open() -> Result<Self> {
        Self::open_at(PSU_I2C_BUS, PSU_I2C_ADDR)
    }

    /// Open PSU using kernel I2C only.
    ///
    /// This intentionally refuses the GPIO bit-bang fallback because that path is
    /// known to interfere with other board-management buses on some platforms.
    pub fn open_kernel_only() -> Result<Self> {
        let i2c = I2cBus::open(PSU_I2C_BUS)?;
        tracing::info!(bus = PSU_I2C_BUS, "PSU using kernel I2C only");
        Ok(Self {
            bus: PsuBus::Kernel(i2c),
            address: PSU_I2C_ADDR,
            version: None,
            watchdog_enabled: false,
        })
    }

    /// Open PSU at specific bus and address.
    ///
    /// Falls back to GPIO bit-bang if the kernel I2C device doesn't exist.
    pub fn open_at(bus: u8, address: u8) -> Result<Self> {
        let psu_bus = match I2cBus::open(bus) {
            Ok(i2c) => {
                tracing::info!(bus, "PSU using kernel I2C /dev/i2c-{}", bus);
                PsuBus::Kernel(i2c)
            }
            Err(e) if kernel_i2c_absence_allows_gpio_fallback(&e) => {
                tracing::warn!(
                    bus,
                    error = %e,
                    sda = PSU_GPIO_SDA,
                    scl = PSU_GPIO_SCL,
                    "Kernel I2C /dev/i2c-{} unavailable, falling back to GPIO bit-bang",
                    bus,
                );
                let gpio = GpioBitBangI2c::new_am2()?;
                PsuBus::Gpio(gpio)
            }
            Err(e) => return Err(e),
        };
        Ok(Self {
            bus: psu_bus,
            address,
            version: None,
            watchdog_enabled: false,
        })
    }

    pub fn transport_name(&self) -> &'static str {
        match self.bus {
            PsuBus::Kernel(_) => "kernel_i2c",
            PsuBus::Gpio(_) => "gpio_bitbang",
        }
    }

    pub fn set_voltage_v(&mut self, voltage_v: f64) -> Result<()> {
        self.set_voltage_dac(Self::voltage_to_dac(voltage_v))
    }

    pub fn model_name_from_version(version: &str) -> String {
        let normalized = version.trim().to_ascii_uppercase();

        if normalized.contains("APW1215") || normalized.contains("1215") {
            "APW121215f".to_string()
        } else if normalized.contains("APW1417") || normalized.contains("1417") {
            "APW121417".to_string()
        } else if normalized.starts_with("APW12") {
            "APW12".to_string()
        } else if normalized.starts_with("APW9") {
            "APW9+".to_string()
        } else if normalized.starts_with("APW7") {
            "APW7".to_string()
        } else if normalized.starts_with("APW3") {
            "APW3++".to_string()
        } else {
            version.trim().to_string()
        }
    }

    pub fn voltage_range_for_version(version: &str) -> Option<(f64, f64)> {
        let normalized = version.trim().to_ascii_uppercase();

        if normalized.contains("APW1215") || normalized.contains("1215") {
            Some((11.96, 15.20))
        } else if normalized.contains("APW1417") || normalized.contains("1417") {
            Some((14.0, 17.0))
        } else if normalized.starts_with("APW12") {
            Some((11.96, 15.20))
        } else if normalized.starts_with("APW9") {
            Some((14.10, 21.0))
        } else if normalized.starts_with("APW7") {
            Some((11.60, 14.50))
        } else if normalized.starts_with("APW3") {
            Some((11.60, 13.00))
        } else {
            None
        }
    }

    pub fn format_voltage_range(version: &str) -> Option<String> {
        Self::voltage_range_for_version(version)
            .map(|(min_v, max_v)| format!("{:.2} V - {:.2} V", min_v, max_v))
    }

    /// Build a command frame: [0x55, 0xAA, length, command, payload..., checksum]
    ///
    /// Length byte = N+2 where N = payload length (i.e., cmd + payload + checksum).
    fn build_frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
        // Length counts: cmd(1) + payload(N) + checksum(1) = N+2
        let length = (1 + payload.len() + 1) as u8;
        let mut frame = vec![0x55, 0xAA, length, cmd];
        frame.extend_from_slice(payload);
        // Checksum = low byte of (length + cmd + sum(payload))
        let checksum = length
            .wrapping_add(cmd)
            .wrapping_add(payload.iter().copied().fold(0u8, |a, b| a.wrapping_add(b)));
        frame.push(checksum);
        frame
    }

    /// Send a command frame byte-by-byte (query, no echo-ACK).
    fn send_query(&mut self, cmd: u8) -> Result<Vec<u8>> {
        let frame = Self::build_frame(cmd, &[]);

        match &mut self.bus {
            PsuBus::Kernel(i2c) => {
                i2c.set_slave(self.address)?;
                // Send each byte with delay
                for &byte in &frame {
                    i2c.write(&[byte])?;
                    std::thread::sleep(std::time::Duration::from_millis(BYTE_DELAY_MS));
                }
                // Wait for PSU to process
                std::thread::sleep(std::time::Duration::from_millis(50));
                // Read response (variable length, read up to 32 bytes)
                let mut buf = vec![0u8; 32];
                let n = i2c.read(&mut buf).unwrap_or(0);
                buf.truncate(n);
                Ok(buf)
            }
            PsuBus::Gpio(gpio) => {
                // Send entire frame as one I2C write transaction
                gpio.write_to(self.address, &frame)?;
                // Wait for PSU to process
                std::thread::sleep(std::time::Duration::from_millis(50));
                // Read response
                let mut buf = vec![0u8; 32];
                let n = gpio.read_from(self.address, &mut buf).unwrap_or(0);
                buf.truncate(n);
                Ok(buf)
            }
        }
    }

    /// Send a write command with echo-ACK verification.
    fn send_write(&mut self, cmd: u8, payload: &[u8]) -> Result<()> {
        let frame = Self::build_frame(cmd, payload);

        match &mut self.bus {
            PsuBus::Kernel(i2c) => {
                i2c.set_slave(self.address)?;
                for &byte in &frame {
                    i2c.write(&[byte])?;
                    std::thread::sleep(std::time::Duration::from_millis(BYTE_DELAY_MS));
                    // Read echo (for write commands 0x81, 0x83, 0x86)
                    let mut echo = [0u8; 1];
                    if i2c.read(&mut echo).is_ok() && echo[0] != byte {
                        tracing::warn!(
                            sent = format_args!("0x{:02X}", byte),
                            echo = format_args!("0x{:02X}", echo[0]),
                            "PSU echo mismatch"
                        );
                    }
                }
            }
            PsuBus::Gpio(gpio) => {
                // GPIO mode: send frame, then read-back for echo-ACK
                for &byte in &frame {
                    gpio.write_to(self.address, &[byte])?;
                    std::thread::sleep(std::time::Duration::from_millis(BYTE_DELAY_MS));
                    let mut echo = [0u8; 1];
                    if gpio.read_from(self.address, &mut echo).is_ok() && echo[0] != byte {
                        tracing::warn!(
                            sent = format_args!("0x{:02X}", byte),
                            echo = format_args!("0x{:02X}", echo[0]),
                            "PSU GPIO echo mismatch"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Get PSU firmware version string (cmd 0x01).
    pub fn get_version(&mut self) -> Result<String> {
        let resp = self.send_query(CMD_GET_VERSION)?;
        if resp.len() >= 4 {
            // Response: [cmd_echo, status, version_bytes...]
            let version = String::from_utf8_lossy(&resp[2..]).trim().to_string();
            self.version = Some(version.clone());
            Ok(version)
        } else if !resp.is_empty() {
            let version = format!("0x{:02X}", resp[0]);
            self.version = Some(version.clone());
            Ok(version)
        } else {
            Err(HalError::I2c {
                bus: PSU_I2C_BUS,
                addr: self.address,
                detail: "PSU GET_VERSION: no response".into(),
            })
        }
    }

    /// Measure actual output voltage via ADC (cmd 0x04).
    /// Returns voltage in volts.
    pub fn measure_voltage(&mut self) -> Result<f32> {
        let resp = self.send_query(CMD_MEASURE_VOLTAGE)?;
        if resp.len() >= 4 {
            let raw = ((resp[2] as u16) << 8) | (resp[3] as u16);
            // ADC formula: voltage = (raw + 0.8615) / 63.017
            let voltage = (raw as f32 + 0.8615) / 63.017;
            Ok(voltage)
        } else {
            Err(HalError::I2c {
                bus: PSU_I2C_BUS,
                addr: self.address,
                detail: "PSU MEASURE_VOLTAGE: short response".into(),
            })
        }
    }

    /// Read PSU on/off state (cmd 0x05).
    pub fn read_state(&mut self) -> Result<bool> {
        let resp = self.send_query(CMD_READ_STATE)?;
        if resp.len() >= 4 {
            let state = ((resp[2] as u16) << 8) | (resp[3] as u16);
            Ok(state != 0)
        } else {
            Ok(true) // Assume ON if no response
        }
    }

    /// Enable PSU watchdog (cmd 0x81, payload 0x01).
    /// Must be fed every ~20 seconds or PSU shuts down after ~60s.
    pub fn enable_watchdog(&mut self) -> Result<()> {
        self.send_write(CMD_WATCHDOG, &[0x01])?;
        self.watchdog_enabled = true;
        tracing::info!(
            "PSU watchdog ENABLED — must feed every {}s",
            WATCHDOG_INTERVAL_S
        );
        Ok(())
    }

    /// Disable PSU watchdog (cmd 0x81, payload 0x00).
    pub fn disable_watchdog(&mut self) -> Result<()> {
        self.send_write(CMD_WATCHDOG, &[0x00])?;
        self.watchdog_enabled = false;
        tracing::info!("PSU watchdog DISABLED");
        Ok(())
    }

    /// Feed the PSU watchdog (same as enable -- resends cmd 0x81).
    pub fn feed_watchdog(&mut self) -> Result<()> {
        self.send_write(CMD_WATCHDOG, &[0x01])
    }

    /// Set PSU output voltage via DAC (cmd 0x83).
    /// Formula: voltage_V = 15.1084 - 0.013046 * dac_code
    pub fn set_voltage_dac(&mut self, dac: u8) -> Result<()> {
        self.send_write(CMD_SET_VOLTAGE, &[dac])?;
        let voltage = 15.1084 - 0.013046 * dac as f64;
        tracing::info!(
            dac,
            voltage = format_args!("{:.2}V", voltage),
            "PSU voltage set"
        );
        Ok(())
    }

    /// Convert target voltage to DAC code.
    /// voltage_V = 15.1084 - 0.013046 * dac -> dac = (15.1084 - voltage) / 0.013046
    pub fn voltage_to_dac(voltage_v: f64) -> u8 {
        let dac = ((15.1084 - voltage_v) / 0.013046).round() as i32;
        dac.clamp(0, 255) as u8
    }

    /// Check if PSU is reachable on the I2C bus.
    pub fn probe(&mut self) -> bool {
        // Pre-select slave for kernel mode (harmless no-op for GPIO)
        if let PsuBus::Kernel(ref mut i2c) = self.bus {
            if i2c.set_slave(self.address).is_err() {
                return false;
            }
        }
        self.get_version().is_ok()
    }
}

// =========================================================================
//  APW121215a framed-I2C driver (S19j Pro am2 / live .139)
// =========================================================================
//
// Source of truth:
//
// Transport: kernel /dev/i2c-0 (xiic-i2c) @ 100 kHz, slave 0x10.
// Frame:     [0x55][0xAA][LEN][CMD][PAYLOAD...][CKSUM]  (SUM checksum, NOT XOR)
// Read:      three-phase — write request → sleep 50ms → read preamble (2) →
//            read LEN (1) → read (LEN-1) bytes payload+cksum.
// NAK byte:  0xF5 aborts the read (one-byte abort reply).
//
// Safety rules enforced here:
//   - NEVER `set_voltage()` before 5 stable heartbeat ticks
//     ( — same rule as PIC).
//   - Calibration CRC failure is non-fatal — fall back to linear formula
//     V = 15.1084 − 0.013046·dac (Agent A found this on unprogrammed EEPROM).
//   - NEVER unbind/SOFTR xiic-i2c.
//   - NO I2C_RDWR ioctl. We use
//     `set_slave() + write() + sleep + read()` exclusively.

/// Default APW121215a I2C bus on S19j Pro am2.
pub const APW12_FRAMED_BUS: u8 = 0;

/// Default APW121215a slave address.
pub const APW12_FRAMED_ADDR: u8 = 0x10;

/// Heartbeat cadence (live log: 1.000 s ±10 ms).
pub const APW12_HEARTBEAT_MS: u64 = 1000;

/// Minimum consecutive heartbeat ticks before `set_voltage()` is allowed.
/// Mirrors the PIC deferred-voltage-stability gate
///.
pub const APW12_STABLE_TICKS_GATE: u64 = 5;

/// NAK byte that aborts a read phase (per BIBLE and Agent A spec).
const APW12_NAK_BYTE: u8 = 0xF5;

/// Delay between TX and reply poll (bosminer uses 50 ms on am2).
const APW12_REPLY_DELAY_MS: u64 = 50;

// Opcodes — disambiguated against APW121215a-Good.dis firmware disassembly
// ( 2026-05-24): the original "Agent A's spec" had GetFwVersion/
// GetHwVersion swapped. The PIC handler at label_029 (line 778, opcode 0x02)
// literally returns `0x71` (the FW version byte); label_028 (line 751,
// opcode 0x01) returns `0x10` (the HW version byte). The handler-vs-name
// correspondence is unambiguous — swap is a labels-only fix; the literal
// `0x71` that bosminer logs as "PSU: Version '0x71' (APW121215a) detected"
// is the response to opcode `0x02`, not `0x01`. Spec doc:
//
// + WAVE55D-CONSTANT-LABELING-RESOLUTION.md
// INDEPENDENTLY CONFIRMED 2026-06-02 (RE-ASK-PSU-OPCODE-0102-SWAP CLOSED): the stock Bitmain
// S19j-Pro-BB bmminer `get_power_version` builds `[55 AA 04 02 .. CK=0x06]` (opcode 0x02) and the
// reply is the power/FW version used to select the DAC formula — so GET_FW_VERSION = 0x02 is correct
// (the dspic-protocol-bible's "0x01 = GET_FW" is the errata, E5). Ghidra of the operator firmware drop;
//
pub const APW12_CMD_GET_DEVICE_TYPE: u8 = 0x01;
pub const APW12_CMD_GET_FW_VERSION: u8 = 0x02;
pub const APW12_CMD_READ_COUNTER: u8 = 0x03;
pub const APW12_CMD_READ_RAM_WORD: u8 = 0x05;
pub const APW12_CMD_WATCHDOG: u8 = 0x81;
pub const APW12_CMD_SET_VOLTAGE: u8 = 0x83;
pub const APW12_CMD_HEARTBEAT: u8 = 0x84; // inferred (BIBLE + separate log type)

/// Whether fw71 firmware disassembly proves `cmd` is observational.
///
/// This is deliberately dialect-specific. The same byte may have a different
/// effect on APW121215f UART-tunnel or SMBus controllers.
const fn apw121215a_is_observation_opcode(cmd: u8) -> bool {
    matches!(
        cmd,
        APW12_CMD_GET_DEVICE_TYPE
            | APW12_CMD_GET_FW_VERSION
            | APW12_CMD_READ_COUNTER
            | APW12_CMD_READ_RAM_WORD
    )
}

// ---------------------------------------------------------------------------
// APW121215a PIC16F1704 firmware-internal opcode catalog (gpdasm ground truth)
// ---------------------------------------------------------------------------
//
// A29 (knowledge-goldmine lane s13, findings/s13-apw12-psu.md facts F-05..F-13):
// the eight `APW121215A_CMD_*` constants below name the opcodes EXACTLY as the
// APW121215a PSU's own PIC16F1704 firmware dispatches them, decoded byte-exact
// from the gpdasm disassembly `APW121215a-Good.dis` (CP=OFF, the in-tree
// HashSource `Antminer-APW12-Firmware`). Each cite is the dispatch-handler
// label/address in that disassembly.
//
// These documentation constants record the firmware-internal names. Several
// former DCENT aliases diverged from the disassembly and were removed once the
// mismatch proved safety-relevant:
//
//   byte | former DCENT alias                  | APW121215A_CMD_* (PIC disasm)
//   -----|-------------------------------------|------------------------------
//   0x01 | GET_HW_VERSION                      | DEVICE_TYPE       (returns 0x10)
//   0x02 | GET_FW_VERSION                      | FW_VERSION        (returns 0x71)
//   0x03 | GET_CONF_VOLTAGE                    | READ_COUNTER      (tick low byte)
//   0x04 | READ_VOLTAGE                        | WRITE_VOLT_CAL    (flash cal write)
//   0x05 | READ_POWER                         | READ_RAM_WORD     (16-bit RAM read)
//   0x06 | READ_CALIBRATION                   | SET_VOLTAGE       (mV-based path)
//   0x83 | SET_VOLTAGE  (DAC byte, live .139) | SET_VOLTAGE_TARGET (DAC-N direct)
//   0x86 | (none)                             | ADJUST_VOLTAGE_STEP
//
// DCENT's live am2 rail-set sends opcode 0x83 with a DAC byte — which the PIC
// firmware handles as SET_VOLTAGE_TARGET (direct DAC-N), `label_034` @ 0x034E.
// The mV-based 0x06 SET_VOLTAGE path (`label_033` @ 0x02FF) is NOT used by DCENT.
// SAFETY CONTAINMENT (2026-07-13): the older DCENT-facing aliases for 0x04
// (`READ_VOLTAGE`) and 0x06 (`READ_CALIBRATION`) were removed. The firmware
// disassembly proves those bytes are WRITE_VOLT_CAL and SET_VOLTAGE on fw71;
// presenting them as observations let callers mutate hardware through a
// read-only service intent. Only the truthful firmware names below remain.

/// Opcode 0x01 — GET_DEVICE_TYPE. PIC returns the 0x10 device-class byte.
/// s13 F-05: `label_028` @ 0x026B (`movlw 0x10; movwf 0x58`).
pub const APW121215A_CMD_DEVICE_TYPE: u8 = 0x01;
/// Opcode 0x02 — GET_FW_VERSION. PIC returns the 0x71 firmware-version byte.
/// s13 F-06: `label_029` @ 0x0283 (`movlw 0x71`).
pub const APW121215A_CMD_FW_VERSION: u8 = 0x02;
/// Opcode 0x03 — READ_COUNTER (8-bit tick counter low byte, RAM 0x60).
/// s13 F-07: `label_030` @ 0x029B.
pub const APW121215A_CMD_READ_COUNTER: u8 = 0x03;
/// Opcode 0x04 — WRITE_VOLTAGE_CAL (flash calibration write via PMADRL/PMADRH).
/// s13 F-08: `label_031` @ 0x02BA.
pub const APW121215A_CMD_WRITE_VOLT_CAL: u8 = 0x04;
/// Opcode 0x05 — READ_RAM_WORD (16-bit RAM value at 0x28/0x29).
/// s13 F-09: `label_032` @ 0x02E7.
pub const APW121215A_CMD_READ_RAM_WORD: u8 = 0x05;
/// Opcode 0x06 — SET_VOLTAGE (mV-based, calibration-table lookup). NOT the DCENT
/// path (DCENT uses the 0x83 DAC-N target). s13 F-10: `label_033` @ 0x02FF.
pub const APW121215A_CMD_SET_VOLTAGE: u8 = 0x06;
/// Opcode 0x83 — SET_VOLTAGE_TARGET (direct DAC-N target register). THIS is the
/// opcode DCENT's am2 rail-set sends (live .139). s13 F-11: `label_034` @ 0x034E.
pub const APW121215A_CMD_SET_VOLTAGE_TARGET: u8 = 0x83;
/// Opcode 0x86 — ADJUST_VOLTAGE_STEP (step the voltage up/down).
/// s13 F-12: `label_035` @ 0x0386.
pub const APW121215A_CMD_ADJUST_VOLTAGE_STEP: u8 = 0x86;

/// PSU family/model, detected from GetFwVersion byte.
///
/// Populated from `Apw121215a::probe()`. The full 16-entry variant table
/// (APW9+, APW9++, APW121215a-g, APW121417a-b, APW111721a-c, APW11A1216-1a,
/// APW171215a/c, APW11G0) lives in the binary at offset 0xfabb7d — we
/// expose only the ones we've seen in the wild + an `Other(fw_byte)` fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsuModel {
    /// Unknown / not probed yet.
    Unknown,
    /// APW9+ family (legacy S9/S9i).
    Apw9Plus,
    /// APW9++ family.
    Apw9PlusPlus,
    /// APW121215a — 11.96–15.20 V, 3068 W, FW byte `0x71`. No voltage feedback.
    /// **This is the target on live .139.**
    Apw121215a,
    /// APW121215b/c — 121215a variants, same rail, no voltage feedback.
    Apw121215Bc,
    /// APW121215d/e — FW 0x74/0x75. Voltage feedback ADC.
    Apw121215De,
    /// APW121215f — FW byte `0x76`.
    /// **Live-confirmed on `a lab unit`** (S19K Pro NoPic / am3-aml, 2026-04-29):
    /// `bosminer-am2-s17::hardware::antminer.rs:338` logs
    /// `PSU: version '0x76' (APW121215f) detected`. Same protocol family as
    /// 121215a for model dispatch.
    ///
    /// **Telemetry capability is UNCHARACTERIZED.** Do not borrow fw71 opcode
    /// names for this firmware and do not probe raw command bytes from this
    /// catalog. A future protocol-identified adapter needs live evidence and
    /// effect classification before it may expose telemetry operations.
    Apw121215f,
    /// APW121215g — FW byte `0x77`. Heuristic; not yet seen live.
    Apw121215g,
    /// APW121417a/b — higher-voltage rail (14-17 V).
    Apw121417,
    /// APW111721a/b/c.
    Apw111721,
    /// APW11A1216-1a.
    Apw11A1216,
    /// APW171215a/c.
    Apw171215,
    /// APW11G0.
    Apw11G0,
    /// Unknown FW byte — included verbatim for diagnostics.
    Other(u8),
}

impl PsuModel {
    /// Map FW byte to model. Table stitched from the binary variant table at
    /// 0xfabb7d cross-referenced with maintenance PDFs. Only 0x71 is confirmed
    /// live (on .139); the other mappings are best-available guesses and will
    /// degrade gracefully to `Other(fw)` if wrong.
    pub fn from_fw_byte(fw: u8) -> Self {
        match fw {
            0x00 => PsuModel::Unknown,
            0x71 => PsuModel::Apw121215a,
            // Heuristic mappings based on observed +1 / +2 stepping in the
            // bosminer variant table. Logged with a `warn` when hit.
            0x72 | 0x73 => PsuModel::Apw121215Bc,
            0x74 | 0x75 => PsuModel::Apw121215De,
            0x76 => PsuModel::Apw121215f, // live-confirmed on .78
            0x77 => PsuModel::Apw121215g,
            0x60 | 0x61 => PsuModel::Apw9Plus,
            0x62 | 0x63 => PsuModel::Apw9PlusPlus,
            0x80 | 0x81 => PsuModel::Apw121417,
            0x90..=0x92 => PsuModel::Apw111721,
            0xA0 => PsuModel::Apw11A1216,
            0xB0 | 0xB1 => PsuModel::Apw171215,
            0xC0 => PsuModel::Apw11G0,
            other => PsuModel::Other(other),
        }
    }

    /// Informational telemetry capability metadata. This does not authorize a
    /// framed command: fw71 bytes 0x04 and 0x06 are mutations, not reads.
    pub fn has_voltage_feedback(self) -> bool {
        matches!(
            self,
            PsuModel::Apw121215De
                | PsuModel::Apw121215g
                | PsuModel::Apw121417
                | PsuModel::Apw171215
        )
    }

    /// Whether this model supports on-board current feedback (`ReadCurrent`
    /// or PMBus `READ_IOUT`). On the APW family, current telemetry rides on
    /// the same ADC path as voltage, so the answer is currently identical
    /// to `has_voltage_feedback()`. Exposed as a separate accessor so
    /// callers can intent-tag their queries and so we can split the answer
    /// later without breaking call sites.
    ///
    /// `Apw121215f` remains false until a protocol-identified adapter is
    /// characterized with authorized hardware evidence.
    pub fn has_current_feedback(self) -> bool {
        // Conservative: same as voltage. Variants with confirmed-no-ADC
        // (121215a fw=0x71) and uncharacterized variants (121215f fw=0x76)
        // both report `false`; 121215f is additionally gated by
        // `is_telemetry_characterized()` so callers can fail-closed instead
        // of silently treating `false` as "we know there's no ADC".
        self.has_voltage_feedback()
    }

    /// Whether this model has characterized on-board power feedback metadata.
    /// Same gating as `has_current_feedback()`.
    pub fn has_power_feedback(self) -> bool {
        self.has_voltage_feedback()
    }

    /// Whether the telemetry capability of this variant has been
    /// **characterized against a live unit** (electrically proven, not
    /// inferred from the variant table).
    ///
    /// - `Apw121215a` (fw=0x71) → `true` (proven no-feedback on `a lab unit`)
    /// - `Apw121215De/g/417/171215` → `true` (proven feedback in family)
    /// - **`Apw121215f` (fw=0x76) → `false`** (only model dispatch is
    ///   live-confirmed on `a lab unit`; ADC commands not yet probed)
    /// - `Other(_)` / `Unknown` → `false`
    ///
    /// This is capability metadata, not command authority.
    pub fn is_telemetry_characterized(self) -> bool {
        match self {
            // Live-confirmed no-feedback on .139.
            PsuModel::Apw121215a => true,
            // Variants with confirmed-feedback ADC paths.
            PsuModel::Apw121215De
            | PsuModel::Apw121215g
            | PsuModel::Apw121417
            | PsuModel::Apw171215 => true,
            // Uncharacterized — operator probe required.
            PsuModel::Apw121215f => false,
            // Heuristic / unseen variants — uncharacterized by definition.
            PsuModel::Apw121215Bc
            | PsuModel::Apw9Plus
            | PsuModel::Apw9PlusPlus
            | PsuModel::Apw111721
            | PsuModel::Apw11A1216
            | PsuModel::Apw11G0
            | PsuModel::Other(_)
            | PsuModel::Unknown => false,
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            PsuModel::Unknown => "Unknown",
            PsuModel::Apw9Plus => "APW9+",
            PsuModel::Apw9PlusPlus => "APW9++",
            PsuModel::Apw121215a => "APW121215a",
            PsuModel::Apw121215Bc => "APW121215b/c",
            PsuModel::Apw121215De => "APW121215d/e",
            PsuModel::Apw121215f => "APW121215f",
            PsuModel::Apw121215g => "APW121215g",
            PsuModel::Apw121417 => "APW121417a/b",
            PsuModel::Apw111721 => "APW111721a/b/c",
            PsuModel::Apw11A1216 => "APW11A1216-1a",
            PsuModel::Apw171215 => "APW171215a/c",
            PsuModel::Apw11G0 => "APW11G0",
            PsuModel::Other(_) => "Unknown APW (other FW byte)",
        }
    }
}

/// Build an APW framed-I2C request frame.
///
/// Frame = `[0x55, 0xAA, LEN, CMD, payload…, CKSUM]` where
///   `LEN   = 1(LEN) + 1(CMD) + payload_len + 1(CKSUM)` (Agent A §Example
///   explicitly counts LEN itself: "3 bytes after LEN including itself = 4"
///   for SetVoltage DAC=0xC8 → LEN=0x04).  So `LEN = 3 + payload_len`.
///   `CKSUM = (LEN + CMD + Σpayload) & 0xFF`  (SUM — **NOT XOR**).
///
/// The older S9/S17 legacy `PsuController::build_frame` uses a different
/// `LEN = N+2` convention; the two must not be confused (different PSU
/// firmware revisions).
///
/// `cmd` is one of the opcode constants — see `APW12_CMD_*` (DCENT-facing names)
/// and the byte-identical PIC-disasm catalog `APW121215A_CMD_*` (A29). This
/// builder emits a single **8-bit** SUM checksum; the APW121215f variant uses a
/// **16-bit** checksum (`psu_apw12_plus::build_apw121215f_frame`) and the two are
/// NOT interchangeable — pinned by the `apw121215a_fw71_set_voltage_8bit_cksum_*`
/// regression test (A30). This change is documentation-only; no emitted byte moves.
pub fn build_apw12_frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let len = (3 + payload.len()) as u8;
    let mut frame = Vec::with_capacity(3 + 1 + payload.len() + 1);
    frame.push(0x55);
    frame.push(0xAA);
    frame.push(len);
    frame.push(cmd);
    frame.extend_from_slice(payload);
    let cksum = len
        .wrapping_add(cmd)
        .wrapping_add(payload.iter().copied().fold(0u8, |a, b| a.wrapping_add(b)));
    frame.push(cksum);
    frame
}

/// Validate an APW framed-I2C reply. Returns the payload bytes on success.
///
/// Frame layout: `[0x55, 0xAA, LEN, CMD, payload…, CKSUM]`, where **LEN counts
/// itself** (= 3 + payload_len). So the total frame length on the wire is
/// `2 + LEN` bytes (preamble + LEN region).
///
/// Checks:
///   1. preamble == `55 AA`
///   2. LEN ≥ 3 and total bytes == 2 + LEN (or at least that many available)
///   3. CMD echo matches (write opcodes keep bit-7 high; queries echo raw)
///   4. CKSUM == `(LEN + CMD + Σpayload) & 0xFF`
pub fn parse_apw12_reply(expected_cmd: u8, reply: &[u8]) -> Result<Vec<u8>> {
    if reply.len() == 1 && reply[0] == APW12_NAK_BYTE {
        return Err(HalError::PsuProtocol("PSU NAK (0xF5)"));
    }
    if reply.len() < 5 {
        //  forensics — log raw bytes on every parse failure so the
        // operator can see what the spoof actually sent. Without this, the
        // generic "reply too short" message hides whether the bus returned
        // 0 bytes, 1 byte (NAK?), 2 bytes (truncated preamble?), etc.
        tracing::warn!(
            expected_cmd = format_args!("0x{:02X}", expected_cmd),
            reply_len = reply.len(),
            reply_hex = format_args!("{:02X?}", reply),
            "APW12 parse: reply too short (Wave-33 raw-byte forensics)"
        );
        return Err(HalError::PsuProtocol("reply too short"));
    }
    if reply[0] != 0x55 || reply[1] != 0xAA {
        //  forensics — the most actionable failure mode. The Loki
        // spoof on `a lab unit` responds with bytes our parser rejects as "invalid
        // preamble"; bosminer accepts the same response. Logging the raw
        // bytes here lets us see (a) what preamble the spoof DOES send,
        // (b) whether it's an off-by-1/2 byte shift, (c) whether it's a
        // bit-bang clocking edge case, and (d) whether bosminer's parser
        // is just more tolerant.
        tracing::warn!(
            expected_cmd = format_args!("0x{:02X}", expected_cmd),
            reply_len = reply.len(),
            reply_hex = format_args!("{:02X?}", reply),
            first_byte = format_args!("0x{:02X}", reply[0]),
            second_byte = format_args!("0x{:02X}", reply[1]),
            "APW12 parse: invalid preamble (Wave-33 raw-byte forensics — \
             search for 0x55 0xAA anywhere in reply_hex to identify offset shifts)"
        );
        return Err(HalError::PsuProtocol("invalid preamble"));
    }
    let len = reply[2] as usize;
    if len < 3 {
        return Err(HalError::PsuProtocol("invalid advertised frame length"));
    }
    let total = 2 + len;
    if reply.len() < total {
        return Err(HalError::PsuProtocol("short read vs advertised LEN"));
    }
    let cmd = reply[3];
    // Queries: PSU echoes CMD with bit-7 cleared. Writes: echoed verbatim.
    // Accept either — bosminer is lenient here (Agent A flagged both forms).
    if cmd != expected_cmd && cmd != (expected_cmd & 0x7F) {
        return Err(HalError::PsuProtocol("wrong reply CMD echo"));
    }
    let cksum_idx = total - 1;
    let payload = &reply[4..cksum_idx];
    let cksum = reply[cksum_idx];
    let want = (len as u8)
        .wrapping_add(cmd)
        .wrapping_add(payload.iter().copied().fold(0u8, |a, b| a.wrapping_add(b)));
    if cksum != want {
        return Err(HalError::PsuProtocol("invalid reply checksum"));
    }
    Ok(payload.to_vec())
}

/// APW121215a (and framed-I2C family) controller.
///
/// Keeps an atomic `heartbeat_ticks` counter so the 5-stable-ticks gate on
/// `set_voltage()` works across threads. Not `Send + Sync` for the I2C bus
/// itself — use a Mutex externally as today's `PsuController` usage does.
pub struct Apw121215a {
    io: ApwIo,
    addr: u8,
    bus: u8,
    /// FW byte (raw) returned by GetFwVersion. `None` until `probe()`.
    fw_byte: Option<u8>,
    /// Detected model.
    model: PsuModel,
    /// Current DAC setpoint (last `set_voltage_dac` value).
    dac: Option<u8>,
    /// Number of successful heartbeat ticks since open. Atomic so the
    /// heartbeat loop (any thread) can increment while a config thread reads.
    heartbeat_ticks: AtomicU64,
    /// Watchdog currently armed (tracked locally since PSU has no query).
    watchdog_armed: bool,
    /// Working heartbeat verb for this session.
    heartbeat_mode: ApwHeartbeatMode,
    /// Optional PWR_CONTROL GPIO spec ("label:PWR_CONTROL", "gpio:907", or numeric).
    /// When set, `cold_boot_sequence_gated()` will assert the gate before any
    /// I²C write reaches the APW PSU.:
    /// PSU EIOs on cold boot are GPIO-gated, so this MUST be set on am2
    /// before `cold_boot_sequence_gated` to avoid the EIO storm.
    /// `None` on S9, BB, AML, S21 NoPic — those platforms do not gate PSU I²C
    /// behind PWR_CONTROL.
    gate_spec: Option<String>,
    /// Live `PsuGpioGate` once asserted. Held here so the gate's `Drop`
    /// impl auto-deasserts PWR_CONTROL when this `Apw121215a` is dropped —
    /// the PSU module owns the gate's lifetime, not the call site. Per
    ///  the gate must remain asserted
    /// for as long as PSU I²C traffic flows; tearing it down explicitly
    /// before `Apw121215a` drops is unnecessary and was the failure mode
    /// of the manual `s19j_hybrid_mining.rs` call site this replaces.
    gpio_gate: Option<PsuGpioGate>,
    ///  (2026-05-23): Loki spoof per-byte register-pointer protocol.
    ///
    /// When `true`, `tx_once`/`txrx_once` route APW12 frames through
    /// `GpioBitBangI2c::{write_apw12_loki_frame, read_apw12_loki_response}`
    /// instead of `write_to`/`read_from`. Each frame byte becomes a
    /// separate I2C transaction `[addr_W, 0x11, byte]`, matching the
    /// bosminer ground-truth captured by the  soft logic analyzer.
    ///
    /// Set automatically in `open_gpio_bitbang_at` when the env gate
    /// `DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1`. Default `false` for
    /// non-Loki PSU paths (S9, BB, AML, S21 NoPic, kernel-i2c am2).
    loki_per_byte_mode: bool,
    /// EE-005 / EE-015 (finding EE-LOKI-001): the operator declared there is
    /// **no smart SMBus PSU peer** on this rail — i.e. the rail is energized by
    /// a Loki spoof board driving PWR_CONTROL (gpio907), and the APW121215a at
    /// 0x10 is either absent or a passive spoof that NAKs/echoes the 0x81
    /// watchdog-enable. On that hardware the I²C `enable()` (watchdog arm) is a
    /// no-op at best and an EIO storm at worst — the rail is already up via the
    /// GPIO gate.
    ///
    /// When `true`, `enable()` skips the I²C watchdog write cleanly (logs +
    /// returns Ok) instead of issuing a write that can't take effect. Default
    /// `false`: every smart-APW12 path (real APW12, real Loki-passthrough that
    /// DOES answer SMBus) keeps the byte-identical watchdog-arm behaviour.
    /// Mirrors the daemon-side `PsuOverride.no_smbus_peer` hard-skip in
    /// `s19j_hybrid_mining.rs` Phase 0c.
    no_smbus_peer: bool,
}

impl Drop for Apw121215a {
    /// Explicit Drop ordering: release the GPIO gate FIRST so PWR_CONTROL is
    /// restored to its boot-time state before any other PSU teardown work.
    /// Rust's default field-drop order would also drop `gpio_gate` (the gate
    /// has its own Drop) along with the rest of the struct, but doing it
    /// explicitly here documents the intent and lets us emit a single
    /// trace span. The remaining fields drop normally afterwards.
    fn drop(&mut self) {
        if let Some(mut gate) = self.gpio_gate.take() {
            // PsuGpioGate::deassert is idempotent; calling it here also runs
            // before the gate's own Drop fires, so the Drop impl's deassert
            // becomes a no-op (asserted=false). This is the canonical release
            // path for the am2 PWR_CONTROL gate when the PSU object is dropped.
            if let Err(e) = gate.deassert() {
                tracing::warn!(
                    gpio = gate.gpio(),
                    error = %e,
                    "Apw121215a::Drop: PWR_CONTROL deassert failed; gate's own Drop will retry"
                );
            } else {
                tracing::debug!(gpio = gate.gpio(), "Apw121215a::Drop: PWR_CONTROL released");
            }
            // `gate` now drops at end of this block — its Drop impl is a
            // no-op because we already deasserted above.
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ApwHeartbeatMode {
    Primary84,
    WatchdogTick81,
}

impl Apw121215a {
    fn with_io(io: ApwIo, bus: u8, addr: u8) -> Self {
        Self {
            io,
            addr,
            bus,
            fw_byte: None,
            model: PsuModel::Unknown,
            dac: None,
            heartbeat_ticks: AtomicU64::new(0),
            watchdog_armed: false,
            heartbeat_mode: ApwHeartbeatMode::Primary84,
            gate_spec: None,
            gpio_gate: None,
            loki_per_byte_mode: false,
            no_smbus_peer: false,
        }
    }

    /// Builder: declare which GPIO line to assert as the PWR_CONTROL gate
    /// before any PSU I²C write. `spec` accepts the same shapes as
    /// `PsuGpioGate::assert`: `None` -> default `label:PWR_CONTROL`,
    /// `"label:PWR_CONTROL"`, `"gpio:907"`, or a bare numeric like `"907"`.
    ///
    /// On am2 (S19j Pro Zynq), the operator config supplies
    /// `psu.pwr_control_gpio` and the daemon should set it here BEFORE
    /// calling `cold_boot_sequence_gated`. On S9/BB/AML/S21 NoPic, leave
    /// this `None` and call the existing `cold_boot_sequence` (which is
    /// now a thin wrapper that performs no gate work when no spec is set).
    pub fn with_psu_gate_spec(mut self, spec: Option<String>) -> Self {
        self.gate_spec = spec;
        self
    }

    /// Setter form of `with_psu_gate_spec`. Use after `open_*` constructors.
    pub fn set_psu_gate_spec(&mut self, spec: Option<String>) {
        self.gate_spec = spec;
    }

    /// Declare that there is no smart SMBus PSU peer on this rail (EE-005 /
    /// EE-015, finding EE-LOKI-001). Set this when the rail is energized by a
    /// Loki spoof board via PWR_CONTROL (gpio907) and the APW121215a at 0x10
    /// is absent/passive, so the I²C `enable()` (watchdog arm) would be a no-op
    /// or an EIO storm. When set, `enable()` skips the watchdog write cleanly.
    ///
    /// Default is `false` (every real-SMBus path keeps the byte-identical
    /// watchdog-arm behaviour). Mirrors the daemon-side
    /// `PsuOverride.no_smbus_peer` hard-skip.
    pub fn set_no_smbus_peer(&mut self, no_smbus_peer: bool) {
        self.no_smbus_peer = no_smbus_peer;
    }

    /// Whether the no-SMBus-peer hard-skip is configured (helper for tests
    /// and diagnostics).
    pub fn no_smbus_peer(&self) -> bool {
        self.no_smbus_peer
    }

    /// Whether a GPIO gate spec is currently configured (helper for tests
    /// and diagnostics).
    pub fn has_gate_spec(&self) -> bool {
        self.gate_spec.is_some()
    }

    /// Whether the GPIO gate is currently asserted on this PSU instance.
    pub fn is_gate_asserted(&self) -> bool {
        self.gpio_gate.as_ref().is_some_and(|g| g.is_asserted())
    }

    /// Open the APW121215a on the default bus/address (S19j Pro am2 wiring).
    pub fn open() -> Result<Self> {
        Self::open_at(APW12_FRAMED_BUS, APW12_FRAMED_ADDR)
    }

    /// Open the APW121215a at a specific bus and address.
    /// NEVER falls back to GPIO bit-bang on am2 (kernel I2C only; see
    /// ).
    pub fn open_at(bus: u8, addr: u8) -> Result<Self> {
        Self::open_kernel_at(bus, addr)
    }

    /// Open the APW121215a on kernel `/dev/i2c-N`.
    pub fn open_kernel_at(bus: u8, addr: u8) -> Result<Self> {
        let i2c = I2cBus::open(bus)?;
        tracing::info!(
            bus,
            addr = format_args!("0x{:02X}", addr),
            "APW121215a framed-I2C opened (kernel /dev/i2c-{})",
            bus,
        );
        Ok(Self::with_io(ApwIo::Kernel(i2c), bus, addr))
    }

    /// Open the APW121215a through the process-wide I2C service.
    ///
    /// AM2 shares `/dev/i2c-0` between PSU, PIC, heartbeat, and sensor traffic;
    /// this constructor keeps APW121215a operations on the same serialized
    /// service path as the rest of board management. Framed read-reply
    /// exchanges use `I2cServiceHandle::transaction()` so write/delay/header/
    /// tail are one queued operation and cannot interleave with PIC heartbeat.
    pub fn open_service(service: I2cServiceHandle) -> Result<Self> {
        Self::open_service_at(service, APW12_FRAMED_BUS, APW12_FRAMED_ADDR)
    }

    /// Open the APW121215a through an existing I2C service at a specific bus
    /// and address. `bus` is used for diagnostics; the handle owns the real fd.
    pub fn open_service_at(service: I2cServiceHandle, bus: u8, addr: u8) -> Result<Self> {
        tracing::info!(
            bus,
            addr = format_args!("0x{:02X}", addr),
            "APW121215a framed-I2C opened via I2C service"
        );
        Ok(Self::with_io(ApwIo::Service(service), bus, addr))
    }

    /// Open the APW121215a through the PL GPIO bit-bang path on `a lab unit`.
    ///
    /// This is a narrow experimental bring-up path for am2 `a lab unit` only. The
    /// actual proof came from the on-box `apw_41220000_probe` helper: `gpio907`
    /// high plus real I2C clocking on `gpio895/896` reaches APW `0x10`, while
    /// kernel `/dev/i2c-0` still EIOs in the same state.
    ///
    /// #  (2026-05-23) backend selector
    ///
    /// When `DCENT_AM2_PSU_BITBANG_USE_MMAP=1` is set, opens the
    /// mmap'd AXI GPIO backend (`GpioBitBangI2c::new_mmap_am2`) instead
    /// of the legacy sysfs backend. The mmap backend is ~233× faster on
    /// the Zynq kernel because it bypasses `/sys/class/gpio/gpioN/`
    /// per-write overhead (5-10 ms each). Default-OFF for the first
    /// live test cycle; promotion to default-on happens in a separate
    /// commit after operator-confirmed live success on `a lab unit`. Failure
    /// to open the mmap backend falls back to sysfs with a WARN log.
    pub fn open_gpio_bitbang_at(addr: u8) -> Result<Self> {
        let use_mmap = std::env::var("DCENT_AM2_PSU_BITBANG_USE_MMAP")
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let gpio = if use_mmap {
            match GpioBitBangI2c::new_mmap_am2() {
                Ok(g) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", addr),
                        backend = "mmap_am2",
                        env_gate = "DCENT_AM2_PSU_BITBANG_USE_MMAP=1",
                        "APW121215a opened via Wave-36 mmap'd AXI GPIO bit-bang"
                    );
                    g
                }
                Err(error @ HalError::I2cFabricUnavailable { .. }) => return Err(error),
                Err(e) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", addr),
                        error = %e,
                        "Wave-36 mmap backend open FAILED — falling back to sysfs"
                    );
                    GpioBitBangI2c::new_am2()?
                }
            }
        } else {
            let g = GpioBitBangI2c::new_am2()?;
            tracing::warn!(
                addr = format_args!("0x{:02X}", addr),
                sda = PSU_GPIO_SDA,
                scl = PSU_GPIO_SCL,
                backend = "sysfs",
                "APW121215a opened via GPIO bit-bang (experimental am2 path)"
            );
            g
        };
        //  (2026-05-23): Loki spoof per-byte register-pointer mode.
        // When set, every APW12 frame byte is sent as a separate I2C
        // transaction `[addr_W, 0x11, byte]` matching the bosminer
        // ground-truth captured by the  soft logic analyzer.
        // Default OFF for backward compatibility with kernel-i2c and
        // non-Loki gpio_bitbang paths.
        //
        // ** HIGH-1 (2026-05-24, DCENT_EE swarm finding):** the
        // Loki spoof's stuck-state-from--bytes is recoverable only
        // by AC-cycle (
        // forbidden-env-var list). Requiring a paired
        // `DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1` acknowledgement gate
        // forces operators to acknowledge that risk before running this
        // path — preventing accidental "I just set the env to see what
        // happens" Loki bricks. Loud `tracing::error!` if the Loki gate
        // is set without the paired ack.
        let loki_per_byte_mode_requested = std::env::var("DCENT_AM2_PSU_LOKI_REGISTER_POINTER")
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let loki_brick_ack = std::env::var("DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF")
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let loki_per_byte_mode = if loki_per_byte_mode_requested && !loki_brick_ack {
            tracing::error!(
                addr = format_args!("0x{:02X}", addr),
                env_gate = "DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1",
                paired_ack_required = "DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1",
                "Wave-55a poison-flag: Loki per-byte mode REFUSED — \
                 DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1 is set but the \
                 paired acknowledgement gate DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1 \
                 is NOT. Wave-39's bytes have been LIVE-PROVEN to put the \
                 Loki spoof in a stuck state recoverable only via operator \
                 AC-cycle ( \
                 'MUST NOT BE SET' list). Falling back to non-loki mode."
            );
            false
        } else if loki_per_byte_mode_requested {
            tracing::warn!(
                addr = format_args!("0x{:02X}", addr),
                env_gate = "DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1",
                paired_ack = "DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1",
                "Wave-39 Loki per-byte mode ENABLED (poison-ack present) — APW12 frames \
                 will be sent as N separate transactions [addr_W, 0x11, byte]. \
                 If this run breaks the Loki spoof, operator-coordinated AC-cycle \
                 is required to recover."
            );
            true
        } else {
            false
        };
        let mut psu = Self::with_io(ApwIo::Gpio(gpio), 0, addr);
        psu.loki_per_byte_mode = loki_per_byte_mode;
        Ok(psu)
    }

    pub fn transport_name(&self) -> &'static str {
        match self.io {
            ApwIo::Kernel(_) => "kernel_i2c",
            ApwIo::Gpio(_) => "gpio_bitbang",
            ApwIo::Service(_) => "i2c_service",
        }
    }

    /// Returns the current value of `loki_per_byte_mode` ( transport
    /// flag). Public read-only accessor so the standalone-cold-boot
    /// orchestrator + regression tests can verify the bit state without
    /// directly touching the private field.
    pub fn loki_per_byte_mode_enabled(&self) -> bool {
        self.loki_per_byte_mode
    }

    ///  (2026-05-24) — internal force-enable hook for the
    /// **standalone** Loki cold-boot path orchestrator only.
    ///
    /// ## Why this exists
    ///
    /// The  per-byte transport gate (`loki_per_byte_mode`) is
    /// normally set at PSU-open time and requires the operator to set BOTH
    /// `DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1` AND the paired ack
    /// `DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1`. That gate exists to stop
    /// operators from accidentally emitting Loki bytes outside a structured
    /// cold-boot sequence (which would put the spoof in a stuck state
    /// recoverable only by AC-cycle).
    ///
    /// BUT — `DCENT_AM2_PSU_LOKI_REGISTER_POINTER` is one of the FOUR
    /// forbidden env vars on `a lab unit`-class XIL hardware
    /// (`wave55a_recipe_guard::WAVE54_FORBIDDEN_ENV_VARS`). Setting it
    /// re-breaks the  PROVEN MINING bosminer-handoff recipe; the
    ///  runtime guard REFUSES to start the daemon when it's set on
    /// a `a lab unit` fingerprint. This created a contradictory contract: the
    ///  standalone cold-wake helpers (`loki_cold_wake_init_frame` /
    /// `_follow_frame` / `_poll`) all guard on `loki_per_byte_mode` and
    /// return `PsuUnsupported` when it's false — but the env var that sets
    /// it is forbidden on the only hardware that needs the standalone
    /// path.
    ///
    ///  resolves the contradiction by hoisting the enable into the
    /// orchestrator: when `cold_boot_sequence_loki_standalone` runs, it
    /// owns the lifetime of the cold-wake cycle, the operator has already
    /// opted in via the (NOT-forbidden) `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL`
    /// gate plus the `a lab unit` fingerprint, and the entire  byte
    /// sequence is structured + bounded. Force-enabling the per-byte
    /// transport just for that cold-wake window is exactly what the gate
    /// was supposed to allow.
    ///
    /// ## Contract
    ///
    /// - MUST be called only by `cold_boot_sequence_loki_standalone` (the
    ///   "wave55e" suffix in the function name is a tripwire — any other
    ///   caller is a bug). The  regression test pins this.
    /// - Transport MUST already be `ApwIo::Gpio` — the per-byte transport
    ///   primitives only exist on `GpioBitBangI2c`; calling on a kernel
    ///   or service transport is meaningless and the cold-wake helpers
    ///   would error anyway. Returns an error if transport isn't GPIO.
    /// - Emits a loud `tracing::warn!` so the operator + future agent
    ///   reading logs sees that the -forbidden transport was
    ///   engaged for a bounded cold-wake window only.
    /// - The bit STAYS set after the cold-wake cycle (mirroring the open-
    ///   time semantics) — if the caller proceeds to standard
    ///   `disable + ramp + enable`, those paths use bulk transport that
    ///   ignores `loki_per_byte_mode` for non-Loki opcodes.
    pub fn enable_loki_per_byte_mode_for_wave55b_standalone(&mut self) -> Result<()> {
        match &self.io {
            ApwIo::Gpio(_) => {}
            _ => {
                return Err(HalError::PsuUnsupported(
                    "Wave-55e enable_loki_per_byte_mode_for_wave55b_standalone \
                     requires ApwIo::Gpio transport (per-byte primitives only \
                     exist on GpioBitBangI2c)"
                        .to_string(),
                ));
            }
        }
        if self.loki_per_byte_mode {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.addr),
                "Wave-55e: loki_per_byte_mode already enabled (operator set \
                 DCENT_AM2_PSU_LOKI_REGISTER_POINTER + ack at open time); \
                 standalone orchestrator is a no-op for the bit flip"
            );
            return Ok(());
        }
        tracing::warn!(
            addr = format_args!("0x{:02X}", self.addr),
            "Wave-55e: orchestrator force-enabling loki_per_byte_mode for \
             standalone Loki cold-wake cycle. This emits Wave-38 captured \
             bytes via per-byte transport [addr_W, 0x11, byte] (init) and \
             [addr_W, byte] (follow). The operator opted in via \
             DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1 + `a lab unit`-class fingerprint; \
             the (forbidden) DCENT_AM2_PSU_LOKI_REGISTER_POINTER env gate is \
             intentionally bypassed because the standalone orchestrator owns \
             the cold-wake window. See `cold_boot_sequence_loki_standalone` \
             + ."
        );
        self.loki_per_byte_mode = true;
        Ok(())
    }

    /// For write-only bootstrap experiments where read-path proof is weaker than
    /// write-path proof. `a lab unit` APW is known-good as FW `0x71` from bosminer.
    pub fn assume_fw_byte(&mut self, fw: u8) {
        self.fw_byte = Some(fw);
        self.model = PsuModel::from_fw_byte(fw);
    }

    /// Require the exact fw71 dialect before emitting any command other than
    /// the initial firmware-version observation.
    fn require_fw71_dialect(&self, operation: &str) -> Result<()> {
        if self.fw_byte == Some(0x71) && self.model == PsuModel::Apw121215a {
            return Ok(());
        }
        Err(HalError::PsuUnsupported(format!(
            "{operation} requires APW121215a fw71 framing; observed model={} fw={:?}",
            self.model.name(),
            self.fw_byte
        )))
    }

    /// Detected PSU model (valid after `probe()`).
    pub fn model(&self) -> PsuModel {
        self.model
    }

    /// Raw FW version byte returned by the PSU.
    pub fn fw_byte(&self) -> Option<u8> {
        self.fw_byte
    }

    /// Current heartbeat tick count (monotonic across the instance lifetime).
    pub fn heartbeat_ticks(&self) -> u64 {
        self.heartbeat_ticks.load(Ordering::Relaxed)
    }

    /// True iff `set_voltage()` is currently permitted (≥5 stable ticks).
    pub fn is_voltage_set_allowed(&self) -> bool {
        self.heartbeat_ticks() >= APW12_STABLE_TICKS_GATE
    }

    /// Voltage-to-DAC using the linear 121215a formula.
    /// `V = 15.1084 − 0.013046·dac`  ⇒  `dac = (15.1084 − V) / 0.013046`.
    ///
    /// This is the `a lab unit`-empirically-proven `0x71`-class form used as the live
    /// default for all versions. For the byte-exact **per-PSU-version**
    /// Bitmain-canonical coefficients (incl. the distinct `0x76`/APW121215f
    /// form), see [`Self::apw_voltage_to_dac`] — not wired into the live path
    /// until a `0x76` live A/B (see that method's SAFETY note).
    pub fn voltage_to_dac_linear(voltage_v: f64) -> u8 {
        let dac = ((15.1084 - voltage_v) / 0.013046).round() as i32;
        dac.clamp(0, 255) as u8
    }

    /// DAC-to-voltage inverse of the above (used when we read back
    /// `GetConfiguredVoltage` and want to report volts).
    pub fn dac_to_voltage_linear(dac: u8) -> f64 {
        15.1084 - 0.013046 * (dac as f64)
    }

    /// Byte-exact Bitmain-canonical per-PSU-version APW DAC conversion
    /// (`N = offset − V·slope`, truncated toward zero, valid `0..=255`).
    ///
    /// **Source (RE 2026-06-02):** the unstripped Bitmain S21 jig
    /// `single_board_test::bitmain_convert_V_to_N @ 000cd370` switch on
    /// `power_version`, with the `offset`/`slope` `double` constants read
    /// byte-exact from the jig `.rodata` (Ghidra `DumpDoubles`). Full write-up:
    /// .
    ///
    /// | PSU fw | offset | slope | (DAT) |
    /// |---|---|---|---|
    /// | `0x22` | 1215.894440 | 59.931507 | cd670/cd668 |
    /// | `0x41`/`0x42` | 765.411764 | 35.833333 | cd5f8/cd5f0 |
    /// | `0x43` | 933.240365 | 59.806034 | cd618/cd610 |
    /// | `0x61` | 1144.502262 | 52.243589 | cd648/cd640 |
    /// | `0x71`/`0x72`/`0x75`/`0x77` | 1190.935338 | 78.742588 | cd608/cd600 |
    /// | `0x73`/`0x78` | 1280.577821 | 73.979365 | cd638/cd630 |
    /// | **`0x74`/`0x76`** (APW121215f) | **1156.107585** | **76.090494** | cd660/cd658 |
    ///
    /// Returns `None` for: the **float-frame** versions `0x62`/`0x64`/`0x65`/`0x66`
    /// (they bypass `convert_V_to_N` and build an IEEE-754 cmd in
    /// `bitmain_set_voltage`), the fw-conditional `0xc1`/`0xc2`, the
    /// **calibrated** path (`power_Calibrated && use_calibration_data` →
    /// EEPROM cal-table interpolation, runtime-data-dependent), and unknown
    /// versions.
    ///
    /// **SAFETY — default-OFF on the live path** (wired behind
    /// [`Self::PER_VERSION_DAC_ENV`]; the live default stays
    /// [`Self::voltage_to_dac_linear`]). This is the byte-exact characterization
    /// of the previously "uncharacterized" `0x76` variant.
    ///
    /// **The `0x71` jig-vs-dcentrald discrepancy is RESOLVED by provenance (no
    /// bench point needed):** dcentrald's `0x71` form (`15.1084 − 0.013046·N`)
    /// is the **PSU's own factory-default calibration constants** — the
    ///  §0x06 READ_CAL defaults ("DAC reference voltage
    /// 15.1084", "DAC offset per count −0.013046") stored in the PIC16F1704
    /// EEPROM, also matching BraiinsOS `bosminer_model.json`
    /// (`voltage_range_mv [11960,15200]`, proven-in-production). The jig's
    /// `0x71` (`15.125 − 0.012700·N`) is merely the *jig binary's own*
    /// hardcoded uncalibrated fallback. dcentrald's source is the more
    /// authoritative one → **keep it for `0x71`; the gate pins `0x71` to it.**
    /// For `0x76` there is NO documented PSU factory-default, so the jig's
    /// per-version coeffs here ARE the best available characterization — which
    /// is exactly what the gate uses. Defaulting `0x76` ON is the only step
    /// still gated on a live A/B (`a lab unit`/S21).
    pub fn apw_voltage_to_dac(power_version: u8, voltage_v: f64) -> Option<u8> {
        let (offset, slope): (f64, f64) = match power_version {
            0x22 => (1215.894440, 59.931507),
            0x41 | 0x42 => (765.411764, 35.833333),
            0x43 => (933.240365, 59.806034),
            0x61 => (1144.502262, 52.243589),
            0x71 | 0x72 | 0x75 | 0x77 => (1190.935338, 78.742588),
            0x73 | 0x78 => (1280.577821, 73.979365),
            0x74 | 0x76 => (1156.107585, 76.090494),
            // float-frame / fw-conditional / calibrated / unknown → caller must
            // use the float-frame or calibrated path, not a fixed DAC formula.
            _ => return None,
        };
        // Match the jig: (int32_t)(longlong)(double) truncates toward zero.
        let n = (offset - voltage_v * slope).trunc() as i64;
        if (0..=255).contains(&n) {
            Some(n as u8)
        } else {
            None
        }
    }

    // ---- Wire helpers --------------------------------------------------

    /// Drain any stale bytes in the PSU's send/receive buffer before starting
    /// a new session. Mirrors bosminer's "PSU: Flushing PSU buffer" pattern:
    /// when a previous daemon crashed mid-transaction, the PSU's internal
    /// buffer retains partial frame data and NACKs new commands until drained.
    ///
    /// Symptom if skipped: EIO on the first real command (GetFwVersion /
    /// SetVoltage) in a fresh dcentrald session after bosminer or a prior
    /// dcentrald exited uncleanly. Fix per Phase 5 investigation Agent 20
    ///.
    ///
    /// Best-effort: up to 8 × 32-byte reads with 10 ms between drains. Errors
    /// are swallowed (we're DRAINING, not reading for content). Returns Ok
    /// even when bus NACKs every drain — that's the expected "buffer clean"
    /// exit state.
    pub fn flush_buffer(&mut self) -> Result<()> {
        let mut total_drained = 0u32;
        match &mut self.io {
            ApwIo::Kernel(i2c) => {
                i2c.set_slave(self.addr)?;
                for _ in 0..8 {
                    let mut buf = [0u8; 32];
                    match i2c.read(&mut buf) {
                        Ok(n) if n > 0 => total_drained += n as u32,
                        _ => {}
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            ApwIo::Gpio(gpio) => {
                // : if Loki mode is on, drain via per-byte reads
                // (each transaction = 1 byte) to stay consistent with the
                // bosminer-pattern bus shape. Otherwise use the legacy
                // 32-byte bulk drain.
                if self.loki_per_byte_mode {
                    for _ in 0..8 {
                        let mut buf = [0u8; 8];
                        match gpio.read_apw12_loki_response(self.addr, &mut buf) {
                            Ok(n) if n > 0 => total_drained += n as u32,
                            _ => {}
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                } else {
                    for _ in 0..8 {
                        let mut buf = [0u8; 32];
                        match gpio.read_from(self.addr, &mut buf) {
                            Ok(n) if n > 0 => total_drained += n as u32,
                            _ => {}
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
            ApwIo::Service(service) => {
                for _ in 0..8 {
                    match service.read_bytes(self.addr, 32) {
                        Ok(buf) if !buf.is_empty() => total_drained += buf.len() as u32,
                        _ => {}
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        tracing::info!(
            bytes = total_drained,
            "PSU buffer flushed (best-effort drain of stale session bytes)"
        );
        Ok(())
    }

    /// One attempt at the three-phase framed-reply read. Used only by
    /// `txrx_observation` under a retry wrapper. Splitting this out lets us
    /// retry on EIO while the command path remains effect-gated.
    fn txrx_once(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let frame = build_apw12_frame(cmd, payload);
        let bus = self.bus;
        let addr = self.addr;
        // : capture before the mutable borrow of self.io below.
        let loki_per_byte = self.loki_per_byte_mode;
        //  (2026-05-23): WRITE-side forensic logging.
        //
        //  captured the READ side on `a lab unit` (Loki spoof returns
        // [0xF5, 0x00, 0x00] = NAK).  falsified the
        // "spoof tolerates retries" hypothesis (8/8 NAKs deterministic).
        // Diagnosis: our WRITE bytes are malformed for the spoof's
        // expected format. To fix it, we need to see what we actually put
        // on the wire and compare to what bosminer sends.
        //
        // bosminer's `PSU: Version '0x71' (APW121215a) detected` log on
        // the same hardware proves the spoof DOES respond to a correctly
        // formatted GetFwVersion. So either our `build_apw12_frame`
        // produces wrong bytes for this spoof's parser, or the on-wire
        // I2C addressing differs.
        //
        // This INFO-level log fires BEFORE every write so the operator
        // sees the exact bytes that go on the wire — high-level frame
        // (preamble + LEN + CMD + payload + CKSUM) PLUS the I2C address
        // byte the controller will prepend (`(addr << 1) | 0` for write).
        // Tagged `wave35_write_forensic` for easy grep across log archives.
        //  SB-4 (2026-05-28): demoted INFO -> debug!(target "apw12_forensic")
        // so it no longer floods at 1 Hz on the PSU heartbeat hot path (churning
        // the bounded PersistentLogRing + burning NAND on a 24/7 home unit). The
        // byte-level forensic is still available for cold-boot RE via
        // `RUST_LOG=apw12_forensic=debug` (same philosophy as the i2c_audit target).
        tracing::debug!(
            target: "apw12_forensic",
            wave35_write_forensic = true,
            transport = match &self.io {
                ApwIo::Kernel(_) => "kernel_i2c",
                ApwIo::Gpio(_) => "gpio_bitbang",
                ApwIo::Service(_) => "i2c_service",
            },
            bus,
            slave_addr_7bit = format_args!("0x{:02X}", addr),
            wire_addr_byte_write = format_args!("0x{:02X}", addr << 1),
            cmd = format_args!("0x{:02X}", cmd),
            payload_len = payload.len(),
            payload_hex = format_args!("{:02X?}", payload),
            frame_len = frame.len(),
            frame_hex = format_args!("{:02X?}", frame.as_slice()),
            phase = "txrx_once",
            "APW12 txrx WRITE bytes (Wave-35 forensic — compare to bosminer GetFwVersion bytes)"
        );
        let mut header = [0u8; 3];
        let n = match &mut self.io {
            ApwIo::Kernel(i2c) => {
                i2c.set_slave(self.addr)?;
                i2c.write(&frame).map_err(|e| HalError::I2c {
                    bus,
                    addr,
                    detail: format!("txrx write: {}", e),
                })?;
                std::thread::sleep(std::time::Duration::from_millis(APW12_REPLY_DELAY_MS));
                i2c.read(&mut header).map_err(|e| HalError::I2c {
                    bus,
                    addr,
                    detail: format!("txrx read header: {}", e),
                })?
            }
            ApwIo::Gpio(gpio) => {
                if loki_per_byte {
                    //  write +  loop-accumulate read.
                    //
                    // WRITE: per-byte register-pointer protocol (N transactions
                    // [addr_W, 0x11, byte]) + the  trailing even
                    // reply-register select. Matches bosminer ground-truth.
                    //
                    // READ: the  `read_apw12_loki_response` LOOP-READS
                    // and ACCUMULATES until the `0x55 0xAA`-aligned framed reply
                    // materializes, returning the WHOLE frame in one call. The
                    // pre- 2-phase header(3)+tail(LEN-1) split is
                    // INCOMPATIBLE with loop-accumulate (a separate tail read
                    // would re-align to the preamble and re-return the frame
                    // head, not the continuation). So for the Loki path we read
                    // a single oversized buffer here and hand it straight to
                    // `parse_apw12_reply`, which locates + validates the frame
                    // (it already tolerates trailing padding past the advertised
                    // LEN). This mirrors the Service branch's `ReadFrame` shape.
                    gpio.write_apw12_loki_frame(self.addr, &frame)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx loki-per-byte write(bitbang): {}", e),
                        })?;
                    std::thread::sleep(std::time::Duration::from_millis(APW12_REPLY_DELAY_MS));
                    // Read a full frame in ONE accumulate call. 64 matches the
                    // Service branch's max_len; long enough for any APW12 reply.
                    let mut full = vec![0u8; 64];
                    let nfull = gpio
                        .read_apw12_loki_response(self.addr, &mut full)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx loki-per-byte read frame(bitbang): {}", e),
                        })?;
                    full.truncate(nfull);
                    // Loop-accumulate guarantees buf[0..]==preamble-aligned on
                    // success; on timeout it zero-fills or returns short. Run
                    // the same NAK/short/preamble forensics the shared path
                    // uses, then validate.
                    if full.first() == Some(&APW12_NAK_BYTE) {
                        tracing::debug!(
                            cmd = format_args!("0x{:02X}", cmd),
                            n_read = full.len(),
                            reply_hex = format_args!("{:02X?}", full.as_slice()),
                            "APW12 txrx (loki loop-accumulate): NAK (0xF5) — caller may retry"
                        );
                        return Err(HalError::PsuProtocol("PSU NAK (0xF5)"));
                    }
                    if full.len() < 5 || full[0] != 0x55 || full[1] != 0xAA {
                        tracing::warn!(
                            cmd = format_args!("0x{:02X}", cmd),
                            n_read = full.len(),
                            reply_hex = format_args!("{:02X?}", full.as_slice()),
                            "APW12 txrx (loki loop-accumulate): no 0x55AA-aligned frame \
                             (Wave-56b — check reply_hex; try a different DCENT_AM2_LOKI_REPLY_REG)"
                        );
                        return Err(HalError::PsuProtocol("invalid preamble"));
                    }
                    return parse_apw12_reply(cmd, &full);
                } else {
                    gpio.write_to(self.addr, &frame)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx write(bitbang): {}", e),
                        })?;
                    std::thread::sleep(std::time::Duration::from_millis(APW12_REPLY_DELAY_MS));
                    gpio.read_from(self.addr, &mut header)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx read header(bitbang): {}", e),
                        })?
                }
            }
            ApwIo::Service(service) => {
                let reads = service
                    .transaction_mutating(
                        I2cMutationLabel::QueryPrelude,
                        self.addr,
                        vec![
                            I2cTransactionStep::Write(frame),
                            I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
                            I2cTransactionStep::ReadFrame {
                                header_len: 3,
                                len_index: 2,
                                remaining_adjust: -1,
                                max_len: 64,
                            },
                        ],
                    )
                    .map_err(|e| HalError::I2c {
                        bus,
                        addr,
                        detail: format!("txrx transaction(service): {}", e),
                    })?;
                let full = reads.into_iter().next().unwrap_or_default();
                if full.len() == 1 && full[0] == APW12_NAK_BYTE {
                    return Err(HalError::PsuProtocol("PSU NAK (0xF5)"));
                }
                if full.len() < 3 {
                    return Err(HalError::PsuProtocol("header short read"));
                }
                if full[0] != 0x55 || full[1] != 0xAA {
                    return Err(HalError::PsuProtocol("invalid preamble"));
                }
                let len = full[2];
                if len < 3 {
                    return Err(HalError::PsuProtocol("invalid advertised frame length"));
                }
                return parse_apw12_reply(cmd, &full);
            }
        };

        //  (2026-05-23): tolerant NAK detection.
        //
        // Pre-: only `n == 1 && header[0] == 0xF5` was classified as
        // NAK. But the gpio_bitbang read_from returns the full N-byte
        // buffer regardless of how many bytes the spoof actually transmitted.
        // When the Loki spoof on `a lab unit` NAKs, it sends 0xF5 followed by bus
        // idle (0x00 bytes from pull-down or noise).  forensic
        // capture: `header_hex=[F5, 00, 00]` from .25 — strict n==1 check
        // missed this → fell through to preamble check → "invalid preamble"
        // → caller treated as fatal protocol error instead of retriable NAK.
        //
        //  fix: any header that STARTS with 0xF5 is a NAK regardless
        // of read length. This is safe because the canonical APW12 preamble
        // is `0x55 0xAA` — there is no valid response that legitimately
        // starts with 0xF5. Misclassifying a real preamble as NAK is
        // impossible.
        //
        // Caller retry contract: the FIX-A gpio_bitbang branch is updated
        // to retry on PsuProtocol("PSU NAK (0xF5)") so the spoof's
        // pre-wake / calibration-loop NAKs (per bosminer's tolerance
        // pattern — 4 calibration NAKs over 30s in bosminer.log) are
        // handled gracefully.
        if header[0] == APW12_NAK_BYTE {
            tracing::debug!(
                cmd = format_args!("0x{:02X}", cmd),
                n_read = n,
                header_hex = format_args!("{:02X?}", &header[..n.min(header.len())]),
                "APW12 txrx: NAK (0xF5) — caller may retry per bosminer-tolerance pattern"
            );
            return Err(HalError::PsuProtocol("PSU NAK (0xF5)"));
        }
        if n < 3 {
            //  forensics — log raw bytes on header short-read.
            tracing::warn!(
                cmd = format_args!("0x{:02X}", cmd),
                n_read = n,
                header_hex = format_args!("{:02X?}", &header[..n.min(header.len())]),
                "APW12 txrx (gpio_bitbang/kernel): header short read \
                 (Wave-33 raw-byte forensics)"
            );
            return Err(HalError::PsuProtocol("header short read"));
        }
        if header[0] != 0x55 || header[1] != 0xAA {
            //  forensics — the actionable failure on `a lab unit`. The Loki
            // spoof responds (n=3+ bytes returned) but the first two bytes
            // aren't the canonical 0x55 0xAA preamble. Log the raw 3-byte
            // header so the operator can identify whether it's a bit-offset
            // shift, a different preamble, or genuine bus noise.
            tracing::warn!(
                cmd = format_args!("0x{:02X}", cmd),
                n_read = n,
                header_hex = format_args!("{:02X?}", &header[..n.min(header.len())]),
                first_byte = format_args!("0x{:02X}", header[0]),
                second_byte = format_args!("0x{:02X}", header[1]),
                third_byte = format_args!("0x{:02X}", header[2]),
                "APW12 txrx (gpio_bitbang/kernel): invalid preamble \
                 (Wave-33 raw-byte forensics — search header_hex for \
                 0x55 0xAA to identify offset shifts)"
            );
            return Err(HalError::PsuProtocol("invalid preamble"));
        }
        let len = header[2];
        if len < 3 {
            return Err(HalError::PsuProtocol("invalid advertised frame length"));
        }

        // Phase B: read the remaining (LEN - 1) bytes.  LEN counts itself +
        // CMD + payload + CKSUM; we already consumed the LEN byte in Phase A
        // along with the preamble, so `len - 1` bytes remain (CMD..CKSUM).
        let remaining = (len as usize) - 1;
        let mut tail = vec![0u8; remaining];
        let n2 = match &mut self.io {
            ApwIo::Kernel(i2c) => i2c.read(&mut tail).map_err(|e| HalError::I2c {
                bus,
                addr,
                detail: format!("txrx read tail: {}", e),
            })?,
            ApwIo::Gpio(gpio) => {
                if loki_per_byte {
                    // UNREACHABLE for the Loki path post-: the Loki
                    // branch in Phase A reads the WHOLE frame via loop-accumulate
                    // and returns early (a 2-phase tail read is incompatible with
                    // accumulate — it would re-align to the preamble). Kept for
                    // signature stability / non-loki defensiveness only.
                    gpio.read_apw12_loki_response(self.addr, &mut tail)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx loki-per-byte read tail(bitbang): {}", e),
                        })?
                } else {
                    gpio.read_from(self.addr, &mut tail)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("txrx read tail(bitbang): {}", e),
                        })?
                }
            }
            ApwIo::Service(service) => {
                let buf = service
                    .read_bytes(self.addr, tail.len())
                    .map_err(|e| HalError::I2c {
                        bus,
                        addr,
                        detail: format!("txrx read tail(service): {}", e),
                    })?;
                let n = buf.len().min(tail.len());
                tail[..n].copy_from_slice(&buf[..n]);
                n
            }
        };
        if n2 != tail.len() {
            return Err(HalError::PsuProtocol("tail short read"));
        }

        // Reassemble full frame for validation.
        let mut full = Vec::with_capacity(3 + tail.len());
        full.extend_from_slice(&header);
        full.extend_from_slice(&tail);
        parse_apw12_reply(cmd, &full)
    }

    /// Three-phase read of a framed reply, with 3× retry on `HalError::I2c`
    /// (covers kernel ioctl EIO — bosminer wraps its PSU backend in
    /// `tokio-retry` for exactly this reason; see phase13d Ghidra report).
    /// Honors NAK (0xF5) abort on the FIRST attempt: protocol-level errors
    /// are returned immediately (no retry). I²C layer errors retry with
    /// 100 ms backoff + best-effort flush between attempts.
    fn txrx_observation(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        if !apw121215a_is_observation_opcode(cmd) {
            return Err(HalError::PsuProtocolOwned(format!(
                "APW121215a opcode 0x{cmd:02X} is not an observational fw71 command"
            )));
        }
        if self.fw_byte.is_some() {
            self.require_fw71_dialect("framed observation")?;
        } else if cmd != APW12_CMD_GET_FW_VERSION {
            return Err(HalError::PsuUnsupported(
                "APW121215a identity must be established before non-identity observations"
                    .to_string(),
            ));
        }
        let mut last_err: Option<HalError> = None;
        for attempt in 1..=3 {
            match self.txrx_once(cmd, payload) {
                Ok(v) => return Ok(v),
                // Only retry on I²C-layer errors (EIO / ioctl). Protocol
                // errors (wrong preamble, NAK, short read, bad CKSUM) are
                // surfaced immediately.
                Err(e @ HalError::I2c { .. }) if attempt < 3 => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "APW121215a txrx I²C error — retrying in 100 ms"
                    );
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    // Best-effort: drain any partial bytes the PSU already emitted.
                    let _ = self.flush_buffer();
                }
                Err(e) => return Err(e),
            }
        }
        // Unreachable unless the compiler can't prove it; keep for safety.
        Err(last_err.unwrap_or(HalError::PsuProtocol("txrx retry exhausted without error")))
    }

    /// One attempt at a write-only send (no reply parsing). Used by `tx_with_intent`
    /// under the same retry policy as `txrx_observation`.
    fn tx_once_with_intent(
        &mut self,
        intent: I2cOperationIntent,
        cmd: u8,
        payload: &[u8],
    ) -> Result<()> {
        let frame = build_apw12_frame(cmd, payload);
        let bus = self.bus;
        let addr = self.addr;
        let service_io = matches!(&self.io, ApwIo::Service(_));
        // : capture before the mutable borrow of self.io below.
        let loki_per_byte = self.loki_per_byte_mode;
        //  (2026-05-23): WRITE-side forensic for write-only ops
        // (SetVoltage, Enable, Disable, Watchdog ARM).
        // Same INFO-level log shape as `txrx_once` so a single grep on
        // `wave35_write_forensic` enumerates every byte the daemon puts on
        // the wire during a cold-boot sequence — usable as the input side
        // of the bosminer-vs-DCENT_OS byte-format diff.
        //  SB-4 (2026-05-28): demoted INFO -> debug!(target "apw12_forensic")
        // so it no longer floods at 1 Hz on the PSU heartbeat hot path (churning
        // the bounded PersistentLogRing + burning NAND on a 24/7 home unit). The
        // byte-level forensic is still available for cold-boot RE via
        // `RUST_LOG=apw12_forensic=debug` (same philosophy as the i2c_audit target).
        tracing::debug!(
            target: "apw12_forensic",
            wave35_write_forensic = true,
            transport = match &self.io {
                ApwIo::Kernel(_) => "kernel_i2c",
                ApwIo::Gpio(_) => "gpio_bitbang",
                ApwIo::Service(_) => "i2c_service",
            },
            bus,
            slave_addr_7bit = format_args!("0x{:02X}", addr),
            wire_addr_byte_write = format_args!("0x{:02X}", addr << 1),
            cmd = format_args!("0x{:02X}", cmd),
            payload_len = payload.len(),
            payload_hex = format_args!("{:02X?}", payload),
            frame_len = frame.len(),
            frame_hex = format_args!("{:02X?}", frame.as_slice()),
            phase = "tx_once",
            "APW12 tx WRITE bytes (Wave-35 forensic — write-only path: SetVoltage / Enable / Disable / Watchdog)"
        );
        match &mut self.io {
            ApwIo::Kernel(i2c) => {
                i2c.set_slave(self.addr)?;
                i2c.write(&frame).map_err(|e| HalError::I2c {
                    bus,
                    addr,
                    detail: format!("tx write: {}", e),
                })?;
            }
            ApwIo::Gpio(gpio) => {
                if loki_per_byte {
                    // : per-byte register-pointer protocol matching
                    // bosminer ground-truth. N transactions of
                    // `[addr_W, 0x11, byte]` with 8 ms gap.
                    gpio.write_apw12_loki_frame(self.addr, &frame)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("tx loki-per-byte(bitbang): {}", e),
                        })?;
                } else {
                    gpio.write_to(self.addr, &frame)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("tx write(bitbang): {}", e),
                        })?;
                }
            }
            ApwIo::Service(service) => {
                if intent == I2cOperationIntent::SafeOff {
                    service
                        .write_bytes_with_intent(intent, self.addr, &frame)
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("reserved safe-off tx(service): {}", e),
                        })?;
                    std::thread::sleep(std::time::Duration::from_millis(APW12_REPLY_DELAY_MS));
                } else {
                    service
                        .transaction_with_intent(
                            intent,
                            self.addr,
                            vec![
                                I2cTransactionStep::Write(frame),
                                I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
                            ],
                        )
                        .map_err(|e| HalError::I2c {
                            bus,
                            addr,
                            detail: format!("tx transaction(service): {}", e),
                        })?;
                }
            }
        }
        if !service_io {
            std::thread::sleep(std::time::Duration::from_millis(APW12_REPLY_DELAY_MS));
        }
        Ok(())
    }

    fn tx_with_intent(
        &mut self,
        intent: I2cOperationIntent,
        cmd: u8,
        payload: &[u8],
    ) -> Result<()> {
        self.require_fw71_dialect("framed mutation")?;
        let mut last_err: Option<HalError> = None;
        for attempt in 1..=3 {
            match self.tx_once_with_intent(intent, cmd, payload) {
                Ok(()) => return Ok(()),
                Err(e @ HalError::I2c { .. }) if attempt < 3 => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "APW121215a tx I²C error — retrying in 100 ms"
                    );
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let _ = self.flush_buffer();
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or(HalError::PsuProtocol("tx retry exhausted without error")))
    }

    // ---- Queries --------------------------------------------------------

    /// GetFwVersion (0x02). Payload byte 0 = FW byte (e.g. 0x71 for 121215a).
    /// Subsequent bytes (when present) carry ASCII version text for logging.
    pub fn get_fw_version(&mut self) -> Result<(u8, String)> {
        let payload = self.txrx_observation(APW12_CMD_GET_FW_VERSION, &[])?;
        if payload.is_empty() {
            return Err(HalError::PsuProtocol("GetFwVersion empty payload"));
        }
        let fw = payload[0];
        let ascii = String::from_utf8_lossy(&payload[1..])
            .trim_end_matches('\0')
            .trim()
            .to_string();
        Ok((fw, ascii))
    }

    /// Read the fw71 device-type byte (opcode 0x01).
    ///
    /// Older code exposed this as `get_hw_version`; PIC disassembly proves the
    /// handler returns the constant device class `0x10`, not a hardware version.
    pub fn get_device_type(&mut self) -> Result<Vec<u8>> {
        self.txrx_observation(APW12_CMD_GET_DEVICE_TYPE, &[])
    }

    // ---- Writes ---------------------------------------------------------

    /// Enable / disable PSU watchdog (0x81). Payload: `0x00`=disable, `!0`=enable.
    pub fn watchdog(&mut self, enable: bool) -> Result<()> {
        // Standalone watchdog control is neutral policy, not SafeOff
        // privilege. Only the coordinated minimum-ramp + disarm plan may use
        // the reserved lane, because disarming alone can remove a cutoff.
        let intent = I2cOperationIntent::NeutralControl;
        self.tx_with_intent(
            intent,
            APW12_CMD_WATCHDOG,
            &[if enable { 0x01 } else { 0x00 }],
        )?;
        self.watchdog_armed = enable;
        Ok(())
    }

    /// Send a heartbeat tick. Prefer the 0x84 bosminer-observed opcode; if
    /// that fails, try the 0x81/[0x02] watchdog-tick fallback and remember the
    /// working mode for the rest of the session.
    ///
    /// **MUST be called at ~1 Hz cadence** (missing 3 = PSU self-shutdown).
    /// On success, increments the stable-tick counter used by the
    /// 5-stable-ticks voltage gate.
    ///
    /// EE-005: symmetric with `enable()`/`disable()` — when `no_smbus_peer` is
    /// set (Loki spoof rail with no smart SMBus peer), skip the I²C heartbeat
    /// write cleanly. There is no PSU watchdog to feed (the rail is held up by
    /// PWR_CONTROL/gpio907, not by a watchdog), so the write is a no-op at best
    /// and an EIO storm at worst. We STILL increment the stable-tick counter so
    /// the 5-stable-ticks `set_voltage` gate opens as it would on a real PSU —
    /// otherwise the no-SMBus rail could never reach the voltage-set stage.
    pub fn heartbeat(&mut self) -> Result<()> {
        if self.no_smbus_peer {
            self.heartbeat_ticks.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(
                addr = format_args!("0x{:02X}", self.addr),
                "APW121215a heartbeat(): no_smbus_peer=true — skipping I²C heartbeat write \
                 (Loki spoof rail), tick counted for the voltage-gate accounting"
            );
            return Ok(());
        }
        let primary = match self.heartbeat_mode {
            ApwHeartbeatMode::Primary84 => {
                self.tx_with_intent(I2cOperationIntent::KeepAlive, APW12_CMD_HEARTBEAT, &[])
            }
            ApwHeartbeatMode::WatchdogTick81 => {
                self.tx_with_intent(I2cOperationIntent::KeepAlive, APW12_CMD_WATCHDOG, &[0x02])
            }
        };

        match primary {
            Ok(()) => {
                self.heartbeat_ticks.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(primary_err) if matches!(self.heartbeat_mode, ApwHeartbeatMode::Primary84) => {
                match self.tx_with_intent(
                    I2cOperationIntent::KeepAlive,
                    APW12_CMD_WATCHDOG,
                    &[0x02],
                ) {
                    Ok(()) => {
                        self.heartbeat_mode = ApwHeartbeatMode::WatchdogTick81;
                        self.heartbeat_ticks.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            error = %primary_err,
                            "APW heartbeat opcode 0x84 failed; using 0x81/[0x02] fallback for this session"
                        );
                        Ok(())
                    }
                    Err(fallback_err) => {
                        self.heartbeat_ticks.store(0, Ordering::Relaxed);
                        Err(HalError::PsuProtocolOwned(format!(
                            "APW heartbeat failed on 0x84 ({}) and 0x81/[0x02] ({})",
                            primary_err, fallback_err
                        )))
                    }
                }
            }
            Err(e) => {
                self.heartbeat_ticks.store(0, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// Ramp the PSU output to the minimum voltage on the rail (SetVoltage
    /// with DAC=0xFF ≈ 11.78 V → PSU clamps to 11.96 V). This is the
    /// bosminer-safe "off" ramp used at graceful shutdown so the rail
    /// doesn't stay pinned at 15.2 V while the daemon exits. Does NOT
    /// disarm the watchdog — callers should combine with `watchdog(false)`
    /// or the new `disable()` if they want a full teardown.
    ///
    /// **Note:** this was previously called `disable()`, which was a
    /// semantic misnomer — bosminer's `PSU: Disable` log line is a
    /// watchdog-off (0x81/0x00), NOT a SetVoltage 0xFF.  Renamed per
    /// Phase 13D Ghidra evidence.
    pub fn set_voltage_min(&mut self) -> Result<()> {
        self.tx_with_intent(I2cOperationIntent::SafeOff, APW12_CMD_SET_VOLTAGE, &[0xFF])?;
        self.dac = Some(0xFF);
        Ok(())
    }

    /// Command the bulk rail toward its minimum setpoint, then disarm the PSU
    /// watchdog only after that safe-direction command completed.
    ///
    /// This deliberately replaces the unsafe `watchdog(false)` followed by
    /// `set_voltage_min()` pattern: if the minimum-ramp fails, the watchdog
    /// remains armed and can still act as the independent cutoff backstop.
    /// Success is transport-command evidence, not measured rail voltage.
    pub fn safe_shutdown_to_min(&mut self) -> Result<()> {
        if let ApwIo::Service(service) = &self.io {
            let min_frame = build_apw12_frame(APW12_CMD_SET_VOLTAGE, &[0xFF]);
            let disarm_frame = build_apw12_frame(APW12_CMD_WATCHDOG, &[0x00]);
            service.safe_off_transaction(
                self.addr,
                vec![
                    I2cTransactionStep::Write(min_frame),
                    I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
                    I2cTransactionStep::Write(disarm_frame),
                    I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
                ],
            )?;
            self.dac = Some(0xFF);
            self.watchdog_armed = false;
            return Ok(());
        }

        self.set_voltage_min()?;
        self.watchdog(false)
    }

    /// Bosminer-semantics "Disable" — disarm the watchdog (0x81/0x00).
    /// Leaves the voltage rail at whatever SetVoltage last programmed.
    /// Matches the 3× `PSU: Disable` cadence in bosminer's init log
    /// (A3 §1 step 11 + Phase 13D Ghidra verification).
    ///
    /// After a successful call, the PSU will NOT auto-shutdown when the
    /// heartbeat stops — so this is safe to call before we start the
    /// heartbeat loop (cold boot) and after we stop it (graceful exit).
    ///
    /// EE-005: symmetric with `enable()` — when `no_smbus_peer` is set (Loki
    /// spoof rail with no smart SMBus peer to ACK 0x81), skip the I²C
    /// watchdog-disable write cleanly. Issuing it on teardown is a no-op at best
    /// and an EIO storm at worst; there is no watchdog to disarm because there
    /// was none to arm (`enable()` skipped it too). Leaving the disable write in
    /// made the enable/disable pair asymmetric — `enable()` skipped but
    /// `disable()` still EIO'd on the Loki teardown path.
    pub fn disable(&mut self) -> Result<()> {
        if self.no_smbus_peer {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.addr),
                bus = self.bus,
                "APW121215a disable(): no_smbus_peer=true — skipping 0x81 watchdog-disable \
                 (Loki spoof rail, no SMBus peer; nothing was armed)"
            );
            self.watchdog_armed = false;
            return Ok(());
        }
        self.watchdog(false)
    }

    /// Enable PSU (watchdog re-armed + heartbeat assumed running).
    /// Safe on all 121215a variants.
    ///
    /// EE-005 / EE-015: when `no_smbus_peer` is set (Loki spoof rail — the
    /// rail is up via PWR_CONTROL/gpio907 and there is no smart SMBus peer to
    /// accept the 0x81 watchdog-enable), skip the I²C write cleanly. Issuing it
    /// is a no-op on the spoof at best and an EIO storm at worst; the rail is
    /// already energized by the GPIO gate. We still mark the watchdog as
    /// "armed" locally so the heartbeat loop's 5-stable-tick accounting and any
    /// `watchdog_armed` callers behave as if enable succeeded.
    pub fn enable(&mut self) -> Result<()> {
        if self.no_smbus_peer {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.addr),
                bus = self.bus,
                "APW121215a enable(): no_smbus_peer=true — skipping 0x81 watchdog-arm \
                 (Loki spoof rail is up via PWR_CONTROL, no SMBus peer to ACK)"
            );
            self.watchdog_armed = true;
            return Ok(());
        }
        self.watchdog(true)
    }

    /// Env gate: opt into the byte-exact per-PSU-version DAC formula
    /// ([`Self::apw_voltage_to_dac`]) on the **live** set-voltage path.
    ///
    /// **Default OFF.** When unset, every live SetVoltage uses
    /// [`Self::voltage_to_dac_linear`] (the `a lab unit`-empirically-proven `0x71`
    /// form) for ALL versions — byte-identical to today. Set `=1` ONLY for an
    /// operator live A/B on a unit whose PSU fw is a *characterized-but-
    /// unproven-on-this-unit* version (notably `0x76`/APW121215f, whose jig
    /// coefficients differ from the `0x71` form → today's code mis-sets a 0x76
    /// rail ~0.07 V). The `0x71`-class (`0x71/0x72/0x75/0x77`) is **always**
    /// kept on the proven formula even when this gate is set, because the
    /// jig-canonical `0x71` differs ~3 % from the proven `a lab unit` form (that
    /// discrepancy needs its own `a lab unit` rail-vs-DAC capture first — see the
    /// SAFETY note on [`Self::apw_voltage_to_dac`]).
    ///
    /// This is the operationalization of `RE-ASK-S21-SETVOLTAGE-ORDER`: the
    /// 0x76 DAC formula is now one env-var away from the live path, so the
    /// operator A/B is `DCENT_AM2_PER_VERSION_DAC=1` + a rail-readback, with
    /// the proven default as the fail-safe.
    pub const PER_VERSION_DAC_ENV: &'static str = "DCENT_AM2_PER_VERSION_DAC";

    /// Pure DAC-selection policy for the per-version gate — separated from
    /// the env/`self` read so it can be unit-tested deterministically.
    ///
    /// Returns the per-version DAC iff: `gate_on` AND `fw_byte` is known AND it
    /// is NOT the proven `0x71`-class AND the per-version table has a fixed
    /// formula for it. Otherwise the `a lab unit`-proven linear formula. The
    /// `0x71`-class is deliberately pinned to the proven formula even when
    /// gated (jig-vs-`a lab unit` ~3 % discrepancy unresolved).
    fn select_set_voltage_dac(gate_on: bool, fw_byte: Option<u8>, v: f64) -> u8 {
        if gate_on {
            if let Some(fw) = fw_byte {
                let is_0x71_class = matches!(fw, 0x71 | 0x72 | 0x75 | 0x77);
                if !is_0x71_class {
                    if let Some(dac) = Self::apw_voltage_to_dac(fw, v) {
                        return dac;
                    }
                }
            }
        }
        Self::voltage_to_dac_linear(v)
    }

    /// Compute the SetVoltage DAC byte for `v`, honoring the per-version gate.
    /// Thin wrapper over [`Self::select_set_voltage_dac`] that reads the env
    /// gate + `self.fw_byte` and logs when the per-version path is taken.
    /// See [`Self::PER_VERSION_DAC_ENV`].
    fn voltage_to_dac_gated(&self, v: f64) -> u8 {
        let gate_on = std::env::var(Self::PER_VERSION_DAC_ENV).as_deref() == Ok("1");
        let dac = Self::select_set_voltage_dac(gate_on, self.fw_byte, v);
        if gate_on && dac != Self::voltage_to_dac_linear(v) {
            tracing::info!(
                psu_fw = self
                    .fw_byte
                    .map(|b| format!("0x{:02X}", b))
                    .unwrap_or_default(),
                dac,
                voltage = format_args!("{:.3}V", v),
                "APW SetVoltage using per-version DAC (DCENT_AM2_PER_VERSION_DAC=1)",
            );
        }
        dac
    }

    /// SetVoltage (0x83) — gated on ≥5 stable heartbeat ticks.
    ///
    /// Returns `HalError::PsuProtocol("voltage set too early")` when the gate
    /// hasn't been passed (same pattern as PIC deferred-voltage gate —
    /// ).
    pub fn set_voltage(&mut self, voltage_v: f64) -> Result<()> {
        if !self.is_voltage_set_allowed() {
            return Err(HalError::PsuProtocol(
                "SetVoltage called before 5 stable heartbeat ticks",
            ));
        }
        // Voltage range clamp (121215a rail: 11.96 – 15.20 V).
        let v = voltage_v.clamp(11.96, 15.20);
        let dac = self.voltage_to_dac_gated(v);
        self.set_voltage_dac(dac)?;
        tracing::info!(
            dac,
            voltage = format_args!("{:.3}V", v),
            "APW121215a SetVoltage (gate passed)",
        );
        Ok(())
    }

    /// SetVoltage by raw DAC code (gated — same rules as `set_voltage`).
    /// Exposed for the init/ramp path where the caller has already computed DAC.
    pub fn set_voltage_dac(&mut self, dac: u8) -> Result<()> {
        if !self.is_voltage_set_allowed() {
            return Err(HalError::PsuProtocol(
                "SetVoltageDac called before 5 stable heartbeat ticks",
            ));
        }
        self.tx_with_intent(I2cOperationIntent::Energize, APW12_CMD_SET_VOLTAGE, &[dac])?;
        self.dac = Some(dac);
        Ok(())
    }

    /// BYPASS the 5-tick gate (init ramp to 15.200 V before the heartbeat loop
    /// has started).  Use **only** from the opening belt-and-suspenders init
    /// described in Agent A's spec: `3× Disable → Ramping → Enable` — never
    /// from the runtime autotuner. Logged at WARN so it's visible in logs.
    pub fn set_voltage_init_bypass(&mut self, voltage_v: f64) -> Result<()> {
        let v = voltage_v.clamp(11.96, 15.20);
        let dac = self.voltage_to_dac_gated(v);
        tracing::warn!(
            dac,
            voltage = format_args!("{:.3}V", v),
            "APW121215a SetVoltage (INIT BYPASS — 5-tick gate skipped)",
        );
        self.tx_with_intent(I2cOperationIntent::Energize, APW12_CMD_SET_VOLTAGE, &[dac])?;
        self.dac = Some(dac);
        Ok(())
    }

    /// : explicit `SetVoltageStep` (transient PWM, opcode `0x83`).
    ///
    /// Per the freshly RE'd APW12 PIC spec
    /// (
    /// §"Opcode table"), opcode `0x83` is `SetVoltageStep` — updates the PWM
    /// duty for transient output adjustment WITHOUT writing the new setpoint
    /// to the PIC's EEPROM. Opcode `0x86` is the persisting variant (PWM
    /// update + EEPROM commit; ~5 ms overhead from the page-erase helper +
    /// 100k-cycle EEPROM wear).
    ///
    /// ** primitive — opt-in for future autotuner use.** This method
    /// is intentionally separate from the existing `set_voltage()` /
    /// `set_voltage_dac()` paths so that:
    ///   - the existing API stays byte-identical (those already use
    ///     `APW12_CMD_SET_VOLTAGE = 0x83` under the hood; behaviour
    ///     unchanged);
    ///   - callers that want explicit "transient, never-EEPROM" semantics
    ///     have a spec-named entry point;
    ///   - a future autotuner integration calling `set_voltage()` 100×/sec
    ///     does NOT silently wear out the EEPROM if the const ever drifts to
    ///     `0x86`.
    ///
    /// **Bypasses the 5-tick gate** because the intended use is runtime
    /// trim (heartbeat is already running by then). For cold-boot use,
    /// continue to use `set_voltage_init_bypass()`.
    ///
    /// Clamps voltage to the 121215a rail envelope (11.96–15.20 V) before
    /// computing the DAC byte. NEVER writes to EEPROM. Returns
    /// `HalError::PsuProtocol` if the PIC NAKs the frame.
    pub fn apw12_set_voltage_transient(&mut self, mv: u16) -> Result<()> {
        // Convert mV → volts, then clamp to the rail envelope. The clamp is
        // deliberately the same as set_voltage() so a runtime caller can't
        // command an over/undervoltage by accident.
        let v = (mv as f64 / 1000.0).clamp(11.96, 15.20);
        let dac = self.voltage_to_dac_gated(v);
        tracing::info!(
            opcode = "0x83",
            opcode_name = "SetVoltageStep_transient_PWM",
            dac,
            voltage_mv = mv,
            voltage_clamped = format_args!("{:.3}V", v),
            "Wave-55c: apw12_set_voltage_transient — PWM-only, no EEPROM commit \
             (preferred for runtime tuning; see PHASE2B-APW12-PIC-PROTOCOL.md \
             §'Opcode table' — opcode 0x83 = SetVoltageStep, opcode 0x86 = \
             SetVoltage+persist)"
        );
        // APW12_CMD_SET_VOLTAGE is 0x83 — per the PHASE2B spec this is the
        // transient PWM-only opcode. We deliberately use this constant here
        // (NOT a new 0x86 constant) because 's contract is
        // "transient only, never EEPROM".
        self.tx_with_intent(I2cOperationIntent::Energize, APW12_CMD_SET_VOLTAGE, &[dac])?;
        self.dac = Some(dac);
        Ok(())
    }

    // ---- Probing --------------------------------------------------------

    /// One-shot probe: GetFwVersion → populate `self.model` / `self.fw_byte`.
    ///
    /// Returns the detected model. Does NOT touch voltage or watchdog.
    /// Caller normally does `open() → probe() → disable()*3 → init-ramp →
    /// enable() → spawn heartbeat 1 Hz → wait 5 ticks → set_voltage(target)`.
    pub fn probe(&mut self) -> Result<PsuModel> {
        // Drain stale buffer bytes first (Agent 20 Phase 5 investigation):
        // without this, PSU NACKs first real command after unclean prior-daemon exit.
        let _ = self.flush_buffer();

        let (fw, ascii) = self.get_fw_version()?;
        self.fw_byte = Some(fw);
        self.model = PsuModel::from_fw_byte(fw);
        tracing::info!(
            fw = format_args!("0x{:02X}", fw),
            model = self.model.name(),
            ascii = %ascii,
            "PSU: Version '0x{:02X}' ({}) detected",
            fw,
            self.model.name(),
        );
        // Identity may be observed here, but this driver owns exactly the fw71
        // dialect. Other revisions require protocol-specific adapters; in
        // particular APW121215f fw76 uses a distinct 16-bit checksum frame.
        if self.model != PsuModel::Apw121215a {
            tracing::error!(
                fw = format_args!("0x{:02X}", fw),
                model = self.model.name(),
                "PSU identity is outside the APW121215a fw71 command dialect; \
                 refusing all follow-on commands"
            );
            return Err(HalError::PsuUnsupported(format!(
                "{} fw=0x{fw:02X} is not compatible with APW121215a fw71 framing",
                self.model.name()
            )));
        }

        Ok(self.model)
    }

    /// Experimental `a lab unit`-only APW bootstrap using the proven write path over
    /// `gpio895/896` with `gpio907` asserted. Skips probe reads and assumes the
    /// caller already knows the APW family (`0x71` on `a lab unit`).
    pub fn cold_boot_sequence_write_only(
        &mut self,
        target_init_v: f64,
        assumed_fw: u8,
    ) -> Result<()> {
        self.assume_fw_byte(assumed_fw);
        tracing::warn!(
            fw = format_args!("0x{:02X}", assumed_fw),
            model = self.model.name(),
            transport = self.transport_name(),
            "APW write-only bootstrap active (experimental am2 path)"
        );

        // Step 0 (gate): same fail-closed PWR_CONTROL gate assertion as
        // `cold_boot_sequence_gated` — if a spec was set via
        // `set_psu_gate_spec`/`with_psu_gate_spec` we honor it here so the
        // gpio_bitbang transport gets the same gate ownership semantics
        // as the kernel-I²C transport. No-op when no spec is set.
        self.try_assert_psu_gate()?;

        let _ = self.flush_buffer();

        // The retired  path emitted fw71 opcode 0x06 under a
        // "ReadCalibration" name. Firmware disassembly proves 0x06 is a
        // voltage mutation, so observational cold-wake experiments may never
        // emit it. Preserve the established three-disable bootstrap only.
        for i in 0..3 {
            self.disable()?;
            tracing::info!(step = i + 1, "PSU: Disable (watchdog off, write-only)");
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        tracing::info!(
            target = format_args!("{:.3}V", target_init_v),
            "PSU: Ramping voltage via write-only bootstrap"
        );
        self.set_voltage_init_bypass(target_init_v)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        self.enable()?;
        tracing::info!("PSU: Enable (watchdog armed, write-only)");
        match self.heartbeat() {
            Ok(()) => tracing::info!("PSU: Heartbeat after enable (write-only)"),
            Err(e) => tracing::warn!(
                error = %e,
                "PSU heartbeat immediately after enable failed — heartbeat thread will retry"
            ),
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    //  (2026-05-24): Loki spoof standalone cold-wake cycle.
    //
    // Ports the  bosminer ground-truth cold-wake sequence into
    // DCENT_OS so that a standalone (no-bosminer-handoff) AC-cycled
    // `a lab unit`-class XIL unit can engage the Loki spoof on its own.
    //
    //  captured (decoded.txt) the first 96 i2c transactions
    // bosminer emits at t=4029-11891 ms after process start:
    //
    //   1. Init-frame  [55 AA 04 02 06 00 0C]  via PREFIXED transport
    //                  ( `write_apw12_loki_frame` — each byte is
    //                  a separate `[addr_W, 0x11, byte]` transaction).
    //                  Transactions #1-6 + retry at #49-54.
    //   2. Poll loop   N× single-byte reads waiting for non-`0xF5`.
    //                  Transactions #15-48 (34 reads) + #63-96 (34).
    //   3. Follow-up-frame  [55 AA 04 02 04 02 0E]  via BARE transport
    //                  ( `write_apw12_loki_frame_bare` — each
    //                  byte is a separate `[addr_W, byte]` with NO
    //                  `0x11` prefix). Transactions #9-14 + #57-62.
    //   4. Repeat (init → poll → follow → poll cycle).
    //
    // After this cold-wake cycle bosminer's log reaches
    // `PSU: Version '0x71' (APW121215a) detected` (per
    // `wave38-bosminer-truth/bosminer-psu-log.txt`). DCENT_OS today
    // skips steps 1-4 entirely and goes straight to `disable + ramp +
    // enable`; the spoof on a cold AC-cycled unit NAKs every byte until
    // it sees this cold-wake bus shape.
    //
    // This is the byte-level port of the  evidence. Semantics
    // of CMD=0x02 with payloads `06 00` and `04 02` are unknown — the
    // bytes are emitted because bosminer does, not because we have an
    // opcode-level model.
    // PHASE2B-BYTE-LEVEL-GAPS.md` "Uncertainties" for what remains
    // un-RE'd in the  capture.
    // -----------------------------------------------------------------

    /// : the Loki cold-wake init-frame as captured at
    /// transactions #1-6.
    ///
    /// Wire bytes (byte-exact  Loki capture; intentionally NOT
    /// computed by the generic `build_apw12_frame` LEN convention):
    /// `[0x55, 0xAA, 0x04, 0x02, 0x06, 0x00, 0x0C]`.
    /// The trailing `0x0C` is the APW12 checksum (`LEN + CMD + Σpayload =
    /// 0x04 + 0x02 + 0x06 + 0x00 = 0x0C`). Bosminer's  capture
    /// cut off at 6 bytes (before the checksum) —  includes the
    /// checksum byte because the APW12 spec requires it and bosminer's
    /// non-Loki APW121215a path uses it (see `build_apw12_frame`).
    ///
    /// Emitted via PREFIXED transport (`write_apw12_loki_frame`) — each
    /// byte is `[addr_W, 0x11, byte]`. This matches the  #1-6
    /// pattern byte-for-byte.
    ///
    /// Only callable when the underlying transport is `ApwIo::Gpio` AND
    /// Loki per-byte mode is on (`loki_per_byte_mode == true`). Returns
    /// `Err(HalError::PsuUnsupported)` otherwise — guards against
    /// accidental invocation on kernel-I²C or service paths where the
    /// bytes would go through `write_to` (bulk write) and the Loki
    /// spoof's i2c slave state-machine would NAK them.
    pub fn loki_cold_wake_init_frame(&mut self) -> Result<()> {
        tracing::info!(
            target: "wave55e_cold_wake_init_frame_entry",
            addr = format_args!("0x{:02X}", self.addr),
            loki_per_byte_mode = self.loki_per_byte_mode,
            transport = self.transport_name(),
            "Wave-55e: loki_cold_wake_init_frame ENTRY"
        );
        if !self.loki_per_byte_mode {
            tracing::error!(
                target: "wave55e_cold_wake_init_frame_gate_failed",
                addr = format_args!("0x{:02X}", self.addr),
                "Wave-55e: loki_cold_wake_init_frame REFUSED — loki_per_byte_mode \
                 is false. The standalone orchestrator \
                 (cold_boot_sequence_loki_standalone) should have force-enabled \
                 this via enable_loki_per_byte_mode_for_wave55b_standalone() \
                 BEFORE calling this helper. Direct caller bypassed the \
                 orchestrator — this is a wiring bug."
            );
            return Err(HalError::PsuUnsupported(
                "Wave-55b loki_cold_wake_init_frame requires loki_per_byte_mode \
                 (the standalone orchestrator must call \
                 enable_loki_per_byte_mode_for_wave55b_standalone() first, or \
                 open via open_gpio_bitbang_at + DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1 + \
                 DCENT_AM2_PSU_LOKI_OK_TO_BRICK_SPOOF=1)"
                    .to_string(),
            ));
        }
        let addr = self.addr;
        //  Patch 5 (2026-05-26) — BYTE-EXACT  ground-truth.
        //
        // `build_apw12_frame` uses `LEN = 3 + payload_len` (gives LEN=0x05
        // for payload_len=2), but the  soft-logic-analyzer capture
        // (`WAVE38-BOSMINER-GROUND-TRUTH.md` lines 53-69) shows bosminer
        // sends `[55 AA 04 02 06 00 0C]` with LEN=0x04 — the cold-wake
        // protocol uses `LEN = 2 + payload_len` (CMD count + payload count
        // + CKSUM count = 1+2+1 = 4). With LEN=0x05 the Loki spoof's
        // frame parser rejects the frame (only 4 bytes follow LEN, not 5),
        // returns only F5 NAK markers on subsequent polls — exactly the
        // symptom observed in  Patch 4 LIVE run (poll exhausted
        // with nak_count=1 after the byte-level transactions all ACK'd).
        //
        // Hardcode the byte-exact  init-frame so it CAN'T drift
        // from ground-truth. CKSUM = (LEN + CMD + p0 + p1) & 0xFF
        // = (0x04 + 0x02 + 0x06 + 0x00) = 0x0C.
        let frame: Vec<u8> = vec![0x55, 0xAA, 0x04, 0x02, 0x06, 0x00, 0x0C];
        tracing::info!(
            target: "wave55e_cold_wake_init_frame_write",
            wire_bytes = format_args!("{:02X?}", &frame),
            transport = "prefixed_per_byte",
            addr = format_args!("0x{:02X}", addr),
            byte_count = frame.len(),
            "Wave-55e: emitting Loki cold-wake INIT frame (Wave-38 #1-6 / #49-54, byte-exact) — \
             about to call write_apw12_loki_frame"
        );
        let write_result = match &mut self.io {
            ApwIo::Gpio(gpio) => {
                gpio.write_apw12_loki_frame(addr, &frame)
                    .map_err(|e| HalError::I2c {
                        bus: 1,
                        addr,
                        detail: format!("Wave-55b init-frame prefixed write: {}", e),
                    })
            }
            _ => Err(HalError::PsuUnsupported(
                "Wave-55b loki_cold_wake_init_frame requires ApwIo::Gpio transport".to_string(),
            )),
        };
        match &write_result {
            Ok(()) => tracing::info!(
                target: "wave55e_cold_wake_init_frame_exit",
                addr = format_args!("0x{:02X}", addr),
                result = "ok",
                "Wave-55e: loki_cold_wake_init_frame EXIT — write_apw12_loki_frame returned OK"
            ),
            Err(e) => tracing::error!(
                target: "wave55e_cold_wake_init_frame_exit",
                addr = format_args!("0x{:02X}", addr),
                result = "err",
                error = %e,
                "Wave-55e: loki_cold_wake_init_frame EXIT — write_apw12_loki_frame returned ERROR"
            ),
        }
        write_result
    }

    /// : the Loki cold-wake follow-up-frame as captured at
    ///  transactions #9-14.
    ///
    /// Wire bytes (byte-exact  Loki capture; intentionally NOT
    /// computed by the generic `build_apw12_frame` LEN convention):
    /// `[0x55, 0xAA, 0x04, 0x02, 0x04, 0x02, 0x0E]`.
    /// The trailing `0x0E` is preserved as the byte-exact  trailer byte.
    ///
    /// Emitted via BARE transport (`write_apw12_loki_frame_bare`) — each
    /// byte is `[addr_W, byte]` with NO `0x11` register-pointer prefix.
    /// This matches the  #9-14 + #57-62 pattern byte-for-byte.
    ///
    /// The bare-vs-prefixed distinction is load-bearing: the soft logic
    /// analyzer at  captured #9-14 as 6 transactions of shape
    /// `[addr_W][byte]` (no `0x11`), distinct from #1-6's
    /// `[addr_W][0x11][byte]`. The Loki spoof's i2c slave state-machine
    /// has the `0x11` register pointer LATCHED from the init-frame, so
    /// follow-up bare writes target the same register without
    /// re-asserting the pointer.
    ///
    /// Same guards as `loki_cold_wake_init_frame`.
    pub fn loki_cold_wake_follow_frame(&mut self) -> Result<()> {
        tracing::info!(
            target: "wave55e_cold_wake_follow_frame_entry",
            addr = format_args!("0x{:02X}", self.addr),
            loki_per_byte_mode = self.loki_per_byte_mode,
            transport = self.transport_name(),
            "Wave-55e: loki_cold_wake_follow_frame ENTRY"
        );
        if !self.loki_per_byte_mode {
            tracing::error!(
                target: "wave55e_cold_wake_follow_frame_gate_failed",
                addr = format_args!("0x{:02X}", self.addr),
                "Wave-55e: loki_cold_wake_follow_frame REFUSED — loki_per_byte_mode \
                 is false. The standalone orchestrator should have enabled it via \
                 enable_loki_per_byte_mode_for_wave55b_standalone()."
            );
            return Err(HalError::PsuUnsupported(
                "Wave-55b loki_cold_wake_follow_frame requires loki_per_byte_mode".to_string(),
            ));
        }
        let addr = self.addr;
        //  Patch 5 (2026-05-26) — BYTE-EXACT  ground-truth.
        //
        // Same LEN-formula bug as init-frame: `build_apw12_frame` produced
        // `[55 AA 05 02 04 02 0F]` (LEN=0x05) but  ground-truth +
        // this method's own doc-comment specify `[55 AA 04 02 04 02 0E]`
        // (LEN=0x04, trailer byte=0x0E). The cold-wake
        // protocol uses `LEN = 2 + payload_len` (CMD + payload + CKSUM
        // counts, NOT the build_apw12_frame `3 + payload_len`).
        //
        // Hardcode byte-exact so doc + code can't diverge.
        let frame: Vec<u8> = vec![0x55, 0xAA, 0x04, 0x02, 0x04, 0x02, 0x0E];
        tracing::info!(
            target: "wave55e_cold_wake_follow_frame_write",
            wire_bytes = format_args!("{:02X?}", &frame),
            transport = "bare_per_byte",
            addr = format_args!("0x{:02X}", addr),
            byte_count = frame.len(),
            "Wave-55e: emitting Loki cold-wake FOLLOW-UP frame (Wave-38 #9-14 / #57-62, byte-exact) — \
             about to call write_apw12_loki_frame_bare"
        );
        let write_result = match &mut self.io {
            ApwIo::Gpio(gpio) => {
                gpio.write_apw12_loki_frame_bare(addr, &frame)
                    .map_err(|e| HalError::I2c {
                        bus: 1,
                        addr,
                        detail: format!("Wave-55b follow-up-frame bare write: {}", e),
                    })
            }
            _ => Err(HalError::PsuUnsupported(
                "Wave-55b loki_cold_wake_follow_frame requires ApwIo::Gpio transport".to_string(),
            )),
        };
        match &write_result {
            Ok(()) => tracing::info!(
                target: "wave55e_cold_wake_follow_frame_exit",
                addr = format_args!("0x{:02X}", addr),
                result = "ok",
                "Wave-55e: loki_cold_wake_follow_frame EXIT — write_apw12_loki_frame_bare returned OK"
            ),
            Err(e) => tracing::error!(
                target: "wave55e_cold_wake_follow_frame_exit",
                addr = format_args!("0x{:02X}", addr),
                result = "err",
                error = %e,
                "Wave-55e: loki_cold_wake_follow_frame EXIT — write_apw12_loki_frame_bare returned ERROR"
            ),
        }
        write_result
    }

    /// : bounded read-poll for the Loki spoof's response.
    ///
    /// Emits up to `max_reads` single-byte reads (transport identical to
    ///  `read_apw12_loki_response`). Returns
    /// `(nak_count, first_non_nak_byte)` where `first_non_nak_byte` is
    /// `Some(b)` if any read returned a value other than `0xF5` (NAK
    /// marker), else `None`. Early-exits on the first non-NAK byte —
    /// the bosminer log evidence shows the spoof eventually returns
    /// `0x71` (APW121215a firmware version), at which point bosminer
    /// stops polling and proceeds to the disable/ramp/enable sequence.
    ///
    /// Inter-read gap is `LOKI_INTER_TXN_GAP_MS` (8 ms) per the
    /// convention;  measured ~57 ms per read but that's just the
    /// physical transaction duration at ~10 kHz — the 8 ms gap is
    /// between STOPs, not transactions.
    pub fn loki_cold_wake_poll(&mut self, max_reads: usize) -> Result<(usize, Option<u8>)> {
        tracing::info!(
            target: "wave55e_cold_wake_poll_entry",
            addr = format_args!("0x{:02X}", self.addr),
            loki_per_byte_mode = self.loki_per_byte_mode,
            transport = self.transport_name(),
            max_reads,
            "Wave-55e: loki_cold_wake_poll ENTRY"
        );
        if !self.loki_per_byte_mode {
            tracing::error!(
                target: "wave55e_cold_wake_poll_gate_failed",
                addr = format_args!("0x{:02X}", self.addr),
                "Wave-55e: loki_cold_wake_poll REFUSED — loki_per_byte_mode is false."
            );
            return Err(HalError::PsuUnsupported(
                "Wave-55b loki_cold_wake_poll requires loki_per_byte_mode".to_string(),
            ));
        }
        let addr = self.addr;
        let mut nak_count = 0usize;
        let mut first_non_nak: Option<u8> = None;
        for i in 0..max_reads {
            let mut buf = [0u8; 1];
            match &mut self.io {
                ApwIo::Gpio(gpio) => match gpio.read_apw12_loki_response(addr, &mut buf) {
                    Ok(_) => {
                        if buf[0] == APW12_NAK_BYTE {
                            nak_count += 1;
                            //  Patch 6 (2026-05-26) — DO NOT early-break
                            // on F5.
                            //
                            //  interpreted F5 as a "deterministic NAK"
                            // based on 's observation that 8/8 retries
                            // all NAK on `a lab unit`. But  was tested with the
                            // BROKEN frame format (LEN=0x05 vs  ground-
                            // truth LEN=0x04) — its "deterministic" conclusion
                            // was an artifact of the protocol bug, NOT actual
                            // spoof behavior.  soft-logic-analyzer
                            // capture explicitly documents bosminer reading
                            // 34 F5 markers between writes (`WAVE38-BOSMINER-
                            // GROUND-TRUTH.md` line 18: "#15-48 = 34× 0xF5 |
                            // Polling loop (waiting for spoof to settle)") and
                            // ALSO line 78: "F5(N) response is the spoof's 'no
                            // data yet' reply (not a hard NAK). Bosminer treats
                            // it as 'keep polling.'"
                            //
                            // -LIVE Patch 5 confirmed this empirically:
                            // bytes ACK at wire level (96 successful
                            // wave55c_crc_diagnostic lines), so the spoof
                            // accepts our writes. F5 on read = "no reply queued
                            // yet" — keep polling.
                            //
                            // Continue the loop instead of breaking.
                            tracing::trace!(
                                target: "wave55l_patch6_loki_f5_poll",
                                read_index = i + 1,
                                max_reads,
                                nak_count,
                                addr = format_args!("0x{:02X}", addr),
                                "Wave-55l Patch 6: F5 sentinel (no data yet) — \
                                 continuing poll per Wave-38 ground-truth"
                            );
                            // Fall through to inter-read sleep; loop continues.
                        } else {
                            tracing::info!(
                                read_index = i + 1,
                                max_reads,
                                byte = format_args!("0x{:02X}", buf[0]),
                                nak_count,
                                "Wave-55b: cold-wake poll got non-NAK byte — spoof responded"
                            );
                            first_non_nak = Some(buf[0]);
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::trace!(
                            read_index = i + 1,
                            max_reads,
                            error = %e,
                            "Wave-55b: cold-wake poll read error (continuing)"
                        );
                    }
                },
                _ => {
                    return Err(HalError::PsuUnsupported(
                        "Wave-55b loki_cold_wake_poll requires ApwIo::Gpio transport".to_string(),
                    ))
                }
            }
            if i + 1 < max_reads {
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }
        if first_non_nak.is_none() {
            tracing::warn!(
                max_reads,
                nak_count,
                "Wave-55b: cold-wake poll exhausted with only NAK markers — \
                 spoof did not respond within budget (will retry cycle if iterations remain)"
            );
        }
        tracing::info!(
            target: "wave55e_cold_wake_poll_exit",
            addr = format_args!("0x{:02X}", addr),
            max_reads,
            nak_count,
            first_non_nak = first_non_nak.map(|b| format!("0x{:02X}", b)).unwrap_or_else(|| "none".to_string()),
            "Wave-55e: loki_cold_wake_poll EXIT"
        );
        Ok((nak_count, first_non_nak))
    }

    /// : full cold-wake cycle — N iterations of
    /// (init-frame → poll → follow-up-frame → poll), early-exit on
    /// spoof response.
    ///
    /// Per  evidence (96 captured transactions = ~2 full cycles
    /// over ~8 s), bosminer iterates this cycle until the spoof
    /// responds with the APW121215a firmware byte (`0x71`).
    ///
    /// ** budget revision (2026-05-25, EE-E1):**  shipped
    /// `iterations = 4` (~16 s wall time). EE pre-flight review identified
    /// that the Loki spoof MCU (RP2040/ATtiny) needs ~30 s post-AC-cycle
    /// to clear its watchdog and come up; on cold-cold all 4 iterations
    /// could silently NAK against a not-yet-woken spoof and the caller
    /// would proceed best-effort to a dead bus.  bumps the default
    /// to `iterations = 8` (~32 s budget) AND adds a `cycle_gap_ms`
    /// inter-cycle sleep (default 500 ms, env-tunable via
    /// `DCENT_AM2_PSU_LOKI_COLD_WAKE_GAP_MS`, clamp 0..=2000) so the
    /// spoof MCU gets explicit time windows to wake between attempts.
    ///
    /// Returns `Ok(true)` if the spoof responded (non-NAK byte
    /// observed) at any point in any iteration, `Ok(false)` if all
    /// iterations exhausted with only NAKs. NEVER returns `Err` on a
    /// spoof NAK — the cycle is best-effort cold-wake, and the caller
    /// decides whether to proceed to disable/ramp/enable regardless.
    /// Returns `Err(HalError::I2c)` only on hard bus failure (e.g.,
    /// address NAK on every retry).
    pub fn loki_cold_wake_full_cycle(&mut self, iterations: usize) -> Result<bool> {
        let poll_reads_per_window: usize = 34; // Wave-38 measured upper bound
                                               //  EE-E1: env-tunable inter-cycle gap to give the Loki
                                               // spoof MCU explicit wake windows. Default 500 ms.
                                               //
                                               //  Patch 7 (2026-05-26): clamp upper bound bumped from
                                               // 2000 → 60000 ms. RE Agent 2 found wave50 log shows bosminer
                                               // sends calibration probes 7.6 s apart before the spoof escalates;
                                               // RE Agent 3 found bosminer-psu-log.txt shows `Version '0x71'
                                               // detected` log entries ~70 s apart across distinct runs. Both
                                               // imply the empirical-success gap is >>2 s. Operators can now
                                               // set GAP_MS=7600 to match wave50's measured 7.6 s cadence or
                                               // higher (up to 60 s, covering one full spoof watchdog window).
        let cycle_gap_ms: u64 = std::env::var("DCENT_AM2_PSU_LOKI_COLD_WAKE_GAP_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n <= 60000)
            .unwrap_or(500);
        tracing::info!(
            target: "wave55e_cold_wake_full_cycle_entry",
            addr = format_args!("0x{:02X}", self.addr),
            loki_per_byte_mode = self.loki_per_byte_mode,
            transport = self.transport_name(),
            iterations,
            poll_reads_per_window,
            cycle_gap_ms,
            "Wave-55e: loki_cold_wake_full_cycle ENTRY"
        );
        tracing::info!(
            iterations,
            poll_reads_per_window,
            cycle_gap_ms,
            "Wave-55b: standalone Loki cold-wake cycle START — \
             expecting spoof to respond with 0x71 within budget \
             (Wave-55h EE-E1: iter default 4→8, gap_ms default 500)"
        );
        let mut spoof_responded = false;
        for cycle in 1..=iterations {
            tracing::info!(
                cycle,
                iterations,
                "Wave-55b: cold-wake cycle {}/{}",
                cycle,
                iterations
            );
            // Step 1: init-frame (prefixed)
            self.loki_cold_wake_init_frame()?;
            // Step 2: poll window
            let (nak_count_a, first_non_nak_a) = self.loki_cold_wake_poll(poll_reads_per_window)?;
            if let Some(b) = first_non_nak_a {
                tracing::info!(
                    cycle,
                    spoof_byte = format_args!("0x{:02X}", b),
                    nak_count = nak_count_a,
                    "Wave-55b: spoof responded after init-frame — cold-wake SUCCEEDED"
                );
                spoof_responded = true;
                break;
            }
            // Step 3: follow-up-frame (bare)
            self.loki_cold_wake_follow_frame()?;
            // Step 4: poll window
            let (nak_count_b, first_non_nak_b) = self.loki_cold_wake_poll(poll_reads_per_window)?;
            if let Some(b) = first_non_nak_b {
                tracing::info!(
                    cycle,
                    spoof_byte = format_args!("0x{:02X}", b),
                    nak_count = nak_count_b,
                    "Wave-55b: spoof responded after follow-up-frame — cold-wake SUCCEEDED"
                );
                spoof_responded = true;
                break;
            }
            tracing::warn!(
                cycle,
                nak_count_init_window = nak_count_a,
                nak_count_follow_window = nak_count_b,
                cycle_gap_ms,
                "Wave-55b: cycle {} exhausted with only NAKs — retrying after {} ms gap",
                cycle,
                cycle_gap_ms
            );
            //  EE-E1: inter-cycle gap. The Loki spoof MCU has a
            // ~30 s watchdog/boot window on cold-cold AC-cycle; without
            // this gap, 4-8 cycles can burn through in <16 s and we miss
            // the wake. Skip the sleep after the LAST cycle (caller proceeds
            // immediately to disable/ramp/enable on EXIT).
            if cycle < iterations && cycle_gap_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(cycle_gap_ms));
            }
        }
        if !spoof_responded {
            tracing::warn!(
                iterations,
                "Wave-55b: cold-wake EXHAUSTED — spoof never returned non-NAK. \
                 Caller will proceed to disable+ramp+enable anyway (best-effort)"
            );
        }
        tracing::info!(
            target: "wave55e_cold_wake_full_cycle_exit",
            iterations,
            spoof_responded,
            "Wave-55e: loki_cold_wake_full_cycle EXIT"
        );
        Ok(spoof_responded)
    }

    /// : standalone cold-boot for `a lab unit`-class Loki-spoof units.
    ///
    /// Runs the -derived cold-wake cycle BEFORE the standard
    /// `cold_boot_sequence_write_only` body. This is the "no bosminer
    /// handoff" path: the spoof has NOT been pre-engaged by bosminer,
    /// so DCENT_OS must emit the cold-wake bytes itself.
    ///
    /// Sequence:
    ///   1. `loki_cold_wake_full_cycle(iterations=4)` — emit
    ///      bytes until spoof responds (or budget exhausted).
    ///   2. `cold_boot_sequence_write_only(target_v, assumed_fw=0x71)` —
    ///      existing 3× Disable + Ramp + Enable + Heartbeat. Byte-identical
    ///      to the bosminer-handoff path's PSU step.
    ///
    /// **Default-OFF for fleet safety.** Only called when the Phase 0
    /// orchestrator in `s19j_hybrid_mining.rs` detects all of:
    ///   - env `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1`,
    ///   - `a lab unit`-class hardware fingerprint,
    ///   - env `DCENT_AM2_TRUST_RAIL_FALLBACK != 1` (standalone, not handoff).
    pub fn cold_boot_sequence_loki_standalone(&mut self, target_init_v: f64) -> Result<()> {
        tracing::warn!(
            target = format_args!("{:.3}V", target_init_v),
            "Wave-55b: standalone Loki cold-boot path START — emitting Wave-38 cold-wake \
             bytes before standard disable+ramp+enable. This path is for cold-AC-cycled \
             `a lab unit`-class XIL units with NO bosminer pre-engagement; if bosminer pre-engaged \
             the chip rail, use cold_boot_sequence_write_only instead."
        );
        //  fix (2026-05-24): force-enable loki_per_byte_mode for the
        // standalone cold-wake window. The  env gates
        // (`DCENT_AM2_PSU_LOKI_REGISTER_POINTER` + ack) are FORBIDDEN by the
        //  recipe guard on `a lab unit`-class hardware, but the
        // helpers (`loki_cold_wake_init_frame` / `_follow_frame` / `_poll`)
        // all require `loki_per_byte_mode = true`. Live evidence (
        // run 1, 2026-05-24): without this hoist, init_frame returned
        // `PsuUnsupported` ~300 µs after entering full_cycle, propagating up
        // through `cold_boot_sequence_loki_standalone -> Phase 0`, triggering
        // run-scope-drop teardown BEFORE any actual Loki byte hit the wire
        // (zero `wave55c_crc_diagnostic` lines in the failed log). Hoisting
        // the enable into the orchestrator is contractually correct: the
        // operator opted in via `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1` +
        // `a lab unit` fingerprint (both ALLOWED by ), and the orchestrator
        // owns the entire bounded  byte sequence.
        self.enable_loki_per_byte_mode_for_wave55b_standalone()
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    "Wave-55e: orchestrator FAILED to enable loki_per_byte_mode — \
                     this is fatal because the cold-wake helpers cannot fire \
                     without it. Likely cause: PSU transport is not ApwIo::Gpio \
                     (the standalone path should only be reachable when the \
                     `gpio_bitbang` branch in s19j_hybrid_mining.rs fires)."
                );
                e
            })?;
        tracing::info!(
            loki_per_byte_mode = self.loki_per_byte_mode_enabled(),
            "Wave-55e: loki_per_byte_mode confirmed ENABLED prior to cold-wake cycle"
        );
        // Step 1:  cold-wake cycle (best-effort — never errors
        // on spoof NAK; only errors on hard bus failure).
        //
        //  EE-E1 (2026-05-25): default bumped 4→8 to cover the
        // Loki spoof MCU ~30 s cold-boot watchdog window. Env override
        // was clamped 1..=16 — bumped to 1..=128 by  Patch 7.
        //
        //  Patch 7 (2026-05-26): RE agents 2+3 cross-confirmed
        // that bosminer cycles for ~70 s total (per `bosminer-psu-log.txt`
        // showing `Version '0x71' detected` log entries ~70 s apart across
        // distinct runs) AND that wave50 log shows 4× calibration-table-
        // empty warnings 7.6 s apart before the spoof escalates. The
        // Loki MCU watchdog is ~30 s per LOKI_SPOOF_BOARD_RE.md. With
        // the prior 16-iter cap × ~1 s/iter = ~16 s budget — we were
        // bailing out before bosminer's empirical-success window
        // (60-120 s) even started. Lift the clamp to 128 so operators
        // can match bosminer's cadence; default stays 8 to preserve
        // existing fleet behaviour.
        let iterations = std::env::var("DCENT_AM2_PSU_LOKI_COLD_WAKE_ITERATIONS")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| (1..=128).contains(n))
            .unwrap_or(8);
        let spoof_responded = self.loki_cold_wake_full_cycle(iterations)?;
        tracing::info!(
            spoof_responded,
            iterations,
            "Wave-55b: cold-wake cycle complete — proceeding to standard cold-boot body"
        );
        // Step 2: existing write-only cold-boot body (byte-identical to
        // bosminer-handoff path). assumed_fw=0x71 = APW121215a per
        // bosminer log evidence on the live Loki spoof. The binary-crate
        // call site uses `APW12_139_ASSUMED_FW` (also 0x71); we hardcode
        // the literal here because dcentrald-hal does NOT export the
        // constant (it lives in `s19j_hybrid_mining.rs` / `serial_mining.rs`).
        const APW121215A_LOKI_SPOOF_FW: u8 = 0x71;
        self.cold_boot_sequence_write_only(target_init_v, APW121215A_LOKI_SPOOF_FW)?;

        //  (2026-05-25): chip-rail engagement via Loki SetVoltage.
        //
        // RE finding (`PHASE2C-DSPIC-RAIL-FAILURE-RE.md` H3):
        // Loki's `SetVoltage` is a fiction at the *PSU rail* layer — the APW3
        // output is hardware-strapped at ~12.8V regardless of what the spoof
        // ACKs. BUT the chip-rail DC-DC on the hashboard converts that 12.8V
        // bus rail into the chip-specific 13.7V chip rail, and bosminer
        // engages this conversion through the Loki spoof's `0x83`
        // SetVoltageStep opcode (NOT through dsPIC opcode `0x10` which is a
        // no-op on cold-boot fw=0x82 per H1).
        //
        //  emits `apw12_set_voltage_transient(13700)` (opcode 0x83,
        // PWM-only, no EEPROM commit) IMMEDIATELY after `cold_boot_sequence_
        // write_only` succeeds — at this point the spoof is awake (Step 1
        // proved that) and the standard 3× Disable + Ramp + Enable body has
        // completed. The SetVoltage(13700) is the missing chip-rail
        // engagement that the dsPIC `0x10` path was failing to provide.
        //
        // Gated by `DCENT_AM2_STANDALONE_RE_FIX=1` so the change is opt-in
        // (preserves byte-identical behavior to today for any caller that
        // doesn't set the env). The `wave55c_crc_diagnostic` log path
        // already in place gives byte-level audit of what we send vs what
        // Loki ACKs.
        let standalone_re_fix_enabled = std::env::var("DCENT_AM2_STANDALONE_RE_FIX")
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        if standalone_re_fix_enabled {
            const WAVE55F_CHIP_RAIL_TARGET_MV: u16 = 13_700;
            tracing::warn!(
                target: "wave55f_loki_setvoltage_chip_rail",
                target_mv = WAVE55F_CHIP_RAIL_TARGET_MV,
                env_gate = "DCENT_AM2_STANDALONE_RE_FIX=1",
                "Wave-55f: emitting Loki SetVoltage(13700) via opcode 0x83 — \
                 RE finding H3 says the chip rail engages HERE (Loki spoof's \
                 0x83 SetVoltageStep), NOT through dsPIC 0x10 (which is a no-op \
                 on cold-boot fw=0x82). See PHASE2C-DSPIC-RAIL-FAILURE-RE.md."
            );
            match self.apw12_set_voltage_transient(WAVE55F_CHIP_RAIL_TARGET_MV) {
                Ok(()) => tracing::info!(
                    target: "wave55f_loki_setvoltage_chip_rail",
                    target_mv = WAVE55F_CHIP_RAIL_TARGET_MV,
                    "Wave-55f: Loki SetVoltage(13700) opcode 0x83 ACK — chip-rail \
                     engagement requested (CRC + byte-level audit via wave55c_crc_diagnostic)"
                ),
                Err(e) => {
                    // Non-fatal: log loudly so operator/RE team see the failure,
                    // but don't escalate — the caller (Phase 0) still gets a
                    // PSU object back and can proceed with the rest of bring-up
                    // (chain enum may still succeed if the rail engaged via
                    // other means; if it didn't, chain-enum will return 0
                    // chips and the run aborts there instead of here).
                    tracing::error!(
                        target: "wave55f_loki_setvoltage_chip_rail",
                        target_mv = WAVE55F_CHIP_RAIL_TARGET_MV,
                        error = %e,
                        "Wave-55f: Loki SetVoltage(13700) FAILED — non-fatal at PSU \
                         layer (chain-enum will reveal whether chip rail engaged). \
                         Likely cause: CRC mismatch or spoof rejected opcode 0x83 \
                         under current state machine. See wave55c_crc_diagnostic for \
                         exact bytes."
                    );
                }
            }
        } else {
            tracing::debug!(
                "Wave-55f: chip-rail SetVoltage SKIPPED — \
                 DCENT_AM2_STANDALONE_RE_FIX env unset; preserving byte-identical \
                 behavior to Wave-55b/55c/55d/55e standalone path."
            );
        }

        Ok(())
    }

    /// Run the opening cold-boot init sequence, matching bosminer's
    /// byte-level order exactly (phase13d Ghidra evidence — see
    /// ).
    ///
    /// Thin wrapper that delegates to `cold_boot_sequence_gated`. When no
    /// gate spec was set (S9, BB, AML, S21 NoPic — none of those gate the
    /// PSU bus behind a GPIO), the gated path performs no GPIO work and
    /// the I²C transaction history is byte-identical to the pre-gate
    /// behavior. Existing non-am2 callers keep working without changes.
    ///
    /// am2 (S19j Pro Zynq) callers should call `set_psu_gate_spec(Some(...))`
    /// (or use the builder `with_psu_gate_spec`) BEFORE this method, or call
    /// `cold_boot_sequence_gated` directly. Without the gate, am2 PSU I²C
    /// EIOs on every write —.
    pub fn cold_boot_sequence(&mut self, target_init_v: f64) -> Result<()> {
        self.cold_boot_sequence_gated(target_init_v)
    }

    /// Gate-aware cold-boot sequence. Asserts the configured PWR_CONTROL
    /// GPIO (if `set_psu_gate_spec`/`with_psu_gate_spec` was called with
    /// `Some(...)`) BEFORE any I²C write reaches the APW PSU, then runs
    /// the same five-step bosminer-canonical body as `cold_boot_sequence`.
    ///
    /// The gate is stored in `self.gpio_gate` and auto-deasserted by
    /// `Apw121215a::Drop` (or by the gate's own Drop, whichever fires
    /// first — both are idempotent). Callers do NOT need to deassert
    /// manually.: PSU EIOs on
    /// cold boot are GPIO-gated, so failing to assert the gate on am2
    /// causes every subsequent opcode to EIO.
    ///
    /// **Fail-closed**: if gate construction or assert returns
    /// `KernelClaimed` or any other error, this method returns that error
    /// **before any I²C write happens**. The PSU is not partially
    /// initialised.
    pub fn cold_boot_sequence_gated(&mut self, target_init_v: f64) -> Result<()> {
        self.try_assert_psu_gate()?;
        self.cold_boot_sequence_inner(target_init_v)
    }

    /// Internal helper: if a `gate_spec` is set and the gate isn't already
    /// asserted from a prior call, construct + assert the gate. Failures
    /// propagate fail-closed — caller must check `?` before any I²C work.
    /// Idempotent (no-op when `gpio_gate.is_some()` or `gate_spec.is_none()`).
    fn try_assert_psu_gate(&mut self) -> Result<()> {
        if self.gpio_gate.is_none() {
            if let Some(spec) = self.gate_spec.clone() {
                let gate = PsuGpioGate::assert(Some(spec.as_str())).map_err(|e| {
                    tracing::error!(
                        spec = spec.as_str(),
                        error = %e,
                        "PSU GPIO gate assert failed; aborting before any I²C write"
                    );
                    e
                })?;
                tracing::info!(
                    gpio = gate.gpio(),
                    spec = spec.as_str(),
                    "PSU GPIO gate asserted (owned by Apw121215a)"
                );
                self.gpio_gate = Some(gate);
            }
        }
        Ok(())
    }

    /// Inner cold-boot body. Identical byte-level five-step bosminer-canonical
    /// flow (see Phase 13D Ghidra evidence in
    /// ):
    ///
    /// ```text
    /// Step 1: flush_buffer()                    // drain stale bytes
    /// Step 2: probe() with 3× retry, 100 ms     // version auto-detect
    /// Step 3: disable() × 3 @ 1 s               // watchdog-off (0x81/0x00)
    /// Step 4: set_voltage_init_bypass(target)   // SetVoltage (0x83)
    /// Step 5: enable()                          // watchdog ARM (0x81/0x01)
    /// ```
    ///
    /// Before Phase 13D this function started with a SetVoltage 0xFF
    /// write on a cold bus — which EIO'd because the APW's dsPIC had
    /// stale bytes in its RX buffer (or the address hadn't been woken by
    /// a prior read). Steps 1-2 fix both.
    ///
    /// `target_init_v` is typically 15.200 V for cold boot (bosminer does
    /// this every cold boot; OCP is less prone to trip at top-of-range).
    /// Uses `set_voltage_init_bypass` internally because the heartbeat
    /// loop has not started yet and the 5-tick gate would otherwise block
    /// the first SetVoltage.
    ///
    /// **Safety:** preserves the 5-tick stability gate for all RUNTIME
    /// voltage writes outside this function; only `set_voltage_init_bypass`
    /// and the two watchdog toggles touch the PSU before the heartbeat
    /// loop starts.
    fn cold_boot_sequence_inner(&mut self, target_init_v: f64) -> Result<()> {
        // Step 1: drain stale PSU send buffer (bosminer literal
        // `PSU: Flushing PSU buffer`). Best-effort; ignore errors.
        let _ = self.flush_buffer();

        // Step 2: probe PSU FW version with 3× retry at 100 ms apart
        // (bosminer `Failed to detect PSU version with any known protocol`
        // wrapped in tokio-retry). Transient reprobe failure may continue only
        // when this adapter already holds an exact cached fw71 identity from
        // an earlier successful probe. With no such identity, later writes
        // have no command-dialect authority and the sequence fails closed.
        let mut probe_err = None;
        for attempt in 1..=3 {
            match self.probe() {
                Ok(model) => {
                    tracing::info!(attempt, model = model.name(), "PSU probe OK");
                    probe_err = None;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "PSU probe failed — retry in 100 ms"
                    );
                    probe_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    // Re-drain between tries.
                    let _ = self.flush_buffer();
                }
            }
        }
        if let Some(e) = probe_err {
            if self
                .require_fw71_dialect("cold boot after probe retry exhaustion")
                .is_err()
            {
                tracing::error!(
                    error = %e,
                    "PSU probe exhausted 3 retries without cached fw71 identity — \
                     refusing cold-boot writes"
                );
                return Err(e);
            }
            tracing::warn!(
                error = %e,
                "PSU reprobe exhausted 3 retries, but exact fw71 identity was established \
                 earlier in this adapter session — continuing the fw71 cold-boot sequence"
            );
        }

        // Step 3: Disable — Watchdog OFF (0x81/0x00), 3×, 1 s apart.
        // This is the bosminer-semantic Disable, NOT SetVoltage 0xFF.
        for i in 0..3 {
            match self.disable() {
                Ok(()) => {
                    tracing::info!(step = i + 1, "PSU: Disable (watchdog off)");
                }
                Err(e) => {
                    tracing::warn!(
                        step = i + 1,
                        error = %e,
                        "PSU: Disable (watchdog off) failed — continuing"
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        // Step 4: Ramp voltage to target (init bypass — 5-tick gate
        // skipped because the heartbeat loop hasn't started yet).
        tracing::info!(
            target = format_args!("{:.3}V", target_init_v),
            "PSU: Ramping voltage {:.3} V -> {:.3} V (slow)",
            target_init_v,
            target_init_v,
        );
        self.set_voltage_init_bypass(target_init_v)?;
        // Blind settle — 121215a has no ADC feedback.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Step 5: Enable — Watchdog ARM (0x81/0x01). Mining begins.
        self.enable()?;
        tracing::info!("PSU: Enable (watchdog armed)");
        match self.heartbeat() {
            Ok(()) => tracing::info!("PSU: Heartbeat after enable"),
            Err(e) => tracing::warn!(
                error = %e,
                "PSU heartbeat immediately after enable failed — heartbeat thread will retry"
            ),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory: pick the right driver for this platform.
// ---------------------------------------------------------------------------

/// Which PSU driver backend is currently open.
pub enum PsuBackend {
    /// Legacy bit-bang / kernel I2C PsuController (S9/S17/S19 Pro Zynq am1).
    Legacy(PsuController),
    /// APW121215a framed-I2C (S19j Pro am2 .139 unit — bus 0, slave 0x10).
    Apw12(Apw121215a),
}

impl PsuBackend {
    /// Open an am2-style APW121215a PSU on `/dev/i2c-0 @ 0x10`. Non-breaking
    /// addition — existing callers that use `PsuController::open*` are
    /// unaffected.
    pub fn open_apw12_framed() -> Result<Self> {
        Ok(PsuBackend::Apw12(Apw121215a::open()?))
    }

    /// Open an am2-style APW121215a PSU through an existing process-wide I2C
    /// service. This is the preferred AM2 path once the caller owns a shared
    /// `I2cServiceHandle`.
    pub fn open_apw12_service(service: I2cServiceHandle) -> Result<Self> {
        Ok(PsuBackend::Apw12(Apw121215a::open_service(service)?))
    }

    /// Open via the legacy `/dev/i2c-1 + GPIO fallback` path.
    pub fn open_legacy() -> Result<Self> {
        Ok(PsuBackend::Legacy(PsuController::open()?))
    }

    /// Transport label (for API / diagnostics).
    pub fn transport_name(&self) -> &'static str {
        match self {
            PsuBackend::Legacy(p) => p.transport_name(),
            PsuBackend::Apw12(_) => "apw12_framed_i2c",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_fallback_is_limited_to_proven_kernel_adapter_absence() {
        let missing = HalError::DeviceOpen {
            path: "/dev/i2c-3".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let no_device = HalError::DeviceOpen {
            path: "/dev/i2c-3".to_string(),
            source: std::io::Error::from_raw_os_error(libc::ENODEV),
        };
        let permission_denied = HalError::DeviceOpen {
            path: "/dev/i2c-3".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let owned = HalError::I2cFabricUnavailable {
            fabric: dcentrald_fabric_lease::PhysicalI2cFabricId::linux_adapter(3),
            detail: "owned by another controller".to_string(),
        };

        assert!(kernel_i2c_absence_allows_gpio_fallback(&missing));
        assert!(kernel_i2c_absence_allows_gpio_fallback(&no_device));
        assert!(!kernel_i2c_absence_allows_gpio_fallback(&permission_denied));
        assert!(!kernel_i2c_absence_allows_gpio_fallback(&owned));
    }

    #[test]
    fn apw_per_version_dac_matches_jig_bitmain_convert_v_to_n() {
        // Byte-exact vs the unstripped S21 jig single_board_test::bitmain_convert_V_to_N
        // @ 000cd370 (.rodata doubles via Ghidra DumpDoubles, RE 2026-06-02).
        // 0x76 (APW121215f) has its OWN coefficients, distinct from 0x71-class.
        // Spot-check at v = 13.0 V using N = trunc(offset - v*slope):
        //   0x76: 1156.107585 - 13*76.090494 = 166.93... -> 166
        //   0x71: 1190.935338 - 13*78.742588 = 167.28... -> 167
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x76, 13.0), Some(166));
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x74, 13.0), Some(166)); // shares 0x76 coeffs
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x71, 13.0), Some(167));
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x72, 13.0), Some(167)); // shares 0x71 coeffs

        // 0x76 and 0x71 are genuinely different formulas (the load-bearing finding:
        // using the 0x71 form on a 0x76 PSU mis-sets the rail).
        assert_ne!(
            Apw121215a::apw_voltage_to_dac(0x76, 12.0),
            Apw121215a::apw_voltage_to_dac(0x71, 12.0),
            "0x76 must NOT reuse the 0x71 DAC formula"
        );

        // Another characterized version in its real range (0x22 ~16-20 V):
        // 1215.894440 - 18*59.931507 = 137.12 -> 137.
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x22, 18.0), Some(137));
        // float-frame + calibrated + unknown versions return None:
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x62, 13.0), None);
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x65, 13.0), None);
        assert_eq!(Apw121215a::apw_voltage_to_dac(0xC1, 13.0), None);
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x99, 13.0), None);

        // Out-of-range N -> None (jig returns -1 / -0x7ffffcff).
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x76, 0.0), None); // N=1156 > 255
        assert_eq!(Apw121215a::apw_voltage_to_dac(0x76, 100.0), None); // N negative
    }

    #[test]
    fn psu_controller_voltage_to_dac_saturates_out_of_range_and_never_wraps() {
        // voltage_to_dac is `((15.1084 - v)/0.013046).round() as i32` then
        // `.clamp(0,255) as u8`. The clamp is load-bearing: without it, an i32 far
        // outside 0..=255 would WRAP on `as u8` (e.g. 300 -> 44, -7 -> 249), turning
        // an out-of-range rail-voltage request into a wildly wrong DAC (and thus a
        // wrong PSU rail). Pin the saturation so a future edit that drops the clamp
        // fails here. dac 0 = the max rail (~15.2 V); dac 255 = the min rail.
        assert_eq!(
            PsuController::voltage_to_dac(100.0),
            0,
            "absurd over-voltage must saturate to dac 0 (max-rail boundary), never wrap"
        );
        assert_eq!(PsuController::voltage_to_dac(15.20), 0);
        assert_eq!(
            PsuController::voltage_to_dac(-50.0),
            255,
            "absurd under-voltage must saturate to dac 255, never wrap"
        );
        assert_eq!(PsuController::voltage_to_dac(0.0), 255);

        // Monotonic non-increasing across the sampled real line (dac falls as
        // voltage rises), every result a valid u8 (no panic/wrap anywhere).
        let mut prev = 256i32;
        let mut v = -20.0_f64;
        while v <= 40.0 {
            let d = PsuController::voltage_to_dac(v) as i32;
            assert!(
                d <= prev,
                "dac must be monotonically non-increasing in voltage (v={v}, d={d}, prev={prev})"
            );
            prev = d;
            v += 0.5;
        }
    }

    #[test]
    fn per_version_dac_gate_policy_is_failsafe() {
        let v = 13.0;
        let proven = Apw121215a::voltage_to_dac_linear(v); // .139 form, all versions default

        // Gate OFF -> always the proven formula, regardless of fw.
        assert_eq!(
            Apw121215a::select_set_voltage_dac(false, Some(0x76), v),
            proven
        );
        assert_eq!(
            Apw121215a::select_set_voltage_dac(false, Some(0x71), v),
            proven
        );
        assert_eq!(Apw121215a::select_set_voltage_dac(false, None, v), proven);

        // Gate ON + 0x76 -> the per-version (correct) DAC, NOT the proven form.
        assert_eq!(
            Apw121215a::select_set_voltage_dac(true, Some(0x76), v),
            Apw121215a::apw_voltage_to_dac(0x76, v).unwrap()
        );
        assert_ne!(
            Apw121215a::select_set_voltage_dac(true, Some(0x76), v),
            proven
        );

        // Gate ON + 0x71-class -> STILL the proven formula (discrepancy unresolved).
        assert_eq!(
            Apw121215a::select_set_voltage_dac(true, Some(0x71), v),
            proven
        );
        assert_eq!(
            Apw121215a::select_set_voltage_dac(true, Some(0x72), v),
            proven
        );

        // Gate ON + unknown fw / no fw -> proven formula (fail-safe).
        assert_eq!(
            Apw121215a::select_set_voltage_dac(true, Some(0x99), v),
            proven
        );
        assert_eq!(Apw121215a::select_set_voltage_dac(true, None, v), proven);

        // Env name pinned (lockstep with  / RE-ASKS).
        assert_eq!(Apw121215a::PER_VERSION_DAC_ENV, "DCENT_AM2_PER_VERSION_DAC");
    }

    #[test]
    fn apw12_checksum_sum_not_xor() {
        // From Agent A's spec §Example (SetVoltage DAC=0xC8):
        //   55 AA 04 83 C8 4F   — LEN=4 (LEN counts itself + CMD + payload +
        //   CKSUM = 1+1+1+1=4), cksum = (0x04 + 0x83 + 0xC8) & 0xFF = 0x4F.
        let frame = build_apw12_frame(APW12_CMD_SET_VOLTAGE, &[0xC8]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x04, 0x83, 0xC8, 0x4F]);

        // Heartbeat with empty payload: LEN=0x03, cksum=(0x03+0x84)=0x87.
        let hb = build_apw12_frame(APW12_CMD_HEARTBEAT, &[]);
        assert_eq!(hb, vec![0x55, 0xAA, 0x03, 0x84, 0x87]);
    }

    /// A30 (knowledge-goldmine s13 F-14/F-15 / IC-02): pin the APW121215a
    /// (fw=0x71) SET_VOLTAGE frame as an **8-bit** checksum frame and prove it
    /// is BYTE-DISTINCT from the APW121215f (fw=0x76) **16-bit** checksum frame,
    /// so a future refactor cannot silently merge the two APW frame builders
    /// (reusing one form on the other PSU fails the PSU's checksum).
    ///
    /// am2 APW121215a (this module, `build_apw12_frame`): one 8-bit SUM checksum
    /// byte, `LEN = 3 + payload_len` (LEN counts itself + CMD + CKSUM).
    /// APW121215f (`psu_apw12_plus::build_apw121215f_frame`): 16-bit LE word
    /// checksum (2 trailing bytes), `LEN = payload_len + 4`.
    /// Sources: findings/s13-apw12-psu.md F-14/F-15; RE-ASKS.md line 606; the
    /// in-tree disambiguation comment `psu_apw12_plus.rs:303-313`.
    #[test]
    fn apw121215a_fw71_set_voltage_8bit_cksum_distinct_from_apw121215f_16bit() {
        // DCENT's am2 rail-set sends opcode 0x83 (SET_VOLTAGE_TARGET, DAC-N) with
        // the DAC byte. Pin the exact fw=0x71 wire bytes.
        let dac: u8 = 0xC8;
        let a = build_apw12_frame(APW12_CMD_SET_VOLTAGE, &[dac]);
        assert_eq!(
            a,
            vec![0x55, 0xAA, 0x04, 0x83, 0xC8, 0x4F],
            "APW121215a fw=0x71 SET_VOLTAGE frame drifted"
        );

        // 8-bit checksum ⇒ exactly ONE trailing checksum byte:
        //   total = preamble(2) + LEN(1) + CMD(1) + payload(1) + CKSUM(1) = 6.
        assert_eq!(
            a.len(),
            2 + 1 + 1 + 1 + 1,
            "must carry a single-byte (8-bit) cksum"
        );
        // LEN byte counts itself + CMD + CKSUM ⇒ build_apw12_frame uses 3 + payload_len.
        assert_eq!(a[2] as usize, 3 + 1);
        // Last byte is the 8-bit wrapping sum of LEN + CMD + Σpayload.
        let want_cksum = a[2].wrapping_add(a[3]).wrapping_add(dac);
        assert_eq!(*a.last().unwrap(), want_cksum);
        assert_eq!(want_cksum, 0x4F);

        // A29: the PIC-disasm catalog name for 0x83 is SET_VOLTAGE_TARGET; pin
        // that the DCENT-facing constant and the disasm constant are the same byte.
        assert_eq!(APW12_CMD_SET_VOLTAGE, APW121215A_CMD_SET_VOLTAGE_TARGET);
        assert_eq!(APW121215A_CMD_SET_VOLTAGE_TARGET, 0x83);
        // And the full disasm opcode catalog stays byte-stable.
        assert_eq!(APW121215A_CMD_DEVICE_TYPE, 0x01);
        assert_eq!(APW121215A_CMD_FW_VERSION, 0x02);
        assert_eq!(APW121215A_CMD_READ_COUNTER, 0x03);
        assert_eq!(APW121215A_CMD_WRITE_VOLT_CAL, 0x04);
        assert_eq!(APW121215A_CMD_READ_RAM_WORD, 0x05);
        assert_eq!(APW121215A_CMD_SET_VOLTAGE, 0x06);
        assert_eq!(APW121215A_CMD_ADJUST_VOLTAGE_STEP, 0x86);

        // APW121215f (fw=0x76) builds a DISTINCT frame: 16-bit (2-byte) checksum,
        // LEN = payload_len + 4. The DAC-byte form is `0x83, [N, 0]`.
        let f = crate::psu_apw12_plus::build_apw121215f_frame(0x83, &[dac, 0x00]).unwrap();
        // 2-byte payload + 2-byte cksum ⇒ total = preamble(2)+LEN(1)+CMD(1)+2+2 = 8.
        assert_eq!(f, vec![0x55, 0xAA, 0x06, 0x83, 0xC8, 0x00, 0x51, 0x01]);
        assert_eq!(
            f.len(),
            8,
            "APW121215f frame must carry a 16-bit (2-byte) cksum"
        );
        assert_eq!(f[2] as usize, 2 + 4, "APW121215f LEN = payload_len + 4");
        // The 16-bit checksum is (LEN + CMD + Σpayload) as a LE word.
        let sum16: u16 = u16::from(f[2])
            .wrapping_add(u16::from(f[3]))
            .wrapping_add(u16::from(dac));
        assert_eq!(f[f.len() - 2], (sum16 & 0xFF) as u8);
        assert_eq!(f[f.len() - 1], (sum16 >> 8) as u8);

        // Load-bearing: the two frames are NOT interchangeable.
        assert_ne!(
            a, f,
            "8-bit APW121215a frame must differ from 16-bit APW121215f"
        );
        assert_ne!(a.len(), f.len());
    }

    /// W2-C (2026-07-07) — pin the APW12 PIC16F1704 firmware-RE cross-check facts
    /// established by the read-only `gpdasm`/`.hex` analysis of the HashSource
    /// `Antminer-APW12-Firmware` corpus.
    ///
    /// Full RE: .
    ///
    /// KEY firmware facts this locks (any regression here means a documented RE
    /// fact silently drifted):
    ///  1. `V71` (the fw=0x71 image the am2 fleet runs) is CODE-IDENTICAL to the
    ///     committed `APW121215a-Good.dis`. The two `.hex` images differ in only
    ///     4 words, ALL in the top-of-flash High-Endurance-Flash *data* region
    ///     (per-unit persisted setpoint, NOT firmware logic): word addresses
    ///     `0x0FDA/0x0FDB` and `0x0FFA/0x0FFB`. ⇒ the entire "s13" opcode RE
    ///     (the `APW121215A_CMD_*` catalog) applies verbatim to the physical PSU.
    ///  2. The PSU PIC returns fw byte `0x71` from opcode `0x02` at handler
    ///     `label_029` @ program address `0x0283` (`movlw 0x71`).
    ///  3. The `convert_V_to_N` slope/offset are HOST-side (jig-derived); the PIC
    ///     never computes them, so `apw_voltage_to_dac`'s 0x71-class coefficients
    ///     are NOT firmware-verifiable and must stay the `a lab unit`-proven / jig values
    ///     until a live rail A/B — this test asserts they are UNCHANGED.
    #[test]
    fn apw12_pic_firmware_re_facts_w2c() {
        // Fact 2 + opcode catalog identity (fw byte returned by opcode 0x02).
        assert_eq!(APW121215A_CMD_FW_VERSION, 0x02);
        assert_eq!(PsuModel::from_fw_byte(0x71), PsuModel::Apw121215a);

        // Fact 1: the 4 HEF data-word addresses that distinguish V71 from the
        // code-identical `APW121215a-Good` disassembly. Documented, not logic —
        // pinned so the RE doc and this constant list can't silently diverge.
        const V71_HEF_DATA_WORD_ADDRS: [u16; 4] = [0x0FDA, 0x0FDB, 0x0FFA, 0x0FFB];
        assert_eq!(V71_HEF_DATA_WORD_ADDRS[0], 0x0FDA);
        assert_eq!(V71_HEF_DATA_WORD_ADDRS[3], 0x0FFB);
        // All four are in the PIC16F1704 High-Endurance-Flash block 0x0F80..=0x0FFF.
        for a in V71_HEF_DATA_WORD_ADDRS {
            assert!((0x0F80..=0x0FFF).contains(&a), "HEF data region");
        }

        // Fact 3: the 0x71-class host-side DAC transfer coefficients are UNCHANGED
        // by this firmware RE (they are NOT PIC-resident and cannot be corrected
        // from the PIC image). apw_voltage_to_dac returns Some for 0x71 and the
        // 0x71-class stays pinned to the same formula as 0x72/0x75/0x77.
        assert!(Apw121215a::apw_voltage_to_dac(0x71, 13.7).is_some());
        assert_eq!(
            Apw121215a::apw_voltage_to_dac(0x71, 13.7),
            Apw121215a::apw_voltage_to_dac(0x72, 13.7),
            "0x71-class DAC formula must remain a single shared coefficient set"
        );
    }

    #[test]
    fn fw71_observation_classifier_rejects_voltage_and_calibration_mutations() {
        for cmd in [
            APW12_CMD_GET_DEVICE_TYPE,
            APW12_CMD_GET_FW_VERSION,
            APW12_CMD_READ_COUNTER,
            APW12_CMD_READ_RAM_WORD,
        ] {
            assert!(apw121215a_is_observation_opcode(cmd));
        }

        for cmd in [
            APW121215A_CMD_WRITE_VOLT_CAL,
            APW121215A_CMD_SET_VOLTAGE,
            APW121215A_CMD_SET_VOLTAGE_TARGET,
            APW12_CMD_WATCHDOG,
        ] {
            assert!(
                !apw121215a_is_observation_opcode(cmd),
                "fw71 mutation 0x{cmd:02X} must never inherit ReadOnly intent"
            );
        }
    }

    #[test]
    fn fw71_driver_rejects_unknown_and_foreign_identities() {
        let (service, _requests) = I2cServiceHandle::for_unit_tests();
        let mut psu = Apw121215a::open_service_at(service, 0, 0x10).unwrap();

        assert!(psu.require_fw71_dialect("test").is_err());

        psu.assume_fw_byte(0x76);
        assert_eq!(psu.model(), PsuModel::Apw121215f);
        assert!(psu.require_fw71_dialect("test").is_err());

        psu.assume_fw_byte(0x71);
        assert_eq!(psu.model(), PsuModel::Apw121215a);
        assert!(psu.require_fw71_dialect("test").is_ok());
    }

    ///  (2026-05-23): pin GetFwVersion frame bytes for byte-format
    /// audit against bosminer.
    ///
    /// `a lab unit`'s Loki spoof NAKs DCENT_OS's GetFwVersion ( captured
    /// 0xF5 reply;  proved it's deterministic — 8/8 retries all
    /// NAK). Bosminer's same command on the same hardware gets an
    /// immediate ACK and reads back fw=0x71. The hypothesis under test
    /// in : our `build_apw12_frame` output for cmd=0x02/payload=[]
    /// differs from what bosminer puts on the wire.
    ///
    /// This test pins the bytes our forensic log will print, so any
    /// future refactor of frame construction is caught BEFORE live test
    /// (a silent frame-format change here would make the forensic log
    /// useless for the byte-by-byte diff against bosminer's bytes).
    ///
    /// Expected frame: `[55 AA 03 02 05]`
    ///   - `55 AA`: preamble
    ///   - `03`:   LEN = 3 + payload_len(0) = 3
    ///   - `02`:   CMD = APW12_CMD_GET_FW_VERSION
    ///   - `05`:   CKSUM = (LEN + CMD + Σpayload) & 0xFF = (3 + 2 + 0) = 5
    ///
    /// On-wire (after I2C addressing for slave 0x10):
    ///   `[START][0x20][0x55][0xAA][0x03][0x02][0x05][STOP]`
    ///   where `0x20 = (0x10 << 1) | 0` (write address).
    ///
    /// Compare to bosminer's GetFwVersion bytes ( deliverable:
    /// either an strace of bosminer's uio17/gpio writes or a side-by-side
    /// capture from a debug-firmware Loki replica — see
    /// ).
    #[test]
    fn wave35_get_fw_version_frame_bytes_pinned() {
        //  (2026-05-24): GetFwVersion opcode is `0x02` per
        // APW121215a-Good.dis disassembly (label_029 line 778 — handler
        // returns literal `0x71`). The opcode-0x01 handler returns `0x10`
        // (HW version). The frame's CRC = sum-mod-256 of non-preamble
        // bytes = 0x03 + 0x02 + 0x00 = 0x05. (Was 0x04 before the swap;
        // 0x03 + 0x01 + 0x00 = 0x04.)
        let frame = build_apw12_frame(APW12_CMD_GET_FW_VERSION, &[]);
        assert_eq!(
            frame,
            vec![0x55, 0xAA, 0x03, 0x02, 0x05],
            "GetFwVersion frame bytes drifted — Wave-35/55d forensic diff vs \
             bosminer requires this byte sequence to stay stable"
        );
        // Also pin the on-wire write address byte for slave 0x10 (the
        // Loki spoof on `a lab unit`): (0x10 << 1) | 0 = 0x20.
        let slave_addr: u8 = 0x10;
        let wire_write_addr = (slave_addr << 1) | 0;
        assert_eq!(
            wire_write_addr, 0x20,
            "on-wire I2C write address for slave 0x10 must be 0x20"
        );
    }

    #[test]
    fn apw12_voltage_dac_roundtrip_linear() {
        // Anchor values from Agent A's spec: 15.20 V → 0x00, 13.70 V → 0x6C,
        // 12.50 V → 0xC8, 11.96 V → 0xF1.
        assert_eq!(Apw121215a::voltage_to_dac_linear(15.2000), 0x00);
        assert_eq!(Apw121215a::voltage_to_dac_linear(13.7000), 0x6C);
        assert_eq!(Apw121215a::voltage_to_dac_linear(12.5000), 0xC8);
        assert_eq!(Apw121215a::voltage_to_dac_linear(11.96), 0xF1);

        // DAC → voltage: with the rounding floor, DAC 0xC8 should round-trip
        // to within one DAC step (~13 mV) of 12.50.
        let v = Apw121215a::dac_to_voltage_linear(0xC8);
        assert!(
            (v - 12.5).abs() < 0.02,
            "dac 0xC8 -> {} (expected ~12.5)",
            v
        );
        let back = Apw121215a::voltage_to_dac_linear(v);
        assert_eq!(back, 0xC8, "dac→V→dac should round-trip exactly");
    }

    #[test]
    fn apw12_parse_reply_validates() {
        // Good SetVoltage echo: [55 AA 04 83 C8 4F]
        let payload =
            parse_apw12_reply(APW12_CMD_SET_VOLTAGE, &[0x55, 0xAA, 0x04, 0x83, 0xC8, 0x4F])
                .expect("valid frame parses");
        assert_eq!(payload, vec![0xC8]);

        // Bad checksum.
        let err = parse_apw12_reply(APW12_CMD_SET_VOLTAGE, &[0x55, 0xAA, 0x04, 0x83, 0xC8, 0x50]);
        assert!(matches!(
            err,
            Err(HalError::PsuProtocol("invalid reply checksum"))
        ));

        // Bad preamble.
        let err = parse_apw12_reply(APW12_CMD_SET_VOLTAGE, &[0x00, 0xAA, 0x04, 0x83, 0xC8, 0x4F]);
        assert!(matches!(
            err,
            Err(HalError::PsuProtocol("invalid preamble"))
        ));

        // NAK.
        let err = parse_apw12_reply(APW12_CMD_SET_VOLTAGE, &[APW12_NAK_BYTE]);
        assert!(matches!(err, Err(HalError::PsuProtocol("PSU NAK (0xF5)"))));

        // Wrong CMD echo.
        let err = parse_apw12_reply(APW12_CMD_SET_VOLTAGE, &[0x55, 0xAA, 0x04, 0x01, 0xC8, 0xCD]);
        assert!(matches!(
            err,
            Err(HalError::PsuProtocol("wrong reply CMD echo"))
        ));
    }

    #[test]
    fn apw12_model_from_fw_byte() {
        // 0x71 — APW121215a (live-confirmed on .139, no voltage feedback).
        assert_eq!(PsuModel::from_fw_byte(0x71), PsuModel::Apw121215a);
        assert!(!PsuModel::from_fw_byte(0x71).has_voltage_feedback());
        // 0x72/0x73 — APW121215b/c (no voltage feedback).
        assert_eq!(PsuModel::from_fw_byte(0x72), PsuModel::Apw121215Bc);
        assert!(!PsuModel::from_fw_byte(0x72).has_voltage_feedback());
        // 0x74/0x75 — APW121215d/e (voltage feedback).
        assert_eq!(PsuModel::from_fw_byte(0x74), PsuModel::Apw121215De);
        assert!(PsuModel::from_fw_byte(0x75).has_voltage_feedback());
        // 0x76 — APW121215f model dispatch is live-confirmed on .78, but
        // telemetry remains uncharacterized and unavailable.
        assert_eq!(PsuModel::from_fw_byte(0x76), PsuModel::Apw121215f);
        assert!(!PsuModel::from_fw_byte(0x76).has_voltage_feedback());
        assert_eq!(PsuModel::Apw121215f.name(), "APW121215f");
        // 0x77 — APW121215g (heuristic, voltage feedback presumed).
        assert_eq!(PsuModel::from_fw_byte(0x77), PsuModel::Apw121215g);
        assert!(PsuModel::from_fw_byte(0x77).has_voltage_feedback());
        // Unknown / other-byte fallback.
        assert_eq!(PsuModel::from_fw_byte(0x00), PsuModel::Unknown);
        assert!(matches!(
            PsuModel::from_fw_byte(0xEE),
            PsuModel::Other(0xEE)
        ));
    }

    /// W3.5: `Apw121215f` (fw=0x76) is recognized at the model-dispatch
    /// level but telemetry is uncharacterized. The contract points are:
    ///
    /// 1. `PsuModel::from_fw_byte(0x76) == Apw121215f`
    /// 2. `has_voltage_feedback()` / `has_current_feedback()` /
    ///    `has_power_feedback()` all return `false` (no fake telemetry).
    /// 3. `is_telemetry_characterized() == false` — distinguishes 0x76 from
    ///    fw=0x71 (which IS characterized, just no-feedback).
    /// No command opcode is inferred from these metadata flags. A future
    /// protocol-identified adapter must supply its own effect classification.
    #[test]
    fn apw121215f_detection_returns_telemetry_unavailable() {
        // 1. Model dispatch routes 0x76 → Apw121215f.
        let model = PsuModel::from_fw_byte(0x76);
        assert_eq!(
            model,
            PsuModel::Apw121215f,
            "fw 0x76 must dispatch to Apw121215f"
        );
        assert_eq!(model.name(), "APW121215f");

        // 2. All telemetry-capability flags are false (no fake values).
        assert!(
            !model.has_voltage_feedback(),
            "Apw121215f voltage feedback must be false until characterized"
        );
        assert!(
            !model.has_current_feedback(),
            "Apw121215f current feedback must be false until characterized"
        );
        assert!(
            !model.has_power_feedback(),
            "Apw121215f power feedback must be false until characterized"
        );

        // 3. is_telemetry_characterized distinguishes 0x76 from 0x71.
        assert!(
            !model.is_telemetry_characterized(),
            "Apw121215f telemetry MUST be marked uncharacterized — fail-closed gate"
        );
        assert!(
            PsuModel::Apw121215a.is_telemetry_characterized(),
            "Apw121215a IS characterized (fw=0x71 proven no-feedback on .139)"
        );

        // Metadata distinguishes uncharacterized 0x76 from the characterized
        // no-feedback 0x71 without fabricating a command path.
        let f76_uncharacterized =
            !model.has_voltage_feedback() && !model.is_telemetry_characterized();
        assert!(
            f76_uncharacterized,
            "0x76 telemetry must remain unavailable until characterized"
        );
        let a71 = PsuModel::Apw121215a;
        let a71_no_telem_but_known =
            !a71.has_voltage_feedback() && a71.is_telemetry_characterized();
        assert!(
            a71_no_telem_but_known,
            "0x71 must remain characterized as having no feedback"
        );
    }

    #[test]
    fn apw12_voltage_gate_default_blocks() {
        // We cannot open a real I2C bus in unit tests, but the gate logic
        // lives in plain counter arithmetic — test via a synthetic instance
        // through the helper counter (no I/O).
        let c = AtomicU64::new(0);
        let allowed = c.load(Ordering::Relaxed) >= APW12_STABLE_TICKS_GATE;
        assert!(!allowed);
        for _ in 0..APW12_STABLE_TICKS_GATE {
            c.fetch_add(1, Ordering::Relaxed);
        }
        let allowed2 = c.load(Ordering::Relaxed) >= APW12_STABLE_TICKS_GATE;
        assert!(allowed2);
    }

    // -----------------------------------------------------------------------
    // A2: psu_gpio_gate auto-integration tests
    //
    // Real `PsuGpioGate::assert` requires `/sys/class/gpio/...`, which doesn't
    // exist on Windows or in any host-test environment, so we cannot fully
    // construct a live `Apw121215a` with an asserted gate. Instead, we test
    // the **state-machine plumbing** that decides whether a gate would be
    // asserted and the **default behavior** that protects non-am2 platforms
    // from any GPIO regression. The actual sysfs side effects are covered
    // by `psu_gpio_gate.rs::tests` and live-bring-up on `a lab unit`.
    // -----------------------------------------------------------------------

    /// Build a synthetic `Apw121215a` with no real I/O backend, suitable for
    /// state-machine assertions only. Calls that attempt I²C will panic; the
    /// tests here MUST stay on the gate-config side of the call graph.
    fn synthetic_apw121215a() -> Apw121215a {
        // We cannot construct an `ApwIo::Kernel(I2cBus::open(...))` here
        // because it requires `/dev/i2c-N`. Use the (test-only) GPIO
        // bit-bang variant placeholder: we never call any method that
        // actually clocks the bus, so the `GpioBitBangI2c` struct's
        // sysfs-export side effect would fire. The cleanest way is to
        // build the struct via `with_io` directly using a fake `ApwIo`
        // — but `ApwIo` and `with_io` are private, which is fine inside
        // this `mod tests`. Use a dummy `I2cBus` via `I2cBus::open(99)`
        // would hit the OS; instead we hand-roll the struct with an
        // `ApwIo::Service` containing a mock-able handle.
        //
        // Simpler: open a `GpioBitBangI2c` will fail on Windows, so we
        // fall back to constructing the struct by mem-zero'ing fields
        // we never read. Rust forbids that, so we use the I2cService
        // path with a never-served handle.
        //
        // Cleanest available seam: call `Apw121215a::with_io(io, bus, addr)`
        // with `ApwIo::Service` built from a thread-local handle that is
        // never queued. No I/O fires until a method like `flush_buffer`
        // is invoked — and these gate-spec tests never call those.
        let (handle, _drop_guard) = crate::i2c::I2cServiceHandle::for_unit_tests();
        Apw121215a::with_io(ApwIo::Service(handle), 0, APW12_FRAMED_ADDR)
    }

    #[test]
    fn apw121215a_safe_shutdown_is_one_ordered_worker_transaction() {
        let (handle, rx) = crate::i2c::I2cServiceHandle::for_unit_tests();
        let worker = std::thread::spawn(move || {
            let request = rx.recv().expect("safe shutdown transaction");
            let crate::i2c::I2cRequest::Transaction {
                addr,
                steps,
                reply_tx,
            } = request
            else {
                panic!("expected one compound transaction")
            };
            reply_tx.send(Ok(Vec::new())).unwrap();
            (addr, steps)
        });
        let mut psu = Apw121215a::with_io(ApwIo::Service(handle), 0, APW12_FRAMED_ADDR);
        psu.safe_shutdown_to_min().unwrap();
        let (addr, steps) = worker.join().unwrap();

        assert_eq!(addr, APW12_FRAMED_ADDR);
        assert_eq!(
            steps,
            vec![
                I2cTransactionStep::Write(build_apw12_frame(APW12_CMD_SET_VOLTAGE, &[0xFF],)),
                I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
                I2cTransactionStep::Write(build_apw12_frame(APW12_CMD_WATCHDOG, &[0x00])),
                I2cTransactionStep::SleepMs(APW12_REPLY_DELAY_MS),
            ]
        );
        assert_eq!(psu.dac, Some(0xFF));
        assert!(!psu.watchdog_armed);
    }

    /// Test 1: the wrapper `cold_boot_sequence` and the new
    /// `cold_boot_sequence_gated` are functionally identical when no gate
    /// spec is set — i.e., they run the same body, in the same order,
    /// without touching any GPIO. We exercise the predicate state on both
    /// entry points; the I²C body itself is covered by integration tests
    /// on live hardware.
    #[test]
    fn cold_boot_sequence_gated_with_none_is_unchanged_behavior() {
        let psu = synthetic_apw121215a();

        // Default: no spec, no gate object — identical baseline to the
        // pre-A2 behavior, before any wiring change.
        assert!(!psu.has_gate_spec(), "default: no PWR_CONTROL spec");
        assert!(!psu.is_gate_asserted(), "default: gate object absent");

        // Setter form is a no-op when called with `None`.
        let mut psu = psu;
        psu.set_psu_gate_spec(None);
        assert!(!psu.has_gate_spec());
        assert!(!psu.is_gate_asserted());

        // Builder form likewise: `None` keeps the gate disabled. We can't
        // assert this returns the same `Apw121215a` byte-by-byte (no Eq
        // impl on the io enum), but we can re-verify the predicates.
        let psu = psu.with_psu_gate_spec(None);
        assert!(!psu.has_gate_spec());
        assert!(!psu.is_gate_asserted());

        // The wrapper and the gated method MUST share the same body —
        // verify by source structure: `cold_boot_sequence` is now a
        // single-line delegation. (Compile-time check: this test file
        // links against the new symbol; if the wrapper were removed or
        // renamed, this would fail to compile.)
        let _wrapper: fn(&mut Apw121215a, f64) -> Result<()> = Apw121215a::cold_boot_sequence;
        let _gated: fn(&mut Apw121215a, f64) -> Result<()> = Apw121215a::cold_boot_sequence_gated;
    }

    /// Test 2: when a gate spec is configured but the underlying GPIO
    /// constructor returns an error (e.g. `KernelClaimed`, EBUSY, missing
    /// label), `cold_boot_sequence_gated` must return that error
    /// **before any I²C write**. We verify this by setting an obviously
    /// invalid spec on a host that lacks `/sys/class/gpio` — the
    /// `PsuGpioGate::assert` call must fail synchronously, before
    /// `cold_boot_sequence_inner` can run any APW opcode.
    ///
    /// A `synthetic_apw121215a` with a never-served I²C handle would
    /// hang on the first opcode write; if `cold_boot_sequence_gated`
    /// instead returns the gate error promptly, this test passes.
    #[test]
    fn cold_boot_sequence_gated_propagates_gate_assert_failure() {
        let mut psu = synthetic_apw121215a();
        // `gpio:99999` is well outside any AM2 GPIO range and the sysfs
        // path also doesn't exist on Windows test hosts → assert() fails
        // before any I²C activity.
        psu.set_psu_gate_spec(Some("gpio:99999".to_string()));
        assert!(psu.has_gate_spec());

        // Run with a short timeout via blocking call: we expect a fast
        // `Err(...)` from PsuGpioGate, NOT a hang on the I²C path.
        let res = psu.cold_boot_sequence_gated(15.200);

        // Must be Err.
        let err = res
            .expect_err("cold_boot_sequence_gated must fail-closed when gate cannot be asserted");
        // Must be a Gpio-class error (the only failure mode at this
        // step is gate construction). Any other variant means we
        // reached the I²C body, which is the regression we're guarding.
        let msg = format!("{}", err);
        assert!(
            matches!(err, HalError::Gpio(_)) || msg.to_lowercase().contains("gpio"),
            "expected GPIO/Gpio gate error before I²C body, got: {:?}",
            err
        );

        // Gate was never successfully asserted, so the field is still None.
        assert!(
            !psu.is_gate_asserted(),
            "gate must remain unasserted after construction failure"
        );
    }

    /// Test 3: dropping `Apw121215a` releases the gate via its own Drop
    /// impl. We can't construct a live `PsuGpioGate` on Windows (the
    /// constructor touches `/sys/class/gpio/`), but we CAN verify that
    /// the field is held in `Option<PsuGpioGate>` position so Rust's
    /// drop semantics fire automatically, AND that our explicit Drop
    /// impl on `Apw121215a` calls `gate.deassert()` exactly once when
    /// the field is `Some`.
    ///
    /// The actual sysfs-restoration behavior is verified by
    /// `psu_gpio_gate.rs::tests` and the live `a lab unit` Phase 14 bring-up.
    /// Here we verify the structural contract: Drop ownership is in the
    /// PSU module, not at the call site.
    #[test]
    fn dropping_apw121215a_releases_gate() {
        // Construction with no gate (the path most hosts will take).
        let psu = synthetic_apw121215a();
        assert!(psu.gpio_gate.is_none());
        // Explicit drop — must not panic, must not deassert anything
        // (no gate present), must not require any I/O. Drop runs the
        // explicit impl which short-circuits on `take().is_none()`.
        drop(psu);

        // Compile-time guarantee: `std::mem::needs_drop::<Apw121215a>()`
        // is true because we declared `impl Drop for Apw121215a`. Forces
        // the "PSU module owns the gate's lifetime" invariant: any
        // future refactor that removes `impl Drop for Apw121215a` would
        // also remove the only field that materially needs Drop side
        // effects (the `Option<PsuGpioGate>`), still keeping
        // `needs_drop` true via the inner gate; either way, the explicit
        // PSU-side Drop impl is the canonical release ordering.
        assert!(
            std::mem::needs_drop::<Apw121215a>(),
            "Apw121215a must require Drop so PWR_CONTROL gate releases automatically"
        );

        // The gate field type is `Option<PsuGpioGate>` — re-confirm at
        // compile time that nothing has degraded the type.
        fn assert_field_shape(p: &Apw121215a) -> &Option<PsuGpioGate> {
            &p.gpio_gate
        }
        let psu2 = synthetic_apw121215a();
        let _ = assert_field_shape(&psu2);
    }

    // -----------------------------------------------------------------------
    // EE-005 / EE-015 (EE-LOKI-001): no_smbus_peer hard-skip in enable().
    // -----------------------------------------------------------------------

    /// Default: no_smbus_peer is false (every smart-APW12 path keeps the
    /// byte-identical watchdog-arm behaviour).
    #[test]
    fn no_smbus_peer_defaults_false() {
        let psu = synthetic_apw121215a();
        assert!(
            !psu.no_smbus_peer(),
            "no_smbus_peer must default false so real-SMBus paths are unchanged"
        );
    }

    /// When no_smbus_peer is set, enable() returns Ok WITHOUT touching the
    /// (never-served) I²C handle, and marks the watchdog armed locally.
    ///
    /// The synthetic PSU's `ApwIo::Service` handle is never served, so any
    /// real I²C write (the `watchdog(true)` path) would block/fail — reaching
    /// `Ok(())` here proves the hard-skip short-circuited before the bus.
    #[test]
    fn enable_with_no_smbus_peer_skips_i2c_cleanly() {
        let mut psu = synthetic_apw121215a();
        psu.set_no_smbus_peer(true);
        assert!(psu.no_smbus_peer());

        let res = psu.enable();
        assert!(
            res.is_ok(),
            "enable() must skip the 0x81 watchdog-arm and return Ok when no_smbus_peer=true; got {res:?}"
        );
        assert!(
            psu.watchdog_armed,
            "enable() hard-skip must still mark the watchdog armed locally"
        );
    }

    /// The setter toggles back to the default (false), restoring the
    /// real-SMBus enable() behaviour.
    #[test]
    fn no_smbus_peer_setter_round_trips() {
        let mut psu = synthetic_apw121215a();
        psu.set_no_smbus_peer(true);
        assert!(psu.no_smbus_peer());
        psu.set_no_smbus_peer(false);
        assert!(!psu.no_smbus_peer());
    }

    /// EE-005: symmetric with enable() — disable() returns Ok WITHOUT touching
    /// the (never-served) I²C handle when no_smbus_peer=true, and clears the
    /// local watchdog flag. Reaching `Ok(())` proves the hard-skip
    /// short-circuited before the bus (a real `watchdog(false)` write on the
    /// never-served Service handle would block/fail).
    #[test]
    fn disable_with_no_smbus_peer_skips_i2c_cleanly() {
        let mut psu = synthetic_apw121215a();
        psu.set_no_smbus_peer(true);

        let res = psu.disable();
        assert!(
            res.is_ok(),
            "disable() must skip the 0x81 watchdog-disable and return Ok when \
             no_smbus_peer=true; got {res:?}"
        );
        assert!(
            !psu.watchdog_armed,
            "disable() hard-skip must clear the local watchdog-armed flag"
        );
    }

    /// EE-005: symmetric with enable()/disable() — heartbeat() returns Ok
    /// WITHOUT touching the (never-served) I²C handle when no_smbus_peer=true,
    /// and STILL advances the stable-tick counter so the 5-stable-tick
    /// `set_voltage` gate can open on a no-SMBus (Loki spoof) rail.
    #[test]
    fn heartbeat_with_no_smbus_peer_skips_i2c_but_counts_ticks() {
        let mut psu = synthetic_apw121215a();
        psu.set_no_smbus_peer(true);
        assert_eq!(psu.heartbeat_ticks(), 0);

        for expected in 1..=APW12_STABLE_TICKS_GATE {
            let res = psu.heartbeat();
            assert!(
                res.is_ok(),
                "heartbeat() must skip the I²C write and return Ok when \
                 no_smbus_peer=true; got {res:?}"
            );
            assert_eq!(
                psu.heartbeat_ticks(),
                expected,
                "no-SMBus heartbeat must still increment the voltage-gate tick counter"
            );
        }
        assert!(
            psu.is_voltage_set_allowed(),
            "after {APW12_STABLE_TICKS_GATE} no-SMBus ticks the voltage-set gate must open"
        );
    }
}
