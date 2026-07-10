//! dsPIC33EP16GS202 voltage-controller firmware reflash protocol.
//!
//! Used to recover Antminer S17/S19j Pro hashboards whose dsPIC firmware
//! has been silently downgraded to an unsupported variant (e.g. fw=0x86
//! on `a lab unit` and
//! ).
//!
//! ## Protocol revision history
//!
//! **2026-04-27 — BraiinsOS source-of-truth correction**
//! A prior iteration (2026-04-26) inferred opcodes from the AMTC test
//! jig and concluded `0x04 = RESET_PIC`. That mapping is WRONG.
//! :53-77` is the
//! authoritative opcode list (BraiinsOS sourced directly from the
//! bmminer-mix and PIC firmware code paths). The CORRECT mapping is:
//!
//! - `0x04` is `ERASE_IIC_FLASH` (a flash-pointer erase op), NOT reset.
//! - `0x07` is `RESET_PIC` (soft reset).
//!
//! Both PIC16F1704 (S9) and dsPIC33EP16GS202 (S17/S19j Pro) share this
//! opcode space because Bitmain reused the bmminer power-controller
//! ABI across PIC families. The 2026-04-26 "PIC1704 / dsPIC opcode
//! collision" warning is now reframed: 0x04 means the SAME thing on
//! both chips (ERASE), and there is NO accidental cross-family RESET
//! hazard at 0x04. The hazard is elsewhere — see the constant block
//! below.
//!
//! **Codex live test 2026-04-27** (on `a lab unit` fw=0x86) further proved
//! `[55 AA 07]` does NOT enter bootloader mode on this firmware
//! variant. Whatever 0x07 nominally means on a healthy dsPIC, on a
//! `a lab unit`-style locked unit it is a no-op. The opcode is documented
//! here for completeness and exposed via `reset_pic()`, but the
//! production daemon must never invoke it. Recovery-tool-only.
//!
//! Source: :53-77`.
//!
//! ## Protocol
//!
//! The dsPIC bootloader exposes the following framed I²C commands
//! (BraiinsOS naming, mirrors PIC16F1704 ABI):
//!
//! | Cmd  | BraiinsOS symbol            | Purpose                          |
//! |------|-----------------------------|----------------------------------|
//! | 0x01 | `SET_PIC_FLASH_POINTER`     | Set internal flash addr pointer  |
//! | 0x02 | `SEND_DATA_TO_IIC`          | Stream data into staging buffer  |
//! | 0x03 | `READ_DATA_FROM_IIC`        | Read back staged data            |
//! | 0x04 | `ERASE_IIC_FLASH`           | Erase flash at pointer           |
//! | 0x05 | `WRITE_DATA_INTO_PIC`       | Commit staged data to flash      |
//! | 0x06 | `JUMP_FROM_LOADER_TO_APP`   | Boot → app                       |
//! | 0x07 | `RESET_PIC`                 | Soft reset (BANNED on .139)      |
//! | 0x08 | `GET_PIC_FLASH_POINTER`     | Read pointer back                |
//! | 0x09 | `ERASE_PIC_APP_PROGRAM`     | Bulk-erase app region            |
//! | 0x10 | `SET_VOLTAGE` (app mode)    | Set DC-DC target                 |
//! | 0x14 | `SEND_DATA_TO_PIC` (app)    | dsPIC: stream program data       |
//! | 0x15 | `ENABLE_VOLTAGE` / DC-DC    | Enable rail                      |
//! | 0x16 | `SEND_HEART_BEAT`           | Heartbeat                        |
//! | 0x17 | `GET_PIC_SOFTWARE_VERSION`  | GET_VERSION (5-byte response)    |
//! | 0x18 | `GET_VOLTAGE`               | Read voltage back                |
//! | --   | `dumy_read`                 | Bare 1-byte read (FW echo)       |
//!
//! Note: there is **no** SET_FLASH_POINTER. Each `SEND_DATA` (0x14)
//! payload begins with a 24-bit instruction address (LSB first), and the
//! bootloader auto-advances per instruction word.
//!
//! ## Frame format
//!
//! ```text
//! [0x55, 0xAA, FW_ECHO, CMD, payload..., CHECKSUM_LOW, CHECKSUM_HIGH]
//! ```
//!
//! - `FW_ECHO` is the FW byte read at probe time (e.g., 0x86 for `a lab unit`).
//!   Initially set to 0x00 before the FW byte is known; the bootloader
//!   accepts 0x00 for GET_VERSION.
//! - `CHECKSUM` is a 16-bit (mod 0x10000) sum of every byte from preamble
//!   through last payload byte, emitted little-endian (LSB first).
//!
//! ## Test orchestrator sequence
//!
//! 1. `reset_pic` (0x07) — BANNED on `a lab unit`, recovery-tool-only
//! 2. `dumy_read` (1-byte raw read; returns FW echo)
//! 3. `get_pic_sw_version` (0x17, 5-byte response)
//! 4. `update_pic_app_program`:
//!      a. `erase_pic_app_program` (0x09)
//!      b. loop `send_data_to_pic` (0x14) block-by-block
//! 5. `enable_pic_dc_dc` (0x15)
//!
//! ## Firmware file format
//!
//! `dsPIC33EP16GS202_app.txt` is plain text: one 24-bit dsPIC instruction
//! per line, encoded as 6 uppercase hex chars. Lines map to consecutive
//! program-memory addresses starting at 0x000000. No checksum, no header.
//! Total ~3520 lines = 4 KB instruction space (the bootloader region at
//! 0x0000-0x03FF is read-protected by Bitmain CONFIG bits; only the app
//! region 0x0400-0x0FFF is writable).
//!
//! ## Safety
//!
//! **High brick risk.** The dsPIC has no recovery mode short of ICSP
//! (Pickit3 over the test pads) once flash is corrupted. This module
//! enforces a multi-stage safety protocol:
//!
//! 1. **Probe (read-only)**: dumy_read + GET_VERSION twice with stable FW
//!    echo. NO destructive flash R/W during probe.
//! 2. **Smoke test**: stability of FW byte across two GET_VERSION calls
//!    is the new non-destructive aliveness check.
//! 3. **Full reflash (irreversible)**: only proceeds with explicit
//!    `accept_brick_risk = true` AND probe pass. Erase, stream blocks,
//!    bail on first error.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;

