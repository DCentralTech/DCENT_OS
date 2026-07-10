//!  braiins-E — BraiinsOS+ Cooling Mode + Auto Mode + Pause
//! Mode + Pre-Heat DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §7 (Temperature and Fan Control, lines 827-921):
//! - Cooling Modes enum (Auto/Manual/Immersion/Hydro)
//! - PID Controller fan-speed law
//! - CoolingAutoMode message
//! - PauseMode oneof (Auto / Manual)
//! - Pre-Heat Mode (cold-weather protection, < 0 °C)
//!
//!  braiins-A pinned the `CoolingService` method catalog
//! (GetCoolingState / SetCoolingMode / SetImmersionMode). This module
//! ships the typed payloads.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Cooling mode enum
// ---------------------------------------------------------------------------

/// Per RE doc §7 lines 829-836. Note: proto value `3` is reserved /
/// unused — variants jump from `MANUAL=2` to `IMMERSION=4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum CoolingMode {
    /// `COOLING_MODE_AUTO = 1` — PID controller targets configured
    /// temperature.
    Auto = 1,
    /// `COOLING_MODE_MANUAL = 2` — Fixed fan speed, no temperature
    /// regulation.
    Manual = 2,
    /// `COOLING_MODE_IMMERSION = 4` — Fans disabled, for liquid cooling.
    Immersion = 4,
    /// `COOLING_MODE_HYDRO = 5` — For hydro-cooled models. Introduced
    /// in v1.4.0.
    Hydro = 5,
}

impl CoolingMode {
    /// Proto3 numeric tag value.
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }

    /// Look up by numeric tag. Note: tag 3 is intentionally unmapped
    /// (reserved/unused per the proto definition).
    pub fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Auto,
            2 => Self::Manual,
            4 => Self::Immersion,
            5 => Self::Hydro,
            _ => return None,
        })
    }

    /// True iff fan control is fully disabled in this mode.
    /// Per §7 line 833: immersion mode disables fans entirely.
    pub fn fans_disabled(&self) -> bool {
        matches!(self, Self::Immersion)
    }

    /// True iff this mode requires PID temperature regulation
    /// (Auto + Hydro).
    pub fn requires_pid_loop(&self) -> bool {
        matches!(self, Self::Auto | Self::Hydro)
    }
}

/// First BraiinsOS+ minor version that introduced `Hydro` mode.
pub const HYDRO_MODE_INTRODUCED_VERSION: &str = "1.4.0";

// ---------------------------------------------------------------------------
// CoolingAutoMode message
// ---------------------------------------------------------------------------

/// `CoolingAutoMode` message per RE doc §7 lines 859-868. Wire types
/// per §9.2.28 (Temperature=f64 °C, percentages=u32 0-100).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoolingAutoMode {
    pub target_temperature_c: f64,
    pub hot_temperature_c: f64,
    pub dangerous_temperature_c: f64,
    /// `min_fan_speed` 0-100. Introduced in v1.4.0.
    pub min_fan_speed: Option<u32>,
    /// `max_fan_speed` 0-100. Introduced in v1.4.0.
    pub max_fan_speed: Option<u32>,
    /// Min fans before shutdown.
    pub minimum_required_fans: Option<u32>,
    /// Fan behavior when mining is paused. Introduced in v1.6.0.
    pub pause_mode: Option<CoolingPauseMode>,
}

impl CoolingAutoMode {
    /// True iff `target < hot < dangerous`. The PID controller
    /// requires this ordering per §7 lines 851-854.
    pub fn thresholds_strictly_increasing(&self) -> bool {
        self.target_temperature_c < self.hot_temperature_c
            && self.hot_temperature_c < self.dangerous_temperature_c
    }
}

// ---------------------------------------------------------------------------
// PauseMode (oneof Auto | Manual)
// ---------------------------------------------------------------------------

/// `PauseMode` oneof per RE doc §7 lines 875-881. Mirrors the proto
/// `oneof mode { auto, manual }` with serde-tagged variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoolingPauseMode {
    /// `AutoPauseMode auto = 1` — Gradual decrease from last speed to 0%.
    Auto(AutoPauseMode),
    /// `ManualPauseMode manual = 2` — Fixed speed for duration then off.
    Manual(ManualPauseMode),
}

/// `AutoPauseMode` — gradual decrease. RE doc doesn't specify fields;
/// we ship a unit-shape struct so the oneof matches proto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AutoPauseMode {}

