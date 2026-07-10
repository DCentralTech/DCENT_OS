//! Pure, no-HAL time helpers shared across the schedule-window features
//! (thermal night-mode, scheduled curtailment, and the autotuner
//! electricity-rate schedule) so they all convert UTC -> local the same way.

/// Valid inclusive range for a whole-hour timezone offset (UTC-12 .. UTC+14,
/// the real-world extremes). Callers should reject configs outside this.
pub const MIN_TZ_OFFSET_HOURS: i8 = -12;
pub const MAX_TZ_OFFSET_HOURS: i8 = 14;

/// Convert a UTC hour-of-day (0-23) to the operator's local hour-of-day using a
/// whole-hour timezone offset, wrapping correctly across the midnight boundary.
///
/// Without this, a 22:00 quiet/curtail window configured by an operator in
/// (say) Quebec (UTC-4/-5) fires at 22:00 **UTC** = ~17:00-18:00 local — the
/// wrong wall-clock time. `offset_hours` is the standard signed UTC offset
/// (e.g. -5 for EST, +1 for CET). Inputs outside the expected ranges are still
/// wrapped into 0-23, but callers should validate `offset_hours` to
/// `[MIN_TZ_OFFSET_HOURS, MAX_TZ_OFFSET_HOURS]` at config load.
///
/// FWSTAB-1.
pub fn local_hour_from_utc(utc_hour: u8, offset_hours: i8) -> u8 {
    (((utc_hour as i16 + offset_hours as i16) % 24 + 24) % 24) as u8
}

/// True when `offset_hours` is within the real-world `[-12, 14]` range.
pub fn is_valid_tz_offset(offset_hours: i8) -> bool {
    (MIN_TZ_OFFSET_HOURS..=MAX_TZ_OFFSET_HOURS).contains(&offset_hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_offset_is_identity_for_every_hour() {
        for h in 0..24u8 {
            assert_eq!(local_hour_from_utc(h, 0), h);
        }
    }

    #[test]
    fn negative_offset_wraps_backward_past_midnight() {
        // 02:00 UTC in EST (-5) is 21:00 the previous day.
        assert_eq!(local_hour_from_utc(2, -5), 21);
        assert_eq!(local_hour_from_utc(22, -5), 17);
        assert_eq!(local_hour_from_utc(0, -1), 23);
        // The exact Quebec quiet-window bug this fixes: 22:00 local must map back.
        assert_eq!(local_hour_from_utc(3, -5), 22); // 03:00 UTC == 22:00 EST
    }

    #[test]
    fn positive_offset_wraps_forward_past_midnight() {
        assert_eq!(local_hour_from_utc(23, 1), 0); // CET
        assert_eq!(local_hour_from_utc(22, 4), 2);
        assert_eq!(local_hour_from_utc(20, 14), 10); // UTC+14 (Line Islands)
    }

    #[test]
    fn every_hour_and_valid_offset_stays_in_range() {
        for h in 0..24u8 {
            for off in MIN_TZ_OFFSET_HOURS..=MAX_TZ_OFFSET_HOURS {
                let lh = local_hour_from_utc(h, off);
                assert!(lh < 24, "hour {h} offset {off} -> {lh} out of range");
            }
        }
    }

    #[test]
    fn tz_offset_validation_bounds() {
        assert!(is_valid_tz_offset(0));
        assert!(is_valid_tz_offset(-12));
        assert!(is_valid_tz_offset(14));
        assert!(!is_valid_tz_offset(-13));
        assert!(!is_valid_tz_offset(15));
    }
}
