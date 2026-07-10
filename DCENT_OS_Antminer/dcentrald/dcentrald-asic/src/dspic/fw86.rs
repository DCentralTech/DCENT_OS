//! dsPIC firmware 0x86 — corrupted/degraded state.
//!
//! Per memory rules ,
//! , and
//! :
//!
//! * fw=0x86 always returns a single-byte firmware echo (0x86) for ANY
//!   read — including `GET_VOLTAGE` (0x3B). The bus ACKs ENABLE_VOLTAGE
//!   bare commands at the wire level, but rail engagement is NOT
//!   production-trusted.
//! * Voltage commands are refused by default (gated by the
//!   `DCENT_AM2_TRUST_DEGRADED_FW=1` env override for lab work only).
//! * The framed `[55 AA 04 10 DAC SUM]` SET_VOLTAGE form is the canonical
//!   Bible v1 form; bare `[55 AA 10 DAC]` was live-proven insufficient on
//!   .139 to actually program the DAC even when the bus ACKs.
//! * RESET (0x07) and JUMP_TO_APP (0x06) are BANNED on fw=0x86 per
//!    (sending RESET permanently downgrades
//!   the firmware).
//!
//! Wire format:
//!   * `set_voltage` (always framed): `[55 AA 04 10 DAC SUM]`.
//!   * `enable_voltage` framed: 7-byte VnishPadded form
//!     `[55 AA 05 15 01 00 1B]` (per VNish RE corpus 2026-04-25); the
//!     bare path on the wire is `[55 AA 15 01]`.
//!   * `disable_voltage` framed: `[55 AA 05 15 00 00 1A]` (VnishPadded).
//!   * `heartbeat` bare: `[55 AA 16]` (fw=0x86 negotiates BARE on .139).
//!   * `read_temp` bare: `[55 AA 30 sensor_addr]` — read returns NaN
//!     sentinel because bare mode never delivers real LM75A data.

use super::{
    framed_voltage_dac, CMD_ENABLE_VOLTAGE, CMD_HEARTBEAT, CMD_READ_TEMP, CMD_SET_VOLTAGE,
    DSPIC_PREAMBLE,
};

/// Build the framed SET_VOLTAGE frame for fw=0x86.
///
/// Always framed: bare `[55 AA 10 DAC]` was live-proven insufficient
/// (post-ENABLE chain UART probe returned 0 bytes even though the bus
/// ACKed). Use `[55 AA 04 10 DAC SUM]` per Bible v1 1-power-dspic/00-opcode-map.
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

/// Build the bare-form ENABLE_VOLTAGE frame for fw=0x86 (live-wire form
/// observed on `a lab unit`).
pub(crate) fn enable_voltage_frame_bare() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_ENABLE_VOLTAGE,
        0x01,
    ]
}

/// Build the framed VnishPadded ENABLE_VOLTAGE frame for fw=0x86.
/// `[55 AA 05 15 01 00 1B]` — checksum = (LEN + CMD + 0x01 + 0x00) & 0xFF.
pub(crate) fn enable_voltage_frame_framed() -> &'static [u8] {
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

/// Build the bare-form DISABLE_VOLTAGE frame for fw=0x86.
pub(crate) fn disable_voltage_frame_bare() -> &'static [u8] {
    &[
        DSPIC_PREAMBLE[0],
        DSPIC_PREAMBLE[1],
        CMD_ENABLE_VOLTAGE,
        0x00,
    ]
}

/// Build the framed VnishPadded DISABLE_VOLTAGE frame for fw=0x86.
/// `[55 AA 05 15 00 00 1A]`.
pub(crate) fn disable_voltage_frame_framed() -> &'static [u8] {
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

/// Build the bare HEARTBEAT frame for fw=0x86 (live-wire form).
pub(crate) fn heartbeat_frame_bare() -> &'static [u8] {
    &[DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_HEARTBEAT]
}

/// Build the bare LM75A passthrough READ_TEMP frame.
/// In bare mode this only echoes `0x86`; callers must return NaN sentinel.
pub(crate) fn read_temp_frame_bare(sensor_addr: u8) -> Vec<u8> {
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
    fn fw86_set_voltage_uses_framed_dac_form() {
        // 13.7 V → DAC 0x06, checksum (0x04 + 0x10 + 0x06) = 0x1A.
        // Bare form `[55 AA 10 06]` was live-proven insufficient on .139.
        assert_eq!(
            set_voltage_frame(13_700),
            vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, 0x06, 0x1A]
        );
    }

    #[test]
    fn fw86_enable_disable_bare_form_is_4_byte() {
        assert_eq!(enable_voltage_frame_bare(), &[0x55, 0xAA, 0x15, 0x01]);
        assert_eq!(disable_voltage_frame_bare(), &[0x55, 0xAA, 0x15, 0x00]);
    }

    #[test]
    fn fw86_enable_disable_framed_is_vnish_7byte() {
        // VNish RE corpus 2026-04-25: ENABLE [55 AA 05 15 01 00 1B], status=0x01.
        assert_eq!(
            enable_voltage_frame_framed(),
            &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B]
        );
        assert_eq!(
            disable_voltage_frame_framed(),
            &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
        );
    }

    #[test]
    fn fw86_vnish_checksum_is_sum_of_len_cmd_payload() {
        let cksum = |len: u8, cmd: u8, payload: &[u8]| -> u8 {
            len.wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
        };
        assert_eq!(cksum(0x05, 0x15, &[0x01, 0x00]), 0x1B);
        assert_eq!(cksum(0x05, 0x15, &[0x00, 0x00]), 0x1A);
    }

    #[test]
    fn fw86_heartbeat_bare_is_3_byte() {
        assert_eq!(heartbeat_frame_bare(), &[0x55, 0xAA, CMD_HEARTBEAT]);
    }

    #[test]
    fn fw86_read_temp_bare_is_4_byte() {
        //: this frame goes on the
        // wire but the bare reply is only the FW echo; LM75 data is unavailable.
        assert_eq!(read_temp_frame_bare(0x48), vec![0x55, 0xAA, 0x30, 0x48]);
    }
}