/// `ManualPauseMode` per RE doc §7 lines 883-887.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ManualPauseMode {
    /// `pause_cooldown_fan_speed_ratio` 0.0-1.0.
    pub pause_cooldown_fan_speed_ratio: Option<f64>,
    /// LIMITED → fans run for fan_pause_runtime then off; INDEFINITE
    /// → fans stay on for the entire pause window.
    pub fan_pause_runtime: Option<FanPauseRuntime>,
}

/// `FanPauseRuntime` per RE doc §7 line 886.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FanPauseRuntime {
    /// Fans run for a documented finite window then stop.
    Limited,
    /// Fans run for the entire pause duration.
    Indefinite,
}

impl ManualPauseMode {
    /// True iff `pause_cooldown_fan_speed_ratio` is in [0.0, 1.0]
    /// (or unset). A wire value outside this range is malformed.
    pub fn ratio_is_valid(&self) -> bool {
        match self.pause_cooldown_fan_speed_ratio {
            Some(r) => (0.0..=1.0).contains(&r),
            None => true,
        }
    }
}

/// First BraiinsOS+ minor version that introduced the `pause_mode`
/// oneof.
pub const PAUSE_MODE_INTRODUCED_VERSION: &str = "1.6.0";

// ---------------------------------------------------------------------------
// Pre-Heat Mode (cold-weather protection)
// ---------------------------------------------------------------------------

/// Pre-heat configuration per RE doc §7 lines 897-912.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoolingPreheatConfig {
    /// Board temperature below this (°C) triggers pre-heat sequence.
    /// Per §7 line 903: "below 0C".
    pub detect_threshold_c: f32,
    /// Maximum pre-heat duration before proceeding anyway. Per §7
    /// line 905: "up to 10 minutes".
    pub max_duration_minutes: u32,
}

