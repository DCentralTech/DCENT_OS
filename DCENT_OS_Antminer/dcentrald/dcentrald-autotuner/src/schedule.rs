//! Time-of-use power scheduling (DPS — Dynamic Power Scheduling).
//!
//! Users configure electricity rate schedules by hour of day. The autotuner
//! automatically adjusts the power target per time slot, running at maximum
//! hashrate during cheap hours and throttling during expensive hours.
//!
//! A 60-second frequency ramp smooths transitions between power levels to
//! avoid sudden load changes that could trip home breakers.

use serde::{Deserialize, Serialize};

/// A single time slot in the power schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerSlot {
    /// Start hour (0-23, local time).
    pub start_hour: u8,
    /// End hour (0-23, local time). If end < start, wraps midnight.
    pub end_hour: u8,
    /// Power target for this slot (watts). 0 = max hashrate (no limit).
    pub target_watts: u32,
    /// Optional label for this slot (e.g., "off-peak", "peak", "super-off-peak").
    #[serde(default)]
    pub label: String,
}

/// Power schedule configuration.
///
/// Defines time-of-use electricity rate windows with per-slot power targets.
/// The autotuner checks the schedule every 60 seconds and adjusts power target
/// accordingly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PowerSchedule {
    /// Whether scheduling is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Time slots defining power targets by hour.
    #[serde(default)]
    pub slots: Vec<PowerSlot>,
    /// Frequency ramp duration when transitioning between power levels (seconds).
    /// Default: 60. Prevents sudden power changes that trip breakers.
    #[serde(default = "default_ramp_duration")]
    pub ramp_duration_s: u32,
    /// Timezone offset from UTC in hours (e.g., -5 for EST, -4 for EDT).
    ///
    /// Slot hours are specified in LOCAL time. This offset converts UTC
    /// system clock to local time for slot matching. Without this, a
    /// "22:00 off-peak" slot would fire at 3-4 AM local time in Quebec.
    /// Default: 0 (UTC). Range: [-12, 14].
    #[serde(default)]
    pub timezone_offset_hours: i8,
}

fn default_ramp_duration() -> u32 {
    60
}

impl PowerSchedule {
    /// Get the power target for the current time.
    ///
    /// Returns the `target_watts` from the matching slot, or None if no slot
    /// matches (use default/max hashrate).
    pub fn current_target_watts(&self) -> Option<u32> {
        if !self.enabled || self.slots.is_empty() {
            return None;
        }

        let utc_hour = chrono_free_hour();
        // Apply timezone offset to convert UTC to local time
        let local_hour = ((utc_hour as i16 + self.timezone_offset_hours as i16) % 24 + 24) % 24;
        self.target_for_hour(local_hour as u8)
    }

    /// Get the power target for a specific hour (0-23).
    pub fn target_for_hour(&self, hour: u8) -> Option<u32> {
        for slot in &self.slots {
            if slot_contains_hour(slot.start_hour, slot.end_hour, hour) {
                return Some(slot.target_watts);
            }
        }
        None
    }

    /// Validate the schedule configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.start_hour > 23 {
                return Err(format!("Slot {} start_hour {} > 23", i, slot.start_hour));
            }
            if slot.end_hour > 23 {
                return Err(format!("Slot {} end_hour {} > 23", i, slot.end_hour));
            }
        }
        if self.ramp_duration_s > 300 {
            return Err(format!(
                "ramp_duration_s {} exceeds max 300s",
                self.ramp_duration_s
            ));
        }
        if self.timezone_offset_hours < -12 || self.timezone_offset_hours > 14 {
            return Err(format!(
                "timezone_offset_hours {} out of range [-12, 14]",
                self.timezone_offset_hours,
            ));
        }
        Ok(())
    }
}

/// Check if an hour falls within a slot (handles midnight wrap).
fn slot_contains_hour(start: u8, end: u8, hour: u8) -> bool {
    if start <= end {
        // Normal range: e.g., 8-16
        hour >= start && hour < end
    } else {
        // Wraps midnight: e.g., 22-6 means 22,23,0,1,2,3,4,5
        hour >= start || hour < end
    }
}

/// Get the current hour of day (0-23) without pulling in chrono.
/// Uses std::time for a simple UTC-based hour. Users configure in local time;
/// timezone offset should be applied upstream.
fn chrono_free_hour() -> u8 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    ((secs % 86400) / 3600) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slot_contains_hour_normal() {
        // 8:00 - 16:00
        assert!(slot_contains_hour(8, 16, 8));
        assert!(slot_contains_hour(8, 16, 12));
        assert!(slot_contains_hour(8, 16, 15));
        assert!(!slot_contains_hour(8, 16, 16)); // end is exclusive
        assert!(!slot_contains_hour(8, 16, 7));
        assert!(!slot_contains_hour(8, 16, 20));
    }

    #[test]
    fn test_slot_contains_hour_midnight_wrap() {
        // 22:00 - 06:00 (overnight)
        assert!(slot_contains_hour(22, 6, 22));
        assert!(slot_contains_hour(22, 6, 23));
        assert!(slot_contains_hour(22, 6, 0));
        assert!(slot_contains_hour(22, 6, 3));
        assert!(slot_contains_hour(22, 6, 5));
        assert!(!slot_contains_hour(22, 6, 6)); // end is exclusive
        assert!(!slot_contains_hour(22, 6, 12));
        assert!(!slot_contains_hour(22, 6, 21));
    }

    #[test]
    fn test_schedule_target_for_hour() {
        let schedule = PowerSchedule {
            enabled: true,
            slots: vec![
                PowerSlot {
                    start_hour: 22,
                    end_hour: 6,
                    target_watts: 0, // max hashrate overnight
                    label: "off-peak".to_string(),
                },
                PowerSlot {
                    start_hour: 6,
                    end_hour: 14,
                    target_watts: 800, // mid-peak
                    label: "mid-peak".to_string(),
                },
                PowerSlot {
                    start_hour: 14,
                    end_hour: 22,
                    target_watts: 400, // peak — throttle hard
                    label: "peak".to_string(),
                },
            ],
            ramp_duration_s: 60,
            timezone_offset_hours: 0,
        };

        assert_eq!(schedule.target_for_hour(0), Some(0));
        assert_eq!(schedule.target_for_hour(3), Some(0));
        assert_eq!(schedule.target_for_hour(8), Some(800));
        assert_eq!(schedule.target_for_hour(15), Some(400));
        assert_eq!(schedule.target_for_hour(23), Some(0));
    }

    #[test]
    fn test_schedule_disabled() {
        let schedule = PowerSchedule {
            enabled: false,
            slots: vec![PowerSlot {
                start_hour: 0,
                end_hour: 24,
                target_watts: 400,
                label: String::new(),
            }],
            ramp_duration_s: 60,
            timezone_offset_hours: 0,
        };
        assert_eq!(schedule.current_target_watts(), None);
    }

    #[test]
    fn test_schedule_empty_slots() {
        let schedule = PowerSchedule {
            enabled: true,
            slots: vec![],
            ramp_duration_s: 60,
            timezone_offset_hours: 0,
        };
        assert_eq!(schedule.current_target_watts(), None);
    }

    #[test]
    fn test_schedule_validation() {
        let mut schedule = PowerSchedule::default();
        assert!(schedule.validate().is_ok());

        schedule.slots.push(PowerSlot {
            start_hour: 25,
            end_hour: 6,
            target_watts: 0,
            label: String::new(),
        });
        assert!(schedule.validate().is_err());

        schedule.slots.clear();
        schedule.ramp_duration_s = 600;
        assert!(schedule.validate().is_err());
    }
}
