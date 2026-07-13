//!  dsp-A — dsPIC / PIC16F1704 / APW PSU wire format encoder/decoder.
//!
//! Source RE evidence:
//!
//! §2-3 (lines 53-283).
//!
//! Three wire forms coexist in the DCENT_OS fleet:
//! - **BARE** — PIC16F1704 on S9/L3+ over FPGA AXI IIC, dsPIC FW=0x82.
//!   3 bytes minimum, no LEN, no CKSUM. `[0x55, 0xAA, CMD]` + payload.
//! - **FRAMED-SUM** — dsPIC FW=0x86/0x88/0x89/0x8A, PIC16F1704 on
//!   AM2/Amlogic, all APW PSUs. `[0x55, 0xAA, LEN, CMD, payload..., CKSUM]`
//!   where `LEN = payload_len + 3` and
//!   `CKSUM = (LEN + CMD + Σpayload) & 0xFF`. Preamble bytes are NOT
//!   in the sum.
//! - **FRAMED-SHORT** — dsPIC FW=0x86 special-case for GET_VERSION (and
//!   possibly SET_VOLTAGE). 3 bytes, same as BARE.
//!
//! This module covers FRAMED-SUM (the most common case across the fleet)
//! plus BARE encode/decode helpers. The runtime adapter inside
//! `dcentrald-asic::dspic` selects which mode to use per (chip family,
//! firmware byte) pair.
//!
//! HAL-free: pure byte codec, no I²C / GPIO / clocks.

use serde::{Deserialize, Serialize};

/// Wire-format preamble bytes. Always host→device AND device→host.
pub const PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// 0xF5 in slot 0 of a reply = NAK (PSU/PIC rejects command).
pub const NAK_BYTE: u8 = 0xF5;

/// dsPIC / PIC16F1704 / APW PSU command opcodes.
///
/// Verified from `dspic-protocol-bible.md` §2 verified-checksum-frames
/// table (lines 107-124). All listed entries CKSUM-validated against
/// the reference vectors in the source doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DspicOpcode {
    // --- PIC firmware management (recovery only; gated behind
    // `recovery-tool` feature in the ASIC crate) ---
    SetPicFlashPointer = 0x01,
    SendData = 0x02,
    ReadData = 0x03,
    Jump = 0x06,
    Reset = 0x07,
    // --- runtime voltage / heartbeat ---
    SetVoltage = 0x10,
    Enable = 0x15,
    Heartbeat = 0x16,
    GetVersion = 0x17,
    GetVoltage = 0x18,
    GetDate = 0x19,
    GetWhichMac = 0x20,
    GetMac = 0x21,
    RdTempOffset = 0x23,
    Measure = 0x3A,
    GetV2 = 0x3B,
    // --- APW PSU runtime ---
    PsuWatchdog = 0x81, // payload 0x00 = disarm, 0x01 = arm
    PsuSetVoltage = 0x83,
    PsuHeartbeat = 0x84,
}

impl DspicOpcode {
    /// True if this opcode mutates persistent state on the PIC (flash
    /// pointer, send-data, reset). Caller should gate behind
    /// `--confirm-bricked` operator confirmation.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            DspicOpcode::SetPicFlashPointer
                | DspicOpcode::SendData
                | DspicOpcode::ReadData
                | DspicOpcode::Reset
                | DspicOpcode::Jump
        )
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Errors returned by `decode_framed_sum`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum DspicFrameError {
    /// Buffer is shorter than the minimum 5-byte FRAMED-SUM frame
    /// (preamble × 2 + LEN + CMD + CKSUM).
    Truncated { got: usize, need: usize },
    /// Preamble bytes are not `[0x55, 0xAA]`.
    InvalidPreamble { got: [u8; 2] },
    /// Reply slot 0 is the NAK byte 0xF5 — PIC rejected the command.
    NegativeAck,
    /// Bus noise pattern `[0xff, 0xff, ...]` — SDA was idle-high, no
    /// payload was staged.
    BusNoise,
    /// LEN field doesn't match buffer size.
    LenMismatch { len_field: u8, buf_len: usize },
    /// LEN is too small to even contain its own field + CMD + CKSUM.
    LenTooSmall { len_field: u8 },
    /// Computed CKSUM doesn't match the byte at the end of the frame.
    CksumMismatch { computed: u8, found: u8 },
    /// CMD byte does not map to a known DspicOpcode.
    UnknownOpcode { byte: u8 },
}

