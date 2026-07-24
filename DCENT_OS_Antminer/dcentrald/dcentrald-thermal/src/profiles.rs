//! ATM (Automatic Thermal Management) profiles and power presets.
//!
//! Thermal profiles define the temperature thresholds and fan behavior
//! for different operating conditions. Power presets define the target
//! wattage and associated hashrate/noise for Home mode.

use serde::{Deserialize, Serialize};

/// Thermal profile defining temperature thresholds and fan limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalProfile {
    /// Normal operating temperature target (celsius). Default: 55.
    pub target_temp_c: u8,

    /// Start throttling at this temperature. Default: 65.
    pub hot_temp_c: u8,

    /// Emergency shutdown threshold. Default: 75.
    pub dangerous_temp_c: u8,

    /// Minimum fan speed (PWM, 0-100 on BraiinsOS fan_ctrl scale).
    /// Default: 0 (~900 RPM hardware minimum; fans never fully stop).
    pub fan_min_pwm: u8,

    /// Maximum fan speed (PWM, 0-100 on BraiinsOS fan_ctrl scale).
    /// MUST NOT exceed 100 on BraiinsOS fan_ctrl (writing > 100 panics in the
    /// am2 IP; S9 am1 tolerates it but wastes register bandwidth).
    /// Default: 30 for home-quiet; 80 for S19j Pro industrial.
    /// — safety paths cap at this value,
    /// NEVER at the hardware maximum.
    pub fan_max_pwm: u8,

    /// Post-ramp stabilization delay in seconds. Default: 300.
    pub ramp_delay_s: u16,

    /// Temperature hysteresis band (celsius). Default: 3.
    pub hysteresis_c: u8,
}

impl Default for ThermalProfile {
    fn default() -> Self {
        Self {
            target_temp_c: 55,
            hot_temp_c: 65,
            dangerous_temp_c: 75,
            fan_min_pwm: 0,
            fan_max_pwm: 30, // Home mining default — quiet
            ramp_delay_s: 300,
            hysteresis_c: 3,
        }
    }
}

impl ThermalProfile {
    /// Create a quiet profile for Home mode.
    pub fn home_quiet() -> Self {
        Self {
            target_temp_c: 55,
            hot_temp_c: 65,
            dangerous_temp_c: 70, // Tighter limit for home use
            fan_min_pwm: 10,      // Never fully silent (safety)
            fan_max_pwm: 30,      // Home mining — silent space heater
            ramp_delay_s: 300,
            hysteresis_c: 3,
        }
    }

    /// Create a hacker mode profile with relaxed limits.
    ///
    /// SAFETY: fan_max_pwm is capped at 100 (the BraiinsOS fan_ctrl IP ceiling).
    /// The old 127 value was a pre-am2 bug; writing >100 to the am2 uio16 IP
    /// will panic..
    pub fn hacker() -> Self {
        Self {
            target_temp_c: 60,
            hot_temp_c: 75,
            dangerous_temp_c: 85, // Relaxed for testing
            fan_min_pwm: 0,
            fan_max_pwm: 100, // IP ceiling — never above this
            ramp_delay_s: 120,
            hysteresis_c: 5,
        }
    }

    /// S19j Pro am2 thermal profile — industrial 100 TH/s envelope.
    ///
    /// Thresholds per Phase 1 Agent 7 probe + bosminer TEMPCTRL:
    ///   target=60°C, hot=80°C, dangerous=90°C.
    ///
    /// Scope/truthfulness note: these thresholds are applied by `controller.rs`
    /// to whatever temperature it is handed each tick — the board PCB/composite
    /// when board sensors are present, or the XADC die-temp fallback when they
    /// are not. This profile (and `controller.rs`) does NOT carry a dedicated,
    /// separate chip-die ceiling; the only distinct chip-die limits in the
    /// system live in [`crate::supervisor`] (`chip_hot_c` / `chip_panic_c`,
    /// RE-005 defaults 93°C / 100°C). There is no 110°C BM1362 die-readback
    /// ceiling — earlier comments here that claimed one were inaccurate.
    ///
    /// fan_max_pwm=80 matches the industrial ceiling from
    ///  (home cap 30, industrial cap 80).
    /// BraiinsOS runs this model at PWM 45 under load — 80 provides thermal
    /// headroom without ever exceeding the IP ceiling of 100.
    pub fn s19j_pro_industrial() -> Self {
        Self {
            target_temp_c: 60,
            hot_temp_c: 80,
            dangerous_temp_c: 90,
            fan_min_pwm: 10, // never fully silent (belt-and-suspenders vs 0-RPM tach)
            fan_max_pwm: 80, // industrial cap; user can opt into 100 via custom config
            ramp_delay_s: 300,
            hysteresis_c: 3,
        }
    }
}

