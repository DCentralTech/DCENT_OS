//! Space heater mode controller.
//!
//! Adds a second PID control layer on top of the chip thermal loop.
//! Layer 1 (existing): chip_temp -> PID -> fan_speed + freq_throttle
//! Layer 2 (heater):   target_heat -> PID -> power_target_watts -> Layer 1
//!
//! The heater controller manages power output targeting, room temperature
//! sensing (optional), power presets, and night mode scheduling.
//!
//! Sensor is NOT required. The default mode is Manual -- the user selects
//! a power preset and the dashboard shows estimated BTU output.

use serde::{Deserialize, Serialize};

use crate::controller::PidController;
use crate::profiles::{PowerPreset, WATTS_TO_BTU};

fn finite_nonnegative_or_zero(value: f32) -> f32 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        0.0
    }
}

/// Temperature source for space heater mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum TempSource {
    /// No room sensor. User picks a power preset directly.
    /// Dashboard shows estimated BTU output for each preset.
    #[default]
    Manual,

    /// External sensor (USB, Zigbee, or network sensor).
    /// dcentrald reads via configured URL or device path.
    Sensor { url: String, poll_interval_s: u16 },

    /// Home Assistant entity (e.g., sensor.living_room_temperature).
    /// Requires MQTT or HA REST API integration.
    HomeAssistant { entity_id: String },

    /// User manually sets a room temp value via API/dashboard.
    /// Useful when no sensor is available but user knows approximate room temp.
    UserInput,
}

/// Night mode configuration for space heater mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightMode {
    /// Whether night mode is enabled.
    pub enabled: bool,
    /// Start hour (24h format, e.g., 22 = 10 PM).
    pub start_hour: u8,
    /// End hour (24h format, e.g., 7 = 7 AM).
    pub end_hour: u8,
    /// Reduced fan ceiling during night hours (PWM).
    pub max_fan_pwm: u8,
    /// Reduce power target by this percentage during night hours.
    pub power_reduction_pct: u8,
    /// UTC offset in hours (e.g., -5 for EST, +1 for CET). Default: 0 (UTC).
    /// Applied to SystemTime to derive local wall-clock hour for night mode scheduling.
    #[serde(default)]
    pub timezone_offset_hours: i8,
}

impl Default for NightMode {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: 22,
            end_hour: 7,
            max_fan_pwm: 30,
            power_reduction_pct: 40,
            timezone_offset_hours: 0,
        }
    }
}

/// Space heater mode controller.
pub struct HeaterController {
    /// User-selected power/heat target in watts.
    pub target_heat_watts: u32,
    /// Temperature source (how room temp is obtained, if at all).
    pub temp_source: TempSource,
    /// Current room temperature (None if manual mode without sensor).
    pub room_temp_c: Option<f32>,
    /// Measured/estimated current power consumption.
    pub current_power_w: f32,
    /// PID controller for power -> frequency adjustment.
    pub pid: PidController,
    /// Active power preset name (if using presets).
    pub active_preset: Option<String>,
    /// Night mode configuration.
    pub night_mode: NightMode,
    /// Electricity rate in $/kWh.
    pub electricity_rate: f32,
    /// Currency for cost display.
    pub currency: String,
}

impl HeaterController {
    /// Create a new heater controller with default settings.
    pub fn new(target_watts: u32) -> Self {
        Self {
            target_heat_watts: target_watts,
            temp_source: TempSource::Manual,
            room_temp_c: None,
            current_power_w: 0.0,
            pid: PidController::new(target_watts as f32),
            active_preset: Some("medium".to_string()),
            night_mode: NightMode::default(),
            electricity_rate: 0.12,
            currency: "USD".to_string(),
        }
    }

    /// Set the power target from a preset.
    pub fn set_preset(&mut self, preset: &PowerPreset) {
        self.target_heat_watts = preset.watts;
        self.active_preset = Some(preset.name.clone());
        self.pid.setpoint = preset.watts as f32;
        self.pid.reset();
    }

    /// Set an exact wattage target.
    pub fn set_target_watts(&mut self, watts: u32) {
        self.target_heat_watts = watts;
        self.active_preset = None;
        self.pid.setpoint = watts as f32;
        self.pid.reset();
    }

