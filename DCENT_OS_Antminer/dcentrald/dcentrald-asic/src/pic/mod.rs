//! PIC microcontroller protocol (I2C voltage control).
//!
//! The PIC16F1704 on each hash board controls DC-DC voltage regulation.
//! Communication uses a custom protocol over I2C with 0x55 0xAA preamble.
//!
//! CRITICAL FIX (2026-03-18): ALL PIC16F1704 variants (Stock AND BraiinsOS) share
//! the SAME app-mode command set. The "bmminer" command IDs (0x03/0x02/0x11) were
//! for the dsPIC33EP (S17) or bootloader mode, NOT PIC16F1704 app mode.
//! Confirmed by Bitmain AMTC official test tool (single-board-test binary):
//!   set_pic_voltage  sends [0x55, 0xAA, 0x10, val]  — NOT 0x03
//!   enable_pic_dac   sends [0x55, 0xAA, 0x15, 0x01] — NOT 0x02
//!   pic_heart_beat   sends [0x55, 0xAA, 0x16]       — NOT 0x11
//!   get_pic_version  sends [0x55, 0xAA, 0x17] (BraiinsOS) or [0x55, 0xAA, 0x04] (Stock)
//!   get_pic_voltage  sends [0x55, 0xAA, 0x18]       — NOT 0x08
//!
//! Unified PIC16F1704 app-mode commands:
//!   GET_VERSION:      0x17 (BraiinsOS) or 0x04 (Stock, detection only)
//!   SET_VOLTAGE:      0x10  (set DC-DC output voltage via DAC)
//!   ENABLE_VOLTAGE:   0x15  (0x01=enable, 0x00=disable)
//!   GET_VOLTAGE:      0x18  (read actual voltage from DC-DC feedback)
//!   HEARTBEAT:        0x16  (keep DC-DC alive, ~1min stock / ~10s BraiinsOS timeout)
//!   JUMP_FROM_LOADER: 0x06  (exit bootloader — use byte-by-byte I2C)
//!   RESET_PIC:        0x07  (BraiinsOS only — reboot PIC to bootloader)
//!
//! CRITICAL (BLACKLIST, RE review 2026-03-24 — see `pic_needs_jump`): only JUMP
//! when the raw SSPBUF pre-detect read is a CONFIRMED bootloader/unresponsive
//! state — 0xCC (bootloader ISR default), 0x00 (SSPBUF uninitialized on cold
//! power-on), or 0xFF (unresponsive / devmem RX-FIFO-empty after a watchdog).
//! EVERY other value is APP mode and must NOT be JUMP'd: an app-mode PIC's SSPBUF
//! holds the last command byte (0x20, 0x40, 0x48, 0x60, ...), and JUMP'ing a
//! confirmed-app-mode PIC pushes it BACK into the bootloader and breaks voltage
//! control. Do NOT invert this to a whitelist ("JUMP for any non-app value") —
//! that older whitelist broke on 0x48 (post-watchdog app state) and is exactly
//! the regression `pic_needs_jump` + its test now guard against.
//!
//! PIC I2C addresses (S9, verified):
//!   0x55 - Chain 6 (J6) voltage controller
//!   0x56 - Chain 7 (J7) voltage controller
//!   0x57 - Chain 8 (J8) voltage controller
//!
//! PIC firmware versions (per chain, verified on .97):
//!   Chain 6 (0x55): 0x56 (stock) or 0x03 (BraiinsOS, on .36)
//!   Chain 7 (0x56): 0x5A (stock) or 0x03 (BraiinsOS, on .36)
//!   Chain 8 (0x57): 0x5E (stock) or 0x03 (BraiinsOS, on .36)
//!
//! Voltage conversion formula (verified from live probe):
//!   voltage_V = (1608.420446 - pic_value) / 170.423497
//!   pic_value = 1608.420446 - (voltage_V * 170.423497)
//!   Example: PIC value 75 = 9.00V
//!
//! I2C transaction format:
//!   Write commands: BraiinsOS PICs need byte-by-byte, Stock PICs accept single transaction
//!   Read commands: I2C_RDWR ioctl for combined write+read with repeated START
//!   JUMP command: Always byte-by-byte (bootloader parser requires it)
//!   Separate write() + read() returns garbage (I2C address echo)

use crate::Result;
use dcentrald_hal::i2c::I2cBus;

// -- PIC firmware-family modules --------------------------------------------
//
// PIC16F1704 voltage controllers ship with one of two firmware families:
//
//   * **stock**: Bitmain factory firmware versions 0x56 / 0x5A / 0x5E.
//     ~60 second watchdog, no JUMP-from-bootloader on some units, cmd 0x04
//     is GET_VERSION.
//   * **braiinsos**: BraiinsOS-reflashed PIC firmware version 0x03.
//     ~10 second watchdog, reliable JUMP, cmd 0x04 is destructive
//     (ERASE_IIC_FLASH), additional flash-programming command set.
//
// The runtime controllers (`PicController`, `PicServiceController`) below
// dispatch by `PicFirmware` variant. The per-family modules own the
// firmware-identity-specific constants and tests; the unified app-mode
// command set (`BRAIINS_CMD_*` despite the name — used by both families
// in app mode) lives here.
//
// Per memory rules ,
// , and
// , the 7 PIC heartbeat rules
// must hold across both firmware families: no SET_VOLTAGE before 5
// stable heartbeat ticks, always flush 16 zero bytes after any NACK,
// and always use byte-by-byte writes during init / RESET / JUMP traffic.
pub mod braiinsos;
pub mod stock;

/// PIC16F1704 flash address for the S9 factory BADCORE table.
///
/// BraiinsOS reads this before jumping out of the bootloader and uses it to
/// seed per-chip health/tuning decisions. DCENT_OS does not yet wire the flash
/// read into the boot flow, but keeping the parser here makes the eventual
/// integration testable without hashboard hardware.
pub const S9_BADCORE_FLASH_ADDR: u16 = 0x0f80;
pub const S9_BADCORE_FLASH_BYTES: usize = 0x40;

/// PIC16F1704 flash address for the S9 factory frequency-bin table.
pub const S9_FREQ_FLASH_ADDR: u16 = 0x0fa0;
pub const S9_FREQ_FLASH_BYTES: usize = 0x80;
pub const S9_FACTORY_CHIPS: usize = 63;

