//! Off-Grid / Direct DC power controller.
//!
//! Voltage-based curtailment for battery-powered mining:
//!   - Monitors DC bus voltage via ADC (INA226 I2C, sysfs, or simulated)
//!   - 5-zone state machine: Critical, Low, Normal, High, Full
//!   - When voltage drops: reduce frequency (ramp down)
//!   - When voltage rises: increase frequency (ramp up)
//!   - When voltage critical: deep sleep via curtailment controller
//!   - When voltage recovers: wake from sleep with gradual ramp
//!
//! Control loop runs every 2 seconds (fast response to battery/solar transients).
//! EMA filter (alpha=0.3) prevents oscillation from noisy readings.
//!
//! Integrates with existing:
//!   - FreqCommand channel (same path as autotuner)
//!   - CurtailmentController (sleep/wake)
//!   - Thermal PID loop (runs independently underneath — safety preserved)
//!
//! Inspired by Gridless Compute's Jua-Kali project (Go-based battery voltage
//! control loop for solar-powered mining in Africa). Reimplemented from scratch
//! in Rust with tighter DCENT_OS integration and 5-zone state machine.

use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::battery::VoltageThresholds;
use dcentrald_hal::adc::AdcReading;

fn finite_nonnegative_or_zero(value: f32) -> f32 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        0.0
    }
}

/// Off-grid voltage zone (determines mining behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoltageZone {
    /// Below critical: emergency sleep. Hash boards disabled, fans minimum.
    /// Protects battery from deep discharge damage.
    Critical,
    /// Below low: ramp down frequency. Reduce power consumption.
    Low,
    /// Normal range: hold steady. Mine at current frequency.
    Normal,
    /// Above high: ramp up frequency. Solar surplus available.
    High,
    /// Above full: battery charged. Mine at maximum frequency.
    Full,
}

/// Off-grid controller state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffGridState {
    /// Monitoring, voltage in normal range.
    Normal,
    /// Actively ramping down frequency (voltage dropping).
    RampingDown,
    /// Actively ramping up frequency (voltage rising).
    RampingUp,
    /// Deep sleep — boards disabled, waiting for recovery voltage.
    DeepSleep,
    /// No ADC reading available — defaulting to safe power level.
    SensorFault,
}

/// Action returned by the off-grid controller each tick.
#[derive(Debug, Clone)]
pub enum OffGridAction {
    /// No change needed (voltage in normal range, frequency stable).
    Hold,
    /// Set all chains to this frequency (MHz).
    SetFrequency(u16),
    /// Enter deep sleep (critical low voltage).
    Sleep,
    /// Wake from deep sleep and start ramping from this frequency.
    Wake(u16),
}

/// Live off-grid telemetry (published to dashboard via WebSocket).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OffGridTelemetry {
    /// Whether off-grid mode is active.
    pub enabled: bool,
    /// Current voltage zone.
    pub zone: String,
    /// Current controller state.
    pub state: String,
    /// Smoothed bus voltage in volts.
    pub bus_voltage_v: f32,
    /// Bus current in amps (0 if not measured).
    pub current_a: f32,
    /// Computed power in watts.
    pub power_w: f32,
    /// Estimated battery SoC percentage (0-100).
    pub battery_soc_pct: f32,
    /// Current target frequency in MHz.
    pub target_freq_mhz: u16,
    /// Target frequency as percentage of maximum (0-100).
    pub freq_pct: f32,
    /// Voltage rate of change in V/s (positive = charging).
    pub voltage_rate_vps: f32,
    /// Seconds on battery power this session.
    pub uptime_battery_s: u64,
    /// Cumulative energy consumed in watt-hours.
    pub energy_consumed_wh: f32,
    /// Active voltage thresholds (so dashboard shows correct zone bar).
    pub critical_v: f32,
    pub low_v: f32,
    pub high_v: f32,
    pub full_v: f32,
    /// Human-readable ADC backend/source name.
    #[serde(default)]
    pub sensor_source: String,
    /// Whether the backend provides current measurement.
    #[serde(default)]
    pub has_current: bool,
    /// Whether the voltage/current backend is healthy.
    #[serde(default)]
    pub sensor_ok: bool,
    /// Human-readable status/fault detail.
    #[serde(default)]
    pub message: String,
}

