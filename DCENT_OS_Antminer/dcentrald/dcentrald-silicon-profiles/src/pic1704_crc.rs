//! PIC16F1704 CRC8 constants from the  / R6 reverse-engineering pass.
//!
//! Pure data/helper module. It is intentionally independent of the recovery
//! tool protocol implementations; production daemon code should treat this as
//! a catalog reference unless a caller has explicit protocol evidence.

/// A42 (goldmine 2026-06-10): the S19 PIC16F1704 firmware watchdog expires
/// after **0x003D (61) main-loop counts** without a refresh, at which point it
/// drives "CLOSE DC-DC" (cuts the hashboard DC-DC rail). Recorded as the
/// MEASURE_VOLTAGE / heartbeat timing budget reference (informs the dsPIC
/// ENABLE-drift gap analysis). DATA ONLY — not wired into any live keepalive
/// path here; the live heartbeat cadence lives in
/// `dcentrald-silicon-profiles::pic_heartbeat`.
pub const PIC16F1704_WATCHDOG_MAIN_LOOP_COUNTS: u16 = 0x003D;

/// W15.A4: PIC1704 CRC8 polynomial captured from RE findings.
pub const PIC1704_CRC8_POLY: u8 = 0x07;

/// CRC8 initial value used by the catalog helper.
pub const PIC1704_CRC8_INIT: u8 = 0x00;

/// Compute CRC-8 with polynomial 0x07, init 0x00, no final XOR.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = PIC1704_CRC8_INIT;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ PIC1704_CRC8_POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pic1704_crc8_poly_is_wave15_constant() {
        assert_eq!(PIC1704_CRC8_POLY, 0x07);
        assert_eq!(PIC1704_CRC8_INIT, 0x00);
    }

    #[test]
    fn pic16f1704_watchdog_main_loop_counts_pinned() {
        // A42 (goldmine 2026-06-10): S19 PIC16F1704 watchdog = 0x003D main-loop
        // counts → "CLOSE DC-DC" on expiry. DATA ONLY; pin the value.
        assert_eq!(PIC16F1704_WATCHDOG_MAIN_LOOP_COUNTS, 0x003D);
        assert_eq!(PIC16F1704_WATCHDOG_MAIN_LOOP_COUNTS, 61);
    }

    #[test]
    fn crc8_empty_input_is_init() {
        assert_eq!(crc8(&[]), 0x00);
    }

    #[test]
    fn crc8_standard_check_vector_matches_poly_07() {
        assert_eq!(crc8(b"123456789"), 0xF4);
    }
}
