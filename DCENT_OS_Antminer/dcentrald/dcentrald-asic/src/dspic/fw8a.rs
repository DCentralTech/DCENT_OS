//! dsPIC firmware 0x8A — alternate framed protocol.
//!
//! Also covers fw=0xB9 and fw=0xFE (BraiinsOS/VNish family). All use the
//! canonical framed 6-byte ENABLE/DISABLE form rather than the 7-byte
//! VnishPadded form because VNish RE coverage for those firmware bytes is
//! incomplete and the 7-byte form has not been live-proven for them —
//! flipping unilaterally would risk regressing working units.
//!
//! Per memory rule , RESET (0x07) is BANNED on
//! all framed dsPIC variants except fw=0x82.
//!
//! Wire format:
//!   * `set_voltage`:    `[55 AA 04 10 DAC SUM]`.
//!   * `enable_voltage`: canonical `[55 AA 04 15 01 1A]` — ACK `[15 00]`
//!     or `[15 01]` (both accepted; VNish ACKs with 0x01).
//!   * `disable_voltage`: canonical `[55 AA 04 15 00 19]`.
//!   * `heartbeat`:      `[55 AA 04 16 00 1A]`.
//!   * `read_temp`:      `[55 AA 04 30 sensor_addr SUM]`.

use super::{
    framed_voltage_dac, CMD_ENABLE_VOLTAGE, CMD_HEARTBEAT, CMD_READ_TEMP, CMD_SET_VOLTAGE,
    DSPIC_PREAMBLE,
};

/// Build the framed SET_VOLTAGE frame for fw=0x8A / 0xB9 / 0xFE.
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

/// Build the canonical 6-byte framed ENABLE_VOLTAGE frame.
/// `[55 AA 04 15 01 1A]` — checksum = (0x04 + 0x15 + 0x01) & 0xFF.
pub(crate) fn enable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_ENABLE_VOLTAGE,
        0x01,
        0x1A,
    ]
}

/// Build the canonical 6-byte framed DISABLE_VOLTAGE frame.
/// `[55 AA 04 15 00 19]`.
pub(crate) fn disable_voltage_frame() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        0x04,
        CMD_ENABLE_VOLTAGE,
        0x00,
        0x19,
    ]
}

/// Build the framed HEARTBEAT frame.
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

/// Build the framed LM75A passthrough READ_TEMP frame.
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
    fn fw8a_set_voltage_is_framed_dac() {
        assert_eq!(
            set_voltage_frame(13_700),
            vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, 0x06, 0x1A]
        );
    }

    #[test]
    fn fw8a_enable_disable_keep_canonical_6_byte_form() {
        // Regression guard: must NOT silently switch to VnishPadded form;
        // VNish RE for 0x8A/0xB9/0xFE is incomplete.
        assert_eq!(
            enable_voltage_frame(),
            &[0x55, 0xAA, 0x04, 0x15, 0x01, 0x1A]
        );
        assert_eq!(
            disable_voltage_frame(),
            &[0x55, 0xAA, 0x04, 0x15, 0x00, 0x19]
        );
    }

    #[test]
    fn fw8a_canonical_checksum() {
        let cksum = |len: u8, cmd: u8, payload: &[u8]| -> u8 {
            len.wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
        };
        assert_eq!(cksum(0x04, 0x15, &[0x01]), 0x1A);
        assert_eq!(cksum(0x04, 0x15, &[0x00]), 0x19);
        assert_eq!(cksum(0x04, 0x16, &[0x00]), 0x1A);
    }

    #[test]
    fn fw8a_heartbeat_is_framed_6_byte() {
        assert_eq!(heartbeat_frame(), &[0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A]);
    }

    #[test]
    fn fw8a_read_temp_is_framed_6_byte() {
        assert_eq!(
            read_temp_frame(0x48),
            vec![0x55, 0xAA, 0x04, 0x30, 0x48, 0x7C]
        );
    }
}