/// Off-grid controller.
pub struct OffGridController {
    /// Voltage thresholds for zone classification.
    thresholds: VoltageThresholds,
    /// Current state.
    state: OffGridState,
    /// Current target frequency (MHz).
    target_freq_mhz: u16,
    /// Maximum configured frequency.
    max_freq_mhz: u16,
    /// Minimum frequency floor.
    min_freq_mhz: u16,
    /// Frequency step per adjustment (MHz).
    freq_step_mhz: u16,
    /// EMA-smoothed voltage (alpha=0.3).
    smoothed_voltage: f32,
    /// Voltage history for rate-of-change (last 30 readings).
    voltage_history: Vec<(Instant, f32)>,
    /// Cumulative energy consumed (watt-hours).
    energy_consumed_wh: f32,
    /// Controller start time.
    start_time: Instant,
    /// Last tick time.
    last_tick: Instant,
    /// Whether this is the first tick (no EMA history yet).
    first_tick: bool,
}

impl OffGridController {
    /// Create a new off-grid controller.
    ///
    /// `max_freq_mhz`: the mining config's target frequency (ceiling).
    /// `min_freq_mhz`: minimum frequency floor (keeps chips warm, default 200).
    /// `freq_step_mhz`: frequency adjustment step per tick (default 25).
    pub fn new(
        thresholds: VoltageThresholds,
        max_freq_mhz: u16,
        min_freq_mhz: u16,
        freq_step_mhz: u16,
    ) -> Self {
        Self {
            thresholds,
            state: OffGridState::Normal,
            target_freq_mhz: max_freq_mhz,
            max_freq_mhz,
            // Clamp the floor down to the ceiling so a misconfigured min above the
            // mining ceiling can never invert the (min,max) bounds. The controller
            // uses separate .min()/.max() ops (not clamp), so this is a correctness
            // guard (never command a freq above the operator's ceiling), not a panic
            // guard.
            min_freq_mhz: min_freq_mhz.max(100).min(max_freq_mhz.max(1)),
            freq_step_mhz: freq_step_mhz.max(5),
            smoothed_voltage: 0.0,
            voltage_history: Vec::with_capacity(32),
            energy_consumed_wh: 0.0,
            start_time: Instant::now(),
            last_tick: Instant::now(),
            first_tick: true,
        }
    }

