//! Pure PMBus + DS4432U voltage/telemetry math (no ESP-IDF / no log / no serde).
//!
//! Extracted from `power.rs` — which is `#[cfg(target_os = "espidf")]`-gated at
//! `lib.rs` and therefore has ZERO host coverage — so the pure conversions that
//! decide the regulator write and decode the telemetry are host-unit-testable
//! without the ESP-IDF toolchain. Mirrors the `temp_decode.rs` pattern: every
//! function here is plain arithmetic over integers/floats with no hardware,
//! ESP-IDF, `log`, or heap dependency.
//!
//! `power.rs` re-exports the four PMBus conversions and calls
//! [`ds4432u_dac_code`] from its `Ds4432u::set_voltage_mv`, so the regulator code
//! path stays byte-identical — this is an extraction, NOT a formula change.

// ===========================================================================
// PMBus utilities
// ===========================================================================

/// Convert a PMBus Linear11 value to a floating-point number.
///
/// Linear11 format: upper 5 bits = signed exponent, lower 11 bits = signed mantissa.
/// Value = mantissa * 2^exponent
pub fn pmbus_linear11_to_f32(raw: u16) -> f32 {
    // Extract 5-bit signed exponent (bits 15:11)
    let exp_raw = ((raw >> 11) & 0x1F) as i8;
    let exponent = if exp_raw > 15 {
        exp_raw - 32 // sign-extend 5-bit to 8-bit
    } else {
        exp_raw
    } as i32;

    // Extract 11-bit signed mantissa (bits 10:0)
    let mant_raw = (raw & 0x07FF) as i16;
    let mantissa = if mant_raw > 1023 {
        mant_raw - 2048 // sign-extend 11-bit to 16-bit
    } else {
        mant_raw
    } as f32;

    mantissa * 2.0_f32.powi(exponent)
}

/// Convert a floating-point number to PMBus Linear11 format.
///
/// Finds the best exponent to represent the value with maximum precision
/// in the 11-bit mantissa range (-1024 to +1023).
pub fn f32_to_pmbus_linear11(value: f32) -> u16 {
    if value == 0.0 {
        return 0;
    }

    // Find the best exponent
    let mut exponent: i32 = 0;
    let mut mantissa = value;

    // Scale mantissa to fit in 11-bit signed range [-1024, 1023]
    while mantissa.abs() >= 1024.0 && exponent < 15 {
        mantissa /= 2.0;
        exponent += 1;
    }
    while mantissa.abs() < 512.0 && exponent > -16 {
        mantissa *= 2.0;
        exponent -= 1;
    }

    // Round in i32 to avoid an i16 saturation-to-negative on huge inputs.
    let mut mantissa_int = mantissa.round() as i32;

    // Rounding can push a positive mantissa to exactly 1024 (e.g. 1023.6 -> 1024),
    // which is OUTSIDE the signed 11-bit range [-1024, 1023]: masking 1024 with
    // 0x07FF decodes as -1024 — a SIGN FLIP that would write a NEGATIVE protection
    // limit (VIN_OV / IOUT_OC / OT / FREQUENCY_SWITCH) and trip an instant FALSE
    // fault. Re-normalize (halve mantissa, bump exponent); if the exponent is
    // already saturated (huge input), clamp to the max positive mantissa instead
    // of letting the cast wrap to a negative value.
    if mantissa_int >= 1024 {
        if exponent < 15 {
            mantissa_int /= 2;
            exponent += 1;
        } else {
            mantissa_int = 1023;
        }
    }
    let mantissa_int = mantissa_int.clamp(-1024, 1023) as i16;

    // Encode: 5-bit exponent (two's complement) | 11-bit mantissa (two's complement)
    let exp_bits = ((exponent as u16) & 0x1F) << 11;
    let mant_bits = (mantissa_int as u16) & 0x07FF;

    exp_bits | mant_bits
}

/// Convert a PMBus ULINEAR16 value to voltage using a given exponent.
///
/// VOUT uses ULINEAR16 format: unsigned 16-bit mantissa with a separate
/// exponent read from VOUT_MODE register.
/// Value = mantissa * 2^exponent
pub fn pmbus_ulinear16_to_f32(raw: u16, exponent: i8) -> f32 {
    raw as f32 * 2.0_f32.powi(exponent as i32)
}

/// Convert a voltage to PMBus ULINEAR16 format.
pub fn f32_to_pmbus_ulinear16(value: f32, exponent: i8) -> u16 {
    (value / 2.0_f32.powi(exponent as i32)).round() as u16
}

