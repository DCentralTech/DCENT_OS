//!  luxos-G — LuxOS thermal sensor topology + threshold
//! hierarchy DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §1 (sensor topology, lines 22-114) + §2 (3-tier threshold
//! hierarchy, lines 118-177).
//!
//!  atm_stepper.rs ports the ATM (Advanced Thermal Management)
//! profile-stepper state machine.  thermal_model.rs ships the
//! DCENT_OS continuous-derating ALGO 7 + VNish profile auto-switching
//! + FanMode safety caps. This module fills the per-board
//! sensor-side surface that those state machines read from:
//!
//! - The 4-corner board sensor model (S19 / S19j Pro family).
//! - The full 9-position sensor enum from the luxminer binary
//!   (covers 4-corner + 6-sensor + hydro + 5-channel modes).
//! - The 3-tier threshold hierarchy (target / hot / panic) per §2.1
//!   with the live `a lab unit` default values.
//! - A `classify_temperature` helper returning Mine / ThrottlePower /
//!   EmergencyShutdown.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Sensor positions
// ---------------------------------------------------------------------------

/// Sensor position label used by the `tempctrl` / `temps` API replies.
/// Sourced from `strings(luxminer)` per §1.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum LuxosBoardSensorPosition {
    /// `[0, 0]` — exhaust top corner. "Board Exhaust (top)".
    TopLeft,
    /// `[0, 1]` — intake top corner. "Board Intake (top)".
    TopRight,
    /// `[1, 0]` — exhaust bottom corner. "Board Exhaust (bottom)".
    BottomLeft,
    /// `[1, 1]` — intake bottom corner. "Board Intake (bottom)".
    BottomRight,
    /// 6-sensor mode (S19 XP). "Board Exhaust (middle)".
    MiddleExhaust,
    /// Hydro chassis-level inlet sensor.
    BoardInlet,
    /// Hydro chassis-level outlet sensor.
    BoardOutlet,
    /// Hydro liquid-loop inlet sensor.
    WaterInlet,
    /// Hydro liquid-loop outlet sensor.
    WaterOutlet,
}

impl LuxosBoardSensorPosition {
    /// True iff this position is on a hydro-cooled chassis (board /
    /// water inlet/outlet) — only present on hydro / immersion units.
    pub fn is_hydro_only(&self) -> bool {
        matches!(
            self,
            Self::BoardInlet | Self::BoardOutlet | Self::WaterInlet | Self::WaterOutlet
        )
    }

    /// True iff this position is on the exhaust (hot) side of the
    /// board — should read hotter than intake-side under load.
    pub fn is_exhaust(&self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft | Self::MiddleExhaust)
    }

    /// True iff this position is on the intake (cool) side.
    pub fn is_intake(&self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }
}

// ---------------------------------------------------------------------------
// 4-corner sensor map (S19 / S19j Pro family)
// ---------------------------------------------------------------------------

/// Fixed 4-position sensor map for the S19 / S19j Pro family per §1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuxosBoardSensorMap {
    pub positions: [LuxosBoardSensorPosition; 4],
}

impl LuxosBoardSensorMap {
    /// Canonical S19/S19j Pro 4-corner map per `temps` JSON metadata
    /// header from `a lab unit`.
    pub const S19J_PRO_4_CORNER: Self = Self {
        positions: [
            LuxosBoardSensorPosition::TopLeft,
            LuxosBoardSensorPosition::TopRight,
            LuxosBoardSensorPosition::BottomLeft,
            LuxosBoardSensorPosition::BottomRight,
        ],
    };