    /// Run one control loop iteration (called every 2 seconds).
    ///
    /// Returns an action the daemon should take (set frequency, sleep, or wake).
    pub fn tick(&mut self, reading: &AdcReading) -> OffGridAction {
        let now = Instant::now();
        let dt_s = (now - self.last_tick).as_secs_f32();
        self.last_tick = now;

        // Fail-closed on a non-finite (NaN / ±Inf) bus-voltage reading: a garbage
        // ADC decode must NOT reach the EMA (`0.3*NaN + 0.7*prev` poisons the
        // smoothed voltage permanently — sticky even after the sensor recovers)
        // or `classify_zone` (every `<` comparison against NaN is false, so it
        // falls through to `Full` → `SetFrequency(target + step)`, ramping
        // frequency UP and inverting the deep-discharge protection this
        // controller exists for). Treat it like a missing sensor: enter the
        // fail-safe fault state (target freq 0) and hold load down.
        if !reading.voltage_v.is_finite() {
            self.enter_sensor_fault();
            return OffGridAction::Sleep;
        }

        // EMA voltage filter (alpha=0.3)
        if self.first_tick {
            self.smoothed_voltage = reading.voltage_v;
            self.first_tick = false;
        } else {
            self.smoothed_voltage = 0.3 * reading.voltage_v + 0.7 * self.smoothed_voltage;
        }
        if self.state == OffGridState::DeepSleep && reading.voltage_v >= self.thresholds.recovery_v
        {
            self.smoothed_voltage = reading.voltage_v;
        }

        // Accumulate energy
        self.energy_consumed_wh += finite_nonnegative_or_zero(reading.power_w) * dt_s / 3600.0;

        // Update voltage history (for rate-of-change)
        self.voltage_history.push((now, self.smoothed_voltage));
        if self.voltage_history.len() > 30 {
            self.voltage_history.remove(0);
        }

        let voltage = self.smoothed_voltage;
        let zone = self.classify_zone(voltage);

        if self.state == OffGridState::SensorFault {
            if voltage < self.thresholds.recovery_v {
                self.state = OffGridState::DeepSleep;
                self.target_freq_mhz = 0;
                return OffGridAction::Sleep;
            }

            self.state = OffGridState::RampingUp;
            self.target_freq_mhz = self.min_freq_mhz;
            return OffGridAction::Wake(self.min_freq_mhz);
        }

        // State machine
        match zone {
            VoltageZone::Critical => {
                if self.state != OffGridState::DeepSleep {
                    tracing::warn!(
                        voltage = format_args!("{:.1}V", voltage),
                        critical = format_args!("{:.1}V", self.thresholds.critical_v),
                        "OFF-GRID: CRITICAL voltage — entering deep sleep to protect battery"
                    );
                }
                self.state = OffGridState::DeepSleep;
                self.target_freq_mhz = 0;
                OffGridAction::Sleep
            }

            VoltageZone::Low => {
                // If in deep sleep, stay asleep until recovery voltage
                if self.state == OffGridState::DeepSleep {
                    if voltage >= self.thresholds.recovery_v {
                        tracing::info!(
                            voltage = format_args!("{:.1}V", voltage),
                            recovery = format_args!("{:.1}V", self.thresholds.recovery_v),
                            "OFF-GRID: Recovery voltage reached — waking from deep sleep"
                        );
                        self.state = OffGridState::RampingUp;
                        self.target_freq_mhz = self.min_freq_mhz;
                        return OffGridAction::Wake(self.min_freq_mhz);
                    }
                    return OffGridAction::Sleep;
                }

                // Ramp down
                let old_freq = self.target_freq_mhz;
                self.target_freq_mhz = self
                    .target_freq_mhz
                    .saturating_sub(self.freq_step_mhz)
                    .max(self.min_freq_mhz);
                self.state = OffGridState::RampingDown;

                if self.target_freq_mhz != old_freq {
                    tracing::info!(
                        voltage = format_args!("{:.1}V", voltage),
                        freq = self.target_freq_mhz,
                        "OFF-GRID: Low voltage — ramping down to {} MHz",
                        self.target_freq_mhz
                    );
                }
                OffGridAction::SetFrequency(self.target_freq_mhz)
            }

            VoltageZone::Normal => {
                // If in deep sleep, check recovery
                if self.state == OffGridState::DeepSleep {
                    if voltage >= self.thresholds.recovery_v {
                        self.state = OffGridState::RampingUp;
                        self.target_freq_mhz = self.min_freq_mhz;
                        tracing::info!(
                            voltage = format_args!("{:.1}V", voltage),
                            "OFF-GRID: Waking from deep sleep at {} MHz",
                            self.min_freq_mhz
                        );
                        return OffGridAction::Wake(self.min_freq_mhz);
                    }
                    return OffGridAction::Sleep;
                }

                self.state = OffGridState::Normal;
                OffGridAction::Hold
            }

            VoltageZone::High => {
                // If in deep sleep, voltage jumped straight to High — wake up
                if self.state == OffGridState::DeepSleep {
                    tracing::info!(
                        voltage = format_args!("{:.1}V", voltage),
                        "OFF-GRID: Voltage jumped to High zone — waking from deep sleep"
                    );
                    self.state = OffGridState::RampingUp;
                    self.target_freq_mhz = self.min_freq_mhz;
                    return OffGridAction::Wake(self.min_freq_mhz);
                }

                // Ramp up — solar surplus available
                let old_freq = self.target_freq_mhz;
                self.target_freq_mhz =
                    (self.target_freq_mhz + self.freq_step_mhz).min(self.max_freq_mhz);
                self.state = OffGridState::RampingUp;

                if self.target_freq_mhz != old_freq {
                    tracing::info!(
                        voltage = format_args!("{:.1}V", voltage),
                        freq = self.target_freq_mhz,
                        "OFF-GRID: High voltage (surplus) — ramping up to {} MHz",
                        self.target_freq_mhz
                    );
                }
                OffGridAction::SetFrequency(self.target_freq_mhz)
            }

            VoltageZone::Full => {
                // Full voltage is still ramped in steps so recovery does not slam boards to max.
                if self.state == OffGridState::DeepSleep {
                    tracing::info!(
                        voltage = format_args!("{:.1}V", voltage),
                        freq = self.min_freq_mhz,
                        "OFF-GRID: Voltage jumped to Full zone — waking gradually at {} MHz",
                        self.min_freq_mhz
                    );
                    self.state = OffGridState::RampingUp;
                    self.target_freq_mhz = self.min_freq_mhz;
                    return OffGridAction::Wake(self.min_freq_mhz);
                }

                let old_freq = self.target_freq_mhz;
                self.target_freq_mhz =
                    (self.target_freq_mhz + self.freq_step_mhz).min(self.max_freq_mhz);
                self.state = if self.target_freq_mhz >= self.max_freq_mhz {
                    OffGridState::Normal
                } else {
                    OffGridState::RampingUp
                };

                if self.target_freq_mhz != old_freq {
                    tracing::info!(
                        voltage = format_args!("{:.1}V", voltage),
                        freq = self.target_freq_mhz,
                        "OFF-GRID: Battery full — ramping up gradually to {} MHz",
                        self.target_freq_mhz
                    );
                }
                OffGridAction::SetFrequency(self.target_freq_mhz)
            }
        }
    }