/// Whether a PIC whose raw SSPBUF pre-detect read is `raw_state` must be JUMP'd
/// out of the bootloader before app-mode commands.
///
/// BLACKLIST (RE review 2026-03-24 — LOAD-BEARING, do NOT invert to a whitelist):
/// only the three CONFIRMED bootloader/unresponsive states JUMP —
///   * `0xCC` — bootloader ISR default,
///   * `0x00` — SSPBUF uninitialized on cold power-on,
///   * `0xFF` — unresponsive / devmem RX-FIFO-empty after a watchdog.
/// EVERY other value is APP mode and must NOT be JUMP'd: an app-mode PIC's SSPBUF
/// holds the last command byte (0x20, 0x40, 0x48, 0x60, ...), and JUMP'ing a
/// confirmed-app-mode PIC pushes it BACK into the bootloader and breaks voltage
/// control. The older whitelist ("JUMP for any value not in a known-app set")
/// broke on 0x48 (post-watchdog app state); `pic_needs_jump_blacklist_not_whitelist`
/// pins the correct behavior so a "cleanup" toward the stale doc can't re-break it.
pub const fn pic_needs_jump(raw_state: u8) -> bool {
    raw_state == 0xCC || raw_state == 0x00 || raw_state == 0xFF
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S9FactoryBadCore {
    /// Per-chip bad-core count/bitmap nibble as stored by factory test.
    pub bad_cores: Vec<u8>,
}

impl S9FactoryBadCore {
    const MAGIC: u8 = 0x23;

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != S9_BADCORE_FLASH_BYTES || data.first().copied()? != Self::MAGIC {
            return None;
        }

        let bad_cores = (0..S9_FACTORY_CHIPS)
            .map(|chip| {
                if chip % 2 == 0 {
                    data[chip + 1] >> 4
                } else {
                    data[chip] & 0x0f
                }
            })
            .collect();

        Some(Self { bad_cores })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S9FactoryFreq {
    pub pic_temp_offset: u8,
    pub base_freq_index: u8,
    /// Per-chip factory frequency index after applying the table step-down.
    pub freq_index: Vec<u8>,
}

impl S9FactoryFreq {
    const MAGIC: u8 = 0x7d;

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != S9_FREQ_FLASH_BYTES || data.get(1).copied()? != Self::MAGIC {
            return None;
        }

        let mut step_down = data[0] & 0x3f;
        if step_down == 0x3f {
            step_down = 0;
        }

        let pic_temp_offset = ((data[2] & 0x0f) << 4) | (data[4] & 0x0f);
        let base_freq_index = ((data[6] & 0x0f) << 4) | (data[8] & 0x0f);
        let freq_index = (0..S9_FACTORY_CHIPS)
            .map(|chip| data[3 + chip * 2].saturating_sub(step_down))
            .collect();

        Some(Self {
            pic_temp_offset,
            base_freq_index,
            freq_index,
        })
    }
}

// ---------------------------------------------------------------------------
// PIC command constants (BMMINER firmware — verified from debug_full_chain.py v2)
// See docs/S9_ASIC_DEBUG_FINDINGS.md
// ---------------------------------------------------------------------------

/// PIC protocol preamble.
pub const PIC_PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Jump from bootloader to application firmware (0x06).
/// Send for ANY raw state that is NOT a known app-mode value.
/// Known app-mode values: 0x60 (app mode indicator), 0x56/0x5A/0x5E (stock PIC versions),
/// 0x03 (BraiinsOS PIC version). Sending JUMP to an app-mode PIC breaks it!
pub const CMD_JUMP_FROM_LOADER: u8 = 0x06;

/// DEPRECATED: Old bmminer SET_VOLTAGE constant (0x03) was for dsPIC33EP, NOT PIC16F1704.
/// All PIC16F1704 variants use BRAIINS_CMD_SET_VOLTAGE (0x10) for voltage DAC.
/// Kept for reference only — DO NOT USE in new code.
#[allow(dead_code)]
pub const CMD_SET_VOLTAGE: u8 = 0x03;

/// DEPRECATED: Old bmminer ENABLE constant (0x02) was for dsPIC33EP, NOT PIC16F1704.
/// All PIC16F1704 variants use BRAIINS_CMD_ENABLE_VOLTAGE (0x15) with data byte.
/// Kept for reference only — DO NOT USE in new code.
#[allow(dead_code)]
pub const CMD_ENABLE_VOLTAGE: u8 = 0x02;

/// DEPRECATED: Old bmminer READ_VOLTAGE constant (0x08).
/// All PIC16F1704 variants use BRAIINS_CMD_GET_VOLTAGE (0x18).
/// Kept for reference only — DO NOT USE in new code.
#[allow(dead_code)]
pub const CMD_READ_VOLTAGE: u8 = 0x08;

/// DEPRECATED: Old bmminer HEARTBEAT constant (0x11) was for dsPIC33EP, NOT PIC16F1704.
/// All PIC16F1704 variants use BRAIINS_CMD_SEND_HEARTBEAT (0x16).
/// PIC watchdog: ~1 minute (stock), ~10 seconds (BraiinsOS) without heartbeat.
/// Kept for reference only — DO NOT USE in new code.
#[allow(dead_code)]
pub const CMD_SEND_HEARTBEAT: u8 = 0x11;

/// Get firmware version (bmminer: 0x04).
/// Format: [0x55, 0xAA, 0x04] -> read 1 byte (I2C_RDWR)
/// Returns firmware version (e.g., 0x56, 0x5A, 0x5E) in app mode,
/// or 0xCC in bootloader mode.
pub const CMD_GET_VERSION: u8 = 0x04;

/// PIC bootloader response (returned when PIC is in bootloader mode).
pub const PIC_BOOTLOADER_RESPONSE: u8 = 0xCC;

/// PIC application mode indicator (raw I2C read when in app mode).
/// NOTE: Do NOT use read_raw() for state detection — stock PICs return 0xCC
/// even in app mode. Use get_version() (I2C_RDWR cmd 0x04) instead.
pub const PIC_APP_MODE: u8 = 0x60;

// ---------------------------------------------------------------------------
// BraiinsOS PIC command constants (firmware version 0x03)
// ---------------------------------------------------------------------------
// Verified from braiins_power.rs (BraiinsOS source extraction).
// BraiinsOS PIC firmware uses COMPLETELY DIFFERENT command IDs than stock.
// WARNING: Stock cmd 0x04 (GET_VERSION) maps to ERASE_IIC_FLASH on BraiinsOS PIC!
// Always detect firmware type BEFORE sending any commands.
//
// Full BraiinsOS command map (from braiins_power.rs lines 53-84):
//   0x01 SET_PIC_FLASH_POINTER    0x09 ERASE_PIC_APP_PROGRAM
//   0x02 SEND_DATA_TO_IIC         0x10 SET_VOLTAGE
//   0x03 READ_DATA_FROM_IIC       0x11 SET_VOLTAGE_TIME
//   0x04 ERASE_IIC_FLASH (!!!)    0x12 SET_HASH_BOARD_ID
//   0x05 WRITE_DATA_INTO_PIC      0x13 GET_HASH_BOARD_ID
//   0x06 JUMP_FROM_LOADER_TO_APP  0x14 SET_HOST_MAC_ADDRESS
//   0x07 RESET_PIC                0x15 ENABLE_VOLTAGE
//   0x08 GET_PIC_FLASH_POINTER    0x16 SEND_HEART_BEAT
//                                 0x17 GET_PIC_SOFTWARE_VERSION
//                                 0x18 GET_VOLTAGE

/// BraiinsOS: set DC-DC output voltage (0x10).
pub const BRAIINS_CMD_SET_VOLTAGE: u8 = 0x10;

/// BraiinsOS: enable/disable DC-DC voltage output (0x15).
/// Data byte: 0x01 = enable, 0x00 = disable. (braiins_power.rs lines 559, 563)
pub const BRAIINS_CMD_ENABLE_VOLTAGE: u8 = 0x15;

/// BraiinsOS: send heartbeat / keepalive (0x16).
pub const BRAIINS_CMD_SEND_HEARTBEAT: u8 = 0x16;

/// BraiinsOS: get PIC firmware version (0x17). Returns 0x03 for BraiinsOS PIC.
pub const BRAIINS_CMD_GET_VERSION: u8 = 0x17;

/// BraiinsOS: read actual voltage from DC-DC feedback (0x18).
pub const BRAIINS_CMD_GET_VOLTAGE: u8 = 0x18;

/// BraiinsOS: reset PIC (0x07). Same as stock — shared between firmwares.
pub const BRAIINS_CMD_RESET_PIC: u8 = 0x07;

/// BraiinsOS expected firmware version.
pub const BRAIINS_FIRMWARE_VERSION: u8 = 0x03;

// ---------------------------------------------------------------------------
// Voltage conversion constants
// ---------------------------------------------------------------------------

/// Voltage conversion offset constant.
pub const VOLTAGE_OFFSET: f64 = 1608.420446;

/// Voltage conversion divisor constant.
pub const VOLTAGE_DIVISOR: f64 = 170.423497;

/// Default initial voltage PIC value (~9.4V).
/// Matches bosminer's OPEN_CORE_VOLTAGE — high voltage during init ensures
/// reliable chip enumeration and core activation. Voltage is lowered to
/// the working level (~8.8-9.1V) after mining begins.
pub const DEFAULT_VOLTAGE_PIC: u8 = 6;

/// Detected PIC firmware type — determines which command set to use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PicFirmware {
    /// Stock Bitmain PIC (versions 0x56/0x5A/0x5E).
    /// Commands: 0x04 get_version, 0x03 set_voltage, 0x02 enable, 0x11 heartbeat, 0x08 read_voltage.
    Stock(u8),
    /// BraiinsOS PIC (version 0x03).
    /// Commands: 0x17 get_version, 0x10 set_voltage, 0x15 enable, 0x16 heartbeat, 0x18 read_voltage.
    BraiinsOs,
    /// Not yet detected — will auto-detect on first use.
    Unknown,
}