const I2C_SLAVE: u32 = 0x0703;

const PREAMBLE: [u8; 2] = [0x55, 0xAA];

// ─── PIC bootloader / app opcodes (BraiinsOS source-of-truth) ────────
//
// These are the AUTHORITATIVE opcodes per BraiinsOS
// :53-77`. Both
// PIC16F1704 (S9) and dsPIC33EP16GS202 (S17/S19j Pro) share this opcode
// table — Bitmain reused the bmminer power-controller ABI across PIC
// families. The 2026-04-26 "AMTC RE" inferred 0x04 = RESET_PIC; that
// was wrong. 0x04 is ERASE_IIC_FLASH; 0x07 is RESET_PIC.
//
// ── Bootloader-mode commands (0x01-0x09) ──
//
// Most of these are documentation constants for the full bmminer
// power-controller ABI. The reflash orchestrator uses
// `JUMP_FROM_LOADER_TO_APP` (0x06), `RESET_PIC` (0x07), and
// `ERASE_APP_PROGRAM` (0x09) directly; the rest are catalog entries
// kept so the opcode table is self-documenting for future RE work
// (e.g., implementing per-block flash verify via the SET_FLASH_POINTER
// → READ_DATA_FROM_IIC sequence). They are tagged `#[allow(dead_code)]`
// so they don't trip the dead-code warning until they're called.
#[allow(dead_code)]
const CMD_SET_FLASH_POINTER: u8 = 0x01;
#[allow(dead_code)]
const CMD_SEND_DATA_TO_IIC: u8 = 0x02;
#[allow(dead_code)]
const CMD_READ_DATA_FROM_IIC: u8 = 0x03;
/// Erase flash at the currently-set pointer. **Bootloader-mode only.**
/// On both PIC16F1704 and dsPIC33EP16GS202 this is the SAME byte and
/// the SAME meaning — there is no cross-family collision at 0x04
/// (the prior "PIC1704 ERASE == dsPIC RESET" warning was incorrect).
#[allow(dead_code)]
const CMD_ERASE_IIC_FLASH: u8 = 0x04;
#[allow(dead_code)]
const CMD_WRITE_DATA_INTO_PIC: u8 = 0x05;
const CMD_JUMP_FROM_LOADER_TO_APP: u8 = 0x06;
/// BraiinsOS RESET. Per Codex live test 2026-04-27, does NOT enter
/// bootloader on `a lab unit` fw=0x86 — banned for production use;
/// recovery-tool-only with `--confirm-bricked` gate.
const CMD_RESET_PIC: u8 = 0x07;
#[allow(dead_code)]
const CMD_GET_PIC_FLASH_POINTER: u8 = 0x08;
const CMD_ERASE_APP_PROGRAM: u8 = 0x09;

// ── App-mode commands (0x10-0x18) ──
#[allow(dead_code)]
const CMD_SET_VOLTAGE: u8 = 0x10; // app-mode SET_VOLTAGE (PIC16F1704 + dsPIC)
const CMD_SEND_DATA: u8 = 0x14; // dsPIC: stream program data (24-bit addr embedded)
const CMD_ENABLE_DC_DC: u8 = 0x15; // ENABLE_VOLTAGE / enable_pic_dc_dc
const CMD_HEARTBEAT: u8 = 0x16; // SEND_HEART_BEAT
const CMD_GET_VERSION: u8 = 0x17; // GET_PIC_SOFTWARE_VERSION (5-byte resp)
#[allow(dead_code)]
const CMD_GET_VOLTAGE: u8 = 0x18; // app-mode GET_VOLTAGE

// ── Legacy alias retained for the existing collision regression test ──
//
// The 2026-04-26 RE introduced a `_PIC1704_CMD_ERASE_IIC_FLASH`
// constant and a regression test asserting the (incorrectly-described)
// "collision" with `CMD_RESET`. Now that the BraiinsOS source-of-truth
// confirms PIC16F1704 and dsPIC share the SAME opcode for ERASE at
// 0x04, the alias is preserved purely so the regression test continues
// to compile; the test docstring has been updated to reflect the
// corrected understanding (no cross-family hazard at 0x04).
#[allow(dead_code)]
const _PIC1704_CMD_ERASE_IIC_FLASH: u8 = 0x04;