    /// Classify voltage into a zone.
    ///
    /// Boundary convention is deliberately asymmetric: the deep-discharge edges
    /// use strict `<` (so a reading *exactly at* `critical_v`/`low_v` is the
    /// HIGHER, safer zone — Low/Normal — never Critical/Low), while the upper
    /// edges use `<=`. This keeps the battery-protection trip strictly below
    /// `critical_v`; the boundary tests pin it so a refactor can't silently
    /// shift the deep-discharge point.
    fn classify_zone(&self, v: f32) -> VoltageZone {
        // Fail CLOSED on a non-finite reading (NaN / ±inf from a failed or glitching
        // battery-voltage ADC): map it to the MOST protective zone (Critical), NOT
        // through the `<`/`<=` ladder — every comparison is false for NaN, so the
        // bare ladder falls to `else => Full` (a fail-OPEN that would suppress
        // battery over-discharge protection on a sensor glitch, risking deep
        // discharge / battery damage on an off-grid unit).
        if !v.is_finite() {
            return VoltageZone::Critical;
        }
        if v < self.thresholds.critical_v {
            VoltageZone::Critical
        } else if v < self.thresholds.low_v {
            VoltageZone::Low
        } else if v <= self.thresholds.high_v {
            VoltageZone::Normal
        } else if v <= self.thresholds.full_v {
            VoltageZone::High
        } else {
            VoltageZone::Full
        }
    }

    /// Estimate battery SoC from voltage (linear interpolation).
    pub fn estimate_soc(&self, voltage_v: f32) -> f32 {
        let empty = self.thresholds.critical_v;
        let full = self.thresholds.full_v;
        if !voltage_v.is_finite() || !empty.is_finite() || !full.is_finite() || full <= empty {
            return 0.0;
        }
        ((voltage_v - empty) / (full - empty) * 100.0).clamp(0.0, 100.0)
    }