// ===========================================================================
// DS4432U transfer function
// ===========================================================================

/// DS4432U full-scale current sink coefficient.
/// The actual voltage change depends on the feedback resistor network.
/// These values are calibrated for the BitAxe board design.
// DS4432U Transfer function constants for BitAxe board (from ESP-Miner)
const DS4432U_VREF: f32 = 0.6; // TPS40305 feedback reference voltage
const DS4432U_R_FB_TOP: f32 = 4700.0; // Top feedback resistor RA (ohms)
const DS4432U_R_FB_BOT: f32 = 4700.0; // Bottom feedback resistor RB (ohms)
#[allow(dead_code)] // documentary: explains the IFS derivation below
const DS4432U_R_FS: f32 = 80000.0; // Full-scale current-setting resistor (ohms)
                                   // IFS = (Vrfs / Rfs) x (127/16), Vrfs = 0.997
const DS4432U_IFS: f32 = 0.000098921;

/// Raw DS4432U transfer-function "change" magnitude for `voltage_mv`, as an f32
/// BEFORE the `ceil()`/`as u8` cast.
///
/// `change = |((VFB / RB) - ((Vout - VFB) / RA)) / IFS| * 127` — the exact
/// expression from `Ds4432u::set_voltage_mv`. Split out so the
/// magnitude-stays-<128 invariant can be host-asserted directly.
fn ds4432u_change(voltage_mv: u16) -> f32 {
    let vout = voltage_mv as f32 / 1000.0;
    ((DS4432U_VREF / DS4432U_R_FB_BOT - (vout - DS4432U_VREF) / DS4432U_R_FB_TOP) / DS4432U_IFS
        * 127.0)
        .abs()
}

/// The 7-bit DS4432U magnitude code = `ceil(change) as u8`.
///
/// The source/sink direction bit (`0x80`) is ORed on TOP of this value in
/// [`ds4432u_dac_code`], so this must stay `< 128` across the legal voltage
/// range — a magnitude `>= 128` already carries bit 7 and silently collides with
/// the direction bit, mis-setting the rail. (`as u8` on an f32 saturates at 255,
/// which makes a too-low / out-of-range voltage land on `0xFF`.)
pub fn ds4432u_magnitude_code(voltage_mv: u16) -> u8 {
    ds4432u_change(voltage_mv).ceil() as u8
}