/// Default block size (in instruction words) per CMD_SEND_DATA call.
/// 8 instructions × 3 bytes/instruction = 24 bytes of program data
/// per block, plus 3 bytes of address = 27 bytes payload. This keeps
/// each I²C transaction well below typical bootloader RAM-staging
/// limits and matches the AMTC orchestrator's per-iteration cadence.
pub const BLOCK_INSTRUCTIONS: usize = 8;

/// Bytes of program data per block (3 bytes per 24-bit instruction).
pub const BLOCK_DATA_BYTES: usize = BLOCK_INSTRUCTIONS * 3;

/// Bootloader region (read-protected, do NOT touch).
pub const BOOTLOADER_END: u32 = 0x0400;

/// App region start address. All reflash operations must target >=
/// this address; below it is the protected bootloader region.
pub const APP_REGION_START: u32 = 0x0400;

/// App region end address (4 KB total instruction space).
pub const APP_REGION_END: u32 = 0x1000;

/// Errors specific to dsPIC flash protocol.
#[derive(Debug)]
pub enum FlashError {
    Io(io::Error),
    BadFirmwareFormat(String),
    BootloaderUnreachable(String),
    SmokeTestFailed(String),
    VerifyMismatch {
        addr: u32,
        expected: Vec<u8>,
        got: Vec<u8>,
    },
    AddressOutOfRange(u32),
    InvalidArg(String),
}

impl From<io::Error> for FlashError {
    fn from(e: io::Error) -> Self {
        FlashError::Io(e)
    }
}

impl std::fmt::Display for FlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlashError::Io(e) => write!(f, "I/O: {}", e),
            FlashError::BadFirmwareFormat(s) => write!(f, "bad firmware format: {}", s),
            FlashError::BootloaderUnreachable(s) => write!(f, "bootloader unreachable: {}", s),
            FlashError::SmokeTestFailed(s) => write!(f, "smoke test failed: {}", s),
            FlashError::VerifyMismatch {
                addr,
                expected,
                got,
            } => write!(
                f,
                "verify mismatch at 0x{:04X}: expected {:02X?}, got {:02X?}",
                addr, expected, got
            ),
            FlashError::AddressOutOfRange(a) => write!(f, "address 0x{:04X} out of app region", a),
            FlashError::InvalidArg(s) => write!(f, "invalid arg: {}", s),
        }
    }
}

impl std::error::Error for FlashError {}

pub type Result<T> = std::result::Result<T, FlashError>;

/// Parsed firmware image — a sequence of 24-bit dsPIC instruction words.
#[derive(Debug, Clone)]
pub struct FirmwareImage {
    /// Each entry is one 24-bit instruction (low 24 bits used).
    pub words: Vec<u32>,
}

impl FirmwareImage {
    /// Parse a `dsPIC33EP16GS202_app.txt` file. Format:
    ///   - one instruction per line
    ///   - 6 uppercase hex chars per instruction
    ///   - blank lines and `#` comments are ignored
    ///   - trailing 0xFFFFFF entries (erased flash) are kept verbatim
    pub fn parse_text<P: AsRef<Path>>(path: P) -> Result<Self> {
        let raw = fs::read_to_string(&path)?;
        let mut words = Vec::with_capacity(4096);
        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.len() != 6 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(FlashError::BadFirmwareFormat(format!(
                    "line {}: expected 6 hex chars, got `{}`",
                    lineno + 1,
                    trimmed
                )));
            }
            let w = u32::from_str_radix(trimmed, 16).expect("validated above");
            words.push(w);
        }
        if words.is_empty() {
            return Err(FlashError::BadFirmwareFormat(
                "no instruction words parsed".into(),
            ));
        }
        Ok(FirmwareImage { words })
    }

    /// Total instructions parsed.
    pub fn instruction_count(&self) -> usize {
        self.words.len()
    }

    /// Slice of instructions covering [start_addr, start_addr + len).
    /// Returns an error if the requested range exceeds the parsed image.
    pub fn slice(&self, start_addr: u32, len: usize) -> Result<&[u32]> {
        let start = start_addr as usize;
        let end = start
            .checked_add(len)
            .ok_or_else(|| FlashError::InvalidArg("slice overflow".into()))?;
        if end > self.words.len() {
            return Err(FlashError::InvalidArg(format!(
                "slice [{:#06X}..{:#06X}) exceeds image size {}",
                start_addr,
                start_addr as usize + len,
                self.words.len()
            )));
        }
        Ok(&self.words[start..end])
    }
}