    /// Compute voltage rate of change (V/s, positive = charging).
    pub fn voltage_rate(&self) -> f32 {
        if self.voltage_history.len() < 2 {
            return 0.0;
        }
        let Some(first) = self.voltage_history.first() else {
            return 0.0;
        };
        let Some(last) = self.voltage_history.last() else {
            return 0.0;
        };
        let dt = (last.0 - first.0).as_secs_f32();
        if dt < 1.0 {
            return 0.0;
        }
        let rate = (last.1 - first.1) / dt;
        if rate.is_finite() {
            rate
        } else {
            0.0
        }
    }

    /// Build telemetry snapshot for API/WebSocket.
    pub fn telemetry(
        &self,
        reading: &AdcReading,
        sensor_source: &str,
        has_current: bool,
    ) -> OffGridTelemetry {
        OffGridTelemetry {
            enabled: true,
            zone: format!("{:?}", self.classify_zone(self.smoothed_voltage)).to_lowercase(),
            state: format!("{:?}", self.state),
            bus_voltage_v: self.smoothed_voltage,
            current_a: finite_nonnegative_or_zero(reading.current_a),
            power_w: finite_nonnegative_or_zero(reading.power_w),
            battery_soc_pct: self.estimate_soc(self.smoothed_voltage),
            target_freq_mhz: self.target_freq_mhz,
            freq_pct: if self.max_freq_mhz > 0 {
                self.target_freq_mhz as f32 / self.max_freq_mhz as f32 * 100.0
            } else {
                0.0
            },
            voltage_rate_vps: self.voltage_rate(),
            uptime_battery_s: self.start_time.elapsed().as_secs(),
            energy_consumed_wh: self.energy_consumed_wh,
            critical_v: self.thresholds.critical_v,
            low_v: self.thresholds.low_v,
            high_v: self.thresholds.high_v,
            full_v: self.thresholds.full_v,
            sensor_source: sensor_source.to_string(),
            has_current,
            sensor_ok: true,
            message: String::new(),
        }
    }

    /// Enter a fail-safe state when the DC-bus sensor becomes unavailable.
    pub fn enter_sensor_fault(&mut self) {
        self.state = OffGridState::SensorFault;
        self.target_freq_mhz = 0;
    }

    /// Build a fail-safe telemetry snapshot when the voltage source is unavailable.
    pub fn fault_telemetry(
        &self,
        sensor_source: &str,
        has_current: bool,
        message: &str,
    ) -> OffGridTelemetry {
        OffGridTelemetry {
            enabled: true,
            zone: "sensor_fault".to_string(),
            state: "SensorFault".to_string(),
            bus_voltage_v: self.smoothed_voltage,
            current_a: 0.0,
            power_w: 0.0,
            battery_soc_pct: self.estimate_soc(self.smoothed_voltage),
            target_freq_mhz: self.target_freq_mhz,
            freq_pct: if self.max_freq_mhz > 0 {
                self.target_freq_mhz as f32 / self.max_freq_mhz as f32 * 100.0
            } else {
                0.0
            },
            voltage_rate_vps: 0.0,
            uptime_battery_s: self.start_time.elapsed().as_secs(),
            energy_consumed_wh: self.energy_consumed_wh,
            critical_v: self.thresholds.critical_v,
            low_v: self.thresholds.low_v,
            high_v: self.thresholds.high_v,
            full_v: self.thresholds.full_v,
            sensor_source: sensor_source.to_string(),
            has_current,
            sensor_ok: false,
            message: message.to_string(),
        }
    }

    pub fn state(&self) -> OffGridState {
        self.state
    }
    pub fn target_freq(&self) -> u16 {
        self.target_freq_mhz
    }
    pub fn smoothed_voltage(&self) -> f32 {
        self.smoothed_voltage
    }
}

#[cfg(test)]
mod tests {
    use super::{OffGridAction, OffGridController, OffGridState};
    use crate::battery::BatteryPreset;
    use dcentrald_hal::adc::AdcReading;