/// Power preset for Home mode.
///
/// Each preset maps to a target wattage with estimated heat output,
/// noise level, and hashrate. Presets are model-specific.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerPreset {
    /// Preset name (e.g., "whisper", "low", "medium", "high", "max").
    pub name: String,
    /// Target power consumption in watts (at the hash board).
    pub watts: u32,
    /// Estimated wall power consumption in watts (watts / 0.88 PSU efficiency at 120V).
    pub wall_watts: u32,
    /// Estimated heat output in BTU/h (watts * 3.412).
    pub btu_h: u32,
    /// Estimated noise level in dB.
    pub noise_db: f32,
    /// Estimated hashrate in TH/s.
    pub hashrate_ths: f32,
}

/// PSU efficiency at 120V (typical for home mining). Used to estimate wall power.
pub const PSU_EFFICIENCY_120V: f32 = 0.88;

impl PowerPreset {
    /// Compute wall watts from board watts (accounting for PSU efficiency at 120V).
    pub fn compute_wall_watts(watts: u32) -> u32 {
        (watts as f32 / PSU_EFFICIENCY_120V) as u32
    }

    /// Get the default S9 power presets.
    pub fn s9_presets() -> Vec<PowerPreset> {
        vec![
            PowerPreset {
                name: "whisper".to_string(),
                watts: 300,
                wall_watts: Self::compute_wall_watts(300),
                btu_h: 1024,
                noise_db: 35.0,
                hashrate_ths: 4.5,
            },
            PowerPreset {
                name: "low".to_string(),
                watts: 500,
                wall_watts: Self::compute_wall_watts(500),
                btu_h: 1706,
                noise_db: 40.0,
                hashrate_ths: 7.5,
            },
            PowerPreset {
                name: "medium".to_string(),
                watts: 800,
                wall_watts: Self::compute_wall_watts(800),
                btu_h: 2730,
                noise_db: 48.0,
                hashrate_ths: 10.5,
            },
            PowerPreset {
                name: "high".to_string(),
                watts: 1200,
                wall_watts: Self::compute_wall_watts(1200),
                btu_h: 4094,
                noise_db: 58.0,
                hashrate_ths: 13.5,
            },
            PowerPreset {
                name: "max".to_string(),
                watts: 1400,
                wall_watts: Self::compute_wall_watts(1400),
                btu_h: 4777,
                noise_db: 65.0,
                hashrate_ths: 14.0,
            },
        ]
    }
}

/// Outcome of `enforce_amlogic_tach_safety_policy`. Daemon callers map
/// this to log lines + telemetry; tests assert the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmlogicTachPolicy {
    /// Profile is unchanged — either the platform is not am3-aml, the
    /// requested cap was already at or below `Balanced` (PWM 64), or
    /// the operator explicitly accepted degraded tach.
    Allowed,
    /// Profile was clamped down to `Balanced` (PWM 64) because the
    /// am3-aml tach hasn't been verified per-fan-channel yet. Operator
    /// can opt back into Advanced/HashrateMax with `--accept-degraded-tach`.
    ClampedToBalanced { requested_cap: u8, applied_cap: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "required energized-airflow PWM floor {required_min_pwm} exceeds configured fan_max_pwm {configured_max_pwm}"
)]
pub struct RequiredAirflowRefusal {
    pub required_min_pwm: u8,
    pub configured_max_pwm: u8,
}

/// Apply a platform capability's minimum energized-airflow command without
/// widening the configured maximum. A maximum below the capability floor is a
/// hard refusal: silently lowering the required floor would permit hashing
/// without an airflow command.
pub fn enforce_required_airflow_pwm(
    profile: &mut ThermalProfile,
    required_min_pwm: u8,
) -> std::result::Result<(), RequiredAirflowRefusal> {
    if profile.fan_max_pwm < required_min_pwm {
        return Err(RequiredAirflowRefusal {
            required_min_pwm,
            configured_max_pwm: profile.fan_max_pwm,
        });
    }
    profile.fan_min_pwm = profile
        .fan_min_pwm
        .max(required_min_pwm)
        .min(profile.fan_max_pwm);
    Ok(())
}

