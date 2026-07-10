//! Battery chemistry presets for off-grid voltage thresholds.
//!
//! Each preset defines the 5 voltage zones used by the off-grid controller
//! to decide when to ramp up, ramp down, or enter deep sleep.
//!
//! Voltage zones (from low to high):
//!   Critical → Low → Normal → High → Full
//!
//! Sources:
//!   - LiFePO4: 3.2V/cell nominal, 2.5V empty, 3.65V full (BattleBorn, EG4)
//!   - Lead Acid: 2.0V/cell nominal, 1.75V empty, 2.4V absorption (Trojan, Crown)

use serde::{Deserialize, Serialize};

/// Battery chemistry preset identifier.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BatteryPreset {
    #[default]
    LiFePO4_48V,
    LiFePO4_24V,
    LiFePO4_12V,
    LeadAcid_48V,
    LeadAcid_24V,
    LeadAcid_12V,
    Custom,
}

/// Voltage thresholds for off-grid 5-zone state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoltageThresholds {
    /// Below this: emergency sleep (deep discharge protection).
    pub critical_v: f32,
    /// Below this: ramp down frequency.
    pub low_v: f32,
    /// Normal operating range (no action needed).
    pub normal_v: f32,
    /// Above this: ramp up frequency (surplus available).
    pub high_v: f32,
    /// Above this: battery full, mine at max frequency.
    pub full_v: f32,
    /// Recovery voltage to wake from deep sleep (hysteresis).
    pub recovery_v: f32,
}

impl BatteryPreset {
    /// Get voltage thresholds for this chemistry.
    pub fn thresholds(&self) -> VoltageThresholds {
        match self {
            BatteryPreset::LiFePO4_48V => VoltageThresholds {
                critical_v: 40.0, // 2.50V/cell × 16S
                low_v: 47.0,      // 2.94V/cell
                normal_v: 51.2,   // 3.20V/cell (nominal)
                high_v: 53.6,     // 3.35V/cell
                full_v: 54.4,     // 3.40V/cell (near absorption)
                recovery_v: 49.0, // 3.06V/cell
            },
            BatteryPreset::LiFePO4_24V => VoltageThresholds {
                critical_v: 20.0,
                low_v: 23.5,
                normal_v: 25.6,
                high_v: 26.8,
                full_v: 27.2,
                recovery_v: 24.5,
            },
            BatteryPreset::LiFePO4_12V => VoltageThresholds {
                critical_v: 10.0,
                low_v: 11.8,
                normal_v: 12.8,
                high_v: 13.4,
                full_v: 13.6,
                recovery_v: 12.2,
            },
            BatteryPreset::LeadAcid_48V => VoltageThresholds {
                critical_v: 42.0, // 1.75V/cell × 24
                low_v: 46.0,
                normal_v: 48.0, // 2.0V/cell nominal
                high_v: 50.4,
                full_v: 57.6, // 2.4V/cell absorption
                recovery_v: 48.0,
            },
            BatteryPreset::LeadAcid_24V => VoltageThresholds {
                critical_v: 21.0,
                low_v: 23.0,
                normal_v: 24.0,
                high_v: 25.2,
                full_v: 28.8,
                recovery_v: 24.0,
            },
            BatteryPreset::LeadAcid_12V => VoltageThresholds {
                critical_v: 10.5,
                low_v: 11.5,
                normal_v: 12.0,
                high_v: 12.6,
                full_v: 14.4,
                recovery_v: 12.0,
            },
            BatteryPreset::Custom => VoltageThresholds {
                critical_v: 10.0,
                low_v: 11.5,
                normal_v: 12.0,
                high_v: 13.0,
                full_v: 14.0,
                recovery_v: 12.0,
            },
        }
    }

    /// Human-readable name for dashboard display.
    pub fn label(&self) -> &'static str {
        match self {
            BatteryPreset::LiFePO4_48V => "LiFePO4 48V (16S)",
            BatteryPreset::LiFePO4_24V => "LiFePO4 24V (8S)",
            BatteryPreset::LiFePO4_12V => "LiFePO4 12V (4S)",
            BatteryPreset::LeadAcid_48V => "Lead Acid 48V (24 cells)",
            BatteryPreset::LeadAcid_24V => "Lead Acid 24V (12 cells)",
            BatteryPreset::LeadAcid_12V => "Lead Acid 12V (6 cells)",
            BatteryPreset::Custom => "Custom",
        }
    }

    /// List all available presets with labels and thresholds.
    pub fn all_presets() -> Vec<(BatteryPreset, &'static str, VoltageThresholds)> {
        vec![
            (
                BatteryPreset::LiFePO4_48V,
                "LiFePO4 48V (16S)",
                BatteryPreset::LiFePO4_48V.thresholds(),
            ),
            (
                BatteryPreset::LiFePO4_24V,
                "LiFePO4 24V (8S)",
                BatteryPreset::LiFePO4_24V.thresholds(),
            ),
            (
                BatteryPreset::LiFePO4_12V,
                "LiFePO4 12V (4S)",
                BatteryPreset::LiFePO4_12V.thresholds(),
            ),
            (
                BatteryPreset::LeadAcid_48V,
                "Lead Acid 48V",
                BatteryPreset::LeadAcid_48V.thresholds(),
            ),
            (
                BatteryPreset::LeadAcid_24V,
                "Lead Acid 24V",
                BatteryPreset::LeadAcid_24V.thresholds(),
            ),
            (
                BatteryPreset::LeadAcid_12V,
                "Lead Acid 12V",
                BatteryPreset::LeadAcid_12V.thresholds(),
            ),
        ]
    }
}