impl Default for CoolingPreheatConfig {
    fn default() -> Self {
        // RE doc §7 documented anchors.
        Self {
            detect_threshold_c: 0.0,
            max_duration_minutes: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooling_mode_proto_values_match_re_doc() {
        // RE doc §7 lines 829-836: AUTO=1, MANUAL=2, IMMERSION=4, HYDRO=5.
        // Note: value 3 is reserved/unused.
        assert_eq!(CoolingMode::Auto.as_u8(), 1);
        assert_eq!(CoolingMode::Manual.as_u8(), 2);
        assert_eq!(CoolingMode::Immersion.as_u8(), 4);
        assert_eq!(CoolingMode::Hydro.as_u8(), 5);
    }

    #[test]
    fn cooling_mode_value_3_is_intentionally_unmapped() {
        // The "value 3 is reserved" finding is load-bearing — pin via
        // from_u8 returning None for tag 3.
        assert!(CoolingMode::from_u8(3).is_none());
    }

    #[test]
    fn cooling_mode_round_trips_through_proto_byte() {
        for m in [
            CoolingMode::Auto,
            CoolingMode::Manual,
            CoolingMode::Immersion,
            CoolingMode::Hydro,
        ] {
            let b = m.as_u8();
            assert_eq!(CoolingMode::from_u8(b), Some(m));
        }
        // Out-of-range / unmapped tags.
        for unknown in [0u8, 3, 6, 7, 100, 255] {
            assert!(
                CoolingMode::from_u8(unknown).is_none(),
                "tag {} unexpectedly mapped",
                unknown
            );
        }
    }

    #[test]
    fn cooling_mode_serializes_in_screaming_snake_case() {
        // Proto3 wire form for enum names.
        for (mode, expected) in [
            (CoolingMode::Auto, "\"AUTO\""),
            (CoolingMode::Manual, "\"MANUAL\""),
            (CoolingMode::Immersion, "\"IMMERSION\""),
            (CoolingMode::Hydro, "\"HYDRO\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), expected);
        }
    }

    #[test]
    fn fans_disabled_only_in_immersion() {
        assert!(CoolingMode::Immersion.fans_disabled());
        for m in [CoolingMode::Auto, CoolingMode::Manual, CoolingMode::Hydro] {
            assert!(!m.fans_disabled(), "{:?} should NOT disable fans", m);
        }
    }

    #[test]
    fn pid_loop_only_in_auto_and_hydro() {
        assert!(CoolingMode::Auto.requires_pid_loop());
        assert!(CoolingMode::Hydro.requires_pid_loop());
        assert!(!CoolingMode::Manual.requires_pid_loop());
        assert!(!CoolingMode::Immersion.requires_pid_loop());
    }

    #[test]
    fn hydro_introduced_version_pinned() {
        assert_eq!(HYDRO_MODE_INTRODUCED_VERSION, "1.4.0");
    }

    #[test]
    fn auto_mode_thresholds_strictly_increasing_predicate() {
        let good = CoolingAutoMode {
            target_temperature_c: 75.0,
            hot_temperature_c: 85.0,
            dangerous_temperature_c: 95.0,
            min_fan_speed: Some(20),
            max_fan_speed: Some(100),
            minimum_required_fans: Some(2),
            pause_mode: None,
        };
        assert!(good.thresholds_strictly_increasing());

        // Inverted ordering (dangerous < hot) — rejected.
        let bad = CoolingAutoMode {
            dangerous_temperature_c: 80.0,
            ..good.clone()
        };
        assert!(!bad.thresholds_strictly_increasing());
    }

    #[test]
    fn auto_mode_round_trips_through_serde() {
        let original = CoolingAutoMode {
            target_temperature_c: 72.0,
            hot_temperature_c: 85.0,
            dangerous_temperature_c: 92.0,
            min_fan_speed: Some(10),
            max_fan_speed: Some(100),
            minimum_required_fans: Some(2),
            pause_mode: Some(CoolingPauseMode::Auto(AutoPauseMode {})),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: CoolingAutoMode = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn pause_mode_oneof_round_trips_for_both_variants() {
        // Auto variant
        let auto = CoolingPauseMode::Auto(AutoPauseMode {});
        let json = serde_json::to_string(&auto).unwrap();
        let back: CoolingPauseMode = serde_json::from_str(&json).unwrap();
        assert_eq!(auto, back);

        // Manual variant with both fields populated
        let manual = CoolingPauseMode::Manual(ManualPauseMode {
            pause_cooldown_fan_speed_ratio: Some(0.5),
            fan_pause_runtime: Some(FanPauseRuntime::Limited),
        });
        let json = serde_json::to_string(&manual).unwrap();
        let back: CoolingPauseMode = serde_json::from_str(&json).unwrap();
        assert_eq!(manual, back);
    }

    #[test]
    fn pause_mode_serializes_with_kind_tag() {
        let m = CoolingPauseMode::Auto(AutoPauseMode {});
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"auto\""));

        let m = CoolingPauseMode::Manual(ManualPauseMode::default());
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"manual\""));
    }

    #[test]
    fn fan_pause_runtime_serializes_in_screaming_snake_case() {
        assert_eq!(
            serde_json::to_string(&FanPauseRuntime::Limited).unwrap(),
            "\"LIMITED\""
        );
        assert_eq!(
            serde_json::to_string(&FanPauseRuntime::Indefinite).unwrap(),
            "\"INDEFINITE\""
        );
    }

    #[test]
    fn manual_pause_ratio_validates_zero_to_one() {
        let mut m = ManualPauseMode {
            pause_cooldown_fan_speed_ratio: Some(0.0),
            fan_pause_runtime: None,
        };
        assert!(m.ratio_is_valid());

        m.pause_cooldown_fan_speed_ratio = Some(0.5);
        assert!(m.ratio_is_valid());

        m.pause_cooldown_fan_speed_ratio = Some(1.0);
        assert!(m.ratio_is_valid());

        m.pause_cooldown_fan_speed_ratio = Some(1.5);
        assert!(!m.ratio_is_valid());

        m.pause_cooldown_fan_speed_ratio = Some(-0.1);
        assert!(!m.ratio_is_valid());

        m.pause_cooldown_fan_speed_ratio = None;
        assert!(m.ratio_is_valid());
    }

    #[test]
    fn pause_mode_introduced_version_pinned() {
        assert_eq!(PAUSE_MODE_INTRODUCED_VERSION, "1.6.0");
    }

    #[test]
    fn preheat_default_anchors_match_re_doc() {
        // RE doc §7 lines 897-912: < 0 °C trigger, up to 10 minutes.
        let cfg = CoolingPreheatConfig::default();
        assert_eq!(cfg.detect_threshold_c, 0.0);
        assert_eq!(cfg.max_duration_minutes, 10);
    }

    #[test]
    fn preheat_round_trips_through_serde() {
        let original = CoolingPreheatConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let back: CoolingPreheatConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn auto_mode_with_immersion_pause_mode_round_trips() {
        // Mix-and-match: AutoMode struct with a Manual pause variant.
        let original = CoolingAutoMode {
            target_temperature_c: 75.0,
            hot_temperature_c: 85.0,
            dangerous_temperature_c: 95.0,
            min_fan_speed: None,
            max_fan_speed: None,
            minimum_required_fans: None,
            pause_mode: Some(CoolingPauseMode::Manual(ManualPauseMode {
                pause_cooldown_fan_speed_ratio: Some(0.3),
                fan_pause_runtime: Some(FanPauseRuntime::Indefinite),
            })),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: CoolingAutoMode = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }
}
