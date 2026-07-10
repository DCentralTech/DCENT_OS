//! Silicon quality analytics and reporting.
//!
//! Generates a comprehensive silicon quality report from tuning profiles,
//! including quality scoring, grade distribution, frequency statistics,
//! and per-chain breakdowns. Used by the API and dashboard to display
//! miner silicon health at a glance.

use crate::profile::{ChipGrade, TuningProfile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Silicon quality report for a miner (all chains).
///
/// All fields are PURE TELEMETRY derived from the saved tuning profiles. The
/// report drives no hardware — it only reads per-chip data the autotuner /
/// chain already captured (freq-bin, error-rate, nonce count) and buckets it
/// into A/B/C/D for operators and fleet tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiliconReport {
    /// Whether any chip in the report carries real measured characterization
    /// data (at least one chip with `nonces_counted > 0`). When `false`, the
    /// report reflects profiles that exist on disk but have no measured nonce
    /// data yet — grades are NOT fabricated and the tier is "Not Characterized".
    /// Operators / fleet tools should treat a `false` report as "run tuning
    /// first" rather than as a silicon-quality verdict.
    #[serde(default = "default_true")]
    pub characterized: bool,
    /// Count of chips that have no measured nonce data yet
    /// (`nonces_counted == 0`). These are EXCLUDED from the grade distribution
    /// and quality score so an un-measured chip is never reported as a weak
    /// (grade D) chip. A high count here means tuning hasn't fully run.
    #[serde(default)]
    pub not_characterized_chips: u16,
    /// Overall silicon quality score (0-100).
    pub quality_score: f64,
    /// Quality tier: "Excellent", "Good", "Average", "Below Average", "Poor",
    /// or "Not Characterized" (when `characterized == false`).
    pub quality_tier: String,
    /// Total chips analyzed (includes not-yet-characterized chips).
    pub total_chips: u16,
    /// Grade distribution counts.
    ///
    /// These reflect the EFFECTIVE grade — the frequency-bin grade refined by
    /// measured error-rate and nonce count (see [`refine_grade`]). A chip that
    /// clocks high but has a poor error rate is demoted here even though its
    /// stored freq-bin grade was higher.
    pub grade_a_count: u16,
    pub grade_b_count: u16,
    pub grade_c_count: u16,
    pub grade_d_count: u16,
    /// Grade distribution percentages.
    pub grade_a_pct: f64,
    pub grade_b_pct: f64,
    pub grade_c_pct: f64,
    pub grade_d_pct: f64,
    /// Frequency statistics (across all chips, all chains).
    pub avg_max_stable_mhz: f64,
    pub best_chip_mhz: u16,
    pub worst_chip_mhz: u16,
    pub frequency_std_dev_mhz: f64,
    /// Per-chain breakdown.
    pub chain_reports: Vec<ChainSiliconReport>,
    /// Notable chips: top 5 by max_stable_mhz.
    pub top_5_chips: Vec<ChipRanking>,
    /// Notable chips: bottom 5 by max_stable_mhz.
    pub bottom_5_chips: Vec<ChipRanking>,
}

/// serde default for the `characterized` field so old serialized reports
/// (which predate the field) deserialize as characterized.
fn default_true() -> bool {
    true
}

/// Per-chain silicon quality breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSiliconReport {
    /// Chain ID.
    pub chain_id: u8,
    /// Number of chips on this chain.
    pub chip_count: u8,
    /// Chain-level quality score (same formula as overall).
    pub quality_score: f64,
    /// Average max stable frequency for this chain.
    pub avg_max_stable_mhz: f64,
    /// Grade distribution: [A, B, C, D].
    pub grade_distribution: [u16; 4],
}

