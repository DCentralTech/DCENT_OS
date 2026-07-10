//! Pure (no-HAL) decode + sanity guards for dsPIC voltage replies.
//!
//! Context (swarm `wf_e0647147` finding #1/#2, 2026-05-29 — the rail-honesty fix):
//! the dsPIC `GET_VOLTAGE` (0x3B) / `MEASURE_VOLTAGE` (0x3A) reply is decoded as a
//! fixed 4-byte `[cmd_echo, status, v_hi, v_lo]` frame — valid ONLY on the BARE
//! protocol (fw=0x82). On FRAMED firmware (fw=0x89) the reply is a longer
//! LEN/preamble-led frame, so `buf[2]`/`buf[3]` are NOT `v_hi`/`v_lo` and a blind
//! decode fabricates garbage (live: dsPIC 0x22 → 64760 mV = 0xFCF8, then mis-read by
//! the chip-rail proxy as "rail NOT energized" — a FALSE low-rail verdict from the
//! only non-DMM rail measure).
//!
//! This is the single, host-tested home for the guard: require the bare cmd-echo at
//! `buf[0]` AND a physically-plausible value (<= the dsPIC max), else return an error
//! so callers log "readback unreliable / no rail proof" instead of a fabricated
//! verdict. For bosminer's framed fw=0x89 selector path, Ghidra RE of `bosminer.bin`
//! found a distinct ADC reply: the first two post-envelope reply bytes are a
//! big-endian raw ADC count and bosminer scales it as
//! `volts = raw * 0.02448 - 0.35`. Local Ghidra/strace evidence does NOT prove an
//! independent unnormalized fw=0x8A selector path; treat 0x8A parity as unproven
//! unless it is normalized upstream or live-captured.

/// Why a BARE-shape dsPIC voltage reply could not be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareVoltageReplyError {
    /// `buf[0]` is not the expected command echo → the reply is not the bare
    /// `[cmd_echo, status, v_hi, v_lo]` shape (a FRAMED fw=0x89 reply has a different
    /// layout), so `buf[2]`/`buf[3]` are not `v_hi`/`v_lo`. Decoding would fabricate.
    NotBareShape { cmd_echo: u8, expected: u8 },
    /// The decoded value exceeds the dsPIC's physical max → the reply is misframed.
    ExceedsMax { mv: u16, max_mv: u16 },
}

/// Why a framed fw=0x89-shape `0x3A` ADC voltage reply could not be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramedMeasureVoltageReplyError {
    /// The reply does not contain the two raw ADC bytes bosminer consumes.
    TooShort { len: usize },
    /// The scaled ADC value would be negative AND the raw count is large enough
    /// that this is bus noise / a non-ADC reply shape — NOT a dead rail.
    ///
    /// A genuinely-dead rail reads back as a *small* raw count (the ADC sees
    /// ~0 V), which decodes to a slightly-negative millivolt via the
    /// `raw * 0.02448 - 0.35` affine fit (the `-0.35 V` offset means even a
    /// true 0 V rail lands a few LSB below zero). That case is reported as the
    /// distinct [`FramedMeasureVoltageReplyError::ZeroRail`] below so callers
    /// can tell a DEAD rail apart from line noise. `BelowZero` is reserved for
    /// a raw count above the dead-rail band that still scales sub-zero — which
    /// can only happen on a misframed/noisy reply.
    BelowZero { raw: u16 },
    /// The reply IS the fw=0x89-shape ADC payload and decodes to ~0 V — i.e. the
    /// chip rail is de-energized (DEAD). This is a *trustworthy* low-rail
    /// verdict, not bus noise: the only non-DMM rail proxy DCENT_OS has.
    ///
    /// Callers should treat this as "rail provably NOT energized" (e.g. abort
    /// the cold-wake / enum attempt with a clear diagnostic) rather than the
    /// generic "readback unreliable" they emit for [`BelowZero`]/[`TooShort`].
    /// `raw` is the captured ADC count for logging.
    ZeroRail { raw: u16 },
    /// The scaled ADC value exceeds the dsPIC's physical max, so the reply is
    /// not the fw=0x89-shape ADC payload.
    ExceedsMax { mv: u32, max_mv: u16, raw: u16 },
}

impl core::fmt::Display for BareVoltageReplyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BareVoltageReplyError::NotBareShape { cmd_echo, expected } => write!(
                f,
                "reply not bare-shape (cmd_echo=0x{:02X} != 0x{:02X}); framed decode unsupported — readback unreliable",
                cmd_echo, expected
            ),
            BareVoltageReplyError::ExceedsMax { mv, max_mv } => write!(
                f,
                "decoded {} mV > dsPIC max {} mV — reply misframed, readback unreliable",
                mv, max_mv
            ),
        }
    }
}

