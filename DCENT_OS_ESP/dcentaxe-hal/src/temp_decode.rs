//! Pure EMC2101 external-diode temperature decode (no ESP-IDF / no log deps).
//!
//! Extracted from `temp.rs::Emc2101::read_external_temp` so the
//! sensor-availability decision is host-unit-testable without the ESP-IDF
//! toolchain (`temp.rs` links esp-idf-hal and only compiles for the espidf
//! target). No hardware/ESP-IDF deps live here.
//!
//! HALT-3: a valid external-diode reading at or above 127C is REAL danger and
//! must propagate to the caller (so the 105C emergency path sees it). The only
//! reading dropped as "unavailable" here is the negative open-circuit sentinel
//! (`< -10C`). Availability is gated solely on the STATUS external-fault bit.

/// EMC2101 status bit: External diode fault (single source of truth — this
/// module is also compiled standalone into `dcentaxe-core` where `temp.rs`,
/// the previous owner of this const, is not present).
pub const EMC2101_STATUS_EXT_FAULT: u8 = 0x04;

/// Lower reject bound: a temperature below this is the negative open-circuit
/// sentinel (e.g. MSB=0x80 decodes to -128C), not a real ASIC reading.
const EXTERNAL_TEMP_MIN_C: f32 = -10.0;

/// Decode the EMC2101 external-diode temperature from the raw STATUS / MSB / LSB
/// register bytes.
///
/// - Returns `None` only when the diode is faulted (STATUS external-fault bit
///   set) or when the decoded value is the negative open-circuit sentinel
///   (`< -10C`).
/// - A non-fault reading at or above 127C is returned as `Some(temp)` so a
///   runaway chip reaches `max_temp` and the 105C thermal-emergency path
///   (HALT-3 — previously these readings were dropped as `None`, failing open).
pub fn decode_external_temp(status: u8, msb: u8, lsb: u8) -> Option<f32> {
    // Availability is gated ONLY on the STATUS external-fault bit.
    if (status & EMC2101_STATUS_EXT_FAULT) != 0 {
        return None;
    }

    // Combine: integer part (signed 8-bit) + fractional (3 bits = 0.125C res).
    let integer = msb as i8 as f32;
    let fraction = ((lsb >> 5) & 0x07) as f32 * 0.125;
    let temp = integer + fraction;

    // Keep ONLY the low reject for the negative open-circuit sentinel; a high
    // (>=127C) non-fault reading is REAL danger and is intentionally returned.
    if temp < EXTERNAL_TEMP_MIN_C {
        return None;
    }

    Some(temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_non_fault_reading_is_some() {
        // THE HALT-3 case: 127C now propagates instead of being dropped to None.
        assert_eq!(decode_external_temp(0x00, 0x7F, 0x00), Some(127.0));
    }

    #[test]
    fn high_non_fault_reading_with_fraction_is_some() {
        assert_eq!(decode_external_temp(0x00, 0x7F, 0xE0), Some(127.875));
    }

    #[test]
    fn fault_bit_overrides_valid_temp() {
        // Fault bit set despite a benign 50C integer reading => unavailable.
        assert_eq!(decode_external_temp(0x04, 0x32, 0x00), None);
    }

    #[test]
    fn fault_bit_among_other_status_bits_is_none() {
        assert_eq!(decode_external_temp(0xFF, 0x7F, 0x00), None);
    }

    #[test]
    fn negative_open_circuit_sentinel_is_none() {
        // MSB=0x80 decodes to -128C (open) and MSB=0xF5 to -11C: both < -10C.
        assert_eq!(decode_external_temp(0x00, 0x80, 0x00), None);
        assert_eq!(decode_external_temp(0x00, 0xF5, 0x00), None);
    }

    #[test]
    fn lower_bound_minus_ten_is_kept() {
        // Reject is strictly `< -10`, so exactly -10C is a valid reading.
        assert_eq!(decode_external_temp(0x00, 0xF6, 0x00), Some(-10.0));
    }

    #[test]
    fn normal_midrange_reading_is_some() {
        assert_eq!(decode_external_temp(0x00, 0x32, 0x40), Some(50.25));
    }

    #[test]
    fn high_status_bits_without_fault_are_ignored() {
        let status = 0xFF & !EMC2101_STATUS_EXT_FAULT;
        assert_eq!(decode_external_temp(status, 0x46, 0x00), Some(70.0));
    }
}