/// A chip's ranking entry (for top/bottom lists).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipRanking {
    /// Which chain this chip is on.
    pub chain_id: u8,
    /// Chip index on the chain.
    pub chip_index: u8,
    /// Max stable frequency achieved during tuning.
    pub max_stable_mhz: u16,
    /// Silicon grade as stored in the tuning profile (frequency-bin only).
    /// Kept for backwards compatibility with existing consumers.
    pub grade: ChipGrade,
    /// Effective grade: the stored freq-bin grade refined by measured
    /// error-rate and nonce count. This is the grade counted in the report's
    /// distribution. Equal to `grade` for a clean, well-characterized chip;
    /// demoted when the chip's error rate is high.
    #[serde(default = "default_effective_grade")]
    pub effective_grade: ChipGrade,
    /// Measured error rate during characterization (fraction, not percent).
    #[serde(default)]
    pub error_rate: f64,
    /// Total nonces counted during characterization. `0` means the chip was
    /// never measured (not characterized) — its grade is not trustworthy.
    #[serde(default)]
    pub nonces_counted: u64,
    /// Whether this chip has real measured nonce data
    /// (`nonces_counted > 0`).
    #[serde(default = "default_true")]
    pub characterized: bool,
}

/// serde default for `ChipRanking::effective_grade` (old reports without the
/// field fall back to grade B — a neutral mid grade rather than a fabricated
/// extreme).
fn default_effective_grade() -> ChipGrade {
    ChipGrade::B
}

/// Minimum nonce count for a chip to be considered characterized. Below this
/// the binary search did not gather enough samples to trust the grade, so the
/// chip is reported as not-characterized rather than graded.
const MIN_NONCES_FOR_CHARACTERIZED: u64 = 1;

/// Error-rate thresholds for grade demotion. These mirror the autotuner's own
/// stability expectations: a well-tuned chip sits well under 1% hardware error.
/// PURE TELEMETRY — demotion only ever LOWERS a reported grade, it never raises
/// a grade and never feeds back into any frequency/voltage/fan command.
const ERROR_RATE_DEMOTE_ONE_GRADE: f64 = 0.02; // >2% error → one grade worse
const ERROR_RATE_DEMOTE_TO_D: f64 = 0.10; // >10% error → grade D regardless of clock

/// Refine a chip's stored frequency-bin grade using measured stability data.
///
/// The stored [`ChipGrade`] is computed purely from max-stable frequency vs
/// nominal (see `binary_search::grade_chip`). That ignores how cleanly the chip
/// actually hashed. LuxOS / BraiinsOS chip-quality reporting factors error rate;
/// this brings DCENT_OS to parity by demoting (never promoting) the grade when:
/// - the chip has no measured nonces → returns `None` (not characterized), or
/// - the chip's error rate is high → drops one grade (>2%) or to D (>10%).
///
/// Returns `None` when the chip cannot be meaningfully graded (no measured
/// nonce data). Callers must surface that as "not characterized", never as a
/// fabricated grade.
fn refine_grade(stored: ChipGrade, error_rate: f64, nonces_counted: u64) -> Option<ChipGrade> {
    if nonces_counted < MIN_NONCES_FOR_CHARACTERIZED {
        return None;
    }
    // Severe error rate floors the grade at D regardless of clock speed.
    if error_rate > ERROR_RATE_DEMOTE_TO_D {
        return Some(ChipGrade::D);
    }
    // Elevated error rate drops the grade by one bucket.
    let refined = if error_rate > ERROR_RATE_DEMOTE_ONE_GRADE {
        demote_one(stored)
    } else {
        stored
    };
    Some(refined)
}

/// Lower a grade by one bucket (A→B→C→D, D stays D). Never promotes.
fn demote_one(grade: ChipGrade) -> ChipGrade {
    match grade {
        ChipGrade::A => ChipGrade::B,
        ChipGrade::B => ChipGrade::C,
        ChipGrade::C => ChipGrade::D,
        ChipGrade::D => ChipGrade::D,
    }
}

/// Map a grade to its distribution index ([A, B, C, D]).
fn grade_index(grade: ChipGrade) -> usize {
    match grade {
        ChipGrade::A => 0,
        ChipGrade::B => 1,
        ChipGrade::C => 2,
        ChipGrade::D => 3,
    }
}

