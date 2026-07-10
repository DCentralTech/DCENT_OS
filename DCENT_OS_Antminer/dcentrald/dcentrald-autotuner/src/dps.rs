//! Dynamic power scaling target walker.
//!
//! The runtime loop should not jump directly between distant power or hashrate
//! targets. This module computes bounded one-step moves that can be reused by
//! REST controls, schedules, and background DPS policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpsDirection {
    Increase,
    Decrease,
    Hold,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpsStep<T> {
    pub current: T,
    pub desired: T,
    pub next: T,
    pub direction: DpsDirection,
    pub high_performance_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DpsWalkerConfig {
    pub power_step_w: u32,
    pub hashrate_step_ths: f64,
    pub min_power_w: u32,
    pub max_power_w: u32,
    pub min_hashrate_ths: f64,
    pub max_hashrate_ths: f64,
    pub high_performance_mode: bool,
}

impl Default for DpsWalkerConfig {
    fn default() -> Self {
        Self {
            power_step_w: 300,
            hashrate_step_ths: 11.0,
            min_power_w: 200,
            max_power_w: crate::config::ABSOLUTE_MAX_WATTS,
            min_hashrate_ths: 1.0,
            max_hashrate_ths: 300.0,
            high_performance_mode: false,
        }
    }
}

impl DpsWalkerConfig {
    pub fn effective_power_step_w(&self) -> u32 {
        let step = self.power_step_w.max(1);
        if self.high_performance_mode {
            step.saturating_mul(2)
        } else {
            step
        }
    }

    pub fn effective_hashrate_step_ths(&self) -> f64 {
        let step = if self.hashrate_step_ths.is_finite() {
            self.hashrate_step_ths.max(0.1)
        } else {
            11.0
        };
        if self.high_performance_mode {
            step * 2.0
        } else {
            step
        }
    }

    pub fn walk_power_target(&self, current_w: u32, desired_w: u32) -> DpsStep<u32> {
        let min_w = self.min_power_w.min(self.max_power_w);
        let max_w = self.max_power_w.max(min_w);
        let current = current_w.clamp(min_w, max_w);
        let desired = desired_w.clamp(min_w, max_w);
        let step = self.effective_power_step_w();

        let (next, direction) = if desired > current {
            (
                current.saturating_add(step).min(desired).min(max_w),
                DpsDirection::Increase,
            )
        } else if desired < current {
            (
                current.saturating_sub(step).max(desired).max(min_w),
                DpsDirection::Decrease,
            )
        } else {
            (current, DpsDirection::Hold)
        };

        DpsStep {
            current,
            desired,
            next,
            direction,
            high_performance_mode: self.high_performance_mode,
        }
    }

    pub fn walk_hashrate_target(&self, current_ths: f64, desired_ths: f64) -> DpsStep<f64> {
        let min_ths = self.min_hashrate_ths.min(self.max_hashrate_ths);
        let max_ths = self.max_hashrate_ths.max(min_ths);
        let current = clamp_finite(current_ths, min_ths, max_ths);
        let desired = clamp_finite(desired_ths, min_ths, max_ths);
        let step = self.effective_hashrate_step_ths();

        let (next, direction) = if desired > current {
            (
                (current + step).min(desired).min(max_ths),
                DpsDirection::Increase,
            )
        } else if desired < current {
            (
                (current - step).max(desired).max(min_ths),
                DpsDirection::Decrease,
            )
        } else {
            (current, DpsDirection::Hold)
        };

        DpsStep {
            current,
            desired,
            next,
            direction,
            high_performance_mode: self.high_performance_mode,
        }
    }
}

pub fn watts_for_btu_h(btu_h: u32) -> u32 {
    ((btu_h as f64 / 3.412).round() as u32).max(1)
}

pub fn btu_h_for_watts(watts: u32) -> u32 {
    (watts as f64 * 3.412).round() as u32
}

fn clamp_finite(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_target_walks_by_one_step() {
        let config = DpsWalkerConfig {
            power_step_w: 300,
            min_power_w: 200,
            max_power_w: 1800,
            ..Default::default()
        };

        let step = config.walk_power_target(900, 1500);
        assert_eq!(step.direction, DpsDirection::Increase);
        assert_eq!(step.next, 1200);

        let step = config.walk_power_target(900, 400);
        assert_eq!(step.direction, DpsDirection::Decrease);
        assert_eq!(step.next, 600);
    }

    #[test]
    fn high_performance_mode_uses_larger_steps() {
        let config = DpsWalkerConfig {
            power_step_w: 300,
            high_performance_mode: true,
            ..Default::default()
        };

        let step = config.walk_power_target(600, 1500);
        assert_eq!(step.next, 1200);
        assert!(step.high_performance_mode);
    }

    #[test]
    fn hashrate_target_walks_and_clamps() {
        let config = DpsWalkerConfig {
            hashrate_step_ths: 11.0,
            min_hashrate_ths: 10.0,
            max_hashrate_ths: 140.0,
            ..Default::default()
        };

        let step = config.walk_hashrate_target(100.0, 132.0);
        assert_eq!(step.direction, DpsDirection::Increase);
        assert!((step.next - 111.0).abs() < 0.001);

        let clamped = config.walk_hashrate_target(100.0, 200.0);
        assert_eq!(clamped.desired, 140.0);
    }

    #[test]
    fn heater_btu_conversion_is_reversible_enough() {
        let watts = watts_for_btu_h(5_118);
        assert_eq!(watts, 1500);
        assert!((btu_h_for_watts(watts) as i32 - 5_118).abs() <= 2);
    }
}