impl std::fmt::Display for PicFirmware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PicFirmware::Stock(v) => write!(f, "Stock(0x{:02X})", v),
            PicFirmware::BraiinsOs => write!(f, "BraiinsOS(0x03)"),
            PicFirmware::Unknown => write!(f, "Unknown"),
        }
    }
}

fn classify_pic_raw_state(raw: u8) -> PicFirmware {
    match raw {
        0x03 | 0x60 => PicFirmware::BraiinsOs,
        0x56 | 0x5A | 0x5E => PicFirmware::Stock(raw),
        _ => PicFirmware::Unknown,
    }
}

/// PIC microcontroller controller for one hash board.
pub struct PicController<'a> {
    /// I2C bus reference.
    i2c: &'a mut I2cBus,
    /// I2C slave address (0x55, 0x56, or 0x57).
    address: u8,
    /// Detected firmware type — determines command dispatch.
    firmware: PicFirmware,
    /// Whether the heartbeat thread is running.
    heartbeat_running: bool,
    /// Current voltage PIC value.
    current_voltage_pic: u8,
}

impl<'a> PicController<'a> {
    /// Create a new PIC controller for the given I2C address.
    /// Firmware type defaults to Unknown (auto-detected on cold_boot_init).
    pub fn new(i2c: &'a mut I2cBus, address: u8) -> Self {
        Self {
            i2c,
            address,
            firmware: PicFirmware::Unknown,
            heartbeat_running: false,
            current_voltage_pic: 0,
        }
    }

    /// Create a PIC controller with a known firmware type.
    /// Use this after initial detection to avoid re-probing on every operation.
    pub fn new_with_firmware(i2c: &'a mut I2cBus, address: u8, firmware: PicFirmware) -> Self {
        Self {
            i2c,
            address,
            firmware,
            heartbeat_running: false,
            current_voltage_pic: 0,
        }
    }

    /// Get the detected firmware type.
    pub fn firmware(&self) -> PicFirmware {
        self.firmware
    }

