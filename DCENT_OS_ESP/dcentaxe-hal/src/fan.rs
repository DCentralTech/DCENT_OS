//! Fan PWM control and tachometer reading (LEDC/GPIO path — **reference, not
//! the shipping fan path**).
//!
//! BitAxe boards use a standard 4-pin PC fan header with:
//! - PWM control via ESP32 LEDC peripheral at 25 kHz (Intel spec)
//! - Tachometer input (2 pulses per revolution, open-drain with pull-up)
//!
//! The fan PWM duty cycle controls fan speed from 0-100%. A safety floor
//! prevents accidentally turning the fan fully off while the ASIC is mining.
//!
//! # HALT-2: this [`FanController`] is NOT the shipping fan-control authority
//!
//! Every supported BitAxe board drives its fan through the EMC2101 / EMC2103 /
//! EMC2302 **I2C** controller, not this LEDC/GPIO path. The shipping daemon in
//! `dcentaxe/src/main.rs` uses a local `FanState` shim + inline EMC register
//! writes; [`FanController::new`] is never instantiated on any board variant,
//! and [`count_tach_pulses`](FanController::count_tach_pulses) returns 0 (the
//! GPIO PCNT tach is unused — boards read RPM from the EMC TACH registers over
//! I2C). This type is kept as a reference implementation for a hypothetical
//! future board with a direct GPIO/LEDC fan and no fan-IC.
//!
//! Consequence for the `lib.rs` "fan has a safety floor when mining is active
//! (cannot go below 20%)" guarantee: that floor is NOT enforced by this named
//! type on shipping hardware. The real floor lives in the shipping path
//! (`FanState::set_speed` clamps `pct.max(20)` **unconditionally**, even
//! stronger than this type's mining-gated [`FAN_SAFETY_FLOOR_PERCENT`]).
//!
//! Minimum-duty reconciliation (HALT-2): two different floors coexist by design
//! and must not be conflated —
//! - **20%** ([`FAN_SAFETY_FLOOR_PERCENT`] here, and `FanState`'s `.max(20)`):
//!   the hard *safety* floor that may never be undercut while mining.
//! - **30%** (the autotuner/PID `min_pct` in `main.rs`): the *acoustic/comfort*
//!   lower bound the closed-loop controller is allowed to command. It sits
//!   above the 20% safety floor, so the two never conflict — the PID simply
//!   never asks for the bottom 10% of the legal range.
//!
//! Do NOT "wire up" this `FanController` as the single authority without first
//! reconciling these floors at the call sites; a naive swap would replace the
//! unconditional 20% clamp with a mining-gated one and change behavior.

use esp_idf_hal::gpio::OutputPin;
use esp_idf_hal::ledc::{self, LedcDriver, LedcTimerDriver};
use log::*;

/// Standard PC fan PWM frequency: 25 kHz
const FAN_PWM_FREQ_HZ: u32 = 25_000;

/// Minimum fan speed percentage during mining (safety floor).
/// If the ASIC is active, never allow the fan below this speed.
const FAN_SAFETY_FLOOR_PERCENT: u8 = 20;

/// Number of tachometer pulses per fan revolution (standard = 2).
const TACH_PULSES_PER_REV: u32 = 2;

/// Errors from fan operations
#[derive(Debug)]
pub enum FanError {
    /// Failed to initialize PWM output
    PwmInitFailed(String),
    /// Failed to set PWM duty cycle
    DutySetFailed(String),
    /// Failed to read tachometer
    TachReadFailed(String),
}

impl core::fmt::Display for FanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PwmInitFailed(msg) => write!(f, "Fan PWM init failed: {}", msg),
            Self::DutySetFailed(msg) => write!(f, "Fan duty set failed: {}", msg),
            Self::TachReadFailed(msg) => write!(f, "Fan tach read failed: {}", msg),
        }
    }
}

impl std::error::Error for FanError {}

/// Fan controller using ESP32 LEDC PWM and pulse counting for tachometer.
///
/// The LEDC peripheral generates a 25 kHz PWM signal with 8-bit resolution
/// (256 steps from 0-100% duty). The tachometer is read by counting pulses
/// over a fixed time window and converting to RPM.
pub struct FanController<'d> {
    /// LEDC PWM driver for fan speed control
    pwm: LedcDriver<'d>,
    /// Current speed setting (0-100%)
    current_speed_percent: u8,
    /// Whether mining is active (enables safety floor)
    mining_active: bool,
    /// Tachometer pin number for pulse counting
    _tach_pin: i32,
    /// Last measured RPM
    last_rpm: u32,
}