    #[test]
    fn deep_sleep_full_zone_wakes_at_min_frequency() {
        let thresholds = BatteryPreset::LiFePO4_48V.thresholds();
        let mut controller = OffGridController::new(thresholds.clone(), 700, 200, 25);

        let critical = AdcReading {
            voltage_v: thresholds.critical_v - 1.0,
            current_a: 0.0,
            power_w: 0.0,
        };
        assert!(matches!(controller.tick(&critical), OffGridAction::Sleep));
        assert_eq!(controller.state(), OffGridState::DeepSleep);

        let full = AdcReading {
            voltage_v: thresholds.full_v + 1.0,
            current_a: 0.0,
            power_w: 0.0,
        };
        assert!(matches!(controller.tick(&full), OffGridAction::Wake(200)));
        assert_eq!(controller.target_freq(), 200);
    }

    #[test]
    fn wake_frequency_never_exceeds_inverted_ceiling() {
        // Regression: an eco mining ceiling (50 MHz) below the default off-grid
        // floor (200 MHz) must not let Wake/ramp command a frequency above the
        // operator's ceiling. new() clamps the floor down to the ceiling.
        let thresholds = BatteryPreset::LiFePO4_48V.thresholds();
        let mut controller = OffGridController::new(thresholds.clone(), 50, 200, 25);

        let critical = reading(thresholds.critical_v - 1.0);
        assert!(matches!(controller.tick(&critical), OffGridAction::Sleep));

        let full = reading(thresholds.full_v + 1.0);
        if let OffGridAction::Wake(freq) = controller.tick(&full) {
            assert!(freq <= 50, "wake freq {} must not exceed ceiling 50", freq);
        } else {
            panic!("expected Wake action after full-zone recovery from deep sleep");
        }
        assert!(controller.target_freq() <= 50);
    }

    #[test]
    fn full_zone_ramps_up_in_steps_after_wake() {
        let thresholds = BatteryPreset::LiFePO4_48V.thresholds();
        let mut controller = OffGridController::new(thresholds.clone(), 700, 200, 25);

        controller.enter_sensor_fault();
        let recovered = AdcReading {
            voltage_v: thresholds.full_v + 1.0,
            current_a: 0.0,
            power_w: 0.0,
        };

        assert!(matches!(
            controller.tick(&recovered),
            OffGridAction::Wake(200)
        ));
        assert!(matches!(
            controller.tick(&recovered),
            OffGridAction::SetFrequency(225)
        ));
        assert_eq!(controller.target_freq(), 225);
    }

    // -- Shared helpers for the deep-discharge / fail-safe / boundary tests --

    /// A 700/200/25 LiFePO4-48V controller (critical 40.0, low 47.0, high 53.6,
    /// full 54.4, recovery 49.0). Fresh: state=Normal, target=700, first_tick.
    fn lifepo4_controller() -> OffGridController {
        OffGridController::new(BatteryPreset::LiFePO4_48V.thresholds(), 700, 200, 25)
    }

    /// A voltage-only ADC reading (no current/power).
    fn reading(voltage_v: f32) -> AdcReading {
        AdcReading {
            voltage_v,
            current_a: 0.0,
            power_w: 0.0,
        }
    }