    /// Detect PIC firmware type by probing version commands.
    ///
    /// Strategy (SAFE on both firmware types):
    /// 1. Try BraiinsOS cmd 0x17 FIRST — on stock PIC this is unknown and returns 0xCC/NAK.
    ///    On BraiinsOS PIC this returns 0x03. SAFE because stock firmware ignores unknown cmds.
    /// 2. If 0x17 returns 0x03 → BraiinsOS firmware confirmed.
    /// 3. If 0x17 fails → try stock cmd 0x04.
    ///    SAFE because we only reach here if 0x17 failed, meaning it's NOT BraiinsOS firmware
    ///    (where 0x04 = ERASE_IIC_FLASH — dangerous!).
    /// 4. If 0x04 returns 0x56/0x5A/0x5E → stock firmware confirmed.
    pub fn detect_firmware(&mut self) -> Result<PicFirmware> {
        let detected = match self.read_raw() {
            Ok(raw) => classify_pic_raw_state(raw),
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "PIC firmware detection fell back to Unknown after raw read failure",
                );
                PicFirmware::Unknown
            }
        };
        self.firmware = detected;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %detected,
            "PIC firmware classified from raw state",
        );
        Ok(detected)
    }

    /// Initialize PIC for mining — bosminer-matched sequence with firmware auto-detection.
    ///
    /// Detects PIC firmware type (Stock Bitmain vs BraiinsOS) and uses the
    /// correct command set automatically. BraiinsOS PICs (version 0x03) use
    /// completely different command IDs than stock (0x56/0x5A/0x5E).
    ///
    /// Sequence (matches bosminer reset_and_start_app):
    /// 1. detect_firmware() — probe 0x17 then 0x04 to determine firmware type
    /// 2. If already in app mode: heartbeat immediately (warm boot path)
    /// 3. If bootloader/unknown: RESET(0x07) → clear parser → JUMP(0x06) → re-detect
    /// 4. Set voltage (stock: 0x03, BraiinsOS: 0x10)
    /// 5. Enable voltage output (stock: 0x02, BraiinsOS: 0x15)
    ///
    /// IMPORTANT: RESET(0x07) only exists on BraiinsOS PICs. Stock PICs ignore it.
    /// JUMP(0x06) must ONLY be sent to PICs in bootloader mode (0xCC). Sending JUMP
    /// to a PIC already in app mode (0x60) pushes it BACK into bootloader!
    pub fn cold_boot_init(&mut self, initial_pic_value: u8) -> Result<()> {
        let voltage_v = Self::pic_to_voltage(initial_pic_value);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            target_voltage = format_args!("{:.2}V", voltage_v),
            pic_value = initial_pic_value,
            "PIC init starting (auto-detecting firmware type)",
        );

        // Step 0: Read raw PIC state BEFORE detect_firmware().
        // CRITICAL FIX (2026-03-18): detect_firmware() uses I2C_RDWR which
        // corrupts the PIC's I2C parser. After I2C_RDWR, raw read returns
        // garbage (0x00) instead of the real state (0xCC=bootloader, 0x60=app).
        // Reading raw FIRST gives us the true PIC state for JUMP decision.
        // I2C RELIABILITY FIX (Agent 1, 2026-03-24): Retry raw read 5 times.
        // On cold boot, the first 1-2 I2C transactions may fail due to bus not
        // yet settled (SDA stuck low, xiic driver initializing). A single failed
        // read returns 0xFF ("unresponsive") and writes off a working PIC.
        // BraiinsOS retries 15x per byte with 100ms delay; we retry 5x.
        let pre_detect_raw = {
            let _ = self.i2c.set_slave(self.address);
            let mut raw_val = 0xFF_u8;
            for attempt in 0..5u8 {
                let mut buf = [0u8; 1];
                match self.i2c.read(&mut buf) {
                    Ok(_) => {
                        raw_val = buf[0];
                        break;
                    }
                    Err(e) => {
                        if attempt < 4 {
                            tracing::debug!(
                                addr = format_args!("0x{:02X}", self.address),
                                attempt = attempt + 1,
                                error = %e,
                                "PIC raw read attempt {}/5 failed — retrying in 100ms",
                                attempt + 1,
                            );
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }
            }
            raw_val
        };
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            raw = format_args!("0x{:02X}", pre_detect_raw),
            "PIC raw state (BEFORE detect_firmware): 0x{:02X} ({})",
            pre_detect_raw,
            if pre_detect_raw == 0xCC {
                "BOOTLOADER — will JUMP"
            } else if pre_detect_raw == 0x60 {
                "BraiinsOS app"
            } else if pre_detect_raw == 0xFF {
                "unresponsive"
            } else {
                "stock app or unknown"
            },
        );

        // Step 1: Classify firmware type from raw app-mode state.
        let detected = self.detect_firmware()?;

        // Step 1b: Reset PIC I2C parser state after detect_firmware().
        //
        // CRITICAL FIX (2026-03-14, expert team review):
        // detect_firmware() uses I2C_RDWR (combined write+read with Repeated START).
        // The stock PIC's I2C state machine does NOT handle Repeated START correctly —
        // it leaves the command parser in a corrupted state where subsequent write-only
        // commands (set_voltage, enable_voltage) are silently ignored.
        //
        // BraiinsOS documents this exact bug (braiins_power.rs line 483):
        //   "The I2C state machine in PIC controller is broken"
        //
        // CRITICAL FIX (2026-03-24): Flush MUST be byte-by-byte, NOT a single I2C
        // transaction. The PIC16F1704's MSSP hardware buffer is 1 byte deep. Sending
        // 16 bytes in one transaction causes SSPOV overflow — the PIC firmware only
        // processes byte 0; bytes 1-15 are lost. The parser state machine advances by
        // only 1 step, not 16, so it is NOT reliably returned to idle.
        //
        // BraiinsOS braiins_power.rs:176-183 confirms: their write() sends each byte
        // as a separate I2C transaction via write_retry() (one START+addr+byte+STOP
        // per byte). The 16-zero flush in BraiinsOS is therefore 16 separate
        // transactions, not one 16-byte transaction.
        //
        // Fix: Use write_byte_by_byte() so each zero byte is its own I2C transaction,
        // giving the PIC ISR time to clear SSPBUF between each byte.
        // The flush payload is [0x55, 0xAA, 0x00, 0x00...×13] — starts with the
        // preamble so the parser advances to cmd-byte state, then 13 zeros keep it
        // cycling until it falls back to idle (no valid command follows 0x00).
        let _ = self.i2c.set_slave(self.address);
        if let Err(e) = self.i2c.write_byte_by_byte(&[
            0x55, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ]) {
            tracing::debug!(
                addr = format_args!("0x{:02X}", self.address),
                "I2C parser reset (byte-by-byte 16 bytes) failed: {} — continuing",
                e,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Step 2: Use the pre-detect raw state (read BEFORE I2C_RDWR corruption).
        //
        // CRITICAL FIX (2026-03-18): The raw read AFTER detect_firmware() returns
        // garbage because I2C_RDWR corrupts the PIC's I2C parser. We use the raw
        // state captured in Step 0 (before any I2C_RDWR) for the JUMP decision.
        //
        // Raw I2C probe values:
        //   0xCC = bootloader mode (needs JUMP to enter app)
        //   0x60 = BraiinsOS app mode (do NOT JUMP — it pushes back to bootloader!)
        //   0x56/0x5A/0x5E = stock app mode (version bytes returned by raw read in app mode)
        //   0x03 = BraiinsOS version (sometimes returned as raw state)
        //   0x00 = SSPBUF uninitialized on cold boot — PIC is in BOOTLOADER, not app mode!
        //          (Expert team review 2026-03-24: 0x00 was incorrectly treated as app mode.
        //           Stock PIC16F1704 returns 0x00 when SSPBUF has never been written by the
        //           bootloader ISR. This is NOT app mode — must trigger JUMP.)
        //   0xFF = unresponsive (no hash board connected)
        let raw_state = pre_detect_raw;
        // BLACKLIST approach (RE review fix 2026-03-24): Only JUMP for CONFIRMED bootloader states.
        // The previous whitelist broke on 0x48 (post-watchdog app state) — JUMP pushed
        // app-mode PICs back to bootloader unnecessarily. The PIC SSPBUF can contain
        // any value in app mode (0x20, 0x40, 0x48, 0x60, etc. — depends on last command).
        // Only 0xCC (bootloader ISR default) and 0x00 (SSPBUF uninitialized on cold power-on)
        // are confirmed bootloader indicators. Everything else = app mode → DO NOT JUMP.
        // v0.8.4.2: Also JUMP on 0xFF. With devmem I2C, the read path returns 0xFF
        // when the RX FIFO is empty (read bug). This does NOT mean "no board" — the
        // PIC IS responding to writes (heartbeats succeed). After a watchdog timeout,
        // PICs need a full RESET+JUMP cycle to unlock the voltage controller.
        // BraiinsOS always sends RESET regardless of raw state.
        let needs_jump = pic_needs_jump(raw_state);

        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            raw_state = format_args!("0x{:02X}", raw_state),
            needs_jump,
            firmware = %detected,
            "PIC raw state probe: 0x{:02X} ({}), firmware detected: {}",
            raw_state,
            if raw_state == 0xCC { "BOOTLOADER — needs JUMP" }
            else if raw_state == 0x60 { "BraiinsOS app mode" }
            else if raw_state == 0xFF { "unresponsive" }
            else { "stock app mode" },
            detected,
        );

        // Step 3: Warm-app path must NEVER issue RESET/JUMP.
        if !needs_jump {
            if matches!(detected, PicFirmware::Unknown) {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    raw_state = format_args!("0x{:02X}", raw_state),
                    "PIC raw state is app-like but firmware is ambiguous — using unified app commands without RESET/JUMP",
                );
            }
            if !matches!(detected, PicFirmware::Unknown) {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    "PIC already in app mode — skipping RESET/JUMP on warm boot",
                );
            }

            self.send_heartbeat()?;
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                firmware = %self.firmware,
                "Immediate heartbeat sent to prevent watchdog timeout",
            );

            self.set_voltage(initial_pic_value)?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            match self.enable_voltage() {
                Ok(()) => tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    "Voltage restored (warm boot — may have been disabled by watchdog)",
                ),
                Err(e) => tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "enable_voltage failed on warm boot — voltage may already be active",
                ),
            }
        } else {
            // COLD BOOT PATH: PIC is in bootloader or unresponsive.
            // Use the full bosminer-matched sequence:
            //   1. Send 16 zero bytes (clear I2C parser state machine)
            //   2. RESET (0x07) — forces clean reboot into bootloader
            //   3. Wait 500ms (PIC NAK window during reboot)
            //   4. JUMP (0x06) — transition from bootloader to app mode
            //   5. Wait for app to start, re-detect firmware
            //
            // NOTE: RESET (0x07) only works on BraiinsOS PICs (v0x03).
            // Stock PICs ignore unknown commands. Either way, it's safe to send.

            // Step 2a: Clear I2C parser state (bosminer workaround).
            // Use write_byte_by_byte directly — send_command() routes Unknown
            // firmware to write() (single transaction), which causes SSPOV overflow
            // on the PIC's 1-byte MSSP buffer. See Step 1b comment for full explanation.
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                "PIC in bootloader/unknown — starting full RESET sequence (bosminer-matched)",
            );
            let _ = self.i2c.set_slave(self.address);
            let _ = self.i2c.write_byte_by_byte(&[
                0x55, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ]);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 2b: Send RESET (0x07) for clean bootloader entry
            // This clears any stuck PIC state from a previous watchdog timeout
            // or partial command. BraiinsOS braiins_power.rs:479-498 does this.
            match self.reset_pic() {
                Ok(()) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        "PIC RESET (0x07) sent — waiting 500ms for reboot",
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        error = %e,
                        "PIC RESET failed (may be stock PIC that ignores 0x07) — continuing with JUMP",
                    );
                }
            }

            // Step 2c: Send JUMP to transition from bootloader to app
            match self.jump_to_app() {
                Ok(()) => {
                    // RE REVIEW: bosminer waits only 100ms after JUMP (braiins_power.rs:501).
                    // We use 500ms as a conservative compromise — enough for PIC to
                    // transition to app mode even on slower boards. 2200ms was excessive.
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        "JUMP sent — waiting 500ms for app start"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "JUMP failed — continuing",
                ),
            }

            // Re-detect firmware after RESET + JUMP
            for attempt in 1..=3 {
                match self.detect_firmware() {
                    Ok(PicFirmware::BraiinsOs) | Ok(PicFirmware::Stock(_)) => {
                        tracing::info!(
                            addr = format_args!("0x{:02X}", self.address),
                            firmware = %self.firmware,
                            attempt,
                            "PIC transitioned to app mode after RESET+JUMP",
                        );
                        self.send_heartbeat()?;
                        break;
                    }
                    Ok(PicFirmware::Unknown) => {
                        if attempt < 3 {
                            tracing::warn!(
                                    addr = format_args!("0x{:02X}", self.address),
                                    attempt,
                                    "PIC still unresponsive after RESET+JUMP — retrying in 500ms ({}/3)",
                                    attempt,
                                );
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            error = %e,
                            attempt,
                            "PIC detect_firmware failed after RESET+JUMP (attempt {}/3)",
                            attempt,
                        );
                        if attempt < 3 {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                    }
                }
            }

            if self.firmware == PicFirmware::Unknown {
                tracing::error!(
                    addr = format_args!("0x{:02X}", self.address),
                    "PIC stuck in bootloader after RESET+JUMP — continuing with stock command set",
                );
                // Default to stock for voltage setup attempt
                self.firmware = PicFirmware::Stock(0x00);
            }

            // CRITICAL FIX (2026-03-14, expert team review):
            // The re-detect loop above called detect_firmware() up to 3 times,
            // each using I2C_RDWR which corrupts the stock PIC's I2C parser.
            // Must flush the parser AGAIN before set_voltage/enable_voltage,
            // or those commands will be silently ignored by the corrupted parser.
            //
            // CRITICAL FIX (2026-03-24): Must use write_byte_by_byte — see
            // Step 1b comment above for the full SSPOV/MSSP explanation.
            let _ = self.i2c.set_slave(self.address);
            if let Err(e) = self.i2c.write_byte_by_byte(&[
                0x55, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ]) {
                tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        "I2C parser reset (byte-by-byte 16 bytes) after re-detect loop failed: {} — continuing",
                        e,
                    );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Step 4: Set initial voltage (dispatches to correct cmd based on firmware)
        self.set_voltage(initial_pic_value)?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            pic_value = initial_pic_value,
            voltage = format_args!("{:.2}V", voltage_v),
            "Voltage set to {:.2}V (PIC value {})",
            voltage_v, initial_pic_value,
        );

        // BraiinsOS PIC needs a short delay between commands — the PIC's I2C state
        // machine processes one command at a time. BraiinsOS sends bytes individually
        // with implicit inter-byte delays; we batch commands, so we need explicit delays.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Step 5: Enable voltage output (dispatches to correct cmd based on firmware)
        // NOTE: enable_voltage can fail on BraiinsOS PICs even when other commands work.
        // BraiinsOS sends commands byte-by-byte; the PIC may NAK batched writes for
        // enable_voltage specifically. If it fails, we continue anyway — the PIC may
        // already have voltage enabled from its previous state (PIC RESET doesn't
        // necessarily disable the DC-DC converter).
        match self.enable_voltage() {
            Ok(()) => {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    "Voltage OUTPUT ENABLED",
                );
            }
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    error = %e,
                    "enable_voltage failed — continuing (voltage may already be enabled from previous state)",
                );
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Step 6: Send heartbeat to keep PIC alive regardless of enable_voltage result
        self.send_heartbeat()?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            "Initial heartbeat sent — must continue every 1s",
        );

        Ok(())
    }

    /// Send PIC RESET command (0x07) — forces PIC into bootloader.
    ///
    /// BraiinsOS-only command (stock PICs ignore unknown commands — safe to send).
    /// After RESET, PIC reboots into bootloader mode. Must send JUMP (0x06)
    /// afterward to return to app mode.
    ///
    /// Matches bosminer's braiins_power.rs:479-498 reset() implementation:
    ///   1. Send 16 zero bytes (clear I2C parser state) — caller should do this first
    ///   2. Send [0x55, 0xAA, 0x07] (RESET_PIC command)
    ///   3. Wait 500ms (PIC NAK window during reboot)
    pub fn reset_pic(&mut self) -> Result<()> {
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], BRAIINS_CMD_RESET_PIC])
    }

    /// Send the JUMP_FROM_LOADER_TO_APP command (0x06).
    ///
    /// CRITICAL: Only send this if PIC is in bootloader mode (raw read = 0xCC
    /// or firmware detection returned Unknown). Sending JUMP to a PIC already
    /// in app mode (0x60) puts it BACK into bootloader!
    ///
    /// CRITICAL FIX (2026-03-18): JUMP is sent to PICs in BOOTLOADER mode (0xCC).
    /// The bootloader's I2C parser expects byte-by-byte writes (each byte as a
    /// separate I2C transaction). send_command() dispatches Unknown firmware to
    /// single-transaction write(), but bootloader PICs need byte-by-byte.
    /// Use write_byte_by_byte() directly here — JUMP only targets bootloader PICs.
    pub fn jump_to_app(&mut self) -> Result<()> {
        let data = [PIC_PREAMBLE[0], PIC_PREAMBLE[1], CMD_JUMP_FROM_LOADER];
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        // Always use byte-by-byte for JUMP — bootloader PICs require it.
        // This is safe for both stock and BraiinsOS bootloaders.
        self.i2c
            .write_byte_by_byte(&data)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write (JUMP) failed: {}", e),
            })?;
        Ok(())
    }

    /// Read raw PIC state via plain I2C read (no command).
    /// Known return values: 0x60 = app mode, 0xCC = bootloader, 0x56/0x5A/0x5E = stock
    /// PIC version, 0x03 = BraiinsOS PIC version. Other values (0x80, etc.) = ambiguous.
    pub fn read_raw(&mut self) -> Result<u8> {
        let mut buf = [0u8; 1];
        self.read_response(&mut buf)?;
        Ok(buf[0])
    }

    /// Get PIC firmware version — dispatches to correct command based on firmware type.
    ///
    /// Stock: cmd 0x04, returns 0x56/0x5A/0x5E or 0xCC (bootloader).
    /// BraiinsOS: cmd 0x17, returns 0x03 or 0xCC (bootloader).
    ///
    /// Uses I2C_RDWR (combined write+read) for reliable reads.
    pub fn get_version(&mut self) -> Result<u8> {
        let cmd_byte = match self.firmware {
            PicFirmware::BraiinsOs => BRAIINS_CMD_GET_VERSION,
            _ => CMD_GET_VERSION,
        };
        let cmd = [PIC_PREAMBLE[0], PIC_PREAMBLE[1], cmd_byte];
        let mut buf = [0u8; 1];
        self.write_read(&cmd, &mut buf)?;
        Ok(buf[0])
    }

    /// Set the output voltage via PIC value — dispatches based on firmware type.
    ///
    /// Set voltage DAC value on PIC16F1704.
    ///
    /// CRITICAL FIX (2026-03-18): ALL PIC16F1704 variants (Stock AND BraiinsOS)
    /// use cmd 0x10 for SET_VOLTAGE. The Bitmain official test tool (single-board-test)
    /// confirms: set_pic_voltage sends [0x55, 0xAA, 0x10, voltage_byte].
    /// Cmd 0x03 is a BOOTLOADER flash command (or dsPIC33EP command), NOT app-mode voltage.
    /// Using 0x03 in app mode was silently ignored — PICs never set voltage.
    /// Minimum safe PIC DAC value (9.4V absolute max for BM1387).
    /// Formula: voltage = (1608.42 - pic_value) / 170.42
    /// pic_value=6 → 9.40V, pic_value=0 → 9.44V (DANGEROUS)
    const MIN_SAFE_PIC_VALUE: u8 = 6;

    pub fn set_voltage(&mut self, pic_value: u8) -> Result<()> {
        // SAFETY CLAMP (Agent 5, 2026-03-24): Prevent overvolting.
        // PIC DAC value 0 = 9.44V which exceeds BM1387 safe operating voltage.
        // Clamp to MIN_SAFE_PIC_VALUE (6 = 9.40V).
        let clamped = pic_value.max(Self::MIN_SAFE_PIC_VALUE);
        if clamped != pic_value {
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                requested = pic_value,
                clamped = clamped,
                "Voltage PIC value {} clamped to {} (safety limit: {:.2}V max)",
                pic_value,
                clamped,
                Self::pic_to_voltage(clamped),
            );
        }
        let cmd_byte = BRAIINS_CMD_SET_VOLTAGE; // 0x10 for ALL PIC16F1704
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], cmd_byte, clamped])?;
        self.current_voltage_pic = clamped;
        Ok(())
    }

    /// Enable voltage output (DC-DC enable pin HIGH).
    ///
    /// CRITICAL FIX (2026-03-18): ALL PIC16F1704 variants use cmd 0x15 with data 0x01.
    /// Confirmed by Bitmain official test tool: enable_pic_dac sends [0x55, 0xAA, 0x15, 0x01].
    /// Cmd 0x02 is a dsPIC33EP command (or bootloader SEND_DATA_TO_IIC), NOT PIC16F1704 enable.
    pub fn enable_voltage(&mut self) -> Result<()> {
        self.send_command(&[
            PIC_PREAMBLE[0],
            PIC_PREAMBLE[1],
            BRAIINS_CMD_ENABLE_VOLTAGE,
            0x01,
        ])
    }

    /// Disable voltage output (DC-DC enable pin LOW).
    ///
    /// ALL PIC16F1704: cmd 0x15 + data 0x00 — disables DC-DC enable pin.
    pub fn disable_voltage(&mut self) -> Result<()> {
        self.send_command(&[
            PIC_PREAMBLE[0],
            PIC_PREAMBLE[1],
            BRAIINS_CMD_ENABLE_VOLTAGE,
            0x00,
        ])?;
        tracing::debug!(
            addr = format_args!("0x{:02X}", self.address),
            "DC-DC disabled via cmd 0x15 (enable_voltage=false)",
        );
        Ok(())
    }

    /// Read actual voltage from DC-DC feedback — dispatches based on firmware type.
    ///
    /// Read voltage DAC from PIC. ALL PIC16F1704: cmd 0x18.
    ///
    /// WARNING: Uses I2C_RDWR which may corrupt PIC's I2C parser.
    /// Send 16 zero bytes after to reset parser. Diagnostics only.
    pub fn read_voltage(&mut self) -> Result<u8> {
        let cmd_byte = BRAIINS_CMD_GET_VOLTAGE; // 0x18 for ALL PIC16F1704
        let cmd = [PIC_PREAMBLE[0], PIC_PREAMBLE[1], cmd_byte];
        let mut buf = [0u8; 1];
        self.write_read(&cmd, &mut buf)?;
        Ok(buf[0])
    }

    /// Send heartbeat to prevent PIC watchdog timeout — dispatches based on firmware type.
    ///
    /// Send heartbeat to prevent PIC watchdog timeout.
    ///
    /// CRITICAL FIX (2026-03-18): ALL PIC16F1704 variants use cmd 0x16.
    /// Bitmain test tool confirms: pic_heart_beat sends [0x55, 0xAA, 0x16].
    /// Heartbeat interval: 10 seconds (from Bitmain test tool sleep(10)).
    /// Cmd 0x11 is a dsPIC33EP command, NOT PIC16F1704.
    ///
    /// See [`dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`]
    /// for the per-`(Platform, PicFw)` heartbeat-interval matrix
    /// (`Platform::S9Am1` × `PicFw::Stock` = 1 s tick / ~60 s watchdog;
    /// `× PicFw::Braiins` = 1 s tick / ~10 s watchdog). The 10 s
    /// reference above is Bitmain's test-tool cadence; production
    /// runtime ticks at 1 s — see PIC heartbeat rule #1 in
    /// .
    pub fn send_heartbeat(&mut self) -> Result<()> {
        let cmd_byte = BRAIINS_CMD_SEND_HEARTBEAT; // 0x16 for ALL PIC16F1704
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], cmd_byte])
    }

    /// Get the current voltage PIC register value (cached).
    pub fn get_voltage_pic(&self) -> u8 {
        self.current_voltage_pic
    }

    /// Get the I2C address of this PIC.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Convert voltage in volts to PIC register value.
    ///
    /// Uses rounding (not truncation) to minimize voltage error.
    /// Formula: pic_val = round(1608.420446 - 170.423497 * voltage_V)
    pub fn voltage_to_pic(voltage_v: f64) -> u8 {
        (VOLTAGE_OFFSET - (voltage_v * VOLTAGE_DIVISOR)).round() as u8
    }

    /// Convert PIC register value to voltage in volts.
    pub fn pic_to_voltage(pic_value: u8) -> f64 {
        (VOLTAGE_OFFSET - pic_value as f64) / VOLTAGE_DIVISOR
    }

    // -- private helpers --

    fn send_command(&mut self, data: &[u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        // PIC init commands use byte-by-byte writes.
        //
        // The PIC16F1704's MSSP I2C slave module in bootloader mode has a
        // 1-byte receive buffer. Multi-byte writes overflow the buffer before
        // the bootloader ISR can process each byte. App-mode firmware has a
        // proper ISR but send_command() is called during init when the PIC
        // may still be transitioning between bootloader and app mode.
        //
        // The mining heartbeat thread bypasses send_command() and sends
        // heartbeats directly via single-transaction i2c.write(), which is
        // safe because the PIC is guaranteed to be in app mode during mining.
        self.i2c
            .write_byte_by_byte(data)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write failed: {}", e),
            })?;
        Ok(())
    }

    fn read_response(&mut self, buf: &mut [u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        self.i2c.read(buf).map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("I2C read failed: {}", e),
        })?;
        Ok(())
    }

    /// Combined write+read using I2C_RDWR for reliable PIC reads.
    /// Separate write() + read() returns garbage (I2C address echo).
    fn write_read(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        self.i2c
            .write_read(write_data, read_buf)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write_read failed: {}", e),
            })?;
        Ok(())
    }

    // =========================================================================
    // PIC Flash Programming (BraiinsOS firmware reflash)
    // =========================================================================
    //
    // Flashes BraiinsOS PIC firmware (hash_s8_app.txt) onto stock PICs.
    // Stock PICs can't reliably JUMP from bootloader via Linux I2C, but
    // BraiinsOS PICs handle JUMP correctly. This is a one-time operation
    // that runs automatically on first boot when stock PICs are detected.
    //
    // Protocol (from braiins_power.rs + pic_recovery.py):
    //   1. PIC must be in bootloader (raw=0xCC)
    //   2. Send RESET(0x07) to ensure clean bootloader state
    //   3. Set flash pointer to PROGRAM_LOAD_ADDR (0x0300)
    //   4. Erase 100 sectors (PROGRAM_SIZE / SECTOR_SIZE)
    //   5. Write firmware in 16-byte blocks
    //   6. Verify flash pointer advanced correctly
    //   7. JUMP(0x06) to app mode
    // =========================================================================

    /// Send a PIC bootloader command (multi-byte write with preamble).
    /// Uses a single I2C transaction for reliability.
    /// Retries up to 10 times on I2C errors.
    fn flash_cmd(&mut self, cmd: u8, data: &[u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("flash_cmd set_slave failed: {}", e),
            })?;
        let mut payload = vec![PIC_PREAMBLE[0], PIC_PREAMBLE[1], cmd];
        payload.extend_from_slice(data);
        for attempt in 0..10 {
            match self.i2c.write(&payload) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if attempt == 9 {
                        return Err(crate::AsicError::Pic {
                            addr: self.address,
                            detail: format!(
                                "flash_cmd 0x{:02X} failed after 10 retries: {}",
                                cmd, e
                            ),
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Ok(())
    }

    /// Send command then read response bytes.
    fn flash_cmd_read(&mut self, cmd: u8, rlen: usize) -> Result<Vec<u8>> {
        self.flash_cmd(cmd, &[])?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("flash_cmd_read set_slave failed: {}", e),
            })?;
        let mut result = Vec::with_capacity(rlen);
        for _ in 0..rlen {
            let mut buf = [0u8; 1];
            for attempt in 0..10 {
                match self.i2c.read(&mut buf) {
                    Ok(_) => {
                        result.push(buf[0]);
                        break;
                    }
                    Err(e) => {
                        if attempt == 9 {
                            return Err(crate::AsicError::Pic {
                                addr: self.address,
                                detail: format!("flash_cmd_read failed: {}", e),
                            });
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        }
        Ok(result)
    }

    /// Read the PIC flash pointer (2 bytes, big-endian address).
    fn get_flash_pointer(&mut self) -> Result<Option<u16>> {
        for _ in 0..5 {
            if let Ok(resp) = self.flash_cmd_read(FLASH_CMD_GET_POINTER, 2) {
                if resp.len() >= 2 {
                    let val = ((resp[0] as u16) << 8) | (resp[1] as u16);
                    if val != 0xCCCC {
                        return Ok(Some(val));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(None)
    }

    /// Set the PIC flash pointer and verify it took.
    fn set_flash_pointer(&mut self, ptr: u16) -> Result<bool> {
        let hi = (ptr >> 8) as u8;
        let lo = (ptr & 0xFF) as u8;
        for _ in 0..8 {
            let _ = self.flash_cmd(FLASH_CMD_SET_POINTER, &[hi, lo]);
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok(Some(actual)) = self.get_flash_pointer() {
                if actual == ptr {
                    return Ok(true);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(false)
    }

    /// Flash BraiinsOS firmware onto this PIC.
    ///
    /// The PIC must be in bootloader mode (raw=0xCC). This replaces the stock
    /// firmware with BraiinsOS v0x03, which has reliable JUMP and heartbeat.
    ///
    /// `fw_data` is the firmware as raw bytes (3200 words x 2 bytes = 6400 bytes),
    /// parsed from hash_s8_app.txt by `parse_pic_firmware()`.
    ///
    /// Returns Ok(true) on success, Ok(false) if PIC is unresponsive.
    pub fn flash_firmware(&mut self, fw_data: &[u8]) -> Result<bool> {
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            fw_size = fw_data.len(),
            "PIC flash starting — replacing stock firmware with BraiinsOS v0x03",
        );

        for full_attempt in 0..3 {
            if full_attempt > 0 {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    attempt = full_attempt + 1,
                    "PIC flash retry",
                );
                // On retry, just clear the parser — do NOT send RESET(0x07).
                // Stock bootloader doesn't implement 0x07, and sending it after
                // a partial erase causes I2C errors.
                let _ = self.flash_cmd(0x00, &[0u8; 16]);
                std::thread::sleep(std::time::Duration::from_secs(2));
            } else {
                // First attempt: clear parser state, then try RESET
                let _ = self.flash_cmd(0x00, &[0u8; 16]);
                std::thread::sleep(std::time::Duration::from_millis(50));
                // RESET(0x07) only works on BraiinsOS PICs — stock ignores it (safe to try)
                let _ = self.flash_cmd(BRAIINS_CMD_RESET_PIC, &[]);
                std::thread::sleep(std::time::Duration::from_millis(800));
            }

            // Verify bootloader is alive
            match self.get_flash_pointer() {
                Ok(Some(ptr)) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        pointer = format_args!("0x{:04X}", ptr),
                        "PIC bootloader alive",
                    );
                }
                _ => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        "PIC bootloader not responding — retrying",
                    );
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if self.get_flash_pointer().ok().flatten().is_none() {
                        tracing::error!(
                            addr = format_args!("0x{:02X}", self.address),
                            "PIC bootloader dead — needs power cycle",
                        );
                        return Ok(false);
                    }
                }
            }

            // Erase
            let num_sectors = FLASH_PROGRAM_SIZE / FLASH_SECTOR_SIZE;
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                sectors = num_sectors,
                "Erasing PIC flash",
            );
            if !self.set_flash_pointer(FLASH_PROGRAM_LOAD_ADDR)? {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    "Cannot set pointer for erase"
                );
                continue;
            }

            let mut erase_ok = true;
            for i in 0..num_sectors {
                let mut ok = false;
                for _ in 0..5 {
                    if self.flash_cmd(FLASH_CMD_ERASE, &[]).is_ok() {
                        ok = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if !ok {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        sector = i,
                        "Erase sector failed"
                    );
                    erase_ok = false;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            if !erase_ok {
                continue;
            }
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                "Erase complete"
            );

            // Recovery pause after erase — PIC needs time to settle.
            // The AXI IIC controller can also get confused after 100 transactions.
            std::thread::sleep(std::time::Duration::from_secs(2));

            // Write
            let num_blocks = fw_data.len() / FLASH_XFER_BLOCK_SIZE;
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                blocks = num_blocks,
                "Writing PIC firmware",
            );
            if !self.set_flash_pointer(FLASH_PROGRAM_LOAD_ADDR)? {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    "Cannot set pointer for write"
                );
                continue;
            }

            let mut write_ok = true;
            for i in 0..num_blocks {
                let offset = i * FLASH_XFER_BLOCK_SIZE;
                let block = &fw_data[offset..offset + FLASH_XFER_BLOCK_SIZE];

                // Stage data
                let mut ok = false;
                for _ in 0..5 {
                    if self.flash_cmd(FLASH_CMD_SEND_DATA, block).is_ok() {
                        ok = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if !ok {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        block = i,
                        "Send block failed"
                    );
                    write_ok = false;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));

                // Commit to flash
                ok = false;
                for _ in 0..5 {
                    if self.flash_cmd(FLASH_CMD_WRITE, &[]).is_ok() {
                        ok = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if !ok {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        block = i,
                        "Write block failed"
                    );
                    write_ok = false;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));

                if (i + 1) % 100 == 0 {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        progress = format_args!("{}/{}", i + 1, num_blocks),
                        "PIC flash progress",
                    );
                }
            }
            if !write_ok {
                continue;
            }

            // Verify pointer
            let expected = FLASH_PROGRAM_LOAD_ADDR + FLASH_PROGRAM_SIZE as u16;
            match self.get_flash_pointer() {
                Ok(Some(ptr)) if ptr == expected => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        pointer = format_args!("0x{:04X}", ptr),
                        "PIC flash write sequence finished; pointer at expected address. Verify after power-cycle/probe.",
                    );
                }
                Ok(Some(ptr)) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        pointer = format_args!("0x{:04X}", ptr),
                        expected = format_args!("0x{:04X}", expected),
                        "PIC flash pointer mismatch — may still work",
                    );
                }
                _ => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        "Cannot read flash pointer after write",
                    );
                }
            }

            // JUMP to app
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                "Sending JUMP to app mode"
            );
            let _ = self.flash_cmd(CMD_JUMP_FROM_LOADER, &[]);
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Verify app mode
            self.firmware = PicFirmware::Unknown;
            match self.detect_firmware() {
                Ok(PicFirmware::BraiinsOs) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        "PIC flash SUCCESS — BraiinsOS v0x03 running",
                    );
                    return Ok(true);
                }
                Ok(fw) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        firmware = %fw,
                        "PIC flash done but unexpected firmware detected",
                    );
                    return Ok(true); // flashed but version mismatch — still OK
                }
                Err(e) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        error = %e,
                        "Cannot detect firmware after flash — retrying",
                    );
                }
            }
        }

        tracing::error!(
            addr = format_args!("0x{:02X}", self.address),
            "PIC flash FAILED after 3 attempts",
        );
        Ok(false)
    }
}