impl SiliconReport {
    /// Generate a silicon quality report from tuning profiles.
    ///
    /// Analyzes all chips across all chains to produce quality scores,
    /// grade distributions, frequency statistics, and notable chip lists.
    pub fn generate(profiles: &HashMap<u8, TuningProfile>) -> Self {
        // Per-chip ranking record carrying the data the report exposes.
        // (chain_id, chip_index, max_stable_mhz, stored_grade, effective_grade,
        //  error_rate, nonces_counted, characterized)
        let mut all_chips: Vec<ChipRanking> = Vec::new();
        // Distribution over the EFFECTIVE grade (freq-bin refined by error
        // rate / nonce count). Not-characterized chips are excluded.
        let mut grade_counts = [0u16; 4]; // [A, B, C, D]
        let mut not_characterized_chips = 0u16;

        let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
        chain_ids.sort();

        let mut chain_reports = Vec::new();

        for &chain_id in &chain_ids {
            let tp = &profiles[&chain_id];
            let mut chain_grades = [0u16; 4];
            let mut chain_freq_sum = 0.0f64;
            // Frequencies of CHARACTERIZED chips on this chain (for the
            // chain-level quality score / uniformity bonus).
            let mut chain_characterized_freqs: Vec<u16> = Vec::new();

            for chip in &tp.chips {
                let freq = chip.max_stable_mhz;
                chain_freq_sum += freq as f64;

                // Refine the stored freq-bin grade with measured stability.
                // `None` means the chip has no measured nonce data → it is
                // reported as not-characterized, NOT graded.
                let effective = refine_grade(chip.grade, chip.error_rate, chip.nonces_counted);
                let characterized = effective.is_some();

                if let Some(eff_grade) = effective {
                    let idx = grade_index(eff_grade);
                    grade_counts[idx] += 1;
                    chain_grades[idx] += 1;
                    chain_characterized_freqs.push(freq);
                } else {
                    not_characterized_chips += 1;
                }

                all_chips.push(ChipRanking {
                    chain_id,
                    chip_index: chip.chip_index,
                    max_stable_mhz: freq,
                    grade: chip.grade,
                    // For a not-characterized chip there is no trustworthy
                    // effective grade; mirror the stored grade for display
                    // but flag `characterized = false` so consumers don't
                    // treat it as a quality verdict.
                    effective_grade: effective.unwrap_or(chip.grade),
                    error_rate: chip.error_rate,
                    nonces_counted: chip.nonces_counted,
                    characterized,
                });
            }

            let chain_chip_count = tp.chips.len() as u8;
            let chain_characterized = chain_grades.iter().sum::<u16>();
            let chain_avg = if chain_chip_count > 0 {
                chain_freq_sum / chain_chip_count as f64
            } else {
                0.0
            };
            let chain_score = compute_quality_score(
                &chain_grades,
                chain_characterized,
                &chain_characterized_freqs,
            );

            chain_reports.push(ChainSiliconReport {
                chain_id,
                chip_count: chain_chip_count,
                quality_score: chain_score,
                avg_max_stable_mhz: chain_avg,
                grade_distribution: chain_grades,
            });
        }

        let total_chips = all_chips.len() as u16;
        let characterized_chips: u16 = grade_counts.iter().sum();
        let characterized = characterized_chips > 0;

        // Frequency statistics (over CHARACTERIZED chips only — an un-measured
        // chip's stored 0 MHz must not skew best/worst/avg).
        let all_freqs: Vec<u16> = all_chips
            .iter()
            .filter(|c| c.characterized)
            .map(|c| c.max_stable_mhz)
            .collect();
        let avg_max_stable = if !all_freqs.is_empty() {
            all_freqs.iter().map(|&f| f as f64).sum::<f64>() / all_freqs.len() as f64
        } else {
            0.0
        };
        let best_chip = all_freqs.iter().copied().max().unwrap_or(0);
        let worst_chip = all_freqs.iter().copied().min().unwrap_or(0);
        let std_dev = compute_std_dev(&all_freqs);

        // Quality score + tier. When nothing is characterized yet, do NOT
        // fabricate a "Poor" verdict — report the not-characterized state.
        let quality_score = compute_quality_score(&grade_counts, characterized_chips, &all_freqs);
        let quality_tier = if characterized {
            quality_tier(quality_score)
        } else {
            "Not Characterized".to_string()
        };

        // Grade percentages are over CHARACTERIZED chips (so they sum to 100%
        // among graded chips; not-characterized chips are tracked separately).
        let total_f = if characterized_chips > 0 {
            characterized_chips as f64
        } else {
            1.0
        };

        // Top 5 and bottom 5 by max_stable_mhz, CHARACTERIZED chips only.
        let mut ranked: Vec<ChipRanking> = all_chips
            .iter()
            .filter(|c| c.characterized)
            .cloned()
            .collect();

        ranked.sort_by(|a, b| {
            b.max_stable_mhz
                .cmp(&a.max_stable_mhz)
                .then_with(|| a.chain_id.cmp(&b.chain_id))
                .then_with(|| a.chip_index.cmp(&b.chip_index))
        });
        let top_5: Vec<ChipRanking> = ranked.iter().take(5).cloned().collect();

        ranked.sort_by(|a, b| {
            a.max_stable_mhz
                .cmp(&b.max_stable_mhz)
                .then_with(|| a.chain_id.cmp(&b.chain_id))
                .then_with(|| a.chip_index.cmp(&b.chip_index))
        });
        let bottom_5: Vec<ChipRanking> = ranked.iter().take(5).cloned().collect();

        SiliconReport {
            characterized,
            not_characterized_chips,
            quality_score,
            quality_tier,
            total_chips,
            grade_a_count: grade_counts[0],
            grade_b_count: grade_counts[1],
            grade_c_count: grade_counts[2],
            grade_d_count: grade_counts[3],
            grade_a_pct: grade_counts[0] as f64 / total_f * 100.0,
            grade_b_pct: grade_counts[1] as f64 / total_f * 100.0,
            grade_c_pct: grade_counts[2] as f64 / total_f * 100.0,
            grade_d_pct: grade_counts[3] as f64 / total_f * 100.0,
            avg_max_stable_mhz: avg_max_stable,
            best_chip_mhz: best_chip,
            worst_chip_mhz: worst_chip,
            frequency_std_dev_mhz: std_dev,
            chain_reports,
            top_5_chips: top_5,
            bottom_5_chips: bottom_5,
        }
    }
}