    // (a) Deep-sleep hysteresis: once asleep, a Low-zone voltage that is still
    //     below recovery_v must NOT prematurely wake the boards; only a reading
    //     at/above recovery_v wakes (at min frequency).
    #[test]
    fn deep_sleep_stays_asleep_below_recovery_then_wakes_at_recovery() {
        let mut controller = lifepo4_controller();

        // Enter deep sleep via a critical reading (< critical_v = 40.0).
        // first_tick → smoothed = 39.0 exactly.
        assert!(matches!(
            controller.tick(&reading(39.0)),
            OffGridAction::Sleep
        ));
        assert_eq!(controller.state(), OffGridState::DeepSleep);
        assert_eq!(controller.target_freq(), 0);

        // A Low-zone reading (EMA-smoothed ~41.1 V → Low [40,47), still < recovery
        // 49.0). Must stay asleep — no premature wake while the battery is still
        // depleted.
        let action = controller.tick(&reading(46.0));
        assert!(
            matches!(action, OffGridAction::Sleep),
            "Low-zone voltage below recovery_v must stay asleep, got {action:?}"
        );
        assert_eq!(
            controller.state(),
            OffGridState::DeepSleep,
            "must not leave deep sleep below recovery_v"
        );
        assert_eq!(
            controller.target_freq(),
            0,
            "boards stay OFF below recovery_v"
        );

        // Voltage recovers to >= recovery_v (49.0). The deep-sleep EMA reset
        // snaps smoothed to 50.0 → wake at the minimum frequency.
        let action = controller.tick(&reading(50.0));
        assert!(
            matches!(action, OffGridAction::Wake(200)),
            "reaching recovery_v must Wake at min_freq, got {action:?}"
        );
        assert_eq!(controller.state(), OffGridState::RampingUp);
        assert_eq!(controller.target_freq(), 200);
    }

    // (b) Sensor-fault fail-safe: in SensorFault, a voltage below recovery_v must
    //     keep boards OFF (Sleep, target_freq == 0) — distinct from the existing
    //     recovered-high-voltage test that wakes.
    #[test]
    fn sensor_fault_below_recovery_fails_safe_boards_off() {
        let mut controller = lifepo4_controller();
        controller.enter_sensor_fault();
        assert_eq!(controller.state(), OffGridState::SensorFault);

        // first_tick → smoothed = 45.0, which is < recovery_v (49.0).
        let action = controller.tick(&reading(45.0));
        assert!(
            matches!(action, OffGridAction::Sleep),
            "sensor fault + below-recovery voltage must fail safe to Sleep, got {action:?}"
        );
        assert_eq!(
            controller.target_freq(),
            0,
            "boards must stay OFF on the sensor-fault fail-safe path"
        );
        assert_eq!(controller.state(), OffGridState::DeepSleep);
    }

    // (c) classify_zone boundary asymmetry (`<` for critical/low vs `<=` for
    //     normal/high). Pin the deep-discharge cutoff so a refactor can't shift
    //     it: strictly-below critical_v is Critical, but EXACTLY critical_v is
    //     Low, and EXACTLY low_v is Normal. Asserted via the resulting
    //     action/state (no private classify_zone call).
    #[test]
    fn classify_zone_boundaries_pin_deep_discharge_cutoff() {
        // Strictly below critical_v (40.0) → Critical → Sleep / DeepSleep.
        let mut below_critical = lifepo4_controller();
        let action = below_critical.tick(&reading(39.9));
        assert!(
            matches!(action, OffGridAction::Sleep),
            "v < critical_v must be Critical (Sleep), got {action:?}"
        );
        assert_eq!(below_critical.state(), OffGridState::DeepSleep);

        // EXACTLY critical_v (40.0) → Low (NOT Critical), because the critical
        // edge is strict `<`. Low (from Normal) ramps down → SetFrequency /
        // RampingDown, never Sleep.
        let mut at_critical = lifepo4_controller();
        let action = at_critical.tick(&reading(40.0));
        assert!(
            matches!(action, OffGridAction::SetFrequency(675)),
            "v == critical_v must be Low (ramp down), not Critical, got {action:?}"
        );
        assert_eq!(at_critical.state(), OffGridState::RampingDown);

        // EXACTLY low_v (47.0) → Normal (NOT Low), because the low edge is
        // strict `<` too. Normal holds → Hold / Normal.
        let mut at_low = lifepo4_controller();
        let action = at_low.tick(&reading(47.0));
        assert!(
            matches!(action, OffGridAction::Hold),
            "v == low_v must be Normal (Hold), not Low, got {action:?}"
        );
        assert_eq!(at_low.state(), OffGridState::Normal);
    }