// =============================================================================
// PIC Flash Constants
// =============================================================================

/// Bootloader command: set flash pointer address.
const FLASH_CMD_SET_POINTER: u8 = 0x01;

/// Bootloader command: send 16 bytes of data to IIC buffer.
const FLASH_CMD_SEND_DATA: u8 = 0x02;

/// Bootloader command: erase one flash sector (32 words).
const FLASH_CMD_ERASE: u8 = 0x04;

/// Bootloader command: write IIC buffer to flash at current pointer.
const FLASH_CMD_WRITE: u8 = 0x05;

/// Bootloader command: read flash pointer address (2 bytes response).
const FLASH_CMD_GET_POINTER: u8 = 0x08;

/// Flash program load address (where app code starts in PIC memory).
const FLASH_PROGRAM_LOAD_ADDR: u16 = 0x0300;

/// Program size in words (3200 words = 6400 bytes).
const FLASH_PROGRAM_SIZE: usize = 3200;

/// Flash sector size in words (32 words per erase sector).
const FLASH_SECTOR_SIZE: usize = 32;

/// I2C transfer block size in bytes (16 bytes per SEND_DATA_TO_IIC).
const FLASH_XFER_BLOCK_SIZE: usize = 16;

/// Parse hash_s8_app.txt firmware file into raw bytes.
///
/// The file contains 3200 lines, each a 14-bit PIC word in hex (e.g., "3183").
/// Each word is stored as 2 bytes (big-endian): high byte first.
/// Returns 6400 bytes total.
pub fn parse_pic_firmware(path: &str) -> Option<Vec<u8>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut data = Vec::with_capacity(FLASH_PROGRAM_SIZE * 2);
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let word = u16::from_str_radix(line, 16).ok()?;
        data.push((word >> 8) as u8);
        data.push((word & 0xFF) as u8);
    }
    if data.len() != FLASH_PROGRAM_SIZE * 2 {
        return None;
    }
    Some(data)
}

