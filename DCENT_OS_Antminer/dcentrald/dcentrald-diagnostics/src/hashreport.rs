//! HashReport -- 15-minute test drive diagnostic.
//!
//! The flagship diagnostic. A reseller plugs in a miner, starts HashReport,
//! and 15 minutes later has a complete health report. No pool configuration
//! needed -- HashReport uses an internal test pool or solo mining.
//!
//! Phases:
//!   1. System Identification (10 seconds)
//!   2. Baseline Capture (30 seconds)
//!   3. Mining Performance (12 minutes, 12 x 60s windows)
//!   4. Per-Chip Health Scoring (2 minutes)
//!   5. Report Generation (20 seconds)

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::evidence::DiagnosticEvidence;

/// HashReport test phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashReportPhase {
    /// Phase 1: Read serial, MAC, chip type, FPGA version, board count.
    SystemIdentification,
    /// Phase 2: Read temperatures, fan speed, PSU, voltage, CRC baseline.
    BaselineCapture,
    /// Phase 3: 12 x 60s mining windows, count nonces per chip.
    MiningPerformance,
    /// Phase 4: Calculate per-chip health scores and grades.
    ChipHealthScoring,
    /// Phase 5: Aggregate results, generate HTML report.
    ReportGeneration,
}

/// System identification data (Phase 1 output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Miner serial number (from EEPROM or MAC-derived).
    pub serial: String,
    /// MAC address.
    pub mac: String,
    /// Miner model (e.g., "Antminer S9").
    pub model: String,
    /// ASIC chip type (e.g., "BM1387").
    pub chip_type: String,
    /// Chip ID hex (e.g., "0x1387").
    pub chip_id: String,
    /// FPGA version (e.g., "0x00901002").
    pub fpga_version: String,
    /// Number of hash boards detected.
    pub board_count: u8,
    /// Total chips across all boards.
    pub total_chips: u16,
    /// Control board type (e.g., "Zynq C55").
    pub control_board: String,
}

/// Baseline snapshot (Phase 2 output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineSnapshot {
    /// Per-chain temperatures in celsius.
    pub temperatures_c: Vec<f32>,
    /// Fan RPM at baseline.
    pub fan_rpm: u32,
    /// Fan PWM at baseline.
    pub fan_pwm: u8,
    /// Per-chain voltages in volts.
    pub voltages_v: Vec<f32>,
    /// Per-chain CRC error baseline counts.
    pub crc_baseline: Vec<u32>,
}

/// One 60-second performance measurement window (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowData {
    /// Window index (0-11).
    pub window_index: u8,
    /// Per-chip nonce counts for this window.
    pub chip_nonces: Vec<u32>,
    /// Per-chain CRC errors during this window.
    pub chain_crc_errors: Vec<u32>,
    /// Per-chain temperature at end of window.
    pub chain_temps_c: Vec<f32>,
    /// Total valid nonces in this window.
    pub total_nonces: u64,
}

/// Per-chip health score (Phase 4 output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipHealthScore {
    /// Chip index within the chain.
    pub index: u16,
    /// Chip address.
    pub address: u8,
    /// Health grade: A, B, C, D, or F.
    pub grade: char,
    /// Health score (0.0 to 1.0+).
    pub health_score: f32,
    /// Total nonces found across all windows.
    pub nonce_count: u64,
    /// Expected nonces based on frequency and difficulty.
    pub expected_nonces: u64,
    /// CRC errors attributed to this chip.
    pub crc_errors: u32,
    /// Current frequency in MHz.
    pub frequency_mhz: u16,
    /// Estimated hashrate in GH/s.
    pub hashrate_ghs: f32,
}