    #[test]
    fn classify_zone_itself_fails_closed_on_non_finite_voltage() {
        // Defense-in-depth: the primary guard is upstream in tick() (a non-finite
        // reading enters sensor-fault before reaching the EMA/classify_zone, pinned
        // by the "(z)" test below). This pins that classify_zone ITSELF maps any
        // non-finite voltage to the most protective zone (Critical), so the
        // fall-through-to-Full fail-open can't be reintroduced by a caller that
        // forgets to pre-filter — e.g. the status-string path (`{:?}` of
        // classify_zone(smoothed_voltage)).
        let c = lifepo4_controller();
        assert!(matches!(
            c.classify_zone(f32::NAN),
            super::VoltageZone::Critical
        ));
        assert!(matches!(
            c.classify_zone(f32::INFINITY),
            super::VoltageZone::Critical
        ));
        assert!(matches!(
            c.classify_zone(f32::NEG_INFINITY),
            super::VoltageZone::Critical
        ));
        // Finite ladder still classifies correctly (regression).
        assert!(matches!(
            c.classify_zone(39.9),
            super::VoltageZone::Critical
        ));
        assert!(matches!(c.classify_zone(45.0), super::VoltageZone::Low));
    }

    // (z) A non-finite bus voltage must fail CLOSED, not ramp frequency up, and
    //     must not poison the EMA (regression: classify_zone(NaN) fell through
    //     to Full → SetFrequency(up); 0.3*NaN+0.7*prev stuck the EMA at NaN).
    #[test]
    fn nan_bus_voltage_fails_closed_and_does_not_poison_ema() {
        let th = BatteryPreset::LiFePO4_48V.thresholds();
        let full_v = th.full_v + 1.0;
        let mut c = lifepo4_controller();
        // Warm the EMA with a good reading (past first_tick).
        let _ = c.tick(&reading(full_v));

        // NaN → fail-safe: SensorFault state + Sleep, NEVER a ramp-up.
        let action = c.tick(&reading(f32::NAN));
        assert!(
            matches!(action, OffGridAction::Sleep),
            "NaN bus voltage must fail closed to Sleep, never ramp up, got {action:?}"
        );
        assert_eq!(
            c.state(),
            OffGridState::SensorFault,
            "NaN reading must enter the SensorFault fail-safe state"
        );

        // +Inf must also fail closed.
        assert!(
            matches!(c.tick(&reading(f32::INFINITY)), OffGridAction::Sleep),
            "+Inf bus voltage must also fail closed to Sleep"
        );

        // Recovery: a clean full-zone reading must recover (Wake), proving the
        // garbage readings did NOT leave the smoothed voltage stuck at NaN.
        let action = c.tick(&reading(full_v));
        assert!(
            matches!(action, OffGridAction::Wake(_)),
            "controller must recover after garbage readings (EMA not poisoned), got {action:?}"
        );
    }

    #[test]
    fn non_finite_power_and_current_do_not_poison_energy_or_telemetry() {
        let thresholds = BatteryPreset::LiFePO4_48V.thresholds();
        let mut controller = OffGridController::new(thresholds.clone(), 700, 200, 25);

        let reading = AdcReading {
            voltage_v: thresholds.normal_v,
            current_a: f32::NAN,
            power_w: f32::INFINITY,
        };
        assert!(matches!(controller.tick(&reading), OffGridAction::Hold));
        assert!(
            controller.energy_consumed_wh.is_finite(),
            "non-finite power must not poison accumulated Wh"
        );

        let telemetry = controller.telemetry(&reading, "test", true);
        assert_eq!(telemetry.current_a, 0.0);
        assert_eq!(telemetry.power_w, 0.0);
        assert!(telemetry.battery_soc_pct.is_finite());
    }

    #[test]
    fn soc_estimate_fails_closed_on_non_finite_voltage() {
        let controller = lifepo4_controller();
        assert_eq!(controller.estimate_soc(f32::NAN), 0.0);
        assert_eq!(controller.estimate_soc(f32::INFINITY), 0.0);
    }
}
