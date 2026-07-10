//! Stock Bitmain PIC16F1704 firmware (versions 0x56 / 0x5A / 0x5E).
//!
//! All PIC16F1704 variants — Stock Bitmain AND BraiinsOS-reflashed — share
//! the SAME app-mode command set. The historical "bmminer" command IDs
//! (0x03/0x02/0x11) were for the dsPIC33EP (S17) or for bootloader mode,
//! NOT for PIC16F1704 app mode. This was confirmed by the Bitmain AMTC
//! official `single-board-test` binary and is documented inline as
//! deprecated `CMD_*` constants in `pic/mod.rs`.
//!
//! Stock-specific behavior:
//!   * Detection: stock PICs return version byte `0x56` / `0x5A` / `0x5E`
//!     in app mode. Raw I²C read in app mode returns the version byte;
//!     in bootloader mode returns `0xCC`.
//!   * Watchdog: ~1 minute (vs ~10 s for BraiinsOS). Heartbeat cadence
//!     should still be ~1 s — the matrix cap is in
//!     `dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`.
//!   * Detection probe: stock cmd `0x04` (GET_VERSION) is SAFE only AFTER
//!     ruling out BraiinsOS firmware (where 0x04 = ERASE_IIC_FLASH —
//!     dangerous).
//!   * Bootloader JUMP (0x06): stock bootloader on some units does NOT
//!     respond to JUMP; PIC stays at 0xCC indefinitely. Recovery requires
//!     reflashing the PIC with BraiinsOS firmware via S05pic_recovery.
//!   * BraiinsOS-byte-by-byte writes corrupt the stock PIC parser; send
//!     stock heartbeat FIRST (single transaction) before any byte-by-byte
//!     traffic.
//!
//! All other behavior is shared with BraiinsOS via the unified
//! `BRAIINS_CMD_*` constants in `pic/mod.rs` (the constant naming is
//! historical — the commands are the unified PIC16F1704 app-mode set).

/// Stock Bitmain PIC: GET_VERSION command (0x04).
///
/// Returns version byte 0x56/0x5A/0x5E in app mode, or 0xCC if the PIC is
/// stuck in bootloader. SAFE only on confirmed-stock units; on BraiinsOS
/// PICs cmd 0x04 maps to `ERASE_IIC_FLASH` and is destructive.
pub const CMD_GET_VERSION: u8 = 0x04;

/// Stock PIC version bytes (matches `PicFirmware::Stock(_)` in `pic/mod.rs`).
pub const STOCK_VERSION_BYTES: [u8; 3] = [0x56, 0x5A, 0x5E];

/// Returns true if `version` is a stock-Bitmain firmware version byte.
pub fn is_stock_version(version: u8) -> bool {
    STOCK_VERSION_BYTES.contains(&version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_version_byte_recognition() {
        assert!(is_stock_version(0x56));
        assert!(is_stock_version(0x5A));
        assert!(is_stock_version(0x5E));
        assert!(!is_stock_version(0x03)); // BraiinsOS
        assert!(!is_stock_version(0xCC)); // bootloader
        assert!(!is_stock_version(0x60)); // app-mode marker
        assert!(!is_stock_version(0x00));
        assert!(!is_stock_version(0xFF));
    }

    #[test]
    fn stock_get_version_command_id_is_0x04() {
        // Cmd 0x04 is SAFE on stock but DESTRUCTIVE on BraiinsOS PIC
        // (where 0x04 = ERASE_IIC_FLASH). Detection must rule out BraiinsOS
        // via cmd 0x17 first.
        assert_eq!(CMD_GET_VERSION, 0x04);
    }
}