/// am3-aml fan-channel calibration is not yet live-verified per fan.
/// Until then, refuse Advanced (PWM 65-100) and HashrateMax (PWM 101+)
/// fan modes on Amlogic platforms and clamp the profile down to
/// `Balanced` (PWM 64). The operator can opt out with
/// `--accept-degraded-tach` (or the matching env var); in that case
/// the profile is left untouched and the daemon logs the override.
///
/// Inputs:
///   - `profile`: thermal profile coming from config; mutated in place
///     when clamping is required.
///   - `is_amlogic`: caller passes `true` iff the running platform is
///     am3-aml. Pure-function design keeps this crate no-HAL and unit-
///     testable on Windows.
///   - `accept_degraded_tach`: operator override flag (CLI or env var).
///
/// Returns `AmlogicTachPolicy::Allowed` when no clamp was applied,
/// `ClampedToBalanced` when the profile was lowered. Never panics, never
/// returns `Err`: the policy is fail-closed on safety (clamp down) but
/// it's a clamp, not a hard refusal — the daemon must keep running so
/// the user can still mine in Balanced mode.
///
/// / :
/// home mining default is PWM 30 anyway, so this clamp only ever hits
/// users who explicitly opted into Advanced/HashrateMax on Amlogic.
///
/// The native Amlogic serial-mining path applies this policy before acquiring
/// pre-energization fan-motion evidence. It additionally enforces the platform
/// capability's positive energized-airflow PWM floor; this function remains
/// concerned only with the upper mode ceiling.
pub fn enforce_amlogic_tach_safety_policy(
    profile: &mut ThermalProfile,
    is_amlogic: bool,
    accept_degraded_tach: bool,
) -> AmlogicTachPolicy {
    // Cap below which Advanced/HashrateMax don't apply. Mirrors the
    // FanMode::Balanced cap (PWM 64) in dcentrald-api-types.
    const BALANCED_MAX_PWM: u8 = 64;

    if !is_amlogic {
        return AmlogicTachPolicy::Allowed;
    }
    if profile.fan_max_pwm <= BALANCED_MAX_PWM {
        return AmlogicTachPolicy::Allowed;
    }
    if accept_degraded_tach {
        return AmlogicTachPolicy::Allowed;
    }

    let requested_cap = profile.fan_max_pwm;
    profile.fan_max_pwm = BALANCED_MAX_PWM;
    if profile.fan_min_pwm > profile.fan_max_pwm {
        profile.fan_min_pwm = profile.fan_max_pwm;
    }
    AmlogicTachPolicy::ClampedToBalanced {
        requested_cap,
        applied_cap: BALANCED_MAX_PWM,
    }
}

/// BTU conversion factor (watts to BTU/h).
pub const WATTS_TO_BTU: f32 = 3.412;

/// Convert watts to BTU/h.
pub fn watts_to_btu(watts: u32) -> u32 {
    (watts as f32 * WATTS_TO_BTU) as u32
}

/// Estimate monthly electricity cost.
pub fn monthly_cost(watts: u32, rate_per_kwh: f32) -> f32 {
    watts as f32 * 24.0 * 30.0 * rate_per_kwh / 1000.0
}

/// Estimate daily electricity cost.
pub fn daily_cost(watts: u32, rate_per_kwh: f32) -> f32 {
    watts as f32 * 24.0 * rate_per_kwh / 1000.0
}

#[cfg(test)]
mod amlogic_tach_policy_tests {
    use super::*;

    #[test]
    fn required_airflow_floor_never_allows_an_energized_zero_command() {
        let mut profile = ThermalProfile {
            fan_min_pwm: 0,
            fan_max_pwm: 30,
            ..Default::default()
        };
        enforce_required_airflow_pwm(&mut profile, 10).unwrap();
        assert_eq!(profile.fan_min_pwm, 10);
        assert_eq!(profile.fan_max_pwm, 30);
    }

    #[test]
    fn required_airflow_floor_refuses_a_maximum_below_capability() {
        let mut profile = ThermalProfile {
            fan_min_pwm: 0,
            fan_max_pwm: 9,
            ..Default::default()
        };
        assert_eq!(
            enforce_required_airflow_pwm(&mut profile, 10),
            Err(RequiredAirflowRefusal {
                required_min_pwm: 10,
                configured_max_pwm: 9,
            })
        );
        assert_eq!(profile.fan_min_pwm, 0);
        assert_eq!(profile.fan_max_pwm, 9);
    }

    #[test]
    fn non_amlogic_platforms_are_never_clamped() {
        let mut p = ThermalProfile::hacker(); // fan_max_pwm = 100
        let policy = enforce_amlogic_tach_safety_policy(&mut p, false, false);
        assert_eq!(policy, AmlogicTachPolicy::Allowed);
        assert_eq!(p.fan_max_pwm, 100, "non-Amlogic profiles must be unchanged");
    }

