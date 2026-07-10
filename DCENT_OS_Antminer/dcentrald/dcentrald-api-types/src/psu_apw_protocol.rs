//!  psu-B — APW PSU 9-command protocol catalog (HAL-free).
//!
//! Source RE evidence:
//!  §3-§5
//! (1964-line bible covering the full APW family).
//!
//! APW PSUs (APW3 through APW17) speak a Bitmain-proprietary I²C
//! protocol on `/dev/i2c-0` (am2 Zynq) or `/dev/i2c-1` (Amlogic).
//! Frame format:
//!
//! ```text
//! [0x55] [0xAA] [length] [cmd] [payload bytes...] [checksum]
//! ```
//!
//! Two reply patterns:
//! - **Echo-as-ACK** (cmd ≥ 0x80): the PSU replies with the same frame
//!   bytes. No body — the echo IS the acknowledgement.
//! - **Three-phase framed read** (cmd < 0x80): the PSU returns a body
//!   carrying the read value (DAC code, ADC sample, state word, etc.).
//!
//! This module pins the 9 documented commands, the frame builder
//! (preamble + length + cmd + payload + checksum) and the canonical
//! voltage↔DAC formula. The runtime adapter wires this into
//! `dcentrald-hal::psu` for live-HW writes.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Command catalog
// ---------------------------------------------------------------------------

/// APW protocol commands §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ApwCommand {
    /// 0x01 — Returns 16-byte ASCII firmware version string.
    GetFwVersion = 0x01,
    /// 0x02 — Returns hardware revision info (variable length).
    GetHwVersion = 0x02,
    /// 0x03 — Returns the current DAC setpoint (2 bytes, big-endian).
    GetVoltage = 0x03,
    /// 0x04 — Triggers an ADC conversion and returns measured voltage.
    MeasureVoltage = 0x04,
    /// 0x05 — Returns 2-byte LE state word (0x0001=ON, 0x0000=OFF).
    ReadState = 0x05,
    /// 0x06 — Reads calibration EEPROM data (variable).
    ReadCal = 0x06,
    /// 0x81 — Watchdog enable/disable (echo-as-ACK).
    Watchdog = 0x81,
    /// 0x83 — Sets output voltage via 8-bit DAC code (echo-as-ACK).
    SetVoltage = 0x83,
    /// 0x86 — Writes calibration data to PIC flash (echo-as-ACK).
    WriteCal = 0x86,
}

impl ApwCommand {
    /// Numeric command byte on the wire.
    pub fn code(&self) -> u8 {
        *self as u8
    }

    /// True iff this command uses echo-as-ACK (bit 7 set).
    pub fn is_echo_ack(&self) -> bool {
        (self.code() & 0x80) != 0
    }

    /// True iff this command writes state to the PSU (set voltage,
    /// write cal, watchdog enable, factory reset). Operator-facing
    /// dashboards may want explicit confirmation.
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::SetVoltage | Self::WriteCal)
    }

    /// Look up a command by its numeric byte.
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x01 => Self::GetFwVersion,
            0x02 => Self::GetHwVersion,
            0x03 => Self::GetVoltage,
            0x04 => Self::MeasureVoltage,
            0x05 => Self::ReadState,
            0x06 => Self::ReadCal,
            0x81 => Self::Watchdog,
            0x83 => Self::SetVoltage,
            0x86 => Self::WriteCal,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Frame layout
// ---------------------------------------------------------------------------

/// First preamble byte.
pub const APW_PREAMBLE_0: u8 = 0x55;
/// Second preamble byte.
pub const APW_PREAMBLE_1: u8 = 0xAA;

/// Build an APW protocol frame: preamble + length + cmd + payload +
/// checksum. `length` includes itself + cmd + payload + checksum
/// (mirrors stock Bitmain wire form).
///
/// Returns the wire bytes ready to push to `/dev/i2c-0`.
pub fn build_apw_frame(cmd: ApwCommand, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(APW_PREAMBLE_0);
    frame.push(APW_PREAMBLE_1);
    // length = 1 (length byte) + 1 (cmd) + payload.len() + 1 (checksum)
    let length = 3u8.saturating_add(payload.len() as u8);
    frame.push(length);
    frame.push(cmd.code());
    frame.extend_from_slice(payload);
    let checksum = apw_checksum(&frame[2..]); // length + cmd + payload
    frame.push(checksum);
    frame
}

