//! Per-board health test (2 minutes).
//!
//! Comprehensive board check without requiring mining:
//!   1. Chip enumeration (verify count matches expected)
//!   2. Voltage domain verification (PIC set/get readback)
//!   3. CRC error rate test (100 dummy commands, count errors)
//!   4. Temperature distribution (check for thermal hotspots)
//!   5. EEPROM validation (if present)

use serde::{Deserialize, Serialize};

use crate::evidence::DiagnosticEvidence;

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
    /// Provenance for the voltage value used by grading. Legacy payloads
    /// deserialize as Unavailable and therefore cannot retain an A/B verdict.
    #[serde(default)]
    pub voltage_evidence: DiagnosticEvidence<f32>,

    /// CRC error rate test.
    pub crc_commands_sent: u32,
    pub crc_errors_received: u32,
    pub crc_error_rate_pct: f32,
    pub crc_ok: bool,
    /// Provenance for the CRC verdict/counter window.
    #[serde(default)]
    pub crc_evidence: DiagnosticEvidence<u32>,

    /// Temperature readings.
    pub temperature_c: f32,
    pub temperature_ok: bool,

    /// EEPROM data (if available).
    pub eeprom_present: bool,
    pub eeprom_valid: bool,
    pub eeprom_model: Option<String>,
    pub eeprom_serial: Option<String>,
    /// Provenance for EEPROM validity. Model metadata is Inferred, not a
    /// checksum/schema validation.
    #[serde(default)]
    pub eeprom_evidence: DiagnosticEvidence<bool>,

    /// True only when every evidence item required for a passing grade was
    /// directly measured.
    #[serde(default, skip_deserializing)]
    pub required_evidence_measured: bool,

    /// Overall board health grade.
    pub grade: char,
    pub grade_explanation: String,
}

impl BoardHealthResult {
    /// Calculate the overall board grade from individual test results.
    pub fn calculate_grade(&mut self) {
        let mut issues = 0;
        let mut evidence_gaps = Vec::new();

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
        if self.eeprom_present && !self.eeprom_valid {
            issues += 1;
        }

        if !self.voltage_readback_v.is_finite()
            || !self
                .voltage_evidence
                .is_measured_for(&self.voltage_readback_v)
        {
            evidence_gaps.push("voltage");
        }
        if !self.crc_evidence.is_measured_for(&self.crc_errors_received) {
            evidence_gaps.push("crc");
        }
        if self.eeprom_present && !self.eeprom_evidence.is_measured_for(&self.eeprom_valid) {
            evidence_gaps.push("eeprom");
        }
        self.required_evidence_measured = evidence_gaps.is_empty();

        let dead = self.dead_chip_addresses.len();

        let calculated_grade = if issues == 0 && dead == 0 {
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

        // A/B are passing manufacturing/repair verdicts. They require direct
        // evidence; commanded, inferred or unavailable values cap the result
        // at C without hiding any worse health-derived grade.
        self.grade = if !self.required_evidence_measured && matches!(calculated_grade, 'A' | 'B') {
            'C'
        } else {
            calculated_grade
        };

        self.grade_explanation = format!(
            "{} chips responding/{} expected, {} dead, {} issues{}",
            self.chips_responding,
            self.chips_expected,
            dead,
            issues,
            if evidence_gaps.is_empty() {
                String::new()
            } else {
                format!(
                    "; passing grade withheld: {} evidence not measured",
                    evidence_gaps.join(", ")
                )
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_result() -> BoardHealthResult {
        BoardHealthResult {
            chain_id: 0,
            data_source: "test".into(),
            measurement_type: "dedicated_test".into(),
            status: "ok".into(),
            estimated_hashrate_ghs: 1_000.0,
            notes: Vec::new(),
            chips_expected: 100,
            chips_responding: 100,
            dead_chip_addresses: Vec::new(),
            voltage_setpoint_v: 13.7,
            voltage_readback_v: 13.68,
            voltage_deviation_pct: 0.15,
            voltage_ok: true,
            voltage_evidence: DiagnosticEvidence::measured(13.68, "rail_adc", Some(10)),
            crc_commands_sent: 100,
            crc_errors_received: 0,
            crc_error_rate_pct: 0.0,
            crc_ok: true,
            crc_evidence: DiagnosticEvidence::measured(0, "bounded_crc_window", Some(10)),
            temperature_c: 55.0,
            temperature_ok: true,
            eeprom_present: true,
            eeprom_valid: true,
            eeprom_model: Some("BHB".into()),
            eeprom_serial: Some("serial".into()),
            eeprom_evidence: DiagnosticEvidence::measured_validated(
                true,
                "eeprom_checksum",
                Some(10),
            ),
            required_evidence_measured: false,
            grade: 'F',
            grade_explanation: String::new(),
        }
    }

    #[test]
    fn fully_measured_healthy_board_retains_a_grade() {
        let mut result = healthy_result();
        result.calculate_grade();
        assert_eq!(result.grade, 'A');
        assert!(result.required_evidence_measured);
    }

    #[test]
    fn commanded_inferred_and_unavailable_required_evidence_cannot_pass() {
        for evidence in [
            DiagnosticEvidence::commanded(13.7, "setpoint", None),
            DiagnosticEvidence::inferred(13.7, "power_model", None),
            DiagnosticEvidence::unavailable("rail_sensor_missing"),
        ] {
            let mut result = healthy_result();
            result.voltage_evidence = evidence;
            result.calculate_grade();
            assert_eq!(result.grade, 'C');
            assert!(!result.required_evidence_measured);
            assert!(result
                .grade_explanation
                .contains("voltage evidence not measured"));
        }
    }

    #[test]
    fn inferred_eeprom_validity_cannot_produce_a_measured_pass() {
        let mut result = healthy_result();
        result.eeprom_evidence = DiagnosticEvidence::inferred(true, "model_metadata", None);
        result.calculate_grade();
        assert_eq!(result.grade, 'C');
        assert!(result
            .grade_explanation
            .contains("eeprom evidence not measured"));
    }

    #[test]
    fn measured_provenance_for_a_different_value_cannot_pass() {
        let mut result = healthy_result();
        result.voltage_evidence = DiagnosticEvidence::measured(13.7, "rail_adc", Some(10));
        result.calculate_grade();
        assert_eq!(result.grade, 'C');
        assert!(result
            .grade_explanation
            .contains("voltage evidence not measured"));
    }

    #[test]
    fn serialized_aggregate_evidence_claim_is_ignored_and_recomputed() {
        let mut value = serde_json::to_value(healthy_result()).unwrap();
        value["voltage_evidence"] = serde_json::json!({
            "kind": "commanded",
            "value": 13.68,
            "source": "runtime_setpoint",
            "quality": "observed"
        });
        value["required_evidence_measured"] = serde_json::json!(true);

        let mut imported: BoardHealthResult = serde_json::from_value(value).unwrap();
        assert!(!imported.required_evidence_measured);
        imported.calculate_grade();
        assert_eq!(imported.grade, 'C');
        assert!(!imported.required_evidence_measured);
    }

    #[test]
    fn worse_health_grade_is_never_improved_by_evidence_cap() {
        let mut result = healthy_result();
        result.dead_chip_addresses = vec![1, 2, 3, 4, 5, 6];
        result.voltage_evidence = DiagnosticEvidence::commanded(13.7, "setpoint", None);
        result.calculate_grade();
        assert_eq!(result.grade, 'D');
    }
}