impl<'d> FanController<'d> {
    /// Initialize the fan controller.
    ///
    /// Sets up a 25 kHz PWM output on the specified pin and configures
    /// the tachometer input for pulse counting.
    ///
    /// # Arguments
    /// * `timer` - LEDC timer peripheral
    /// * `channel` - LEDC channel peripheral
    /// * `pwm_pin` - GPIO pin for PWM output (GPIO11 on most BitAxe boards)
    /// * `tach_pin` - GPIO pin number for tachometer input (GPIO14 typically)
    pub fn new<C: ledc::LedcChannel<SpeedMode = T::SpeedMode> + 'd, T: ledc::LedcTimer + 'd>(
        timer: T,
        channel: C,
        pwm_pin: impl OutputPin + 'd,
        tach_pin: i32,
    ) -> Result<Self, FanError> {
        // Configure LEDC timer at 25 kHz with 8-bit resolution
        let timer_config = ledc::config::TimerConfig::new()
            .frequency(esp_idf_hal::units::Hertz(FAN_PWM_FREQ_HZ))
            .resolution(ledc::Resolution::Bits8);

        let timer_driver = LedcTimerDriver::new(timer, &timer_config)
            .map_err(|e| FanError::PwmInitFailed(format!("timer: {:?}", e)))?;

        let pwm = LedcDriver::new(channel, timer_driver, pwm_pin)
            .map_err(|e| FanError::PwmInitFailed(format!("channel: {:?}", e)))?;

        info!(
            "Fan controller initialized: PWM @ {} Hz, tach pin {}",
            FAN_PWM_FREQ_HZ, tach_pin
        );

        let mut controller = Self {
            pwm,
            current_speed_percent: 0,
            mining_active: false,
            _tach_pin: tach_pin,
            last_rpm: 0,
        };

        // Start at 100% to spin up the fan, then caller can reduce
        controller.set_speed(100)?;

        Ok(controller)
    }

    /// Set fan speed as a percentage (0-100%).
    ///
    /// If mining is active and the requested speed is below the safety floor,
    /// the speed is clamped to [`FAN_SAFETY_FLOOR_PERCENT`] and a warning
    /// is logged. Setting speed to 0% while mining is dangerous and will
    /// always be overridden.
    ///
    /// # Arguments
    /// * `percent` - Fan speed from 0 (off) to 100 (full speed)
    pub fn set_speed(&mut self, percent: u8) -> Result<(), FanError> {
        let percent = percent.min(100);

        let actual = if self.mining_active && percent < FAN_SAFETY_FLOOR_PERCENT {
            warn!(
                "Fan speed {}% below safety floor {}% while mining — clamping",
                percent, FAN_SAFETY_FLOOR_PERCENT
            );
            FAN_SAFETY_FLOOR_PERCENT
        } else {
            if percent == 0 {
                warn!("Fan set to 0% — ensure ASIC is not mining!");
            }
            percent
        };

        // Convert percentage to 8-bit duty cycle (0-255)
        let duty = (actual as u32 * 255) / 100;

        self.pwm
            .set_duty(duty)
            .map_err(|e| FanError::DutySetFailed(format!("{:?}", e)))?;

        self.current_speed_percent = actual;

        Ok(())
    }

    /// Read the fan RPM from the tachometer.
    ///
    /// Counts tachometer pulses over a 500 ms window and converts to RPM.
    /// Standard PC fans produce 2 pulses per revolution.
    ///
    /// RPM = (pulse_count / TACH_PULSES_PER_REV) * (60 / measurement_seconds)
    ///
    /// Note: This is a blocking call that takes ~500 ms to complete.
    /// For non-blocking RPM reading, use a background task with a pulse counter.
    pub fn get_rpm(&mut self) -> Result<u32, FanError> {
        // Use ESP-IDF pulse counter or manual GPIO polling
        // For now, we use a simple edge-counting approach over a time window
        let measurement_ms: u32 = 500;

        // Read pulse count using ESP-IDF PCNT (Pulse Counter) peripheral
        // This is a simplified implementation — production code should use
        // the hardware PCNT unit for accurate counting without CPU overhead.
        //
        // For this implementation, we use a software polling approach.
        let pulse_count = self.count_tach_pulses(measurement_ms);

        // RPM = (pulses / pulses_per_rev) * (60000 ms/min / measurement_ms)
        let rpm = (pulse_count * 60_000) / (TACH_PULSES_PER_REV * measurement_ms);

        self.last_rpm = rpm;
        Ok(rpm)
    }

    /// Get the last measured RPM without triggering a new measurement.
    pub fn last_rpm(&self) -> u32 {
        self.last_rpm
    }

    /// Get the current fan speed setting (0-100%).
    pub fn current_speed(&self) -> u8 {
        self.current_speed_percent
    }

    /// Set whether mining is currently active.
    ///
    /// When mining is active, the safety floor prevents the fan from being
    /// set below [`FAN_SAFETY_FLOOR_PERCENT`]. Call this whenever the mining
    /// state changes.
    pub fn set_mining_active(&mut self, active: bool) {
        self.mining_active = active;
        if active && self.current_speed_percent < FAN_SAFETY_FLOOR_PERCENT {
            warn!(
                "Mining activated with fan at {}% — raising to safety floor",
                self.current_speed_percent
            );
            let _ = self.set_speed(FAN_SAFETY_FLOOR_PERCENT);
        }
    }

    /// Count tachometer pulses over the given time window.
    ///
    /// Note: All current BitAxe boards use EMC2101/EMC2302/EMC2103 with built-in
    /// hardware tachometer registers (read via I2C in main.rs). This GPIO-based
    /// PCNT path is reserved for future boards with direct GPIO tach connections.
    ///
    /// The ESP32-S3 PCNT peripheral could be used here via esp_idf_hal::pcnt,
    /// but it conflicts with the legacy PCNT driver if both are linked.
    /// For now, return 0 and rely on the I2C fan controller's built-in tach.
    fn count_tach_pulses(&self, _duration_ms: u32) -> u32 {
        0
    }
}
