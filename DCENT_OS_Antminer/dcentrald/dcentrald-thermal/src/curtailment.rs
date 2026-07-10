//! Curtailment (sleep/wake) for demand response.
//!
//! Sleep mode drops power consumption to approximately 25W:
//!   1. Disable hash board voltages via PIC (ENABLE_VOLTAGE = 0)
//!   2. Set fan PWM to 20% (maintain airflow for PSU cooling)
//!   3. Continue watchdog kicks and API serving
//!   4. Continue Stratum connection (hash-on-disconnect mode if configured)
//!
//! Wake from sleep:
//!   1. Re-run PIC initialization (voltage enable, heartbeat start)
//!   2. Wait for voltage stabilization (500ms)
//!   3. Re-enumerate chips
//!   4. Ramp frequency gradually over 60 seconds
//!   5. Ramp fans based on temperature

use serde::{Deserialize, Serialize};

/// Curtailment state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurtailmentState {
    /// Normal operation (mining).
    Active,
    /// Transitioning to sleep (shutting down boards).
    EnteringSleep,
    /// In sleep mode (~25W).
    Sleeping,
    /// Transitioning to active (ramping up boards).
    Waking,
}

/// Curtailment controller manages sleep/wake transitions.
pub struct CurtailmentController {
    /// Current curtailment state.
    state: CurtailmentState,
    /// Fan PWM during sleep (default: ~20%, for PSU cooling).
    sleep_fan_pwm: u8,
    /// Frequency ramp duration on wake (seconds).
    #[allow(dead_code)]
    wake_ramp_duration_s: u32,
}

impl CurtailmentController {
    /// Create a new curtailment controller.
    pub fn new() -> Self {
        Self {
            state: CurtailmentState::Active,
            sleep_fan_pwm: 25,
            wake_ramp_duration_s: 60,
        }
    }

    /// Initiate sleep mode.
    ///
    /// Returns true if the transition was started, false if already sleeping.
    pub fn enter_sleep(&mut self) -> bool {
        match self.state {
            CurtailmentState::Active => {
                self.state = CurtailmentState::EnteringSleep;
                true
            }
            _ => false,
        }
    }

    /// Initiate wake from sleep.
    ///
    /// Returns true if the transition was started, false if already active.
    pub fn wake(&mut self) -> bool {
        match self.state {
            CurtailmentState::Sleeping => {
                self.state = CurtailmentState::Waking;
                true
            }
            _ => false,
        }
    }

    /// Mark the sleep transition as complete.
    pub fn sleep_complete(&mut self) {
        self.state = CurtailmentState::Sleeping;
    }

    /// Mark the wake transition as complete.
    pub fn wake_complete(&mut self) {
        self.state = CurtailmentState::Active;
    }

    /// Get the current curtailment state.
    pub fn state(&self) -> CurtailmentState {
        self.state
    }

    /// Get the fan PWM for sleep mode.
    pub fn sleep_fan_pwm(&self) -> u8 {
        self.sleep_fan_pwm
    }

    /// Check if the system is in a sleep-related state.
    pub fn is_sleeping(&self) -> bool {
        matches!(
            self.state,
            CurtailmentState::Sleeping | CurtailmentState::EnteringSleep
        )
    }
}

impl Default for CurtailmentController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_api_types::thermal_model::FanMode;

    /// THERM-2 (safety pin): the curtailment sleep fan PWM must stay within the
    /// load-bearing PWM-30 home cap. `sleep_fan_pwm` (default 25) is commanded
    /// while the boards are de-energized in sleep mode (~25W); if it ever drifted
    /// above the home cap it would blast fans on a quiet home unit — a direct
    ///  /  regression.
    ///
    /// The cap is sourced HAL-free from `FanMode::Home.max_pwm()` (== 30, the
    /// canonical home cap mirrored by `dcentrald_hal::fan::PWM_SAFETY_MAX`) so
    /// this test compiles and runs on any host (the `hal` feature is Unix-only).
    #[test]
    fn sleep_fan_pwm_never_exceeds_home_pwm30_cap() {
        const HOME_CAP_PWM: u8 = 30;
        // Pin the cap source so a silent change to FanMode::Home is caught here.
        assert_eq!(
            FanMode::Home.max_pwm(),
            HOME_CAP_PWM,
            "FanMode::Home cap must be the load-bearing PWM-30 home cap"
        );

        let controller = CurtailmentController::new();
        assert!(
            controller.sleep_fan_pwm() <= HOME_CAP_PWM,
            "curtailment sleep_fan_pwm ({}) exceeds the PWM-30 home cap ({}) — \
              /  regression",
            controller.sleep_fan_pwm(),
            HOME_CAP_PWM,
        );
        // Default() must carry the same safe value (it delegates to new()).
        assert!(CurtailmentController::default().sleep_fan_pwm() <= HOME_CAP_PWM);
    }

    /// The sleep PWM the controller reports must never change as the curtailment
    /// FSM walks the full sleep/wake cycle — the cap holds in every state, so no
    /// transition can leak a higher fan command on the sleep path.
    #[test]
    fn sleep_fan_pwm_stays_capped_across_every_curtailment_state() {
        const HOME_CAP_PWM: u8 = 30;
        let mut controller = CurtailmentController::new();

        let assert_capped = |c: &CurtailmentController| {
            assert!(
                c.sleep_fan_pwm() <= HOME_CAP_PWM,
                "sleep_fan_pwm ({}) leaked above the PWM-30 cap in state {:?}",
                c.sleep_fan_pwm(),
                c.state(),
            );
        };

        assert_eq!(controller.state(), CurtailmentState::Active);
        assert_capped(&controller);

        assert!(controller.enter_sleep());
        assert_eq!(controller.state(), CurtailmentState::EnteringSleep);
        assert_capped(&controller);

        controller.sleep_complete();
        assert_eq!(controller.state(), CurtailmentState::Sleeping);
        assert_capped(&controller);

        assert!(controller.wake());
        assert_eq!(controller.state(), CurtailmentState::Waking);
        assert_capped(&controller);

        controller.wake_complete();
        assert_eq!(controller.state(), CurtailmentState::Active);
        assert_capped(&controller);
    }

    #[test]
    fn sleep_controller_has_no_float_sensor_surface() {
        let source = include_str!("curtailment.rs");
        let token32 = ["f", "32"].concat();
        let token64 = ["f", "64"].concat();
        assert!(
            !source.contains(&token32) && !source.contains(&token64),
            "curtailment is an integer PWM/state controller; introducing a float input requires a non-finite safety test"
        );
    }
}