/// Compute the APW checksum: 8-bit sum of `length + cmd + payload`,
/// mod 0x100. (The RE doc shows the sum is taken over the bytes after
/// the preamble, including the length byte.)
pub fn apw_checksum(bytes_after_preamble: &[u8]) -> u8 {
    let mut sum: u8 = 0;
    for b in bytes_after_preamble {
        sum = sum.wrapping_add(*b);
    }
    sum
}

// ---------------------------------------------------------------------------
// DAC ↔ Voltage conversion (canonical PIC16F1704 formula)
// ---------------------------------------------------------------------------

/// DAC reference voltage (default per PIC16F1704 calibration).
pub const DAC_REFERENCE_V: f32 = 15.1084;
/// DAC offset per count (default — negative slope).
pub const DAC_OFFSET_PER_COUNT_V: f32 = -0.013046;

/// Convert a DAC code (0-255) to the produced output voltage.
pub fn dac_code_to_voltage(dac_code: u8) -> f32 {
    DAC_REFERENCE_V + DAC_OFFSET_PER_COUNT_V * (dac_code as f32)
}

/// Convert a target voltage to the closest DAC code (clamped to 0-255).
pub fn voltage_to_dac_code(voltage_v: f32) -> u8 {
    let raw = (DAC_REFERENCE_V - voltage_v) / 0.013046;
    raw.round().clamp(0.0, 255.0) as u8
}