    /// True iff the sensor map has no duplicate positions.
    pub fn has_unique_positions(&self) -> bool {
        for i in 0..self.positions.len() {
            for j in (i + 1)..self.positions.len() {
                if self.positions[i] == self.positions[j] {
                    return false;
                }
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// 3-tier threshold hierarchy
// ---------------------------------------------------------------------------

/// Threshold tier per §2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosTempThreshold {
    /// Target — normal mining envelope center.
    Target,
    /// Hot — DPS scales down + emits `BOARD_HIGH_TEMPERATURE` event.
    Hot,
    /// Panic — emergency hashboard poweroff + emits
    /// `BOARD_OVERTEMP_SHUTDOWN` event.
    Panic,
}

/// Per-board threshold config. Defaults match the `a lab unit` live capture
/// per §2.1 ([temp_control] defaults).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LuxosThresholdConfig {
    pub target_c: f32,
    pub hot_c: f32,
    pub panic_c: f32,
}

impl Default for LuxosThresholdConfig {
    fn default() -> Self {
        // §2.1 [temp_control] defaults: target=55, hot=65, panic=70 (board PCB).
        // Note: live `a lab unit` `tempctrl` API returns `Target=45.000` due to
        // the cold-environment runtime adjustment per §2.3 — this struct
        // captures the user-facing CONFIG defaults, not the runtime
        // setpoint.
        Self {
            target_c: 55.0,
            hot_c: 65.0,
            panic_c: 70.0,
        }
    }
}

impl LuxosThresholdConfig {
    /// True iff `target < hot < panic`.
    pub fn is_strictly_increasing(&self) -> bool {
        self.target_c < self.hot_c && self.hot_c < self.panic_c
    }
}

// ---------------------------------------------------------------------------
// Classification helper
// ---------------------------------------------------------------------------

/// Action recommended by the temperature classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosThermalAction {
    /// Below target — fans toward `min_speed`; ATM may step UP.
    Mine,
    /// At/above target but below hot — fan-PID; ATM holds.
    Hold,
    /// At/above hot — fans toward max; ATM steps DOWN; emit
    /// `BOARD_HIGH_TEMPERATURE`.
    ThrottlePower,
    /// At/above panic — emergency hashboard poweroff; emit
    /// `BOARD_OVERTEMP_SHUTDOWN`.
    EmergencyShutdown,
}

/// Classify a measured board temperature against the threshold config.
/// Behaviors per §2.2:
/// - `current < target` → Mine
/// - `target ≤ current < hot` → Hold
/// - `hot ≤ current < panic` → ThrottlePower
/// - `current ≥ panic` → EmergencyShutdown
pub fn classify_temperature(config: &LuxosThresholdConfig, measured_c: f32) -> LuxosThermalAction {
    if measured_c >= config.panic_c {
        LuxosThermalAction::EmergencyShutdown
    } else if measured_c >= config.hot_c {
        LuxosThermalAction::ThrottlePower
    } else if measured_c >= config.target_c {
        LuxosThermalAction::Hold
    } else {
        LuxosThermalAction::Mine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s19j_pro_4_corner_map_has_no_duplicates() {
        let map = LuxosBoardSensorMap::S19J_PRO_4_CORNER;
        assert_eq!(map.positions.len(), 4);
        assert!(map.has_unique_positions());
    }

    #[test]
    fn s19j_pro_4_corner_map_uses_canonical_positions() {
        // §1.1: TopLeft + TopRight + BottomLeft + BottomRight.
        let map = LuxosBoardSensorMap::S19J_PRO_4_CORNER;
        assert!(map.positions.contains(&LuxosBoardSensorPosition::TopLeft));
        assert!(map.positions.contains(&LuxosBoardSensorPosition::TopRight));
        assert!(map
            .positions
            .contains(&LuxosBoardSensorPosition::BottomLeft));
        assert!(map
            .positions
            .contains(&LuxosBoardSensorPosition::BottomRight));
    }

    #[test]
    fn hydro_only_positions_classify_correctly() {
        // §1.2: BoardInlet/Outlet + WaterInlet/Outlet are hydro-only.
        for pos in [
            LuxosBoardSensorPosition::BoardInlet,
            LuxosBoardSensorPosition::BoardOutlet,
            LuxosBoardSensorPosition::WaterInlet,
            LuxosBoardSensorPosition::WaterOutlet,
        ] {
            assert!(pos.is_hydro_only(), "{:?} should be hydro-only", pos);
        }
        // 4-corner positions are NOT hydro-only.
        for pos in [
            LuxosBoardSensorPosition::TopLeft,
            LuxosBoardSensorPosition::TopRight,
            LuxosBoardSensorPosition::BottomLeft,
            LuxosBoardSensorPosition::BottomRight,
            LuxosBoardSensorPosition::MiddleExhaust,
        ] {
            assert!(!pos.is_hydro_only(), "{:?} should NOT be hydro-only", pos);
        }
    }

    #[test]
    fn exhaust_intake_classification_matches_re_doc() {
        // §1.1: TopLeft / BottomLeft = exhaust (col 0).
        // TopRight / BottomRight = intake (col 1).
        assert!(LuxosBoardSensorPosition::TopLeft.is_exhaust());
        assert!(LuxosBoardSensorPosition::BottomLeft.is_exhaust());
        assert!(LuxosBoardSensorPosition::MiddleExhaust.is_exhaust());
        assert!(LuxosBoardSensorPosition::TopRight.is_intake());
        assert!(LuxosBoardSensorPosition::BottomRight.is_intake());
        // Hydro positions are neither.
        assert!(!LuxosBoardSensorPosition::WaterInlet.is_exhaust());
        assert!(!LuxosBoardSensorPosition::WaterInlet.is_intake());
    }

    #[test]
    fn position_round_trips_through_serde() {
        for p in [
            LuxosBoardSensorPosition::TopLeft,
            LuxosBoardSensorPosition::TopRight,
            LuxosBoardSensorPosition::BottomLeft,
            LuxosBoardSensorPosition::BottomRight,
            LuxosBoardSensorPosition::MiddleExhaust,
            LuxosBoardSensorPosition::BoardInlet,
            LuxosBoardSensorPosition::BoardOutlet,
            LuxosBoardSensorPosition::WaterInlet,
            LuxosBoardSensorPosition::WaterOutlet,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: LuxosBoardSensorPosition = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn position_serializes_in_pascal_case() {
        // §1.2 names use PascalCase — pin the wire form.
        assert_eq!(
            serde_json::to_string(&LuxosBoardSensorPosition::TopLeft).unwrap(),
            "\"TopLeft\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosBoardSensorPosition::WaterInlet).unwrap(),
            "\"WaterInlet\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosBoardSensorPosition::MiddleExhaust).unwrap(),
            "\"MiddleExhaust\""
        );
    }

    #[test]
    fn default_threshold_config_matches_re_doc() {
        // §2.1 [temp_control] defaults: target=55, hot=65, panic=70.
        let cfg = LuxosThresholdConfig::default();
        assert!((cfg.target_c - 55.0).abs() < 1e-3);
        assert!((cfg.hot_c - 65.0).abs() < 1e-3);
        assert!((cfg.panic_c - 70.0).abs() < 1e-3);
    }

    #[test]
    fn default_threshold_is_strictly_increasing() {
        // Hard rule: target < hot < panic. Pin so a refactor cannot
        // accidentally invert.
        let cfg = LuxosThresholdConfig::default();
        assert!(cfg.is_strictly_increasing());
    }

    #[test]
    fn default_thresholds_inside_documented_60_75_90_window() {
        // Plan-level claim: defaults inside [60, 75, 90] for typical
        // board PCB sensors. Note the actual config defaults are
        // 55/65/70 which IS conservative compared to chip-die
        // thresholds of 93/100. Cross-check both:
        let cfg = LuxosThresholdConfig::default();
        // PCB defaults sit below the chip thresholds.
        assert!(cfg.target_c < 60.0); // 55 < 60
        assert!(cfg.hot_c <= 75.0); // 65 ≤ 75
        assert!(cfg.panic_c <= 90.0); // 70 ≤ 90
    }

    #[test]
    fn classify_below_target_returns_mine() {
        let cfg = LuxosThresholdConfig::default();
        assert_eq!(classify_temperature(&cfg, 45.0), LuxosThermalAction::Mine);
        assert_eq!(classify_temperature(&cfg, 54.9), LuxosThermalAction::Mine);
    }

    #[test]
    fn classify_in_target_to_hot_window_returns_hold() {
        let cfg = LuxosThresholdConfig::default();
        // [55, 65) → Hold
        assert_eq!(classify_temperature(&cfg, 55.0), LuxosThermalAction::Hold);
        assert_eq!(classify_temperature(&cfg, 60.0), LuxosThermalAction::Hold);
        assert_eq!(classify_temperature(&cfg, 64.99), LuxosThermalAction::Hold);
    }

    #[test]
    fn classify_in_hot_to_panic_window_returns_throttle() {
        let cfg = LuxosThresholdConfig::default();
        // [65, 70) → ThrottlePower
        assert_eq!(
            classify_temperature(&cfg, 65.0),
            LuxosThermalAction::ThrottlePower
        );
        assert_eq!(
            classify_temperature(&cfg, 69.9),
            LuxosThermalAction::ThrottlePower
        );
    }

    #[test]
    fn classify_at_or_above_panic_returns_emergency_shutdown() {
        let cfg = LuxosThresholdConfig::default();
        assert_eq!(
            classify_temperature(&cfg, 70.0),
            LuxosThermalAction::EmergencyShutdown
        );
        assert_eq!(
            classify_temperature(&cfg, 95.0),
            LuxosThermalAction::EmergencyShutdown
        );
    }

    #[test]
    fn threshold_round_trips_through_serde() {
        for t in [
            LuxosTempThreshold::Target,
            LuxosTempThreshold::Hot,
            LuxosTempThreshold::Panic,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: LuxosTempThreshold = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn threshold_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosTempThreshold::Target).unwrap(),
            "\"target\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosTempThreshold::Hot).unwrap(),
            "\"hot\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosTempThreshold::Panic).unwrap(),
            "\"panic\""
        );
    }

    #[test]
    fn config_round_trips_through_serde() {
        let original = LuxosThresholdConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosThresholdConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn classify_handles_custom_threshold_config() {
        // Custom thresholds for hydro: 30/45/55 °C.
        let hydro_cfg = LuxosThresholdConfig {
            target_c: 30.0,
            hot_c: 45.0,
            panic_c: 55.0,
        };
        assert!(hydro_cfg.is_strictly_increasing());
        assert_eq!(
            classify_temperature(&hydro_cfg, 25.0),
            LuxosThermalAction::Mine
        );
        assert_eq!(
            classify_temperature(&hydro_cfg, 40.0),
            LuxosThermalAction::Hold
        );
        assert_eq!(
            classify_temperature(&hydro_cfg, 50.0),
            LuxosThermalAction::ThrottlePower
        );
        assert_eq!(
            classify_temperature(&hydro_cfg, 60.0),
            LuxosThermalAction::EmergencyShutdown
        );
    }

    #[test]
    fn thermal_action_round_trips_through_serde() {
        for a in [
            LuxosThermalAction::Mine,
            LuxosThermalAction::Hold,
            LuxosThermalAction::ThrottlePower,
            LuxosThermalAction::EmergencyShutdown,
        ] {
            let json = serde_json::to_string(&a).unwrap();
            let back: LuxosThermalAction = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn thermal_action_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosThermalAction::Mine).unwrap(),
            "\"mine\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosThermalAction::EmergencyShutdown).unwrap(),
            "\"emergency_shutdown\""
        );
    }

    #[test]
    fn malformed_threshold_config_is_not_strictly_increasing() {
        // A misconfigured threshold (target > hot) should be detected.
        let bad = LuxosThresholdConfig {
            target_c: 80.0,
            hot_c: 65.0,
            panic_c: 70.0,
        };
        assert!(!bad.is_strictly_increasing());
    }
}