/// One FRAMED-SUM frame (decoded form).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DspicFrame {
    pub opcode: DspicOpcode,
    pub payload: Vec<u8>,
}

impl DspicFrame {
    /// Construct a frame with a fixed payload.
    pub fn new(opcode: DspicOpcode, payload: Vec<u8>) -> Self {
        Self { opcode, payload }
    }

    /// Encode this frame in FRAMED-SUM wire form. Output is
    /// `[0x55, 0xAA, LEN, CMD, payload..., CKSUM]`.
    pub fn encode_framed_sum(&self) -> Vec<u8> {
        let len = (1 + 1 + self.payload.len() + 1) as u8;
        let mut sum: u16 = (len as u16) + (self.opcode.as_u8() as u16);
        for b in &self.payload {
            sum = sum.wrapping_add(*b as u16);
        }
        let cksum = (sum & 0xFF) as u8;
        let mut out = Vec::with_capacity(2 + len as usize);
        out.extend_from_slice(&PREAMBLE);
        out.push(len);
        out.push(self.opcode.as_u8());
        out.extend_from_slice(&self.payload);
        out.push(cksum);
        out
    }

    /// Encode this frame in BARE wire form — `[0x55, 0xAA, CMD,
    /// payload...]`. Used by PIC16F1704 on S9/L3+ over FPGA AXI IIC.
    /// No LEN, no CKSUM.
    pub fn encode_bare(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(3 + self.payload.len());
        out.extend_from_slice(&PREAMBLE);
        out.push(self.opcode.as_u8());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Decode a FRAMED-SUM wire buffer into a `DspicFrame`. Validates
/// preamble, LEN-vs-buffer-size, CKSUM, and opcode mapping.
pub fn decode_framed_sum(buf: &[u8]) -> Result<DspicFrame, DspicFrameError> {
    if buf.len() < 5 {
        return Err(DspicFrameError::Truncated {
            got: buf.len(),
            need: 5,
        });
    }
    // Bus-noise rejection: all-FF leading bytes mean SDA was idle-high.
    if buf[0] == 0xFF && buf[1] == 0xFF {
        return Err(DspicFrameError::BusNoise);
    }
    // NAK rejection: 0xF5 in slot 0.
    if buf[0] == NAK_BYTE {
        return Err(DspicFrameError::NegativeAck);
    }
    if buf[0] != PREAMBLE[0] || buf[1] != PREAMBLE[1] {
        return Err(DspicFrameError::InvalidPreamble {
            got: [buf[0], buf[1]],
        });
    }
    let len = buf[2];
    if (len as usize) < 3 {
        return Err(DspicFrameError::LenTooSmall { len_field: len });
    }
    let total_wire = 2 + len as usize;
    if buf.len() < total_wire {
        return Err(DspicFrameError::Truncated {
            got: buf.len(),
            need: total_wire,
        });
    }
    if buf.len() != total_wire {
        return Err(DspicFrameError::LenMismatch {
            len_field: len,
            buf_len: buf.len(),
        });
    }
    let cmd = buf[3];
    let opcode = opcode_from_u8(cmd).ok_or(DspicFrameError::UnknownOpcode { byte: cmd })?;
    let payload = &buf[4..buf.len() - 1];
    let cksum = buf[buf.len() - 1];
    let mut sum: u16 = (len as u16) + (cmd as u16);
    for b in payload {
        sum = sum.wrapping_add(*b as u16);
    }
    let computed = (sum & 0xFF) as u8;
    if computed != cksum {
        return Err(DspicFrameError::CksumMismatch {
            computed,
            found: cksum,
        });
    }
    Ok(DspicFrame {
        opcode,
        payload: payload.to_vec(),
    })
}

/// Decode a FRAMED-SUM reply body read after the wire preamble has already
/// been consumed.
///
/// Linux I2C transactions used by some control boards write the request and
/// then read only `[LEN, CMD, payload..., CKSUM]`; the device does not repeat
/// the `[0x55, 0xAA]` preamble in that read. This adapter keeps those transport
/// semantics out of board-specific code while reusing the canonical length,
/// checksum, and opcode validation in [`decode_framed_sum`]. Length errors use
/// canonical full-wire byte counts, including the reconstituted preamble.
pub fn decode_framed_sum_reply_body(buf: &[u8]) -> Result<DspicFrame, DspicFrameError> {
    // A NAK may be returned as a single byte, before any framed body exists.
    if buf.first() == Some(&NAK_BYTE) {
        return Err(DspicFrameError::NegativeAck);
    }
    // Two idle-high bytes are sufficient evidence that no reply was staged.
    if buf.starts_with(&[0xFF, 0xFF]) {
        return Err(DspicFrameError::BusNoise);
    }
    if buf.len() < 3 {
        return Err(DspicFrameError::Truncated {
            got: PREAMBLE.len() + buf.len(),
            need: 5,
        });
    }

    let mut wire = Vec::with_capacity(PREAMBLE.len() + buf.len());
    wire.extend_from_slice(&PREAMBLE);
    wire.extend_from_slice(buf);
    decode_framed_sum(&wire)
}

fn opcode_from_u8(b: u8) -> Option<DspicOpcode> {
    use DspicOpcode::*;
    match b {
        0x01 => Some(SetPicFlashPointer),
        0x02 => Some(SendData),
        0x03 => Some(ReadData),
        0x06 => Some(Jump),
        0x07 => Some(Reset),
        0x10 => Some(SetVoltage),
        0x15 => Some(Enable),
        0x16 => Some(Heartbeat),
        0x17 => Some(GetVersion),
        0x18 => Some(GetVoltage),
        0x19 => Some(GetDate),
        0x20 => Some(GetWhichMac),
        0x21 => Some(GetMac),
        0x23 => Some(RdTempOffset),
        0x3A => Some(Measure),
        0x3B => Some(GetV2),
        0x81 => Some(PsuWatchdog),
        0x83 => Some(PsuSetVoltage),
        0x84 => Some(PsuHeartbeat),
        _ => None,
    }
}

/// Factory-provisioning opcodes that must not be exposed through the normal
/// daemon diagnostic builders.
pub const SET_HOST_MAC_ADDRESS_OPCODE: u8 = 0x14;
pub const WR_TEMP_OFFSET_VALUE_OPCODE: u8 = 0x22;

/// Read-only PIC/dsPIC diagnostic opcodes. These are safe to build as host-side
/// byte frames because they only read factory-provisioned metadata or offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum DspicReadOnlyDiagnostic {
    /// `GET_DATE` reads the provisioning timestamp stamped by factory tooling.
    GetDate = 0x19,
    /// `GET_WHICH_MAC` reads the selected MAC-bank index.
    GetWhichMac = 0x20,
    /// `GET_MAC` reads the factory-stamped MAC address.
    GetMac = 0x21,
    /// `RD_TEMP_OFFSET_VALUE` reads the signed board temperature offset.
    RdTempOffset = 0x23,
}

impl DspicReadOnlyDiagnostic {
    pub fn opcode(self) -> u8 {
        self as u8
    }

