//!  thm-C — cold-environment auto-target adjuster (HAL-free).
//!
//! Source RE evidence:
//!
//! §2.3 (lines 158-173).
//!
//! LuxOS observed a discrepancy: config default `target_temp = 55°C` but
//! live `tempctrl` API returned `Target = 45°C`. The runtime log shows:
//! ```text
//!   Environment temperature is 26C; Adjusting profiles for cold environment
//!   Current profile voltage updated voltage=13.8
//! ```
//!
//! At low ambient temperatures, the silicon's thermal sweet spot moves
//! lower (less self-heating overhead means the chip can mine at lower
//! voltage for the same hashrate). LuxOS pre-emptively lowers the
//! effective target by a small amount to keep autotuner convergence in
//! the better band.
//!
//! This module is a **pure function** — it takes the user's configured
//! target plus the current ambient (best-effort, may be `None`) and
//! returns the effective target. No state, no side effects.
//!
//! Implication for DCENT_OS (per H-thermal-fan.md §2.3 closing note):
//! split `user_target_temp_c` (config bound) from `effective_target_temp_c`
//! (runtime, possibly lower). This module computes the latter.

use serde::{Deserialize, Serialize};

/// Configuration for the cold-environment auto-target adjustment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ColdEnvironmentConfig {
    /// Ambient threshold in °C below which we apply the cold-environment
    /// downshift. Default 27 °C (just above the 26 °C LuxOS log line).
    pub cold_threshold_c: f32,
    /// Maximum amount we'll subtract from the user's target, in °C.
    /// LuxOS observed 10 °C (55 → 45) on a 26 °C ambient.
    pub max_downshift_c: f32,
    /// Per-degree-of-coldness downshift in °C/°C. With `cold_threshold=27`,
    /// `max_downshift=10`, and `slope=1.0`, an ambient of 17 °C downshifts
    /// the target by the full 10 °C; 22 °C downshifts by 5 °C; 27 °C and
    /// above downshift by 0 °C.
    pub slope_c_per_c: f32,
    /// Floor for the effective target — never go below this regardless
    /// of ambient. Prevents over-aggressive autotuner band-shifts in
    /// freezer-cold environments.
    pub min_effective_target_c: f32,
}

impl Default for ColdEnvironmentConfig {
    fn default() -> Self {
        Self {
            cold_threshold_c: 27.0,
            max_downshift_c: 10.0,
            slope_c_per_c: 1.0,
            // 35 °C is well below any reasonable mining target; the
            // dashboard wizard prevents user_target below 40.
            min_effective_target_c: 35.0,
        }
    }
}

/// Compute the effective target temp given the user's configured target
/// and the current ambient.
///
/// Behavior:
/// - `ambient = None` → returns `user_target_c` unchanged (fail-safe).
/// - `ambient >= cold_threshold` → returns `user_target_c` unchanged.
/// - `ambient < cold_threshold` → subtracts `slope * (cold_threshold -
///    ambient)` from `user_target_c`, clamped to
///   `[min_effective_target_c, user_target_c]` and capped at
///   `user_target_c - max_downshift_c`.
pub fn effective_target_temp_c(
    user_target_c: f32,
    ambient_c: Option<f32>,
    config: ColdEnvironmentConfig,
) -> f32 {
    let ambient = match ambient_c {
        Some(a) => a,
        None => return user_target_c,
    };
    if ambient >= config.cold_threshold_c {
        return user_target_c;
    }
    let raw_downshift = (config.cold_threshold_c - ambient) * config.slope_c_per_c;
    let bounded_downshift = raw_downshift.min(config.max_downshift_c);
    let effective = user_target_c - bounded_downshift;
    effective
        .max(config.min_effective_target_c)
        .min(user_target_c)
}

/// Convenience overload using default config.
pub fn effective_target_temp_c_default(user_target_c: f32, ambient_c: Option<f32>) -> f32 {
    effective_target_temp_c(user_target_c, ambient_c, ColdEnvironmentConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ambient_returns_user_target_unchanged() {
        let r = effective_target_temp_c_default(55.0, None);
        assert_eq!(r, 55.0);
    }

    #[test]
    fn warm_ambient_returns_user_target_unchanged() {
        let r = effective_target_temp_c_default(55.0, Some(35.0));
        assert_eq!(r, 55.0);
        // Exactly at threshold (27 °C) — also unchanged.
        let r = effective_target_temp_c_default(55.0, Some(27.0));
        assert_eq!(r, 55.0);
    }

    #[test]
    fn luxos_observed_case_26c_ambient_55c_target_yields_54c() {
        // LuxOS log: "Environment temperature is 26C; Adjusting profiles".
        // Default slope=1.0, cold_threshold=27, so 27-26=1 °C downshift.
        let r = effective_target_temp_c_default(55.0, Some(26.0));
        assert!((r - 54.0).abs() < 0.01, "expected 54.0, got {}", r);
    }

    #[test]
    fn cold_ambient_clamps_to_max_downshift() {
        // 27 - 0 = 27 °C raw downshift, capped at 10 °C → effective 45 °C.
        let r = effective_target_temp_c_default(55.0, Some(0.0));
        assert!((r - 45.0).abs() < 0.01);
    }

    #[test]
    fn extremely_cold_ambient_clamps_to_min_effective_floor() {
        // user_target 38, ambient -10 → raw downshift 37, max 10 → 28
        // — but min_effective_target_c is 35, so floor wins.
        let r = effective_target_temp_c_default(38.0, Some(-10.0));
        assert_eq!(r, 35.0);
    }

    #[test]
    fn low_user_target_never_increases_via_adjustment() {
        // User picked 40 °C target. With cold ambient, the function must
        // not push effective target ABOVE the user's choice.
        let r = effective_target_temp_c_default(40.0, Some(20.0));
        assert!(r <= 40.0);
    }

    #[test]
    fn slope_applies_linearly_in_cold_band() {
        // cold_threshold=27, slope=1.0 → at ambient 22, downshift = 5.
        let r = effective_target_temp_c_default(55.0, Some(22.0));
        assert!((r - 50.0).abs() < 0.01);
        // At ambient 17, downshift = 10 (capped).
        let r = effective_target_temp_c_default(55.0, Some(17.0));
        assert!((r - 45.0).abs() < 0.01);
    }

    #[test]
    fn custom_config_round_trips_through_serde() {
        let c = ColdEnvironmentConfig {
            cold_threshold_c: 20.0,
            max_downshift_c: 5.0,
            slope_c_per_c: 0.5,
            min_effective_target_c: 30.0,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: ColdEnvironmentConfig = serde_json::from_str(&json).unwrap();
        assert!((back.cold_threshold_c - 20.0).abs() < 0.001);
        assert!((back.max_downshift_c - 5.0).abs() < 0.001);
    }
}