impl core::fmt::Display for FramedMeasureVoltageReplyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FramedMeasureVoltageReplyError::TooShort { len } => write!(
                f,
                "framed 0x3A ADC reply too short ({len} bytes); need 2 raw ADC bytes"
            ),
            FramedMeasureVoltageReplyError::BelowZero { raw } => write!(
                f,
                "framed 0x3A ADC raw 0x{raw:04X} scales below 0 mV (above dead-rail band) — reply misframed / bus noise"
            ),
            FramedMeasureVoltageReplyError::ZeroRail { raw } => write!(
                f,
                "framed 0x3A ADC raw 0x{raw:04X} decodes to ~0 V — chip rail provably NOT energized (DEAD rail)"
            ),
            FramedMeasureVoltageReplyError::ExceedsMax { mv, max_mv, raw } => write!(
                f,
                "framed 0x3A ADC raw 0x{raw:04X} decoded {mv} mV > dsPIC max {max_mv} mV — reply misframed"
            ),
        }
    }
}

/// Validate + decode a BARE-shape dsPIC voltage reply `[cmd_echo, status, v_hi, v_lo]`.
///
/// Returns the decoded millivolts, or [`BareVoltageReplyError`] when the reply is not
/// bare-shape (wrong cmd-echo) or the value is physically impossible (`> max_mv`).
/// Callers should map the error into their own `AsicError` with site context.
pub fn decode_bare_voltage_reply(
    cmd_echo: u8,
    expected_cmd: u8,
    v_hi: u8,
    v_lo: u8,
    max_mv: u16,
) -> Result<u16, BareVoltageReplyError> {
    if cmd_echo != expected_cmd {
        return Err(BareVoltageReplyError::NotBareShape {
            cmd_echo,
            expected: expected_cmd,
        });
    }
    let mv = ((v_hi as u16) << 8) | (v_lo as u16);
    if mv > max_mv {
        return Err(BareVoltageReplyError::ExceedsMax { mv, max_mv });
    }
    Ok(mv)
}

/// Largest raw ADC count treated as a DEAD rail (chip rail de-energized).
///
/// The bosminer affine fit is `volts = raw * 0.02448 - 0.35`. Any raw that
/// scales sub-zero falls in `0..=14` (raw 15 already scales to ~+17 mV). Within
/// that sub-zero band we split:
///   - `0..=DEAD_RAIL_RAW_MAX` → [`FramedMeasureVoltageReplyError::ZeroRail`]:
///     the rail reads essentially 0 V (≈0..0.12 V), a trustworthy "rail NOT
///     energized" verdict — the only non-DMM rail proxy DCENT_OS has.
///   - `(DEAD_RAIL_RAW_MAX+1)..=14` → [`FramedMeasureVoltageReplyError::BelowZero`]:
///     an ambiguous near-zero (≈0.15..0.34 V before the offset) that is more
///     likely bus noise / a misframed reply than a clean dead rail, so it stays
///     "readback unreliable" rather than asserting DEAD.
///
/// `5` (≈0.12 V) is a conservative threshold: it keeps the ZeroRail verdict to
/// genuinely-flat readings and leaves a noise margin to BelowZero.
const DEAD_RAIL_RAW_MAX: u16 = 5;

/// Validate + decode a framed fw=0x89-shape `MEASURE_VOLTAGE` (`0x3A`) ADC reply.
///
/// Ghidra evidence (`bosminer.bin`): firmware selector `0x0029dcd8` routes fw `0x89`
/// to the vtable method at `0x0034c340`; its poll body at `0x0034c3a0` sends command
/// `0x3A`, requires a reply length of at least 2 bytes, reads `be16(reply+0)`, and
/// computes `volts = raw * 0.02448 - 0.35` using constants at `0x0034c640` and
/// `0x0034c648`. No command echo/status bytes are consumed on this path. A local
/// selector pass found no `0x8A` route; the local bosminer trace reports GET_VERSION
/// `0x89` for both target slaves, so 0x8A parity remains unproven.
///
/// A raw count in the DEAD-rail band (`0..=DEAD_RAIL_RAW_MAX`) returns the
/// distinct [`FramedMeasureVoltageReplyError::ZeroRail`] signal (EE-007) so
/// callers can tell a de-energized rail apart from bus noise.
pub fn decode_framed_measure_voltage_reply(
    reply: &[u8],
    max_mv: u16,
) -> Result<u16, FramedMeasureVoltageReplyError> {
    if reply.len() < 2 {
        return Err(FramedMeasureVoltageReplyError::TooShort { len: reply.len() });
    }

    let [raw_hi, raw_lo, ..] = reply else {
        return Err(FramedMeasureVoltageReplyError::TooShort { len: reply.len() });
    };
    let raw = u16::from_be_bytes([*raw_hi, *raw_lo]);

    // 0.02448 V/LSB = 24.48 mV/LSB. Keep this fixed-point so host tests do not
    // depend on floating-point rounding differences.
    let mv_centi = (u32::from(raw) * 2448).saturating_add(50) / 100;
    if mv_centi < 350 {
        // Sub-zero after the −0.35 V offset. The affine fit puts a true 0 V
        // rail a few LSB below zero, so a *small* raw count (<= DEAD_RAIL_RAW_MAX)
        // is a trustworthy DEAD-rail reading — the chip rail is not energized.
        // A larger raw that still scales sub-zero (DEAD_RAIL_RAW_MAX+1 ..= 14)
        // is more likely a misframed/noisy reply, so it stays the ambiguous
        // BelowZero "readback unreliable" signal. See EE-007.
        if raw <= DEAD_RAIL_RAW_MAX {
            return Err(FramedMeasureVoltageReplyError::ZeroRail { raw });
        }
        return Err(FramedMeasureVoltageReplyError::BelowZero { raw });
    }
    let mv = mv_centi - 350;
    if mv > u32::from(max_mv) {
        return Err(FramedMeasureVoltageReplyError::ExceedsMax { mv, max_mv, raw });
    }
    Ok(mv as u16)
}