    /// Update room temperature from sensor/user input.
    pub fn update_room_temp(&mut self, temp_c: f32) {
        self.room_temp_c = temp_c.is_finite().then_some(temp_c);
    }

    /// Update measured power consumption.
    pub fn update_power(&mut self, watts: f32) {
        self.current_power_w = finite_nonnegative_or_zero(watts);
    }

    /// Compute the next power adjustment.
    ///
    /// Returns a frequency adjustment factor (0.0 to 2.0):
    ///   < 1.0 = reduce frequency (too much power)
    ///   > 1.0 = increase frequency (not enough power)
    /// > = 1.0 = on target
    pub fn compute_adjustment(&mut self) -> f32 {
        let effective_target = self.effective_target_watts();
        let measured_power_w = finite_nonnegative_or_zero(self.current_power_w);
        self.pid.setpoint = effective_target as f32;
        let _output = self.pid.update(measured_power_w);

        // Convert PID output to frequency adjustment factor
        if effective_target == 0 || measured_power_w <= 0.0 {
            return 1.0;
        }

        let ratio = effective_target as f32 / measured_power_w;
        if !ratio.is_finite() {
            return 1.0;
        }
        ratio.clamp(0.5, 1.5)
    }

    /// Get the effective target watts, accounting for night mode.
    pub fn effective_target_watts(&self) -> u32 {
        if self.night_mode.enabled && self.is_night_hours() {
            let reduction =
                self.target_heat_watts * self.night_mode.power_reduction_pct as u32 / 100;
            self.target_heat_watts.saturating_sub(reduction)
        } else {
            self.target_heat_watts
        }
    }

    /// Check if current time falls within night mode hours.
    ///
    /// Handles wrapping across midnight (e.g., start=22, end=7 means
    /// 10 PM to 7 AM is considered night).
    pub fn is_night_hours(&self) -> bool {
        use std::time::{SystemTime, UNIX_EPOCH};

        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Apply timezone offset to derive local wall-clock hour.
        // rem_euclid handles negative offsets correctly (e.g., UTC-5 at 2AM UTC = 9PM previous day).
        let offset_secs = self.night_mode.timezone_offset_hours as i64 * 3600;
        let local_secs = (secs as i64 + offset_secs).rem_euclid(86400) as u64;
        let hour = (local_secs / 3600) as u8;

        let start = self.night_mode.start_hour;
        let end = self.night_mode.end_hour;

        if start <= end {
            // Simple range (e.g., 1:00 to 6:00)
            hour >= start && hour < end
        } else {
            // Wraps midnight (e.g., 22:00 to 7:00)
            hour >= start || hour < end
        }
    }

    /// Get current BTU/h output.
    pub fn current_btu_h(&self) -> u32 {
        (finite_nonnegative_or_zero(self.current_power_w) * WATTS_TO_BTU) as u32
    }

    /// Get target BTU/h output.
    pub fn target_btu_h(&self) -> u32 {
        (self.effective_target_watts() as f32 * WATTS_TO_BTU) as u32
    }

    /// Get estimated daily electricity cost.
    pub fn daily_cost(&self) -> f32 {
        let power_w = finite_nonnegative_or_zero(self.current_power_w);
        let rate = finite_nonnegative_or_zero(self.electricity_rate);
        power_w * 24.0 * rate / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_room_temperature_is_dropped() {
        let mut heater = HeaterController::new(1_000);

        heater.update_room_temp(21.5);
        assert_eq!(heater.room_temp_c, Some(21.5));

        heater.update_room_temp(f32::NAN);
        assert_eq!(
            heater.room_temp_c, None,
            "a non-finite room temperature must not remain as a usable sensor reading"
        );
    }

    #[test]
    fn non_finite_power_never_yields_nan_or_boost() {
        let mut heater = HeaterController::new(1_000);

        heater.update_power(f32::NAN);
        assert_eq!(heater.current_power_w, 0.0);
        assert_eq!(heater.compute_adjustment(), 1.0);

        heater.current_power_w = f32::INFINITY;
        assert_eq!(
            heater.compute_adjustment(),
            1.0,
            "infinite measured power must not produce a non-finite adjustment"
        );

        heater.current_power_w = f32::NAN;
        assert_eq!(heater.current_btu_h(), 0);
        heater.electricity_rate = f32::NAN;
        assert_eq!(heater.daily_cost(), 0.0);
    }
}