    #[test]
    fn amlogic_balanced_or_below_passes_through() {
        let mut p = ThermalProfile::home_quiet(); // 30
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, false);
        assert_eq!(policy, AmlogicTachPolicy::Allowed);
        assert_eq!(p.fan_max_pwm, 30);

        // Edge case: exactly Balanced cap.
        let mut p = ThermalProfile {
            fan_max_pwm: 64,
            ..Default::default()
        };
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, false);
        assert_eq!(policy, AmlogicTachPolicy::Allowed);
        assert_eq!(p.fan_max_pwm, 64);
    }

    #[test]
    fn amlogic_advanced_clamps_to_balanced_without_override() {
        let mut p = ThermalProfile::hacker(); // 100
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, false);
        assert_eq!(
            policy,
            AmlogicTachPolicy::ClampedToBalanced {
                requested_cap: 100,
                applied_cap: 64,
            }
        );
        assert_eq!(
            p.fan_max_pwm, 64,
            "Advanced (PWM 100) clamps to Balanced (PWM 64)"
        );
    }

    #[test]
    fn amlogic_hashratemax_clamps_to_balanced_without_override() {
        // legacy HashrateMax cap (127) — should still clamp.
        let mut p = ThermalProfile {
            fan_max_pwm: 127,
            ..Default::default()
        };
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, false);
        assert_eq!(
            policy,
            AmlogicTachPolicy::ClampedToBalanced {
                requested_cap: 127,
                applied_cap: 64,
            }
        );
        assert_eq!(p.fan_max_pwm, 64);
    }

    #[test]
    fn amlogic_advanced_passes_through_with_explicit_override() {
        let mut p = ThermalProfile::hacker();
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, true);
        assert_eq!(policy, AmlogicTachPolicy::Allowed);
        assert_eq!(
            p.fan_max_pwm, 100,
            "explicit accept-degraded-tach must preserve Advanced cap"
        );
    }

    #[test]
    fn clamp_lowers_fan_min_if_it_exceeds_new_max() {
        let mut p = ThermalProfile {
            fan_max_pwm: 100,
            fan_min_pwm: 90,
            ..Default::default()
        };
        let policy = enforce_amlogic_tach_safety_policy(&mut p, true, false);
        assert!(matches!(
            policy,
            AmlogicTachPolicy::ClampedToBalanced { .. }
        ));
        assert_eq!(p.fan_max_pwm, 64);
        assert_eq!(p.fan_min_pwm, 64, "fan_min_pwm must not exceed new max");
    }

    // -- THERMAL-2: the daemon's "effective ceiling" derivation (the single u8
    // it now applies to EVERY am3-aml fan command) is exactly the post-policy
    // `fan_max_pwm`. This pins the contract the serial-mining loop relies on. --
    fn effective_ceiling(
        configured_max: u8,
        configured_min: u8,
        is_amlogic: bool,
        accept: bool,
    ) -> u8 {
        let mut p = ThermalProfile {
            fan_max_pwm: configured_max,
            fan_min_pwm: configured_min,
            ..Default::default()
        };
        let _ = enforce_amlogic_tach_safety_policy(&mut p, is_amlogic, accept);
        p.fan_max_pwm
    }

    #[test]
    fn thermal2_effective_ceiling_matches_daemon_contract() {
        // Amlogic, no override: >64 clamps to 64; <=64 untouched.
        assert_eq!(
            effective_ceiling(100, 10, true, false),
            64,
            "Advanced clamps to Balanced"
        );
        assert_eq!(
            effective_ceiling(127, 10, true, false),
            64,
            "HashrateMax clamps to Balanced"
        );
        assert_eq!(
            effective_ceiling(30, 10, true, false),
            30,
            "home cap untouched (cut-hash-before-noise preserved)"
        );
        assert_eq!(
            effective_ceiling(64, 10, true, false),
            64,
            "exactly Balanced untouched"
        );
        // Amlogic, operator override: configured cap preserved.
        assert_eq!(
            effective_ceiling(100, 10, true, true),
            100,
            "override preserves Advanced"
        );
        // Non-Amlogic: never clamped regardless.
        assert_eq!(
            effective_ceiling(100, 10, false, false),
            100,
            "non-Amlogic unchanged"
        );
        // The effective ceiling never EXCEEDS the configured max (only lowers).
        for max in [0u8, 10, 30, 64, 65, 100, 127, 255] {
            assert!(
                effective_ceiling(max, 0, true, false) <= max,
                "effective ceiling must only ever LOWER the configured max ({max})"
            );
        }
    }
}
