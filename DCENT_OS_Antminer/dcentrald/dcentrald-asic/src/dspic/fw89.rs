//! dsPIC firmware 0x89 — VNish/BraiinsOS-reflashed S19j Pro framed protocol.
//!
//! fw=0x89 is the primary S19j Pro am2 target. It uses the framed protocol
//! `[55 AA LEN CMD payload SUM]`. Per VNish RE corpus 2026-04-25 (cgminer
//! disasm at VMA 0x05277c, 22 firmwares cross-validated), the
//! ENABLE/DISABLE form is the 7-byte VnishPadded form with status=0x01.
//!
//! Per memory rule , RESET (0x07) is BANNED on
//! fw=0x89 (it permanently downgrades the firmware to fw=0x86). The
//! `dspic_flash` module is gated behind the `recovery-tool` Cargo feature
//! so destructive symbols cannot link into `dcentrald`.
//!
//! Wire format:
//!   * `set_voltage`:  `[55 AA 04 10 DAC SUM]` — framed DAC encoding.
//!   * `enable_voltage`: VnishPadded `[55 AA 05 15 01 00 1B]`. ACK is
//!     `[15 01]` (status=0x01, NOT 0x00).
//!   * `disable_voltage`: VnishPadded `[55 AA 05 15 00 00 1A]`.
//!   * `heartbeat`: framed `[55 AA 04 16 00 1A]`.
//!   * `read_temp`: framed `[55 AA 04 30 sensor_addr SUM]`.

use super::{
    framed_voltage_dac, CMD_ENABLE_VOLTAGE, CMD_HEARTBEAT, CMD_READ_TEMP, CMD_SET_VOLTAGE,
    DSPIC_PREAMBLE,
};

/// Build the framed SET_VOLTAGE frame for fw=0x89.
/// `[55 AA 04 10 DAC SUM]`.
pub(crate) fn set_voltage_frame(voltage_mv: u16) -> Vec<u8> {
    let dac = framed_voltage_dac(voltage_mv);
    let checksum = 0x04u8.wrapping_add(CMD_SET_VOLTAGE).wrapping_add(dac);
    vec![
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_SET_VOLTAGE,
        dac,
        checksum,
    ]
}

/// Build the VnishPadded ENABLE_VOLTAGE frame for fw=0x89.
/// `[55 AA 05 15 01 00 1B]` — ACK is `[15 01]` (status=0x01).
pub(crate) fn enable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x05,
        CMD_ENABLE_VOLTAGE,
        0x01,
        0x00,
        0x1B,
    ]
}

/// Build the VnishPadded DISABLE_VOLTAGE frame for fw=0x89.
/// `[55 AA 05 15 00 00 1A]`.
pub(crate) fn disable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x05,
        CMD_ENABLE_VOLTAGE,
        0x00,
        0x00,
        0x1A,
    ]
}

/// Build the framed HEARTBEAT frame for fw=0x89.
/// `[55 AA 04 16 00 1A]` — checksum = (0x04 + 0x16 + 0x00) & 0xFF.
pub(crate) fn heartbeat_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_HEARTBEAT,
        0x00,
        0x1A,
    ]
}

/// Build the framed LM75A passthrough READ_TEMP frame for fw=0x89.
/// `[55 AA 04 30 sensor_addr SUM]`.
pub(crate) fn read_temp_frame(sensor_addr: u8) -> Vec<u8> {
    let checksum = 0x04u8.wrapping_add(CMD_READ_TEMP).wrapping_add(sensor_addr);
    vec![
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_READ_TEMP,
        sensor_addr,
        checksum,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw89_set_voltage_is_framed_dac() {
        assert_eq!(
            set_voltage_frame(13_700),
            vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, 0x06, 0x1A]
        );
    }

    #[test]
    fn fw89_enable_disable_use_vnish_padded_form() {
        assert_eq!(
            enable_voltage_frame(),
            &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B]
        );
        assert_eq!(
            disable_voltage_frame(),
            &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
        );
    }

    #[test]
    fn fw89_heartbeat_is_framed_6_byte() {
        assert_eq!(heartbeat_frame(), &[0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A]);
    }

    #[test]
    fn fw89_read_temp_is_framed_6_byte() {
        assert_eq!(
            read_temp_frame(0x48),
            vec![0x55, 0xAA, 0x04, 0x30, 0x48, 0x7C]
        );
    }
}