/// ADC raw → measured voltage ( line 282).
pub fn adc_raw_to_voltage(raw: u16) -> f32 {
    ((raw as f32) + 0.8615) / 63.017
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_codes_match_re_doc() {
        //  §4 command table.
        assert_eq!(ApwCommand::GetFwVersion.code(), 0x01);
        assert_eq!(ApwCommand::GetHwVersion.code(), 0x02);
        assert_eq!(ApwCommand::GetVoltage.code(), 0x03);
        assert_eq!(ApwCommand::MeasureVoltage.code(), 0x04);
        assert_eq!(ApwCommand::ReadState.code(), 0x05);
        assert_eq!(ApwCommand::ReadCal.code(), 0x06);
        assert_eq!(ApwCommand::Watchdog.code(), 0x81);
        assert_eq!(ApwCommand::SetVoltage.code(), 0x83);
        assert_eq!(ApwCommand::WriteCal.code(), 0x86);
    }

    #[test]
    fn echo_ack_pattern_matches_high_bit() {
        // Per RE doc: cmd ≥ 0x80 uses echo-as-ACK.
        assert!(!ApwCommand::GetFwVersion.is_echo_ack());
        assert!(!ApwCommand::GetVoltage.is_echo_ack());
        assert!(!ApwCommand::ReadState.is_echo_ack());
        assert!(ApwCommand::Watchdog.is_echo_ack());
        assert!(ApwCommand::SetVoltage.is_echo_ack());
        assert!(ApwCommand::WriteCal.is_echo_ack());
    }

    #[test]
    fn destructive_commands_pinned() {
        // SetVoltage and WriteCal write persistent state. Watchdog is
        // a heartbeat (idempotent) — NOT destructive.
        assert!(ApwCommand::SetVoltage.is_destructive());
        assert!(ApwCommand::WriteCal.is_destructive());
        assert!(!ApwCommand::Watchdog.is_destructive());
        assert!(!ApwCommand::GetFwVersion.is_destructive());
    }

    #[test]
    fn from_code_round_trips_known_commands() {
        for cmd in [
            ApwCommand::GetFwVersion,
            ApwCommand::GetHwVersion,
            ApwCommand::GetVoltage,
            ApwCommand::MeasureVoltage,
            ApwCommand::ReadState,
            ApwCommand::ReadCal,
            ApwCommand::Watchdog,
            ApwCommand::SetVoltage,
            ApwCommand::WriteCal,
        ] {
            let n = cmd.code();
            assert_eq!(ApwCommand::from_code(n), Some(cmd));
        }
    }

    #[test]
    fn from_code_returns_none_for_unknown() {
        // 0x07/0x10/0x80/0xFF are NOT defined in .
        for unknown in [0x00u8, 0x07, 0x10, 0x80, 0x82, 0x84, 0x85, 0xFF] {
            assert!(
                ApwCommand::from_code(unknown).is_none(),
                "unexpected match for 0x{:02X}",
                unknown
            );
        }
    }

    #[test]
    fn watchdog_enable_frame_matches_re_doc() {
        // RE doc line 316: enable = `55 AA 04 81 01 86`.
        let frame = build_apw_frame(ApwCommand::Watchdog, &[0x01]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x04, 0x81, 0x01, 0x86]);
    }

    #[test]
    fn watchdog_disable_frame_matches_re_doc() {
        // RE doc line 315: disable = `55 AA 04 81 00 85`.
        let frame = build_apw_frame(ApwCommand::Watchdog, &[0x00]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x04, 0x81, 0x00, 0x85]);
    }

    #[test]
    fn set_voltage_125v_frame_matches_re_doc() {
        // RE doc line 331: set 12.5V → DAC code 0xC8 → frame
        // `55 AA 04 83 C8 4F`.
        let frame = build_apw_frame(ApwCommand::SetVoltage, &[0xC8]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x04, 0x83, 0xC8, 0x4F]);
    }

    #[test]
    fn get_fw_version_frame_matches_re_doc() {
        // RE doc line 246: `55 AA 03 01 04`.
        let frame = build_apw_frame(ApwCommand::GetFwVersion, &[]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x03, 0x01, 0x04]);
    }

    #[test]
    fn read_state_frame_matches_re_doc() {
        // RE doc line 288: `55 AA 03 05 08`.
        let frame = build_apw_frame(ApwCommand::ReadState, &[]);
        assert_eq!(frame, vec![0x55, 0xAA, 0x03, 0x05, 0x08]);
    }

    #[test]
    fn checksum_is_8bit_sum_after_preamble() {
        // The preamble bytes are NOT part of the checksum.
        let cs = apw_checksum(&[0x04, 0x81, 0x01]);
        assert_eq!(cs, 0x86);
        let cs = apw_checksum(&[0x03, 0x01]);
        assert_eq!(cs, 0x04);
    }

    #[test]
    fn dac_code_to_voltage_matches_canonical_formula() {
        // V = 15.1084 - 0.013046 * dac
        assert!((dac_code_to_voltage(0) - 15.1084).abs() < 1e-3);
        assert!((dac_code_to_voltage(8) - 15.0040).abs() < 1e-3);
        assert!((dac_code_to_voltage(200) - 12.4992).abs() < 1e-3);
        assert!((dac_code_to_voltage(255) - 11.7817).abs() < 1e-3);
    }

    #[test]
    fn voltage_to_dac_code_round_trips_known_anchors() {
        // RE doc line 332: 12.5V → 200 (0xC8).
        assert_eq!(voltage_to_dac_code(12.5), 200);
        // 12.0V → 238 (0xEE).
        assert_eq!(voltage_to_dac_code(12.0), 238);
        // 15.108V → 0.
        assert_eq!(voltage_to_dac_code(15.1084), 0);
    }

    #[test]
    fn voltage_to_dac_code_clamps_out_of_range() {
        // Way above the DAC ref → clamps at 0.
        assert_eq!(voltage_to_dac_code(20.0), 0);
        // Way below the floor → clamps at 255.
        assert_eq!(voltage_to_dac_code(0.0), 255);
        assert_eq!(voltage_to_dac_code(-5.0), 255);
    }

    #[test]
    fn adc_raw_to_voltage_matches_re_doc_formula() {
        // V = (raw + 0.8615) / 63.017
        let v = adc_raw_to_voltage(800);
        // ~12.71 V at raw 800.
        assert!((v - 12.708).abs() < 0.01);
    }

    #[test]
    fn frame_length_field_includes_self_cmd_payload_checksum() {
        // length = 1 (length byte) + 1 (cmd) + payload.len() + 1 (cs).
        let frame = build_apw_frame(ApwCommand::SetVoltage, &[0xC8]);
        assert_eq!(frame[2], 0x04); // length = 1 + 1 + 1 + 1
        let frame = build_apw_frame(ApwCommand::GetFwVersion, &[]);
        assert_eq!(frame[2], 0x03); // length = 1 + 1 + 0 + 1
    }

    #[test]
    fn command_round_trips_through_serde() {
        for cmd in [
            ApwCommand::GetFwVersion,
            ApwCommand::SetVoltage,
            ApwCommand::Watchdog,
            ApwCommand::WriteCal,
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: ApwCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(cmd, back);
        }
    }

    #[test]
    fn apw_command_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ApwCommand::GetFwVersion).unwrap(),
            "\"get_fw_version\""
        );
        assert_eq!(
            serde_json::to_string(&ApwCommand::SetVoltage).unwrap(),
            "\"set_voltage\""
        );
        assert_eq!(
            serde_json::to_string(&ApwCommand::WriteCal).unwrap(),
            "\"write_cal\""
        );
    }
}