// v0.13.0: PicServiceController -- PIC operations via I2C service thread
use dcentrald_hal::i2c::I2cServiceHandle;

pub struct PicServiceController {
    i2c: I2cServiceHandle,
    address: u8,
    firmware: PicFirmware,
    current_voltage_pic: u8,
}

impl PicServiceController {
    pub fn new(i2c: I2cServiceHandle, address: u8) -> Self {
        Self {
            i2c,
            address,
            firmware: PicFirmware::Unknown,
            current_voltage_pic: 0,
        }
    }

    pub fn new_with_firmware(i2c: I2cServiceHandle, address: u8, firmware: PicFirmware) -> Self {
        Self {
            i2c,
            address,
            firmware,
            current_voltage_pic: 0,
        }
    }

    pub fn firmware(&self) -> PicFirmware {
        self.firmware
    }
    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn detect_firmware(&mut self) -> Result<PicFirmware> {
        let detected = match self.read_raw() {
            Ok(raw) => classify_pic_raw_state(raw),
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "PIC service firmware detection fell back to Unknown after raw read failure",
                );
                PicFirmware::Unknown
            }
        };
        self.firmware = detected;
        Ok(detected)
    }

    fn send_command(&self, data: &[u8]) -> Result<()> {
        self.i2c
            .write_byte_by_byte(self.address, data)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("svc write: {}", e),
            })
    }

    pub fn read_raw(&self) -> Result<u8> {
        let buf = self
            .i2c
            .read_bytes(self.address, 1)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("svc read: {}", e),
            })?;
        Ok(buf[0])
    }

    /// Send a single heartbeat (service-handle path).
    ///
    /// See [`dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`]
    /// for the per-`(Platform, PicFw)` interval table. Callers must
    /// tick this at `cfg.interval_ms`, never slower.
    pub fn send_heartbeat(&self) -> Result<()> {
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], BRAIINS_CMD_SEND_HEARTBEAT])
    }

    pub fn set_voltage(&mut self, pic_value: u8) -> Result<()> {
        let clamped = pic_value.max(PicController::MIN_SAFE_PIC_VALUE);
        self.send_command(&[
            PIC_PREAMBLE[0],
            PIC_PREAMBLE[1],
            BRAIINS_CMD_SET_VOLTAGE,
            clamped,
        ])?;
        self.current_voltage_pic = clamped;
        Ok(())
    }

    pub fn enable_voltage(&self) -> Result<()> {
        self.send_command(&[
            PIC_PREAMBLE[0],
            PIC_PREAMBLE[1],
            BRAIINS_CMD_ENABLE_VOLTAGE,
            0x01,
        ])
    }

    pub fn disable_voltage(&self) -> Result<()> {
        self.send_command(&[
            PIC_PREAMBLE[0],
            PIC_PREAMBLE[1],
            BRAIINS_CMD_ENABLE_VOLTAGE,
            0x00,
        ])
    }

    pub fn read_voltage(&self) -> Result<u8> {
        let cmd = [PIC_PREAMBLE[0], PIC_PREAMBLE[1], BRAIINS_CMD_GET_VOLTAGE];
        let buf =
            self.i2c
                .write_read(self.address, &cmd, 1)
                .map_err(|e| crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!("write_read: {}", e),
                })?;
        Ok(buf[0])
    }

    pub fn reset_pic(&self) -> Result<()> {
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], BRAIINS_CMD_RESET_PIC])
    }

    pub fn jump_to_app(&self) -> Result<()> {
        self.send_command(&[PIC_PREAMBLE[0], PIC_PREAMBLE[1], CMD_JUMP_FROM_LOADER])
    }

    pub fn flush_parser(&self) {
        let _ = self.i2c.write_byte_by_byte(
            self.address,
            &[
                0x55, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
        );
    }

    pub fn write_raw(&self, data: &[u8]) -> Result<()> {
        self.i2c
            .write_bytes(self.address, data)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("raw write: {}", e),
            })
    }

    /// Full cold boot init via service thread.
    pub fn cold_boot_init(&mut self, initial_pic_value: u8) -> Result<()> {
        let pre_detect_raw = {
            let mut raw_val = 0xFF_u8;
            for _attempt in 0..5u8 {
                if let Ok(v) = self.read_raw() {
                    raw_val = v;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            raw_val
        };
        let detected = self.detect_firmware()?;
        self.flush_parser();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let needs_jump = pre_detect_raw == 0xCC || pre_detect_raw == 0x00 || pre_detect_raw == 0xFF;
        if !needs_jump {
            self.send_heartbeat()?;
            self.set_voltage(initial_pic_value)?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = self.enable_voltage();
        } else if matches!(detected, PicFirmware::BraiinsOs | PicFirmware::Stock(_)) && needs_jump {
            let _ = self.jump_to_app();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = self.detect_firmware();
            self.flush_parser();
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.send_heartbeat()?;
        } else {
            self.flush_parser();
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = self.reset_pic();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = self.jump_to_app();
            std::thread::sleep(std::time::Duration::from_millis(500));
            for attempt in 1..=3 {
                match self.detect_firmware() {
                    Ok(PicFirmware::BraiinsOs) | Ok(PicFirmware::Stock(_)) => {
                        let _ = self.send_heartbeat();
                        break;
                    }
                    _ => {
                        if attempt < 3 {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                    }
                }
            }
            if self.firmware == PicFirmware::Unknown {
                self.firmware = PicFirmware::Stock(0x00);
            }
            self.flush_parser();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        self.set_voltage(initial_pic_value)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = self.enable_voltage();
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.send_heartbeat()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        pic_needs_jump, S9FactoryBadCore, S9FactoryFreq, S9_BADCORE_FLASH_BYTES, S9_FACTORY_CHIPS,
        S9_FREQ_FLASH_BYTES,
    };
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn s9_factory_tables_never_panic_on_arbitrary_flash_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..256)
        ) {
            let _ = S9FactoryBadCore::parse(&data);
            let _ = S9FactoryFreq::parse(&data);
        }
    }

    /// LOAD-BEARING S9 rule: JUMP only on CONFIRMED bootloader/unresponsive
    /// states (blacklist), never on app-mode SSPBUF values. JUMP'ing an app-mode
    /// PIC pushes it back to the bootloader and breaks voltage control. The old
    /// whitelist broke on 0x48 (post-watchdog app state); pin the blacklist so a
    /// "cleanup" toward the previously-stale module header can't re-introduce it.
    #[test]
    fn pic_needs_jump_blacklist_not_whitelist() {
        // Confirmed bootloader / unresponsive -> JUMP.
        assert!(
            pic_needs_jump(0xCC),
            "0xCC bootloader ISR default must JUMP"
        );
        assert!(pic_needs_jump(0x00), "0x00 SSPBUF-uninitialized must JUMP");
        assert!(pic_needs_jump(0xFF), "0xFF unresponsive/RX-empty must JUMP");
        // App-mode SSPBUF values (last-command residue) must NOT JUMP.
        for app in [
            0x48u8, 0x60, 0x20, 0x40, 0x56, 0x5A, 0x5E, 0x03, 0x80, 0x31, 0xA8,
        ] {
            assert!(
                !pic_needs_jump(app),
                "0x{app:02X} is app mode and must NOT be JUMP'd (would brick voltage control)"
            );
        }
    }

    #[test]
    fn parses_s9_factory_badcore_table() {
        let mut data = vec![0u8; S9_BADCORE_FLASH_BYTES];
        data[0] = 0x23;
        data[1] = 0xA5;
        data[2] = 0x0C;
        data[63] = 0xD0;

        let parsed = S9FactoryBadCore::parse(&data).expect("valid BADCORE table");

        assert_eq!(parsed.bad_cores.len(), S9_FACTORY_CHIPS);
        assert_eq!(parsed.bad_cores[0], 0x0A);
        assert_eq!(parsed.bad_cores[1], 0x05);
        assert_eq!(parsed.bad_cores[2], 0x00);
        assert_eq!(parsed.bad_cores[62], 0x0D);
        assert!(S9FactoryBadCore::parse(&data[..S9_BADCORE_FLASH_BYTES - 1]).is_none());
    }

    #[test]
    fn parses_s9_factory_freq_table() {
        let mut data = vec![0u8; S9_FREQ_FLASH_BYTES];
        data[0] = 0x02;
        data[1] = 0x7d;
        data[2] = 0x03;
        data[4] = 0x04;
        data[6] = 0x05;
        data[8] = 0x06;
        for chip in 0..S9_FACTORY_CHIPS {
            data[3 + chip * 2] = 20 + chip as u8;
        }

        let parsed = S9FactoryFreq::parse(&data).expect("valid FREQ table");

        assert_eq!(parsed.pic_temp_offset, 0x34);
        assert_eq!(parsed.base_freq_index, 0x56);
        assert_eq!(parsed.freq_index.len(), S9_FACTORY_CHIPS);
        assert_eq!(parsed.freq_index[0], 18);
        assert_eq!(parsed.freq_index[62], 80);
        data[1] = 0x00;
        assert!(S9FactoryFreq::parse(&data).is_none());
    }
}