/// Compute quality score from grade distribution.
///
/// Formula:
/// - Base: weighted sum of grades (A=100, B=75, C=50, D=25) / total_chips
/// - Bonus: +5 if std_dev < 20 MHz (uniform silicon), -5 if std_dev > 50 MHz
/// - Clamped to 0-100
fn compute_quality_score(grade_counts: &[u16; 4], total_chips: u16, freqs: &[u16]) -> f64 {
    if total_chips == 0 {
        return 0.0;
    }

    let weighted_sum = grade_counts[0] as f64 * 100.0
        + grade_counts[1] as f64 * 75.0
        + grade_counts[2] as f64 * 50.0
        + grade_counts[3] as f64 * 25.0;

    let mut score = weighted_sum / total_chips as f64;

    // Uniformity bonus/penalty
    let std_dev = compute_std_dev(freqs);
    if std_dev < 20.0 {
        score += 5.0;
    } else if std_dev > 50.0 {
        score -= 5.0;
    }

    score.clamp(0.0, 100.0)
}

/// Determine quality tier from score.
fn quality_tier(score: f64) -> String {
    if score >= 85.0 {
        "Excellent".to_string()
    } else if score >= 70.0 {
        "Good".to_string()
    } else if score >= 55.0 {
        "Average".to_string()
    } else if score >= 40.0 {
        "Below Average".to_string()
    } else {
        "Poor".to_string()
    }
}

