//! dsPIC firmware 0x82 — bare protocol, older S19 Pro / healthy am2 fleet.
//!
//! Wire format (no LEN, no checksum):
//!   * `set_voltage`:    `[55 AA 10 HI LO]` — 16-bit big-endian millivolts.
//!   * `enable_voltage`: `[55 AA 15 01]`.
//!   * `disable_voltage`: `[55 AA 15 00]`.
//!   * `heartbeat`:      `[55 AA 16]`.
//!   * `read_temp`:      `[55 AA 30 sensor_addr]`.
//!
//! Per memory rule , fw=0x82 is the canonical
//! framed-bare baseline; RESET (0x07) and JUMP_TO_APP (0x06) are still
//! permitted on this revision (legacy bootloader-control path).
//!
//! Per memory rule , bare-mode
//! reads return a single firmware-echo byte; multi-byte read tails are
//! kernel `xiic-i2c` shift-left artifacts and must be rejected.

use super::{CMD_ENABLE_VOLTAGE, CMD_HEARTBEAT, CMD_READ_TEMP, CMD_SET_VOLTAGE, DSPIC_PREAMBLE};

/// Build the bare-protocol SET_VOLTAGE frame.
/// `[55 AA 10 HI LO]` — millivolts encoded big-endian (no DAC table).
pub(crate) fn set_voltage_frame(voltage_mv: u16) -> Vec<u8> {
    let hi = (voltage_mv >> 8) as u8;
    let lo = (voltage_mv & 0xFF) as u8;
    vec![
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_SET_VOLTAGE,
        hi,
        lo,
    ]
}

/// Build the bare-protocol ENABLE_VOLTAGE frame: `[55 AA 15 01]`.
pub(crate) fn enable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_ENABLE_VOLTAGE,
        0x01,
    ]
}

/// Build the bare-protocol DISABLE_VOLTAGE frame: `[55 AA 15 00]`.
pub(crate) fn disable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_ENABLE_VOLTAGE,
        0x00,
    ]
}

/// Build the bare-protocol HEARTBEAT frame: `[55 AA 16]`.
pub(crate) fn heartbeat_frame() -> &'static [u8] {
    &[DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_HEARTBEAT]
}

/// Build the bare-protocol LM75A passthrough READ_TEMP frame.
/// `[55 AA 30 sensor_addr]` (no LEN/checksum).
pub(crate) fn read_temp_frame(sensor_addr: u8) -> Vec<u8> {
    vec![
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_READ_TEMP,
        sensor_addr,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw82_set_voltage_uses_be_millivolts_not_dac_table() {
        // 13.80 V = 13800 mV → [55 AA 10 0x35 0xE8].
        // This is the only fw revision that uses BE millivolts; fw=0x86/0x89/0x8A
        // and friends use the framed DAC-encoded form.
        assert_eq!(
            set_voltage_frame(13_800),
            vec![0x55, 0xAA, CMD_SET_VOLTAGE, 0x35, 0xE8]
        );
    }

    #[test]
    fn fw82_enable_disable_are_4_byte_bare_form() {
        assert_eq!(enable_voltage_frame(), &[0x55, 0xAA, 0x15, 0x01]);
        assert_eq!(disable_voltage_frame(), &[0x55, 0xAA, 0x15, 0x00]);
    }

    #[test]
    fn fw82_heartbeat_is_3_byte_bare() {
        assert_eq!(heartbeat_frame(), &[0x55, 0xAA, 0x16]);
    }

    #[test]
    fn fw82_read_temp_is_4_byte_bare() {
        assert_eq!(read_temp_frame(0x48), vec![0x55, 0xAA, 0x30, 0x48]);
        assert_eq!(read_temp_frame(0x4B), vec![0x55, 0xAA, 0x30, 0x4B]);
    }
}