    pub fn expected_response_len(self) -> usize {
        match self {
            Self::GetDate => 4,
            Self::GetWhichMac => 1,
            Self::GetMac => 6,
            Self::RdTempOffset => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::GetDate => "GET_DATE",
            Self::GetWhichMac => "GET_WHICH_MAC",
            Self::GetMac => "GET_MAC",
            Self::RdTempOffset => "RD_TEMP_OFFSET_VALUE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum DspicDiagnosticFrameError {
    /// The opcode is a factory-write operation and is intentionally unavailable
    /// through the read-only daemon diagnostic API.
    MutatingOpcodeUnavailable { opcode: u8 },
    /// The opcode is not one of the read-only diagnostic commands.
    UnsupportedOpcode { opcode: u8 },
}

/// Build one read-only diagnostic request in the PIC16F1704 legacy frame form:
/// `[55 AA 04 CMD CKSUM]`, with `CKSUM = (0x04 + CMD) & 0xFF`.
///
/// This intentionally does not expose `SET_HOST_MAC_ADDRESS` (0x14) or
/// `WR_TEMP_OFFSET_VALUE` (0x22), both of which persist factory data.
pub fn encode_read_only_diagnostic_frame(cmd: DspicReadOnlyDiagnostic) -> [u8; 5] {
    let opcode = cmd.opcode();
    [
        PREAMBLE[0],
        PREAMBLE[1],
        0x04,
        opcode,
        0x04u8.wrapping_add(opcode),
    ]
}

/// Convert a raw opcode into a read-only diagnostic frame, refusing factory
/// write opcodes explicitly.
pub fn try_encode_read_only_diagnostic_opcode(
    opcode: u8,
) -> Result<[u8; 5], DspicDiagnosticFrameError> {
    let cmd = match opcode {
        0x19 => DspicReadOnlyDiagnostic::GetDate,
        0x20 => DspicReadOnlyDiagnostic::GetWhichMac,
        0x21 => DspicReadOnlyDiagnostic::GetMac,
        0x23 => DspicReadOnlyDiagnostic::RdTempOffset,
        SET_HOST_MAC_ADDRESS_OPCODE | WR_TEMP_OFFSET_VALUE_OPCODE => {
            return Err(DspicDiagnosticFrameError::MutatingOpcodeUnavailable { opcode });
        }
        _ => return Err(DspicDiagnosticFrameError::UnsupportedOpcode { opcode }),
    };

    Ok(encode_read_only_diagnostic_frame(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact frames from `dspic-protocol-bible.md` §2 lines 107-124.
    /// Every byte is RE-verified against the live capture.
    fn verified_frames() -> Vec<(DspicOpcode, Vec<u8>, Vec<u8>)> {
        vec![
            (
                DspicOpcode::Jump,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A],
            ),
            (
                DspicOpcode::Reset,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B],
            ),
            (
                DspicOpcode::SetVoltage,
                vec![0x06],
                vec![0x55, 0xAA, 0x04, 0x10, 0x06, 0x1A],
            ),
            (
                DspicOpcode::Enable,
                vec![0x01],
                vec![0x55, 0xAA, 0x04, 0x15, 0x01, 0x1A],
            ),
            (
                DspicOpcode::Enable,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x15, 0x00, 0x19],
            ),
            (
                DspicOpcode::Heartbeat,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A],
            ),
            (
                DspicOpcode::GetVersion,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B],
            ),
            (
                DspicOpcode::GetVoltage,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x18, 0x00, 0x1C],
            ),
            (
                DspicOpcode::Measure,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x3A, 0x00, 0x3E],
            ),
            (
                DspicOpcode::GetV2,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x3B, 0x00, 0x3F],
            ),
            (
                DspicOpcode::PsuSetVoltage,
                vec![0xC8],
                vec![0x55, 0xAA, 0x04, 0x83, 0xC8, 0x4F],
            ),
            (
                DspicOpcode::PsuWatchdog,
                vec![0x01],
                vec![0x55, 0xAA, 0x04, 0x81, 0x01, 0x86],
            ),
            (
                DspicOpcode::PsuWatchdog,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x81, 0x00, 0x85],
            ),
            (
                DspicOpcode::PsuHeartbeat,
                vec![0x00],
                vec![0x55, 0xAA, 0x04, 0x84, 0x00, 0x88],
            ),
        ]
    }

    #[test]
    fn encode_framed_sum_matches_re_doc_verified_table() {
        for (opcode, payload, expected) in verified_frames() {
            let frame = DspicFrame::new(opcode, payload.clone());
            let encoded = frame.encode_framed_sum();
            assert_eq!(
                encoded, expected,
                "opcode {:?} payload {:?}: encoded {:?} != verified {:?}",
                opcode, payload, encoded, expected
            );
        }
    }

    #[test]
    fn decode_framed_sum_round_trips_verified_table() {
        for (opcode, payload, wire) in verified_frames() {
            let decoded = decode_framed_sum(&wire).expect("verified frame must decode");
            assert_eq!(decoded.opcode, opcode);
            assert_eq!(decoded.payload, payload);
        }
    }

    #[test]
    fn decode_framed_sum_reply_body_accepts_verified_am3_heartbeat() {
        let decoded = decode_framed_sum_reply_body(&[0x06, 0x16, 0x01, 0x00, 0x00, 0x1D]).unwrap();
        assert_eq!(decoded.opcode, DspicOpcode::Heartbeat);
        assert_eq!(decoded.payload, [0x01, 0x00, 0x00]);
    }

    #[test]
    fn decode_framed_sum_reply_body_reuses_checksum_validation() {
        let error =
            decode_framed_sum_reply_body(&[0x06, 0x16, 0x01, 0x00, 0x00, 0x1E]).unwrap_err();
        assert_eq!(
            error,
            DspicFrameError::CksumMismatch {
                computed: 0x1D,
                found: 0x1E,
            }
        );
    }

    #[test]
    fn decode_framed_sum_reply_body_rejects_transport_failures() {
        assert_eq!(
            decode_framed_sum_reply_body(&[0xF5]),
            Err(DspicFrameError::NegativeAck)
        );
        assert_eq!(
            decode_framed_sum_reply_body(&[0xFF, 0xFF]),
            Err(DspicFrameError::BusNoise)
        );
        assert_eq!(
            decode_framed_sum_reply_body(&[0x06, 0x16]),
            Err(DspicFrameError::Truncated { got: 4, need: 5 })
        );
    }

    #[test]
    fn truncated_buffer_returns_truncated() {
        let r = decode_framed_sum(&[0x55, 0xAA, 0x04]).unwrap_err();
        assert!(matches!(r, DspicFrameError::Truncated { .. }));
        let r = decode_framed_sum(&[]).unwrap_err();
        assert!(matches!(r, DspicFrameError::Truncated { got: 0, .. }));
    }

    #[test]
    fn bus_noise_rejected_with_dedicated_error() {
        let r = decode_framed_sum(&[0xFF, 0xFF, 0x04, 0x16, 0x00, 0x1A]).unwrap_err();
        assert_eq!(r, DspicFrameError::BusNoise);
    }

    #[test]
    fn nak_rejected_with_dedicated_error() {
        let r = decode_framed_sum(&[0xF5, 0x00, 0x00, 0x00, 0x00]).unwrap_err();
        assert_eq!(r, DspicFrameError::NegativeAck);
    }

    #[test]
    fn invalid_preamble_returns_invalid_preamble() {
        let r = decode_framed_sum(&[0x55, 0x00, 0x04, 0x16, 0x00, 0x1A]).unwrap_err();
        match r {
            DspicFrameError::InvalidPreamble { got } => {
                assert_eq!(got, [0x55, 0x00]);
            }
            _ => panic!("expected InvalidPreamble"),
        }
    }

    #[test]
    fn cksum_mismatch_caught() {
        // Heartbeat frame with cksum off by 1.
        let r = decode_framed_sum(&[0x55, 0xAA, 0x04, 0x16, 0x00, 0xFF]).unwrap_err();
        match r {
            DspicFrameError::CksumMismatch { computed, found } => {
                assert_eq!(computed, 0x1A);
                assert_eq!(found, 0xFF);
            }
            _ => panic!("expected CksumMismatch, got {:?}", r),
        }
    }

    #[test]
    fn len_mismatch_caught() {
        // Claims LEN=5 but buffer is 6-byte LEN=4 frame.
        let r = decode_framed_sum(&[0x55, 0xAA, 0x05, 0x16, 0x00, 0x1A]).unwrap_err();
        // Buffer is 6 bytes, total_wire would be 2+5=7; truncated.
        assert!(matches!(r, DspicFrameError::Truncated { .. }));
    }

    #[test]
    fn unknown_opcode_caught() {
        // Opcode 0x42 not in the catalog; CKSUM = (4+0x42+0)&0xFF = 0x46.
        let r = decode_framed_sum(&[0x55, 0xAA, 0x04, 0x42, 0x00, 0x46]).unwrap_err();
        match r {
            DspicFrameError::UnknownOpcode { byte } => assert_eq!(byte, 0x42),
            _ => panic!("expected UnknownOpcode, got {:?}", r),
        }
    }

    #[test]
    fn set_voltage_dac_payload_encodes_correctly() {
        // DAC value 0x06 (initial 9.4 V) per BraiinsOS S9 default.
        let f = DspicFrame::new(DspicOpcode::SetVoltage, vec![0x06]);
        let bytes = f.encode_framed_sum();
        assert_eq!(bytes, [0x55, 0xAA, 0x04, 0x10, 0x06, 0x1A]);
    }

    #[test]
    fn bare_encode_is_3_bytes_plus_payload() {
        let f = DspicFrame::new(DspicOpcode::GetVersion, vec![]);
        let b = f.encode_bare();
        assert_eq!(b, [0x55, 0xAA, 0x17]);

        let f = DspicFrame::new(DspicOpcode::SetPicFlashPointer, vec![0xF8, 0x00]);
        let b = f.encode_bare();
        assert_eq!(b, [0x55, 0xAA, 0x01, 0xF8, 0x00]);
    }

    #[test]
    fn read_only_diagnostic_builders_match_verified_pic_frames() {
        let cases = [
            (
                DspicReadOnlyDiagnostic::GetDate,
                [0x55, 0xAA, 0x04, 0x19, 0x1D],
                4,
                "GET_DATE",
            ),
            (
                DspicReadOnlyDiagnostic::GetWhichMac,
                [0x55, 0xAA, 0x04, 0x20, 0x24],
                1,
                "GET_WHICH_MAC",
            ),
            (
                DspicReadOnlyDiagnostic::GetMac,
                [0x55, 0xAA, 0x04, 0x21, 0x25],
                6,
                "GET_MAC",
            ),
            (
                DspicReadOnlyDiagnostic::RdTempOffset,
                [0x55, 0xAA, 0x04, 0x23, 0x27],
                1,
                "RD_TEMP_OFFSET_VALUE",
            ),
        ];

        for (cmd, expected, response_len, name) in cases {
            assert_eq!(encode_read_only_diagnostic_frame(cmd), expected);
            assert_eq!(
                try_encode_read_only_diagnostic_opcode(cmd.opcode()).unwrap(),
                expected
            );
            assert_eq!(cmd.expected_response_len(), response_len);
            assert_eq!(cmd.name(), name);
        }
    }

    #[test]
    fn mutating_factory_diagnostic_opcodes_are_not_buildable() {
        for opcode in [SET_HOST_MAC_ADDRESS_OPCODE, WR_TEMP_OFFSET_VALUE_OPCODE] {
            assert_eq!(
                try_encode_read_only_diagnostic_opcode(opcode),
                Err(DspicDiagnosticFrameError::MutatingOpcodeUnavailable { opcode })
            );
        }
    }

    #[test]
    fn unsupported_diagnostic_opcode_is_rejected() {
        assert_eq!(
            try_encode_read_only_diagnostic_opcode(0x18),
            Err(DspicDiagnosticFrameError::UnsupportedOpcode { opcode: 0x18 })
        );
    }

    #[test]
    fn read_only_diagnostic_opcodes_are_known_but_not_destructive() {
        for (opcode, expected) in [
            (0x19, DspicOpcode::GetDate),
            (0x20, DspicOpcode::GetWhichMac),
            (0x21, DspicOpcode::GetMac),
            (0x23, DspicOpcode::RdTempOffset),
        ] {
            let wire = [0x55, 0xAA, 0x04, opcode, 0x00, 0x04u8.wrapping_add(opcode)];
            let decoded = decode_framed_sum(&wire).unwrap();
            assert_eq!(decoded.opcode, expected);
            assert!(!decoded.opcode.is_destructive());
        }
    }

    #[test]
    fn destructive_opcodes_flagged_correctly() {
        for opcode in [
            DspicOpcode::SetPicFlashPointer,
            DspicOpcode::SendData,
            DspicOpcode::ReadData,
            DspicOpcode::Jump,
            DspicOpcode::Reset,
        ] {
            assert!(
                opcode.is_destructive(),
                "{:?} should be destructive",
                opcode
            );
        }
        for opcode in [
            DspicOpcode::SetVoltage,
            DspicOpcode::Heartbeat,
            DspicOpcode::GetVersion,
            DspicOpcode::Enable,
            DspicOpcode::PsuHeartbeat,
        ] {
            assert!(
                !opcode.is_destructive(),
                "{:?} should NOT be destructive",
                opcode
            );
        }
    }

    #[test]
    fn frame_round_trips_through_serde() {
        let f = DspicFrame::new(DspicOpcode::SetVoltage, vec![0x06, 0x12, 0x34]);
        let json = serde_json::to_string(&f).unwrap();
        let back: DspicFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn empty_payload_supported() {
        let f = DspicFrame::new(DspicOpcode::Heartbeat, vec![]);
        let bytes = f.encode_framed_sum();
        // LEN = 0 + 1 + 0 + 1 + 1 = 3; CKSUM = (3 + 0x16) & 0xFF = 0x19.
        assert_eq!(bytes, [0x55, 0xAA, 0x03, 0x16, 0x19]);
        // Round-trip.
        let back = decode_framed_sum(&bytes).unwrap();
        assert_eq!(back.opcode, DspicOpcode::Heartbeat);
        assert!(back.payload.is_empty());
    }
}