/// Complete HashReport result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashReport {
    /// Unique report identifier.
    pub report_id: Uuid,
    /// Report format version.
    pub report_version: String,
    /// ISO 8601 timestamp of report generation.
    pub generated_at: String,
    /// Total test duration in seconds.
    pub duration_seconds: u32,
    /// Honest labeling for timed drive vs snapshot report.
    pub report_kind: String,
    /// High-level data source used to build the report.
    pub source: String,
    /// Firmware version.
    pub firmware_version: String,
    /// System identification.
    pub system: SystemInfo,
    /// Baseline measurements.
    pub baseline: BaselineSnapshot,
    /// Performance windows data.
    pub windows: Vec<WindowData>,
    /// Per-board results with chip health.
    pub boards: Vec<BoardResult>,
    /// Overall unit grade: A, B, C, D, or F.
    pub unit_grade: char,
    /// Explanation of the grade.
    pub unit_grade_explanation: String,
    /// Generated warnings.
    pub warnings: Vec<String>,
    /// Recommendations for the user.
    pub recommendations: Vec<String>,
}

/// Per-board result within a HashReport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardResult {
    /// Chain ID (6, 7, or 8).
    pub chain_id: u8,
    /// Expected chip count.
    pub chips_expected: u8,
    /// Responding chip count.
    pub chips_responding: u8,
    /// Dead chip count (0 nonces across all windows).
    pub chips_dead: u8,
    /// Board hashrate in GH/s.
    pub hashrate_ghs: f32,
    /// Board voltage in volts.
    pub voltage_v: f32,
    /// Provenance for `voltage_v`.
    #[serde(default)]
    pub voltage_evidence: DiagnosticEvidence<f32>,
    /// Board temperature in celsius.
    pub temp_c: f32,
    /// Total CRC errors.
    pub crc_errors: u32,
    /// Provenance for the CRC observation/window.
    #[serde(default)]
    pub crc_evidence: DiagnosticEvidence<u32>,
    /// Board grade: A, B, C, D, or F.
    pub grade: char,
    /// Per-chip health scores.
    pub chips: Vec<ChipHealthScore>,
}

#[cfg(test)]
mod unit_grade_tests {
    use super::*;

    fn board(chips_dead: u8, grade: char) -> BoardResult {
        BoardResult {
            chain_id: 0,
            chips_expected: 108,
            chips_responding: 108u8.saturating_sub(chips_dead),
            chips_dead,
            hashrate_ghs: 0.0,
            voltage_v: 13.7,
            voltage_evidence: DiagnosticEvidence::measured(13.7, "rail_adc", Some(1)),
            temp_c: 60.0,
            crc_errors: 0,
            crc_evidence: DiagnosticEvidence::measured(0, "crc_window", Some(1)),
            grade,
            chips: Vec::new(),
        }
    }

    #[test]
    fn calculate_unit_grade_does_not_overflow_to_false_pass() {
        // 86+85+85 = 256 dead chips → wraps to 0 in a u8 accumulator. The OLD
        // code graded this all-'A'-labeled set 'A' (FALSE PASS on a dead unit);
        // the u32 accumulator grades it 'F' (256 > 3*6). (gap-swarm HAL-safety #7)
        let boards = vec![board(86, 'A'), board(85, 'A'), board(85, 'A')];
        assert_eq!(calculate_unit_grade(&boards), 'F');
    }