/// Decode a local raw `/dev/i2c-0` capture of framed `MEASURE_VOLTAGE` (`0x3A`).
///
/// Bosminer's Linux strace exposes a 7-byte outer reply envelope:
/// `[0x07, 0x3A, status, adc_hi, adc_lo, 0x00, checksum]`. The firmware method
/// consumes the post-envelope ADC buffer, so the bytes used by the affine fit are
/// at raw-envelope offset `3..4`, not raw offset `0..1`. This helper is pure and
/// host-safe for offline capture tooling/tests; it does not imply the live read
/// length should change, because the runtime service may already expose the
/// post-envelope two-byte ADC buffer.
pub fn decode_framed_measure_voltage_i2c0_capture(
    reply: &[u8],
    max_mv: u16,
) -> Result<u16, FramedMeasureVoltageReplyError> {
    if let Some(adc) = framed_measure_voltage_i2c0_envelope_adc(reply) {
        return decode_framed_measure_voltage_reply(&adc, max_mv);
    }
    decode_framed_measure_voltage_reply(reply, max_mv)
}

fn framed_measure_voltage_i2c0_envelope_adc(reply: &[u8]) -> Option<[u8; 2]> {
    let [len, cmd, _status, adc_hi, adc_lo, _reserved, checksum] = reply else {
        return None;
    };
    if *len as usize != reply.len() || *cmd != 0x3A {
        return None;
    }

    let computed_checksum = reply
        .get(..6)?
        .iter()
        .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
    if computed_checksum != *checksum {
        return None;
    }

    Some([*adc_hi, *adc_lo])
}

#[cfg(test)]
mod tests {
    use super::*;

    // DSPIC_MAX_VOLTAGE_MV in dcentrald-asic is 15140; use the same here for realism.
    const MAX: u16 = 15_140;
    const GET_VOLTAGE: u8 = 0x3B;
    const MEASURE_VOLTAGE: u8 = 0x3A;

    #[test]
    fn valid_bare_reply_decodes() {
        // bare fw=0x82: [0x3B, status, 0x35, 0x84] = 13700 mV (0x3584).
        assert_eq!(
            decode_bare_voltage_reply(GET_VOLTAGE, GET_VOLTAGE, 0x35, 0x84, MAX),
            Ok(13_700)
        );
    }

    #[test]
    fn framed_fw89_echo_is_rejected_not_fabricated() {
        // The live regression: framed fw=0x89 reply mis-decoded as 0xFCF8 = 64760 mV.
        // buf[0] is NOT the cmd-echo on a framed reply → NotBareShape, never a fake mV.
        let r = decode_bare_voltage_reply(0xFC, GET_VOLTAGE, 0xFC, 0xF8, MAX);
        assert_eq!(
            r,
            Err(BareVoltageReplyError::NotBareShape {
                cmd_echo: 0xFC,
                expected: GET_VOLTAGE
            })
        );
    }

    #[test]
    fn correct_echo_but_impossible_value_is_rejected() {
        // 0xFCF8 = 64760 mV with a (hypothetical) correct echo still > max → rejected.
        let r = decode_bare_voltage_reply(GET_VOLTAGE, GET_VOLTAGE, 0xFC, 0xF8, MAX);
        assert_eq!(
            r,
            Err(BareVoltageReplyError::ExceedsMax {
                mv: 64_760,
                max_mv: MAX
            })
        );
    }