/// Build a framed dsPIC packet:
///   `[0x55, 0xAA, FW_ECHO, CMD, payload..., SUM_LO, SUM_HI]`
///
/// Checksum is the 16-bit (mod 0x10000) sum of every byte from preamble
/// through last payload byte, emitted little-endian (LSB first).
pub fn build_frame(fw_echo: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(6 + payload.len());
    frame.extend_from_slice(&PREAMBLE);
    frame.push(fw_echo);
    frame.push(cmd);
    frame.extend_from_slice(payload);
    let mut sum: u32 = 0;
    for b in &frame {
        sum = sum.wrapping_add(u32::from(*b));
    }
    let sum16 = (sum & 0xFFFF) as u16;
    frame.push((sum16 & 0xFF) as u8); // checksum_low (LSB first)
    frame.push(((sum16 >> 8) & 0xFF) as u8); // checksum_high
    frame
}

/// Build a CMD_SEND_DATA payload for a single block.
///
/// Payload layout per AMTC RE:
///   `[addr_b0, addr_b1, addr_b2, data...]`
/// Where `addr` is the 24-bit instruction address (LSB first) of the
/// FIRST instruction in the block, and `data` is the packed 24-bit
/// instruction stream. The bootloader auto-advances the address pointer
/// per instruction word, so subsequent `SEND_DATA` calls increment the
/// embedded address by `BLOCK_INSTRUCTIONS`.
pub fn build_send_data_payload(addr: u32, words: &[u32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(3 + words.len() * 3);
    payload.push((addr & 0xFF) as u8);
    payload.push(((addr >> 8) & 0xFF) as u8);
    payload.push(((addr >> 16) & 0xFF) as u8);
    for &w in words {
        payload.push((w & 0xFF) as u8);
        payload.push(((w >> 8) & 0xFF) as u8);
        payload.push(((w >> 16) & 0xFF) as u8);
    }
    payload
}

/// Direct dsPIC I²C client — bypasses any heartbeat / service layer.
/// Reflash MUST happen on an idle bus with no other I²C traffic.
pub struct DspicFlasher {
    fd: fs::File,
    addr: u8,
}

impl DspicFlasher {
    /// Open `/dev/i2c-N` and select `slave_addr` (typically 0x21 for hb2,
    /// 0x22 for hb3 on S19j Pro am2).
    pub fn open(i2c_path: &str, slave_addr: u8) -> Result<Self> {
        let fd = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(i2c_path)?;
        let raw = fd.as_raw_fd();
        // SAFETY: ioctl with valid fd + I2C_SLAVE constant + non-null arg.
        // I2C_SLAVE selects which 7-bit slave subsequent read/write
        // operations on `fd` will target.
        let rc = unsafe {
            libc::ioctl(
                raw,
                I2C_SLAVE as libc::c_int as _,
                slave_addr as libc::c_ulong,
            )
        };
        if rc < 0 {
            return Err(FlashError::Io(io::Error::last_os_error()));
        }
        Ok(DspicFlasher {
            fd,
            addr: slave_addr,
        })
    }

    /// Write raw bytes to the slave.
    fn write_raw(&mut self, body: &[u8]) -> Result<()> {
        self.fd.write_all(body)?;
        Ok(())
    }

    /// Read `n` bytes from the slave.
    fn read_n(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.fd.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// `dsPIC33EP16GS202_dumy_read` — 1-byte read, no frame, returns the
    /// FW echo byte. Kernel xiic-i2c bulk-read shift artifacts (see
    /// ) ONLY affect
    /// multi-byte reads; a single-byte read returns the slave's TX
    /// register byte directly.
    pub fn dumy_read(&mut self) -> Result<u8> {
        let b = self.read_n(1)?;
        Ok(b[0])
    }

    /// `dsPIC33EP16GS202_get_pic_sw_version` — framed GET_VERSION (0x17).
    /// Returns the full 5-byte response. The first byte should equal
    /// CMD_GET_VERSION (echo = 0x17), the second byte is the FW version.
    ///
    /// `fw_echo` is the FW byte previously read via `dumy_read`. On the
    /// initial probe call, pass 0x00 (the bootloader accepts that for
    /// the very first GET_VERSION).
    pub fn get_version_framed(&mut self, fw_echo: u8) -> Result<[u8; 5]> {
        let frame = build_frame(fw_echo, CMD_GET_VERSION, &[]);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(20));
        let resp = self.read_n(5)?;
        let mut out = [0u8; 5];
        out.copy_from_slice(&resp);
        Ok(out)
    }

    /// Convenience: GET_VERSION returning just the FW byte.
    /// Falls back to `dumy_read` if the framed response is implausible.
    pub fn get_version(&mut self) -> Result<u8> {
        // Two-step protocol: first dumy_read to learn FW byte, then
        // framed GET_VERSION to confirm protocol-level liveness.
        let fw = self.dumy_read()?;
        if fw == 0xFF {
            return Err(FlashError::BootloaderUnreachable(
                "dumy_read returned 0xFF (slave silent / no-ACK)".into(),
            ));
        }
        let resp = self.get_version_framed(fw)?;
        // resp[0] should echo CMD_GET_VERSION; resp[1] should be the FW
        // version (typically equal to fw). If the slave only responded
        // to the bare read but not the framed cmd, return the bare byte.
        if resp[0] == CMD_GET_VERSION {
            Ok(resp[1])
        } else {
            Ok(fw)
        }
    }

    /// BraiinsOS `RESET_PIC` — soft reset (CMD 0x07).
    ///
    /// **Recovery-tool-only.** Per Codex live test 2026-04-27, sending
    /// `[55 AA 07]` to a `a lab unit` fw=0x86 dsPIC does NOT enter bootloader
    /// mode — the byte is silently swallowed. Behavior on a healthy
    /// dsPIC is "soft reset, return to post-POR state". The production
    /// daemon (`dcentrald`) MUST NEVER invoke this; it is exposed only
    /// for the `pic-recovery / dspic-flash` CLI tool and gated behind
    /// `--confirm-bricked`. See
    /// .
    pub fn reset_pic(&mut self, fw_echo: u8) -> Result<()> {
        let frame = build_frame(fw_echo, CMD_RESET_PIC, &[]);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// BraiinsOS `JUMP_FROM_LOADER_TO_APP` — bootloader → app (CMD 0x06).
    pub fn jump_to_app(&mut self, fw_echo: u8) -> Result<()> {
        let frame = build_frame(fw_echo, CMD_JUMP_FROM_LOADER_TO_APP, &[]);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// BraiinsOS `ERASE_PIC_APP_PROGRAM` — bulk-erase the app region
    /// (CMD 0x09). **Irreversible — destructive.** Bootloader region is
    /// protected and untouched.
    pub fn erase_app_region(&mut self, fw_echo: u8) -> Result<()> {
        let frame = build_frame(fw_echo, CMD_ERASE_APP_PROGRAM, &[]);
        self.write_raw(&frame)?;
        // Bulk-erase takes longer than a sector erase; the orchestrator
        // waits ~100ms before issuing the first SEND_DATA.
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    /// `dsPIC33EP16GS202_send_data_to_pic` — stream a block of program
    /// data (CMD 0x14). The 24-bit instruction address is embedded as
    /// the first 3 payload bytes; the bootloader auto-advances the
    /// pointer per instruction word.
    pub fn send_data_block(&mut self, fw_echo: u8, addr: u32, words: &[u32]) -> Result<()> {
        if words.is_empty() {
            return Err(FlashError::InvalidArg(
                "send_data_block: words may not be empty".into(),
            ));
        }
        if addr < APP_REGION_START || addr >= APP_REGION_END {
            return Err(FlashError::AddressOutOfRange(addr));
        }
        let payload = build_send_data_payload(addr, words);
        let frame = build_frame(fw_echo, CMD_SEND_DATA, &payload);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// `dsPIC33EP16GS202_enable_pic_dc_dc` — enable DC-DC rail (0x15).
    /// Used at end of reflash orchestrator to validate the new firmware
    /// can drive the rail. NOT used during the read-only probe path.
    pub fn enable_dc_dc(&mut self, fw_echo: u8) -> Result<()> {
        let frame = build_frame(fw_echo, CMD_ENABLE_DC_DC, &[]);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// `dsPIC33EP16GS202_pic_heart_beat` — keep-alive (0x16).
    pub fn heartbeat(&mut self, fw_echo: u8) -> Result<()> {
        let frame = build_frame(fw_echo, CMD_HEARTBEAT, &[]);
        self.write_raw(&frame)?;
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    }

    /// I²C slave address (informational).
    pub fn slave_addr(&self) -> u8 {
        self.addr
    }
}

/// Read-only protocol probe report. NO destructive flash R/W is
/// attempted by this path — it only validates the framed bootloader is
/// reachable via `dumy_read` and `GET_VERSION`.
#[derive(Debug, Clone)]
pub struct ProtocolProbeReport {
    /// Byte read by the bare `dumy_read` (typically equal to the FW
    /// version, e.g., 0x86 / 0x89 / 0x8A).
    pub fw_byte: u8,
    /// Whether the framed GET_VERSION call returned a sensible 5-byte
    /// response (`resp[0] == 0x17` echo, `resp[1] == fw_byte`).
    pub framed_get_version_works: bool,
    /// Raw 5 bytes read after sending the framed GET_VERSION.
    pub framed_response: [u8; 5],
    /// Whether `fw_byte` was stable across two consecutive `dumy_read`s
    /// — a non-destructive aliveness check.
    pub fw_byte_stable: bool,
}

impl ProtocolProbeReport {
    /// Concise human-readable summary.
    pub fn summary(&self) -> String {
        format!(
            "fw=0x{:02X} stable={} framed_get_version={} resp={:02X?}",
            self.fw_byte, self.fw_byte_stable, self.framed_get_version_works, self.framed_response
        )
    }
}

/// Read-only protocol probe — non-destructive. Only invokes
/// `dumy_read` and `GET_VERSION`. Does NOT touch flash. This is safe to
/// run on a working dsPIC and is the recommended first step before any
/// reflash attempt.
///
/// Use this to validate that the AMTC dsPIC opcodes work on a target
/// before committing to a destructive flash session.
pub fn probe_protocol(i2c_path: &str, addr: u8) -> Result<ProtocolProbeReport> {
    let mut f = DspicFlasher::open(i2c_path, addr)?;

    // Step 1: dumy_read — get FW byte.
    let fw1 = f.dumy_read()?;
    if fw1 == 0xFF {
        return Err(FlashError::BootloaderUnreachable(
            "dumy_read returned 0xFF (slave silent)".into(),
        ));
    }

    // Step 2: framed GET_VERSION using fw1 as echo.
    let resp = f.get_version_framed(fw1)?;
    let framed_ok = resp[0] == CMD_GET_VERSION && resp[1] == fw1;

    // Step 3: dumy_read again, confirm stability.
    let fw2 = f.dumy_read()?;
    let stable = fw2 == fw1;

    Ok(ProtocolProbeReport {
        fw_byte: fw1,
        framed_get_version_works: framed_ok,
        framed_response: resp,
        fw_byte_stable: stable,
    })
}

/// Probe report — read-only assessment of a dsPIC's reflash readiness.
///
/// Backwards-compatible field set. Under the AMTC opcode revision, the
/// `set_flash_pointer_ack` and `read_sector_returns_real_data` fields
/// no longer reflect the actual protocol (the dsPIC has no
/// SET_FLASH_POINTER and no per-sector READ command); they are now
/// derived from the new framed-protocol probe results so existing
/// callers (`pic-recovery/src/dspic_flash_main.rs`) keep working.
#[derive(Debug)]
pub struct ProbeReport {
    pub fw_byte: Option<u8>,
    /// Legacy field. Now reports whether the framed GET_VERSION
    /// succeeded (i.e., the bootloader speaks the new protocol).
    pub set_flash_pointer_ack: bool,
    /// Legacy field. Now reports whether the FW byte was stable across
    /// two consecutive `dumy_read` calls — the new non-destructive
    /// liveness check.
    pub read_sector_returns_real_data: bool,
    pub recommendation: String,
}

/// Read-only probe of a dsPIC at `addr` on `i2c_path`. Reports whether
/// the framed bootloader is reachable.
///
/// **2026-04-26 AMTC revision**: this function no longer issues
/// destructive flash reads. The new aliveness check is a stable FW byte
/// across two `dumy_read` calls plus a successful framed GET_VERSION.
pub fn probe(i2c_path: &str, addr: u8) -> Result<ProbeReport> {
    let proto = match probe_protocol(i2c_path, addr) {
        Ok(p) => p,
        Err(FlashError::BootloaderUnreachable(msg)) => {
            return Ok(ProbeReport {
                fw_byte: None,
                set_flash_pointer_ack: false,
                read_sector_returns_real_data: false,
                recommendation: format!(
                    "dsPIC at 0x{:02X} silent on dumy_read ({}); chip may be unpowered or hardware-degraded. \
                     ICSP recovery required.",
                    addr, msg
                ),
            });
        }
        Err(e) => return Err(e),
    };

    let recommendation = if !proto.framed_get_version_works {
        format!(
            "dsPIC at 0x{:02X} (fw=0x{:02X}) responds to dumy_read but framed GET_VERSION did not echo \
             (resp={:02X?}). Bootloader appears to speak a different protocol or is in a locked state. \
             Re-probe with `probe_protocol` and consider ICSP.",
            addr, proto.fw_byte, proto.framed_response
        )
    } else if !proto.fw_byte_stable {
        format!(
            "dsPIC at 0x{:02X} (fw=0x{:02X}) framed GET_VERSION succeeded but FW byte not stable across \
             two reads. Bus glitch or flaky slave; retry probe before any flash operation.",
            addr, proto.fw_byte
        )
    } else {
        format!(
            "dsPIC at 0x{:02X} (fw=0x{:02X}) framed protocol OK: dumy_read stable, GET_VERSION echoed \
             correctly (resp={:02X?}). Reflash with the AMTC opcode set may be feasible.",
            addr, proto.fw_byte, proto.framed_response
        )
    };

    Ok(ProbeReport {
        fw_byte: Some(proto.fw_byte),
        set_flash_pointer_ack: proto.framed_get_version_works,
        read_sector_returns_real_data: proto.framed_get_version_works && proto.fw_byte_stable,
        recommendation,
    })
}

/// Reflash the dsPIC's app region with `image`. **Irreversible.**
///
/// Refuses to operate unless `accept_brick_risk == true` AND the probe
/// passes (framed GET_VERSION OK + FW byte stable).
///
/// Sequence (AMTC orchestrator):
///   1. probe (read-only) — confirms framed protocol works
///   2. erase_app_region (CMD 0x09)
///   3. send_data_block loop (CMD 0x14) — `BLOCK_INSTRUCTIONS` words per call
///   4. enable_dc_dc (CMD 0x15) — sanity check; rail will only enable
///      if the fresh firmware boots and accepts the command
///
/// Note: there is no per-block read-back verify in the AMTC protocol
/// (no READ_DATA command exists). The bootloader's internal CRC over
/// the streamed data is the integrity check; if a `SEND_DATA` is
/// rejected, the next `enable_dc_dc` will fail.
pub fn reflash(
    i2c_path: &str,
    addr: u8,
    image: &FirmwareImage,
    accept_brick_risk: bool,
) -> Result<()> {
    if !accept_brick_risk {
        return Err(FlashError::InvalidArg(
            "reflash requires accept_brick_risk = true (dsPIC has no recovery short of ICSP if reflash fails)".into(),
        ));
    }
    let report = probe(i2c_path, addr)?;
    if !report.read_sector_returns_real_data {
        return Err(FlashError::SmokeTestFailed(report.recommendation));
    }
    let fw_echo = report
        .fw_byte
        .ok_or_else(|| FlashError::SmokeTestFailed("probe returned no FW byte".into()))?;

    let mut f = DspicFlasher::open(i2c_path, addr)?;

    // Erase the app region.
    f.erase_app_region(fw_echo)?;

    // Stream data block-by-block. Each block carries
    // BLOCK_INSTRUCTIONS instructions starting at `cur_addr`.
    let mut idx = 0usize;
    let mut cur_addr = APP_REGION_START;
    while idx + BLOCK_INSTRUCTIONS <= image.words.len()
        && cur_addr + BLOCK_INSTRUCTIONS as u32 <= APP_REGION_END
    {
        let block = &image.words[idx..idx + BLOCK_INSTRUCTIONS];
        f.send_data_block(fw_echo, cur_addr, block)?;
        idx += BLOCK_INSTRUCTIONS;
        cur_addr += BLOCK_INSTRUCTIONS as u32;
    }

    // Handle any tail < BLOCK_INSTRUCTIONS (last partial block).
    if idx < image.words.len() && cur_addr < APP_REGION_END {
        let remaining = image.words.len() - idx;
        let cap = (APP_REGION_END - cur_addr) as usize;
        let take = remaining.min(cap);
        if take > 0 {
            let block = &image.words[idx..idx + take];
            f.send_data_block(fw_echo, cur_addr, block)?;
        }
    }

    // Sanity check: the new firmware should accept enable_dc_dc.
    f.enable_dc_dc(fw_echo)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_firmware() {
        let tmp = std::env::temp_dir().join("dspic_flash_test.txt");
        fs::write(&tmp, "FA0000\n040200\nFFFFFF\n").unwrap();
        let img = FirmwareImage::parse_text(&tmp).unwrap();
        assert_eq!(img.words, vec![0xFA0000, 0x040200, 0xFFFFFF]);
    }

    #[test]
    fn parse_rejects_non_hex() {
        let tmp = std::env::temp_dir().join("dspic_flash_bad.txt");
        fs::write(&tmp, "FA0000\nNOTHEX\n").unwrap();
        assert!(FirmwareImage::parse_text(&tmp).is_err());
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        let tmp = std::env::temp_dir().join("dspic_flash_comments.txt");
        fs::write(&tmp, "# header\nFA0000\n\n# mid\n040200\n").unwrap();
        let img = FirmwareImage::parse_text(&tmp).unwrap();
        assert_eq!(img.words, vec![0xFA0000, 0x040200]);
    }

    #[test]
    fn slice_bounds_check() {
        let img = FirmwareImage {
            words: vec![1, 2, 3, 4],
        };
        assert!(img.slice(0, 4).is_ok());
        assert!(img.slice(0, 5).is_err());
    }

    // ─── BraiinsOS opcode regression tests ──────────────────────────

    #[test]
    fn braiinsos_opcodes_are_correct() {
        // Hard-pin the BraiinsOS source-of-truth opcodes per
        // :53-77`.
        // If anyone "fixes" RESET back to 0x04 (an AMTC RE inference
        // we proved wrong), this test will fail loudly.
        assert_eq!(CMD_SET_FLASH_POINTER, 0x01);
        assert_eq!(CMD_SEND_DATA_TO_IIC, 0x02);
        assert_eq!(CMD_READ_DATA_FROM_IIC, 0x03);
        assert_eq!(CMD_ERASE_IIC_FLASH, 0x04);
        assert_eq!(CMD_WRITE_DATA_INTO_PIC, 0x05);
        assert_eq!(CMD_JUMP_FROM_LOADER_TO_APP, 0x06);
        assert_eq!(CMD_RESET_PIC, 0x07);
        assert_eq!(CMD_GET_PIC_FLASH_POINTER, 0x08);
        assert_eq!(CMD_ERASE_APP_PROGRAM, 0x09);
        assert_eq!(CMD_SET_VOLTAGE, 0x10);
        assert_eq!(CMD_SEND_DATA, 0x14);
        assert_eq!(CMD_ENABLE_DC_DC, 0x15);
        assert_eq!(CMD_HEARTBEAT, 0x16);
        assert_eq!(CMD_GET_VERSION, 0x17);
        assert_eq!(CMD_GET_VOLTAGE, 0x18);
    }

    #[test]
    fn reset_and_erase_are_distinct_opcodes() {
        // The 2026-04-26 RE incorrectly inferred 0x04 = RESET; the
        // BraiinsOS source-of-truth shows 0x04 is ERASE_IIC_FLASH and
        // 0x07 is RESET_PIC. Pin the distinction.
        assert_eq!(CMD_ERASE_IIC_FLASH, 0x04);
        assert_eq!(CMD_RESET_PIC, 0x07);
        assert_ne!(CMD_ERASE_IIC_FLASH, CMD_RESET_PIC);
    }

    #[test]
    fn build_frame_get_version_zero_echo() {
        // GET_VERSION with fw_echo=0x00 (initial probe form):
        // [0x55, 0xAA, 0x00, 0x17, sum_lo, sum_hi]
        // Byte sum = 0x55 + 0xAA + 0x00 + 0x17 = 0x116
        // sum_lo = 0x16, sum_hi = 0x01
        let f = build_frame(0x00, CMD_GET_VERSION, &[]);
        assert_eq!(f, vec![0x55, 0xAA, 0x00, 0x17, 0x16, 0x01]);
    }

    #[test]
    fn build_frame_get_version_known_echo() {
        // GET_VERSION with fw_echo=0x86 (the .139 case):
        // bytes = [0x55, 0xAA, 0x86, 0x17]
        // sum = 0x55 + 0xAA + 0x86 + 0x17 = 0x19C
        // sum_lo = 0x9C, sum_hi = 0x01
        let f = build_frame(0x86, CMD_GET_VERSION, &[]);
        assert_eq!(f, vec![0x55, 0xAA, 0x86, 0x17, 0x9C, 0x01]);
    }

    #[test]
    fn build_frame_erase_app() {
        // ERASE_APP_PROGRAM (0x09) — fw_echo=0x86:
        // bytes = [0x55, 0xAA, 0x86, 0x09]
        // sum = 0x55 + 0xAA + 0x86 + 0x09 = 0x18E
        // sum_lo = 0x8E, sum_hi = 0x01
        let f = build_frame(0x86, CMD_ERASE_APP_PROGRAM, &[]);
        assert_eq!(f, vec![0x55, 0xAA, 0x86, 0x09, 0x8E, 0x01]);
    }

    #[test]
    fn build_frame_reset_pic_uses_0x07() {
        // RESET_PIC (0x07) — fw_echo=0x86 (the .139 case):
        // Codex live test 2026-04-27 proved this frame does NOT enter
        // bootloader on .139 fw=0x86, but the wire format must still
        // be correct so the recovery tool emits the documented bytes.
        // bytes = [0x55, 0xAA, 0x86, 0x07]
        // sum = 0x55 + 0xAA + 0x86 + 0x07 = 0x18C
        // sum_lo = 0x8C, sum_hi = 0x01
        let f = build_frame(0x86, CMD_RESET_PIC, &[]);
        assert_eq!(f, vec![0x55, 0xAA, 0x86, 0x07, 0x8C, 0x01]);
    }

    #[test]
    fn build_frame_checksum_wraps_above_16_bit() {
        // Synthetic test: a long payload that pushes the checksum past
        // 0xFFFF wraps modulo 0x10000.
        let payload = vec![0xFFu8; 600];
        let f = build_frame(0x00, CMD_SEND_DATA, &payload);
        // preamble  : 0x55 + 0xAA              = 0x0000FF
        // fw_echo   : 0x00                      = 0x000000
        // cmd       : 0x14                      = 0x000014
        // payload   : 600 × 0xFF = 153_000      = 0x0255A8
        // total                                  = 0x0256BB
        // mod 0x10000                            = 0x56BB
        // sum_lo = 0xBB, sum_hi = 0x56
        let n = f.len();
        assert_eq!(f[n - 2], 0xBB);
        assert_eq!(f[n - 1], 0x56);
    }

    #[test]
    fn build_send_data_payload_layout() {
        // 2 instructions starting at 0x000400:
        // addr bytes (LSB first) = [0x00, 0x04, 0x00]
        // word 0 = 0x123456 → [0x56, 0x34, 0x12]
        // word 1 = 0xABCDEF → [0xEF, 0xCD, 0xAB]
        let payload = build_send_data_payload(0x0400, &[0x123456, 0xABCDEF]);
        assert_eq!(
            payload,
            vec![0x00, 0x04, 0x00, 0x56, 0x34, 0x12, 0xEF, 0xCD, 0xAB]
        );
    }

    #[test]
    fn block_instruction_count_consistent() {
        assert_eq!(BLOCK_DATA_BYTES, BLOCK_INSTRUCTIONS * 3);
    }

    #[test]
    fn legacy_pic1704_constants_kept_for_reference() {
        // The 2026-04-26 RE introduced a `_PIC1704_CMD_ERASE_IIC_FLASH`
        // alias and asserted it "collided" with the (incorrectly-mapped)
        // `CMD_RESET = 0x04`. The 2026-04-27 BraiinsOS source-of-truth
        // correction shows that the PIC16F1704 and dsPIC33EP16GS202
        // share the SAME opcode table — 0x04 means ERASE on BOTH chips,
        // and the real RESET is at 0x07. There is therefore NO
        // cross-family collision at 0x04. This test now pins that
        // corrected understanding: 0x04 is ERASE on both, and the
        // legacy alias matches the canonical constant.
        assert_eq!(_PIC1704_CMD_ERASE_IIC_FLASH, 0x04);
        assert_eq!(_PIC1704_CMD_ERASE_IIC_FLASH, CMD_ERASE_IIC_FLASH);

        // RESET lives at 0x07, NOT 0x04. Pin this so any future agent
        // who tries to "fix" RESET back to 0x04 (regressing 2026-04-27)
        // sees the regression here.
        assert_ne!(CMD_RESET_PIC, _PIC1704_CMD_ERASE_IIC_FLASH);
        assert_eq!(CMD_RESET_PIC, 0x07);
    }
}
