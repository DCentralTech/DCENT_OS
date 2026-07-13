use std::sync::{Arc, Mutex};

use crate::platform::FanAccess;

#[derive(Clone)]
pub struct SimFan {
    fan_count: u8,
    pwm: Arc<Mutex<u8>>,
}

impl SimFan {
    pub fn new(fan_count: u8) -> Self {
        Self {
            fan_count,
            pwm: Arc::new(Mutex::new(0)),
        }
    }
}

impl FanAccess for SimFan {
    fn set_speed(&self, pwm: u8) {
        if let Ok(mut current) = self.pwm.lock() {
            // Match DCENT_OS's load-bearing home-mining safety ceiling.
            *current = pwm.min(30);
        }
    }

    fn get_rpm(&self) -> u32 {
        u32::from(self.get_speed_pwm()) * 120
    }

    fn get_speed_pwm(&self) -> u8 {
        self.pwm.lock().map(|value| *value).unwrap_or_default()
    }

    fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        (0..self.fan_count).map(|id| (id, self.get_rpm())).collect()
    }

    fn fan_count(&self) -> u8 {
        self.fan_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_fan_preserves_home_mining_pwm_ceiling() {
        let fan = SimFan::new(4);
        fan.set_speed(127);
        assert_eq!(fan.get_speed_pwm(), 30);
    }
}