/// Compute the DS4432U output-0 DAC register byte for a requested core voltage.
///
/// Byte-identical to `Ds4432u::set_voltage_mv`'s inline math:
/// 1. fail-closed (`None`) when `vout < 0` (defensive; `u16` is never negative)
///    or `voltage_mv > ceiling_mv` (the HALPWR-3 absolute driver ceiling), then
/// 2. `reg = ceil(change) as u8`, and fail-closed (`None`) when that magnitude
///    reaches `0x80` — it would collide with the direction bit (low-side guard), then
/// 3. set the `0x80` source bit when `Vout > VREF` (true for any mining setpoint).
///
/// NOTE: this preserves the EXISTING Rust behavior. It intentionally diverges
/// from ESP-Miner's `DS4432U_set_voltage` on two points worth flagging (see the
/// tests): the feedback resistors are `4700/4700` here vs ESP-Miner's
/// `RA=4750 / RB=3320`, and the direction bit is set on `Vout > VREF` here vs
/// ESP-Miner's `Vout < VNOM (1.451 V)`. Changing either alters the voltage on
/// real DS4432U boards (Max/Ultra/Supra) and is out of scope for a
/// behavior-preserving extraction — it needs hardware validation first.
pub fn ds4432u_dac_code(voltage_mv: u16, ceiling_mv: u16) -> Option<u8> {
    let vout = voltage_mv as f32 / 1000.0;
    // Mirror the power.rs HALPWR-3 guard EXACTLY (fail-closed = None):
    //   vout < 0.0 (defensive)  OR  !voltage_within_driver_ceiling == voltage_mv > ceiling.
    if vout < 0.0 || voltage_mv > ceiling_mv {
        return None;
    }

    let magnitude = ds4432u_magnitude_code(voltage_mv);

    // Fail CLOSED on a low-side magnitude collision. For low voltage requests
    // (<= ~735 mV) the DS4432U "change" magnitude exceeds 127, so `ceil() as u8`
    // sets/saturates bit 7 — a 0 mV request even lands on 0xFF (source bit + max
    // magnitude = MAX rail-RAISE). Bit 7 is the source/sink DIRECTION bit ORed on
    // below, so a colliding magnitude silently mis-sets the rail the WRONG way. On
    // DS4432U boards (Max/Ultra/Supra) the rail cannot be disabled over I2C and has
    // no PMBus over-current protection, so a corrupted DAC byte could over-volt the
    // ASIC. Refuse it (power.rs maps None -> VoltageOutOfRange). In-range setpoints
    // have magnitude < 128 (the real-board min clamp of 850-1000 mV sits far above
    // this band), so this never rejects a legitimate voltage.
    if magnitude >= 0x80 {
        return None;
    }

    let mut reg = magnitude;
    // If Vout > VFB (always true for a mining setpoint), set the source bit.
    if vout > DS4432U_VREF {
        reg |= 0x80;
    }

    Some(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ─── PMBus Linear11 round-trip ───────────────────────────────────────────

    #[test]
    fn linear11_round_trips_within_mantissa_precision() {
        // The 11-bit signed mantissa is normalized into [512, 1024), so the
        // worst-case relative error of encode∘decode is ~1/512 (≈0.2%). These
        // representative PMBus Linear11 quantities (temps, currents, limits) must
        // survive the round-trip within that precision.
        for &v in &[
            0.0f32, 25.0, 50.0, 85.0, 100.0, 5.0, 12.5, 0.6, 1.2, 130.0, 10.5, 31.25,
        ] {
            let raw = f32_to_pmbus_linear11(v);
            let back = pmbus_linear11_to_f32(raw);
            let tol = v.abs() * 0.005 + 1e-4;
            assert!(
                (back - v).abs() <= tol,
                "linear11 round-trip {v} -> 0x{raw:04x} -> {back} exceeds tol {tol}"
            );
        }
    }

    #[test]
    fn linear11_round_trips_negative_values() {
        // f32_to_pmbus_linear11 encodes the sign via the two's-complement mantissa;
        // decode must recover negative PMBus quantities (e.g. signed temps).
        for &v in &[-5.0f32, -40.0, -12.5, -100.0] {
            let raw = f32_to_pmbus_linear11(v);
            let back = pmbus_linear11_to_f32(raw);
            let tol = v.abs() * 0.005 + 1e-4;
            assert!(
                (back - v).abs() <= tol,
                "linear11 round-trip {v} -> 0x{raw:04x} -> {back} exceeds tol {tol}"
            );
        }
    }

    #[test]
    fn linear11_round_edge_does_not_sign_flip_protection_limit() {
        // A mantissa that ROUNDS up to exactly 1024 is outside the signed 11-bit
        // range; without the re-normalization guard it masks to -1024 — a SIGN
        // FLIP that writes a NEGATIVE protection limit (VIN_OV / IOUT_OC / OT /
        // FREQUENCY_SWITCH) and trips an instant FALSE fault. A positive input must
        // never encode to a negative quantity (fails before the fix for 1023.6 and
        // for the huge/exponent-saturated inputs).
        for &v in &[1023.6f32, 1024.0, 1023.5, 2047.2, 4095.9, 1.0e9] {
            let back = pmbus_linear11_to_f32(f32_to_pmbus_linear11(v));
            assert!(
                back >= 0.0,
                "positive {v} must not encode to a NEGATIVE limit (sign flip): decoded {back}"
            );
        }
        // 1023.6 must decode to ~1024, not -1024.
        let back = pmbus_linear11_to_f32(f32_to_pmbus_linear11(1023.6));
        assert!(
            (back - 1024.0).abs() <= 2.0,
            "1023.6 must round to ~1024, got {back}"
        );
        // The negative boundary stays negative (never flips positive).
        assert!(pmbus_linear11_to_f32(f32_to_pmbus_linear11(-1023.6)) < 0.0);
    }

    #[test]
    fn linear11_zero_is_zero() {
        assert_eq!(f32_to_pmbus_linear11(0.0), 0);
        assert_eq!(pmbus_linear11_to_f32(0), 0.0);
    }

    // ─── PMBus ULINEAR16 round-trip ──────────────────────────────────────────

    #[test]
    fn ulinear16_round_trips_within_one_lsb() {
        // VOUT ULINEAR16: round-trip resolution is one LSB == 2^exponent. TPS546
        // VOUT_MODE is commonly 2^-9 (=1/512 V). Real core-voltage setpoints must
        // round-trip within a single LSB for both representative exponents.
        for &exp in &[-9i8, -8] {
            let lsb = 2.0_f32.powi(exp as i32);
            for &v in &[0.6f32, 1.0, 1.2, 1.166, 1.4, 1.55, 0.0] {
                let raw = f32_to_pmbus_ulinear16(v, exp);
                let back = pmbus_ulinear16_to_f32(raw, exp);
                assert!(
                    (back - v).abs() <= lsb,
                    "ulinear16 round-trip {v}V @2^{exp} -> {raw} -> {back} exceeds 1 LSB {lsb}"
                );
            }
        }
    }

    #[test]
    fn ulinear16_decode_matches_hand_value() {
        // 1.2 V at 2^-9: raw = round(1.2 * 512) = 614; decode = 614/512.
        let raw = f32_to_pmbus_ulinear16(1.2, -9);
        assert_eq!(raw, 614);
        assert!((pmbus_ulinear16_to_f32(614, -9) - 1.19921875).abs() < 1e-6);
    }

    // ─── DS4432U DAC code ────────────────────────────────────────────────────
    //
    // The driver ceiling passed by power.rs (HALPWR-3 `DRIVER_VOLTAGE_CEILING_MV`).
    const CEIL: u16 = 1600;

    /// Pins the EXISTING Rust DS4432U transfer function (RA=RB=4700, VREF=0.6,
    /// IFS=0.000098921, source bit when `Vout > VREF`). Extraction is
    /// byte-identical, so these are exactly the codes `power.rs` already writes.
    ///
    /// DIVERGENCE FROM ESP-Miner reference `main/power/DS4432U.c` (RA=4750,
    /// RB=3320, VNOM=1.451, MSB set when `Vout < VNOM`) — same input voltages
    /// would yield very different codes there, computed from that file:
    ///   1.000 V -> 0xFC (magnitude 124 | 0x80)   [Rust: 0xB7]
    ///   1.200 V -> 0xC6 (magnitude  70 | 0x80)   [Rust: 0x80]
    ///   1.550 V -> 0x19 (magnitude  25, no bit)  [Rust: 0xE0]
    /// This is flagged for hardware verification against the DCENT_axe DS4432U
    /// feedback-network schematic; it is NOT changed here (would alter the live
    /// rail on Max/Ultra/Supra — out of scope for behavior-preserving extraction).
    #[test]
    fn ds4432u_dac_code_reference_voltages_pin_rust_behavior() {
        assert_eq!(ds4432u_dac_code(1000, CEIL), Some(0xB7)); // 55 | 0x80
        assert_eq!(ds4432u_dac_code(1200, CEIL), Some(0x80)); //  0 | 0x80
        assert_eq!(ds4432u_dac_code(1550, CEIL), Some(0xE0)); // 96 | 0x80
        assert_eq!(ds4432u_dac_code(1600, CEIL), Some(0xEE)); // 110 | 0x80 (at ceiling)
    }

    #[test]
    #[ignore = "operator bench only; validates supplied DS4432U measurements without touching hardware"]
    fn ds4432u_operator_bench_measurements_accept_meter_log() {
        let raw = std::env::var("DCENT_DS4432U_BENCH_MV_CSV")
            .expect("set DCENT_DS4432U_BENCH_MV_CSV like 1000=1003,1200=1195,1550=1540");
        let mut measured_mv = BTreeMap::new();
        for entry in raw
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (requested, measured) = entry.split_once('=').unwrap_or_else(|| {
                panic!("bad bench entry {entry:?}; expected requested=measured")
            });
            let requested = requested
                .parse::<u16>()
                .unwrap_or_else(|err| panic!("bad requested mV in {entry:?}: {err}"));
            let measured = measured
                .parse::<u16>()
                .unwrap_or_else(|err| panic!("bad measured mV in {entry:?}: {err}"));
            measured_mv.insert(requested, measured);
        }

        for (requested, expected_code) in [(1000, 0xB7), (1200, 0x80), (1550, 0xE0)] {
            assert_eq!(
                ds4432u_dac_code(requested, CEIL),
                Some(expected_code),
                "bench harness must pin the existing Rust DAC code for {requested} mV"
            );
            let measured = *measured_mv
                .get(&requested)
                .unwrap_or_else(|| panic!("missing bench measurement for {requested} mV"));
            let delta = measured.abs_diff(requested);
            assert!(
                delta <= 50,
                "bench measurement for {requested} mV was {measured} mV, delta {delta} mV exceeds +/-50 mV"
            );
        }
    }

    #[test]
    fn ds4432u_dac_code_fails_closed_above_ceiling() {
        // power.rs returns VoltageOutOfRange above the driver ceiling; the pure
        // fn fails closed (None) on the same condition (strict `> ceiling`).
        assert_eq!(ds4432u_dac_code(1601, CEIL), None);
        assert_eq!(ds4432u_dac_code(2000, CEIL), None);
        assert_eq!(ds4432u_dac_code(u16::MAX, CEIL), None);
        // Exactly at the ceiling is allowed (inclusive).
        assert!(ds4432u_dac_code(CEIL, CEIL).is_some());
    }

    #[test]
    fn ds4432u_source_bit_reflects_vout_vs_vref() {
        // The source/sink direction bit decision (`Vout > VREF`) tested in
        // ISOLATION from the magnitude: ds4432u_dac_code ORs 0x80 on top of
        // ds4432u_magnitude_code iff Vout > VREF (0.6 V).
        // Mining setpoint (1.2 V > 0.6 V): source bit ORed on. In the reachable
        // (magnitude < 128) range Vout is always > VREF, so the source bit is set.
        let mag = ds4432u_magnitude_code(1200);
        assert!(mag < 0x80, "in-range magnitude must be < 128");
        assert_eq!(
            ds4432u_dac_code(1200, CEIL),
            Some(mag | 0x80),
            "Vout>VREF must OR the 0x80 source bit on top of the magnitude"
        );
    }

    #[test]
    fn ds4432u_low_side_request_fails_closed_not_max_boost() {
        // Low-side fail-closed guard (safety). A voltage request whose DS4432U
        // magnitude reaches 0x80 collides with the source/sink DIRECTION bit; the
        // worst case (0 mV) previously encoded as 0xFF = source + max magnitude =
        // MAX rail-RAISE on a board with no PMBus OC protection. These must all be
        // REFUSED (None), never a corrupted DAC byte.
        for mv in [0u16, 100, 500, 600, 700, 731] {
            assert_eq!(
                ds4432u_dac_code(mv, CEIL),
                None,
                "low-side request {mv} mV (magnitude >= 0x80) must fail closed, not encode a boost"
            );
            assert!(
                ds4432u_magnitude_code(mv) >= 0x80,
                "test vector {mv} mV must actually be in the colliding band"
            );
        }
        // And every legitimate in-range setpoint is still accepted, byte-unchanged.
        for mv in [850u16, 1000, 1200, 1550, 1600] {
            let code = ds4432u_dac_code(mv, CEIL);
            assert!(
                code.is_some(),
                "legitimate setpoint {mv} mV must be accepted"
            );
            // reg low bits equal the raw magnitude; only the source bit adds 0x80.
            let reg = code.unwrap();
            assert_eq!(reg & 0x7F, ds4432u_magnitude_code(mv));
        }
    }

    #[test]
    fn ds4432u_magnitude_stays_below_128_across_legal_range() {
        // INVARIANT: across the legal DS4432U operating range — lowest board min
        // (Supra = 850 mV, board.rs) up to the 1600 mV driver ceiling — the 7-bit
        // magnitude must never reach 128, or it collides with the 0x80 direction
        // bit and silently mis-sets the rail. (In-range max is ~110 @ 1600 mV.)
        let mut worst = 0u8;
        for mv in (850u16..=1600).step_by(5) {
            let mag = ds4432u_magnitude_code(mv);
            assert!(
                mag < 128,
                "DS4432U magnitude {mag} >= 128 at {mv} mV collides with the 0x80 source bit"
            );
            worst = worst.max(mag);
        }
        // Sanity: the worst-case magnitude in range is comfortably clear of 128.
        assert!(worst <= 110, "unexpected in-range magnitude max {worst}");
    }

    #[test]
    fn ds4432u_magnitude_and_dac_code_agree_on_low_bits() {
        // The magnitude bits of the final reg (reg & 0x7F) must equal the raw
        // magnitude code across the legal range (no overflow into bit 7 means the
        // source OR is the ONLY contributor to 0x80). This is the same invariant
        // from the consumer's angle.
        for mv in (850u16..=1600).step_by(25) {
            let mag = ds4432u_magnitude_code(mv);
            let reg = ds4432u_dac_code(mv, CEIL).unwrap();
            assert_eq!(
                reg & 0x7F,
                mag,
                "reg magnitude bits drifted from the code at {mv} mV"
            );
            assert_eq!(
                reg & 0x80,
                0x80,
                "mining setpoint at {mv} mV must set the source bit"
            );
        }
    }
}
