//! Per-board health test (2 minutes).
//!
//! Comprehensive board check without requiring mining:
//!   1. Chip enumeration (verify count matches expected)
//!   2. Voltage domain verification (PIC set/get readback)
//!   3. CRC error rate test (100 dummy commands, count errors)
//!   4. Temperature distribution (check for thermal hotspots)
//!   5. EEPROM validation (if present)

use serde::{Deserialize, Serialize};

/// Board health test configuration.
pub struct BoardHealthTest {
    /// Target chain ID (6, 7, or 8 on S9).
    pub chain_id: u8,
}

impl BoardHealthTest {
    pub fn new(chain_id: u8) -> Self {
        Self { chain_id }
    }
}

/// Board health test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardHealthResult {
    /// Chain ID tested.
    pub chain_id: u8,

    /// Where the data came from.
    #[serde(default)]
    pub data_source: String,

    /// Whether this is a point-in-time snapshot or a dedicated test run.
    #[serde(default)]
    pub measurement_type: String,

    /// Current runtime status string for the chain.
    #[serde(default)]
    pub status: String,

    /// Current estimated runtime hashrate for the chain.
    #[serde(default)]
    pub estimated_hashrate_ghs: f64,

    /// Operator-facing notes about inferred or unavailable fields.
    #[serde(default)]
    pub notes: Vec<String>,

    /// Chip enumeration results.
    pub chips_expected: u8,
    pub chips_responding: u8,
    pub dead_chip_addresses: Vec<u8>,

    /// Voltage domain verification.
    pub voltage_setpoint_v: f32,
    pub voltage_readback_v: f32,
    pub voltage_deviation_pct: f32,
    pub voltage_ok: bool,

    /// CRC error rate test.
    pub crc_commands_sent: u32,
    pub crc_errors_received: u32,
    pub crc_error_rate_pct: f32,
    pub crc_ok: bool,

    /// Temperature readings.
    pub temperature_c: f32,
    pub temperature_ok: bool,

    /// EEPROM data (if available).
    pub eeprom_present: bool,
    pub eeprom_valid: bool,
    pub eeprom_model: Option<String>,
    pub eeprom_serial: Option<String>,

    /// Overall board health grade.
    pub grade: char,
    pub grade_explanation: String,
}

impl BoardHealthResult {
    /// Calculate the overall board grade from individual test results.
    pub fn calculate_grade(&mut self) {
        let mut issues = 0;

        if !self.voltage_ok {
            issues += 1;
        }
        if !self.crc_ok {
            issues += 1;
        }
        if !self.temperature_ok {
            issues += 1;
        }
        if self.chips_responding < self.chips_expected {
            issues += 1;
        }

        let dead = self.dead_chip_addresses.len();

        self.grade = if issues == 0 && dead == 0 {
            'A'
        } else if issues <= 1 && dead <= 2 {
            'B'
        } else if issues <= 2 && dead <= 5 {
            'C'
        } else if dead > 5 || issues > 2 {
            'D'
        } else {
            'F'
        };

        self.grade_explanation = format!(
            "{} chips responding/{} expected, {} dead, {} issues",
            self.chips_responding, self.chips_expected, dead, issues
        );
    }
}