    #[test]
    fn measure_voltage_uses_its_own_expected_cmd() {
        assert_eq!(
            decode_bare_voltage_reply(MEASURE_VOLTAGE, MEASURE_VOLTAGE, 0x35, 0x84, MAX),
            Ok(13_700)
        );
        // a GET_VOLTAGE echo on a MEASURE_VOLTAGE call is not bare-shape for that call.
        assert!(matches!(
            decode_bare_voltage_reply(GET_VOLTAGE, MEASURE_VOLTAGE, 0x35, 0x84, MAX),
            Err(BareVoltageReplyError::NotBareShape { .. })
        ));
    }

    #[test]
    fn boundary_value_at_max_is_ok() {
        // exactly max is allowed; one above is not.
        assert_eq!(
            decode_bare_voltage_reply(0x3B, 0x3B, (MAX >> 8) as u8, (MAX & 0xFF) as u8, MAX),
            Ok(MAX)
        );
    }

    #[test]
    fn framed_fw89_measure_reply_uses_raw_adc_at_offset_zero() {
        // raw=574 -> round(574 * 24.48 mV - 350 mV) = 13,702 mV.
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x02, 0x3E], MAX),
            Ok(13_702)
        );
    }

    #[test]
    fn framed_fw89_measure_i2c0_envelope_strips_adc_payload() {
        // Local bosminer strace raw reply: len/cmd/status/adc_hi/adc_lo/zero/checksum.
        // The raw offset-0 word 0x073A is impossible; the ADC payload is 0x0222.
        assert_eq!(
            decode_framed_measure_voltage_i2c0_capture(
                &[0x07, 0x3A, 0x01, 0x02, 0x22, 0x00, 0x66],
                MAX
            ),
            Ok(13_016)
        );
    }

    #[test]
    fn framed_fw89_measure_i2c0_capture_accepts_already_stripped_adc() {
        assert_eq!(
            decode_framed_measure_voltage_i2c0_capture(&[0x02, 0x22], MAX),
            Ok(13_016)
        );
    }

    #[test]
    fn framed_fw89_measure_i2c0_envelope_requires_valid_checksum() {
        // A bad envelope is not stripped. It falls through to the ordinary
        // post-envelope decoder and rejects raw offset 0 (0x073A) as impossible.
        assert!(matches!(
            decode_framed_measure_voltage_i2c0_capture(
                &[0x07, 0x3A, 0x01, 0x02, 0x22, 0x00, 0x67],
                MAX
            ),
            Err(FramedMeasureVoltageReplyError::ExceedsMax { raw: 0x073A, .. })
        ));
    }

    #[test]
    fn framed_fw89_measure_rejects_old_shift_artifact() {
        let r = decode_framed_measure_voltage_reply(&[0xFC, 0xF8], MAX);
        assert_eq!(
            r,
            Err(FramedMeasureVoltageReplyError::ExceedsMax {
                raw: 0xFCF8,
                mv: 1_584_975,
                max_mv: MAX,
            })
        );
    }

    #[test]
    fn framed_fw89_measure_zero_raw_is_zero_rail_not_below_zero() {
        // EE-007: a raw of 0 means the ADC sees ~0 V → DEAD rail. This is the
        // only non-DMM rail proxy, so it must come back as the distinct,
        // trustworthy ZeroRail signal — never the generic BelowZero ("misframed")
        // verdict.
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x00, 0x00], MAX),
            Err(FramedMeasureVoltageReplyError::ZeroRail { raw: 0 })
        );
    }

    #[test]
    fn framed_fw89_measure_dead_rail_band_boundary() {
        // raw=5 is the top of the DEAD-rail band → trustworthy ZeroRail.
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x00, 0x05], MAX),
            Err(FramedMeasureVoltageReplyError::ZeroRail { raw: 5 })
        );
        // raw=6..=14 still scale sub-zero but are above the dead band → the
        // ambiguous BelowZero "readback unreliable" signal, NOT a DEAD verdict.
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x00, 0x06], MAX),
            Err(FramedMeasureVoltageReplyError::BelowZero { raw: 6 })
        );
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x00, 0x0E], MAX),
            Err(FramedMeasureVoltageReplyError::BelowZero { raw: 14 })
        );
        // raw=15 scales to ~+17 mV — a tiny but valid positive reading, NOT a
        // dead rail and NOT below zero.
        assert_eq!(
            decode_framed_measure_voltage_reply(&[0x00, 0x0F], MAX),
            Ok(17)
        );
    }

    #[test]
    fn framed_fw89_measure_rejects_cmd_echo_shape() {
        // If the reply were `[cmd_echo, status, ...]`, bosminer's ADC decoder
        // would see raw=0x3A00 and reject it as impossible.
        assert!(matches!(
            decode_framed_measure_voltage_reply(&[MEASURE_VOLTAGE, 0x00, 0x35, 0x84], MAX),
            Err(FramedMeasureVoltageReplyError::ExceedsMax { raw: 0x3A00, .. })
        ));
    }
}