    #[test]
    fn calculate_unit_grade_is_monotonic_and_never_wraps_to_a_false_pass() {
        // Property (strengthens the single 256-case pin above): across dead-chip
        // counts sweeping PAST the u8 wrap points (256, 512), the unit grade must be
        // MONOTONICALLY non-improving — more dead chips can NEVER yield a better
        // grade. A regression of total_dead to a narrower type would wrap (256->0,
        // 324->68) and make a MORE-dead unit grade BETTER, which this monotonicity
        // check catches anywhere in the range (not just at 256). Three A-graded
        // boards, so the grade is driven purely by the dead count.
        let rank = |g: char| match g {
            'A' => 4,
            'B' => 3,
            'C' => 2,
            'D' => 1,
            _ => 0,
        };
        let mut prev = 5; // better than any real grade
        for dead in [0u8, 1, 2, 3, 50, 85, 86, 100, 170, 200, 255] {
            let boards = vec![board(dead, 'A'), board(dead, 'A'), board(dead, 'A')];
            let r = rank(calculate_unit_grade(&boards));
            assert!(
                r <= prev,
                "grade IMPROVED (rank {prev} -> {r}) as dead chips rose to {dead}/board — u8-wrap false-pass regression"
            );
            prev = r;
        }
        // 255*3 = 765 dead is unambiguously a dead unit -> worst grade.
        assert_eq!(
            calculate_unit_grade(&[board(255, 'A'), board(255, 'A'), board(255, 'A')]),
            'F'
        );
    }

    #[test]
    fn calculate_unit_grade_in_range_unchanged() {
        assert_eq!(calculate_unit_grade(&[]), 'F');
        assert_eq!(calculate_unit_grade(&[board(0, 'A')]), 'A');
        assert_eq!(calculate_unit_grade(&[board(1, 'A')]), 'B'); // 1 dead > 0 → B
    }
    #[test]
    fn commanded_or_inferred_board_evidence_cannot_produce_unit_pass() {
        let mut commanded = board(0, 'A');
        commanded.voltage_evidence = DiagnosticEvidence::commanded(13.7, "runtime_setpoint", None);
        assert_eq!(calculate_unit_grade(&[commanded]), 'C');

        let mut inferred = board(0, 'A');
        inferred.crc_evidence = DiagnosticEvidence::inferred(0, "cumulative_counter", None);
        assert_eq!(calculate_unit_grade(&[inferred]), 'C');
    }

    #[test]
    fn unavailable_legacy_evidence_is_not_silently_accepted() {
        let mut legacy = board(0, 'A');
        legacy.voltage_evidence = DiagnosticEvidence::default();
        legacy.crc_evidence = DiagnosticEvidence::default();
        assert_eq!(calculate_unit_grade(&[legacy]), 'C');
    }
}

/// Assign a health grade based on score.
pub fn score_to_grade(score: f32) -> char {
    if score >= 0.90 {
        'A'
    } else if score >= 0.75 {
        'B'
    } else if score >= 0.50 {
        'C'
    } else if score >= 0.25 {
        'D'
    } else {
        'F'
    }
}

/// Calculate overall unit grade from board grades.
pub fn calculate_unit_grade(boards: &[BoardResult]) -> char {
    if boards.is_empty() {
        return 'F';
    }

    // u32 accumulator: per-board chips_dead is u8 but a multi-board unit can have
    // far more than 255 dead chips total (3 boards × up to ~108-894 each). A u8
    // sum would overflow → panic in debug, or SILENTLY WRAP in release (256→0,
    // 324→68), yielding a BETTER grade than reality (false-pass on a dead unit) —
    // exactly the worst case this verdict exists to catch. (gap-swarm HAL-safety #7)
    let total_dead: u32 = boards.iter().map(|b| b.chips_dead as u32).sum();
    let worst_grade = boards.iter().map(|b| b.grade).max().unwrap_or('F');

    let health_grade = if worst_grade == 'F' || total_dead > boards.len() as u32 * 6 {
        'F'
    } else if worst_grade == 'D' || total_dead > 5 {
        'D'
    } else if worst_grade == 'C' || total_dead > 2 {
        'C'
    } else if worst_grade == 'B' || total_dead > 0 {
        'B'
    } else {
        'A'
    };

    let required_evidence_measured = boards.iter().all(|board| {
        board.voltage_v.is_finite()
            && board.voltage_evidence.is_measured_for(&board.voltage_v)
            && board.crc_evidence.is_measured_for(&board.crc_errors)
    });
    if !required_evidence_measured && matches!(health_grade, 'A' | 'B') {
        'C'
    } else {
        health_grade
    }
}
