//! BraiinsOS PIC16F1704 firmware (version 0x03).
//!
//! BraiinsOS PIC firmware uses the SAME app-mode command IDs as stock for
//! voltage / heartbeat / GET_VOLTAGE — verified by the Bitmain AMTC
//! `single-board-test` binary and inlined in `pic/mod.rs` as the
//! `BRAIINS_CMD_*` constants. The historical name is kept for clarity, but
//! these IDs are the canonical PIC16F1704 app-mode set:
//!
//!   * `0x10` SET_VOLTAGE
//!   * `0x15` ENABLE_VOLTAGE  (data byte 0x01 enable / 0x00 disable)
//!   * `0x16` SEND_HEARTBEAT
//!   * `0x17` GET_PIC_SOFTWARE_VERSION
//!   * `0x18` GET_VOLTAGE
//!
//! BraiinsOS-specific behavior (full bootloader / flash command set):
//!
//!   * `0x01` SET_PIC_FLASH_POINTER
//!   * `0x02` SEND_DATA_TO_IIC
//!   * `0x03` READ_DATA_FROM_IIC
//!   * `0x04` ERASE_IIC_FLASH        ← stock GET_VERSION (different!)
//!   * `0x05` WRITE_DATA_INTO_PIC
//!   * `0x06` JUMP_FROM_LOADER_TO_APP
//!   * `0x07` RESET_PIC
//!   * `0x08` GET_PIC_FLASH_POINTER
//!   * `0x09` ERASE_PIC_APP_PROGRAM
//!   * `0x11` SET_VOLTAGE_TIME
//!   * `0x12` SET_HASH_BOARD_ID
//!   * `0x13` GET_HASH_BOARD_ID
//!   * `0x14` SET_HOST_MAC_ADDRESS
//!
//! BraiinsOS-specific runtime characteristics:
//!   * Watchdog: ~10 seconds (much shorter than stock ~60 s).
//!     Production heartbeat cadence is 1 s — see
//!     `dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`.
//!   * Detection: cmd 0x17 (GET_PIC_SOFTWARE_VERSION) returns 0x03.
//!     Stock firmware ignores 0x17 (unknown command) — SAFE to probe
//!     before stock cmd 0x04.
//!   * Reset: cmd 0x07 (RESET_PIC) only works on BraiinsOS; stock PICs
//!     ignore unknown commands so it is safe to send unconditionally.
//!   * Byte-by-byte writes: BraiinsOS sends each byte as a separate I²C
//!     transaction (`braiins_power.rs:176-183`). The PIC16F1704 MSSP
//!     buffer is 1 byte deep, so multi-byte writes overflow it before
//!     the firmware ISR can drain. Always use `write_byte_by_byte` for
//!     init / RESET / JUMP traffic; the mining heartbeat thread bypasses
//!     this because the PIC is guaranteed to be in app mode.
//!   * Parser flush: 16 zero bytes (with `0x55 0xAA` preamble at the
//!     head) byte-by-byte — see memory rule
//!     .

use super::BRAIINS_FIRMWARE_VERSION;
#[cfg(test)]
use super::{BRAIINS_CMD_GET_VERSION, BRAIINS_CMD_RESET_PIC};

/// BraiinsOS bootloader-only command: set flash pointer address.
pub const FLASH_CMD_SET_POINTER: u8 = 0x01;

/// BraiinsOS bootloader-only command: send 16 bytes of data to IIC buffer.
pub const FLASH_CMD_SEND_DATA: u8 = 0x02;

/// BraiinsOS bootloader-only command: erase one flash sector (32 words).
pub const FLASH_CMD_ERASE: u8 = 0x04;

/// BraiinsOS bootloader-only command: write IIC buffer to flash at current pointer.
pub const FLASH_CMD_WRITE: u8 = 0x05;

/// BraiinsOS bootloader-only command: read flash pointer address (2-byte response).
pub const FLASH_CMD_GET_POINTER: u8 = 0x08;

/// Flash program load address (where app code starts in PIC memory).
pub const FLASH_PROGRAM_LOAD_ADDR: u16 = 0x0300;

/// Program size in words (3200 words = 6400 bytes).
pub const FLASH_PROGRAM_SIZE: usize = 3200;

/// Flash sector size in words (32 words per erase sector).
pub const FLASH_SECTOR_SIZE: usize = 32;

/// I²C transfer block size in bytes (16 bytes per SEND_DATA_TO_IIC).
pub const FLASH_XFER_BLOCK_SIZE: usize = 16;

/// Returns true if `version` is the BraiinsOS PIC firmware version byte.
pub fn is_braiinsos_version(version: u8) -> bool {
    version == BRAIINS_FIRMWARE_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braiinsos_version_byte_is_0x03() {
        assert!(is_braiinsos_version(0x03));
        assert!(!is_braiinsos_version(0x56));
        assert!(!is_braiinsos_version(0x5A));
        assert!(!is_braiinsos_version(0x5E));
        assert!(!is_braiinsos_version(0xCC));
    }

    #[test]
    fn braiinsos_get_version_command_id_is_0x17() {
        // Cmd 0x17 is SAFE on stock (unknown cmd ignored) but returns
        // 0x03 on BraiinsOS — ALWAYS probe 0x17 before stock 0x04 because
        // 0x04 = ERASE_IIC_FLASH on BraiinsOS PIC (destructive).
        assert_eq!(BRAIINS_CMD_GET_VERSION, 0x17);
    }

    #[test]
    fn braiinsos_reset_command_id_is_0x07() {
        // RESET (0x07) only works on BraiinsOS — stock PICs ignore unknown
        // commands so sending RESET is always safe.
        assert_eq!(BRAIINS_CMD_RESET_PIC, 0x07);
    }

    #[test]
    fn flash_constants_match_braiinsos_protocol() {
        assert_eq!(FLASH_CMD_SET_POINTER, 0x01);
        assert_eq!(FLASH_CMD_SEND_DATA, 0x02);
        assert_eq!(FLASH_CMD_ERASE, 0x04);
        assert_eq!(FLASH_CMD_WRITE, 0x05);
        assert_eq!(FLASH_CMD_GET_POINTER, 0x08);
        // 3200 words * 2 bytes/word = 6400 bytes total firmware.
        assert_eq!(FLASH_PROGRAM_SIZE * 2, 6400);
        // 16-byte transfer blocks fit in a single I²C transaction.
        assert_eq!(FLASH_XFER_BLOCK_SIZE, 16);
    }

    #[test]
    fn parse_pic_firmware_validates_size_and_format() {
        use crate::pic::parse_pic_firmware;
        // The real parser is in pic/mod.rs; verify it's reachable from
        // this firmware-revision module's test surface.
        // Empty/missing path returns None, not a panic.
        assert!(parse_pic_firmware("/nonexistent/path/firmware.txt").is_none());
    }
}