/// Compute population standard deviation of frequency values.
fn compute_std_dev(freqs: &[u16]) -> f64 {
    if freqs.is_empty() {
        return 0.0;
    }
    let n = freqs.len() as f64;
    let mean = freqs.iter().map(|&f| f as f64).sum::<f64>() / n;
    let variance = freqs
        .iter()
        .map(|&f| (f as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    variance.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ChipGrade, ChipProfile, TuningProfile};

    fn make_chip(index: u8, max_stable: u16, operating: u16, grade: ChipGrade) -> ChipProfile {
        ChipProfile {
            chip_index: index,
            max_stable_mhz: max_stable,
            operating_mhz: operating,
            grade,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }
    }

    /// Full builder allowing per-chip error_rate / nonces_counted so the
    /// effective-grade refinement and not-characterized state can be tested.
    fn make_chip_full(
        index: u8,
        max_stable: u16,
        grade: ChipGrade,
        error_rate: f64,
        nonces_counted: u64,
    ) -> ChipProfile {
        ChipProfile {
            chip_index: index,
            max_stable_mhz: max_stable,
            operating_mhz: max_stable,
            grade,
            error_rate,
            nonces_counted,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }
    }

    /// Wrap a chip vec into a single-chain profile for report generation.
    fn profile_from_chips(chain_id: u8, chips: Vec<ChipProfile>) -> HashMap<u8, TuningProfile> {
        let stats = TuningProfile::compute_stats(&chips, 15.0);
        let chip_count = chips.len() as u8;
        let mut profiles = HashMap::new();
        profiles.insert(
            chain_id,
            TuningProfile {
                version: 1,
                chip_type: "BM1362".to_string(),
                chain_id,
                chip_count,
                voltage_mv: 13700,
                tuned_at: "1710000000".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: None,
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: None,
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips,
                stats,
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );
        profiles
    }

    fn make_test_profiles() -> HashMap<u8, TuningProfile> {
        let mut profiles = HashMap::new();

        // Chain 6: mostly grade A
        let chips_6 = vec![
            make_chip(0, 700, 665, ChipGrade::A),
            make_chip(1, 690, 655, ChipGrade::A),
            make_chip(2, 680, 646, ChipGrade::A),
            make_chip(3, 620, 589, ChipGrade::B),
        ];
        let stats_6 = TuningProfile::compute_stats(&chips_6, 15.0);
        profiles.insert(
            6,
            TuningProfile {
                version: 1,
                chip_type: "BM1387".to_string(),
                chain_id: 6,
                chip_count: 4,
                voltage_mv: 9100,
                tuned_at: "1710000000".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: None,
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: None,
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips: chips_6,
                stats: stats_6,
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        // Chain 7: mixed grades
        let chips_7 = vec![
            make_chip(0, 650, 617, ChipGrade::B),
            make_chip(1, 550, 522, ChipGrade::C),
            make_chip(2, 500, 475, ChipGrade::D),
            make_chip(3, 680, 646, ChipGrade::A),
        ];
        let stats_7 = TuningProfile::compute_stats(&chips_7, 15.0);
        profiles.insert(
            7,
            TuningProfile {
                version: 1,
                chip_type: "BM1387".to_string(),
                chain_id: 7,
                chip_count: 4,
                voltage_mv: 9100,
                tuned_at: "1710000000".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: None,
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: None,
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips: chips_7,
                stats: stats_7,
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        profiles
    }

    #[test]
    fn test_quality_score_all_grade_a() {
        let grades = [4, 0, 0, 0];
        let freqs = vec![700, 695, 690, 705]; // low std dev
        let score = compute_quality_score(&grades, 4, &freqs);
        // Base: 100.0, std_dev ~5.6 < 20 -> +5 = 100 (clamped)
        assert!(score >= 100.0, "Score {} should be 100", score);
    }

    #[test]
    fn test_quality_score_all_grade_d() {
        let grades = [0, 0, 0, 4];
        let freqs = vec![400, 390, 410, 405];
        let score = compute_quality_score(&grades, 4, &freqs);
        // Base: 25.0, std_dev ~7.1 < 20 -> +5 = 30
        assert!((score - 30.0).abs() < 1.0, "Score {} should be ~30", score);
    }

    #[test]
    fn test_quality_score_mixed() {
        let grades = [1, 1, 1, 1];
        let freqs = vec![700, 600, 500, 400]; // high std_dev
        let score = compute_quality_score(&grades, 4, &freqs);
        // Base: (100+75+50+25)/4 = 62.5, std_dev ~111.8 > 50 -> -5 = 57.5
        assert!(
            (score - 57.5).abs() < 1.0,
            "Score {} should be ~57.5",
            score
        );
    }

    #[test]
    fn test_quality_tiers() {
        assert_eq!(quality_tier(90.0), "Excellent");
        assert_eq!(quality_tier(85.0), "Excellent");
        assert_eq!(quality_tier(75.0), "Good");
        assert_eq!(quality_tier(70.0), "Good");
        assert_eq!(quality_tier(60.0), "Average");
        assert_eq!(quality_tier(55.0), "Average");
        assert_eq!(quality_tier(45.0), "Below Average");
        assert_eq!(quality_tier(40.0), "Below Average");
        assert_eq!(quality_tier(30.0), "Poor");
        assert_eq!(quality_tier(0.0), "Poor");
    }

    #[test]
    fn test_generate_report() {
        let profiles = make_test_profiles();
        let report = SiliconReport::generate(&profiles);

        assert_eq!(report.total_chips, 8);
        assert_eq!(report.grade_a_count, 4);
        assert_eq!(report.grade_b_count, 2);
        assert_eq!(report.grade_c_count, 1);
        assert_eq!(report.grade_d_count, 1);

        // Percentages should sum to ~100%
        let pct_sum =
            report.grade_a_pct + report.grade_b_pct + report.grade_c_pct + report.grade_d_pct;
        assert!(
            (pct_sum - 100.0).abs() < 0.01,
            "Percentages sum {} != 100",
            pct_sum
        );

        // Best chip should be 700, worst should be 500
        assert_eq!(report.best_chip_mhz, 700);
        assert_eq!(report.worst_chip_mhz, 500);

        // Chain reports
        assert_eq!(report.chain_reports.len(), 2);
        assert_eq!(report.chain_reports[0].chain_id, 6);
        assert_eq!(report.chain_reports[1].chain_id, 7);

        // Top/bottom chips
        assert!(!report.top_5_chips.is_empty());
        assert_eq!(report.top_5_chips[0].max_stable_mhz, 700);
        assert!(!report.bottom_5_chips.is_empty());
        assert_eq!(report.bottom_5_chips[0].max_stable_mhz, 500);

        // Quality tier should be reasonable
        assert!(!report.quality_tier.is_empty());
    }

    #[test]
    fn test_generate_empty_profiles() {
        let profiles: HashMap<u8, TuningProfile> = HashMap::new();
        let report = SiliconReport::generate(&profiles);

        assert_eq!(report.total_chips, 0);
        assert_eq!(report.quality_score, 0.0);
        // No profiles at all → nothing characterized → must NOT fabricate a
        // "Poor" silicon verdict.
        assert!(!report.characterized);
        assert_eq!(report.quality_tier, "Not Characterized");
        assert_eq!(report.not_characterized_chips, 0);
        assert!(report.chain_reports.is_empty());
        assert!(report.top_5_chips.is_empty());
        assert!(report.bottom_5_chips.is_empty());
    }

    #[test]
    fn test_std_dev() {
        // All same value -> std_dev = 0
        assert_eq!(compute_std_dev(&[600, 600, 600]), 0.0);

        // Known values
        let freqs = vec![600, 700];
        let sd = compute_std_dev(&freqs);
        assert!((sd - 50.0).abs() < 0.1, "StdDev {} should be 50", sd);

        // Empty
        assert_eq!(compute_std_dev(&[]), 0.0);
    }

    #[test]
    fn test_top_bottom_ordering() {
        let profiles = make_test_profiles();
        let report = SiliconReport::generate(&profiles);

        // Top 5 should be sorted descending by max_stable_mhz
        for i in 1..report.top_5_chips.len() {
            assert!(
                report.top_5_chips[i - 1].max_stable_mhz >= report.top_5_chips[i].max_stable_mhz
            );
        }

        // Bottom 5 should be sorted ascending by max_stable_mhz
        for i in 1..report.bottom_5_chips.len() {
            assert!(
                report.bottom_5_chips[i - 1].max_stable_mhz
                    <= report.bottom_5_chips[i].max_stable_mhz
            );
        }
    }

    #[test]
    fn test_report_serialization() {
        let profiles = make_test_profiles();
        let report = SiliconReport::generate(&profiles);

        let json = serde_json::to_string_pretty(&report).expect("serialize failed");
        let deserialized: SiliconReport = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.total_chips, report.total_chips);
        assert_eq!(deserialized.quality_tier, report.quality_tier);
    }

    // --- refine_grade unit tests (freq-bin + error-rate + nonce-rate) ---

    #[test]
    fn test_refine_grade_clean_chip_keeps_grade() {
        // Low error, plenty of nonces → effective grade == stored grade.
        assert_eq!(refine_grade(ChipGrade::A, 0.001, 100), Some(ChipGrade::A));
        assert_eq!(refine_grade(ChipGrade::B, 0.0, 50), Some(ChipGrade::B));
    }

    #[test]
    fn test_refine_grade_elevated_error_demotes_one() {
        // >2% error → one grade worse, never promoted.
        assert_eq!(refine_grade(ChipGrade::A, 0.03, 100), Some(ChipGrade::B));
        assert_eq!(refine_grade(ChipGrade::B, 0.05, 100), Some(ChipGrade::C));
        assert_eq!(refine_grade(ChipGrade::C, 0.03, 100), Some(ChipGrade::D));
        assert_eq!(refine_grade(ChipGrade::D, 0.03, 100), Some(ChipGrade::D));
    }

    #[test]
    fn test_refine_grade_severe_error_floors_to_d() {
        // >10% error → grade D regardless of how high it clocked.
        assert_eq!(refine_grade(ChipGrade::A, 0.20, 100), Some(ChipGrade::D));
    }

    #[test]
    fn test_refine_grade_no_nonces_is_not_characterized() {
        // Zero measured nonces → cannot grade → None (not characterized).
        assert_eq!(refine_grade(ChipGrade::A, 0.0, 0), None);
        assert_eq!(refine_grade(ChipGrade::D, 0.0, 0), None);
    }

    // --- report-level: not-characterized state never fabricates grades ---

    #[test]
    fn test_report_all_uncharacterized_reports_not_characterized() {
        // A profile exists on disk but no chip has measured nonces yet.
        let chips = vec![
            make_chip_full(0, 0, ChipGrade::D, 0.0, 0),
            make_chip_full(1, 0, ChipGrade::D, 0.0, 0),
        ];
        let profiles = profile_from_chips(6, chips);
        let report = SiliconReport::generate(&profiles);

        assert!(!report.characterized);
        assert_eq!(report.quality_tier, "Not Characterized");
        assert_eq!(report.total_chips, 2);
        assert_eq!(report.not_characterized_chips, 2);
        // No grades fabricated.
        assert_eq!(report.grade_a_count, 0);
        assert_eq!(report.grade_b_count, 0);
        assert_eq!(report.grade_c_count, 0);
        assert_eq!(report.grade_d_count, 0);
        // No fabricated frequency statistics from un-measured chips.
        assert_eq!(report.avg_max_stable_mhz, 0.0);
        assert_eq!(report.best_chip_mhz, 0);
        assert_eq!(report.worst_chip_mhz, 0);
        // Un-measured chips are excluded from rankings.
        assert!(report.top_5_chips.is_empty());
        assert!(report.bottom_5_chips.is_empty());
    }

    #[test]
    fn test_report_excludes_uncharacterized_from_distribution() {
        // 2 characterized A chips + 1 not-yet-characterized chip.
        let chips = vec![
            make_chip_full(0, 700, ChipGrade::A, 0.001, 100),
            make_chip_full(1, 695, ChipGrade::A, 0.001, 100),
            make_chip_full(2, 0, ChipGrade::D, 0.0, 0), // never measured
        ];
        let profiles = profile_from_chips(6, chips);
        let report = SiliconReport::generate(&profiles);

        assert!(report.characterized);
        assert_eq!(report.total_chips, 3);
        assert_eq!(report.not_characterized_chips, 1);
        // Only the 2 measured chips count toward the distribution.
        assert_eq!(report.grade_a_count, 2);
        assert_eq!(report.grade_d_count, 0);
        // Grade percentages are over characterized chips → A == 100%.
        assert!((report.grade_a_pct - 100.0).abs() < 0.01);
        // Frequency stats over characterized chips only.
        assert_eq!(report.best_chip_mhz, 700);
        assert_eq!(report.worst_chip_mhz, 695);
        // Rankings exclude the un-measured chip.
        assert_eq!(report.top_5_chips.len(), 2);
        assert!(report.top_5_chips.iter().all(|c| c.characterized));
    }

    #[test]
    fn test_report_error_rate_demotes_grade_in_distribution() {
        // Chip clocked high (stored grade A) but hashes dirty (5% error)
        // → effective grade B in the distribution.
        let chips = vec![
            make_chip_full(0, 700, ChipGrade::A, 0.05, 100), // dirty → B
            make_chip_full(1, 700, ChipGrade::A, 0.001, 100), // clean → A
        ];
        let profiles = profile_from_chips(6, chips);
        let report = SiliconReport::generate(&profiles);

        assert_eq!(report.grade_a_count, 1, "only the clean chip stays A");
        assert_eq!(report.grade_b_count, 1, "dirty chip demoted to B");
        // Both still characterized; both surfaced in rankings with telemetry.
        let dirty = report
            .top_5_chips
            .iter()
            .chain(report.bottom_5_chips.iter())
            .find(|c| c.chip_index == 0)
            .expect("dirty chip present");
        assert_eq!(dirty.grade, ChipGrade::A, "stored freq-bin grade preserved");
        assert_eq!(
            dirty.effective_grade,
            ChipGrade::B,
            "effective grade demoted"
        );
        assert!((dirty.error_rate - 0.05).abs() < 1e-9);
        assert_eq!(dirty.nonces_counted, 100);
    }

    #[test]
    fn test_report_grade_bucketing_from_sample_stats() {
        // Sample per-chip stats spanning every freq bin (nominal ~650):
        // 720 → A (>= +50), 660 → B (within +/-25), 580 → C, 500 → D.
        let chips = vec![
            make_chip_full(0, 720, ChipGrade::A, 0.001, 200),
            make_chip_full(1, 660, ChipGrade::B, 0.001, 200),
            make_chip_full(2, 580, ChipGrade::C, 0.001, 200),
            make_chip_full(3, 500, ChipGrade::D, 0.001, 200),
        ];
        let profiles = profile_from_chips(7, chips);
        let report = SiliconReport::generate(&profiles);

        assert_eq!(report.grade_a_count, 1);
        assert_eq!(report.grade_b_count, 1);
        assert_eq!(report.grade_c_count, 1);
        assert_eq!(report.grade_d_count, 1);
        assert!(report.characterized);
        assert_eq!(report.not_characterized_chips, 0);
        let pct_sum =
            report.grade_a_pct + report.grade_b_pct + report.grade_c_pct + report.grade_d_pct;
        assert!((pct_sum - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_api_shape_includes_new_telemetry_fields() {
        // Verify the JSON the API returns carries the new read-only telemetry
        // fields (consumed by dashboard + fleet tools).
        let profiles = make_test_profiles();
        let report = SiliconReport::generate(&profiles);
        let v: serde_json::Value =
            serde_json::to_value(&report).expect("serialize to value failed");

        assert!(v.get("characterized").is_some());
        assert!(v.get("not_characterized_chips").is_some());
        assert!(v["characterized"].as_bool().unwrap());
        // Per-chip rankings expose effective grade + measured telemetry.
        let first = &v["top_5_chips"][0];
        assert!(first.get("effective_grade").is_some());
        assert!(first.get("error_rate").is_some());
        assert!(first.get("nonces_counted").is_some());
        assert!(first.get("characterized").is_some());
    }

    #[test]
    fn test_old_report_json_deserializes_with_defaults() {
        // A report serialized BEFORE the new fields existed must still parse
        // (serde defaults keep the API contract backwards compatible).
        let old_json = r#"{
            "quality_score": 80.0,
            "quality_tier": "Good",
            "total_chips": 1,
            "grade_a_count": 1, "grade_b_count": 0, "grade_c_count": 0, "grade_d_count": 0,
            "grade_a_pct": 100.0, "grade_b_pct": 0.0, "grade_c_pct": 0.0, "grade_d_pct": 0.0,
            "avg_max_stable_mhz": 700.0, "best_chip_mhz": 700, "worst_chip_mhz": 700,
            "frequency_std_dev_mhz": 0.0,
            "chain_reports": [],
            "top_5_chips": [{"chain_id":6,"chip_index":0,"max_stable_mhz":700,"grade":"A"}],
            "bottom_5_chips": []
        }"#;
        let report: SiliconReport =
            serde_json::from_str(old_json).expect("old report should deserialize");
        // Missing `characterized` defaults to true; missing per-chip
        // effective_grade defaults to B.
        assert!(report.characterized);
        assert_eq!(report.not_characterized_chips, 0);
        assert_eq!(report.top_5_chips[0].effective_grade, ChipGrade::B);
        assert!(report.top_5_chips[0].characterized);
    }
}
