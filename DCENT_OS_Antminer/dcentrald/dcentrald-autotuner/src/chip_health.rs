//! Per-chip health tracking for predictive failure detection.
//!
//! Tracks chip health over time using exponential moving averages (EMA) of
//! error rates, frequency back-off history, and trend analysis. Enables the
//! dashboard to show which chips are degrading and may need attention.

use crate::chip_stats::ChipStatsSnapshot;
use crate::profile::TuningProfile;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-chip health status snapshot for API/dashboard consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipHealthStatus {
    /// Chain this chip belongs to.
    pub chain_id: u8,
    /// Chip index on the chain.
    pub chip_index: u8,
    /// Current health score (0.0 = dead, 1.0 = perfect).
    pub health_score: f64,
    /// Health trend: positive = improving, negative = degrading.
    pub trend: f64,
    /// Estimated days until health drops below warning threshold (0.5).
    /// None if trend is stable or improving.
    pub estimated_days_to_warning: Option<f64>,
    /// Current error rate (rolling EMA, as percentage).
    pub error_rate_pct: f64,
    /// Current operating frequency.
    pub freq_mhz: u16,
    /// Number of back-offs applied by background monitor.
    pub backoff_count: u32,
    /// Hashrate ratio: actual nonces / expected nonces (EMA). 1.0 = perfect.
    /// Below min_hashrate_ratio indicates dead cores or stuck chip.
    pub hashrate_ratio: f64,
    /// Health classification.
    pub status: ChipHealthLevel,
}

/// Health level classification for a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChipHealthLevel {
    /// Healthy: error rate < 0.1%, no back-offs.
    Healthy,
    /// Watch: elevated error rate or recent back-off.
    Watch,
    /// Warning: sustained errors, may need attention.
    Warning,
    /// Critical: frequent back-offs, likely failing.
    Critical,
    /// Dead: producing no nonces.
    Dead,
}

impl std::fmt::Display for ChipHealthLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChipHealthLevel::Healthy => write!(f, "Healthy"),
            ChipHealthLevel::Watch => write!(f, "Watch"),
            ChipHealthLevel::Warning => write!(f, "Warning"),
            ChipHealthLevel::Critical => write!(f, "Critical"),
            ChipHealthLevel::Dead => write!(f, "Dead"),
        }
    }
}

/// Internal per-chip health tracking data.
struct ChipHealthData {
    /// Rolling average error rate (EMA), as a fraction (0.0-1.0).
    ema_error_rate: f64,
    /// Previous EMA for trend calculation.
    prev_ema_error_rate: f64,
    /// Count of back-offs applied by the background monitor.
    backoff_count: u32,
    /// Number of snapshots processed (for EMA warm-up).
    snapshot_count: u64,
    /// Current operating frequency (tracks back-offs).
    current_freq_mhz: u16,
    /// Original operating frequency (from tuning profile).
    profile_freq_mhz: u16,
    /// EMA of (actual_nonces / expected_nonces). 1.0 = perfect, <1.0 = deficit.
    /// Detects chips with stuck cores or intermittent resets.
    hashrate_ratio_ema: f64,
}

/// EMA smoothing factor. Higher = more weight on recent data.
/// 0.3 gives ~3-snapshot effective memory.
const EMA_ALPHA: f64 = 0.3;

/// Warning threshold for health score. Below this, a chip is considered
/// at risk and we estimate time-to-warning.
const WARNING_THRESHOLD: f64 = 0.5;

/// Hashrate-ratio classification thresholds (actual/expected nonces, EMA;
/// 1.0 = producing exactly the frequency-derived expectation).
///
/// These let `classify_health` downgrade a chip whose nonce output has
/// collapsed relative to its expectation — dead cores or an intermittently
/// resetting chip — even when its error rate and back-off count look clean.
/// They are an ADDITIVE signal and only ever escalate severity; they never
/// soften a verdict reached from error rate or back-offs.
///
/// Conservative by construction: the ratio EMA starts at 1.0 and is only
/// updated once a chip actually produces nonces (see `update`), so an idle or
/// not-yet-active chip stays at 1.0 and cannot trip these branches. The watch
/// threshold sits above the autotuner's default `min_hashrate_ratio` (0.7) so
/// health *reporting* surfaces a deficit before the back-off controller acts,
/// without changing any control behavior.
const HASHRATE_RATIO_WATCH: f64 = 0.75;
const HASHRATE_RATIO_WARNING: f64 = 0.5;
const HASHRATE_RATIO_CRITICAL: f64 = 0.25;

/// Tracks chip health over time for all chips across all chains.
pub struct ChipHealthTracker {
    /// Per-chip health data, keyed by (chain_id, chip_index).
    chips: HashMap<(u8, u8), ChipHealthData>,
    /// Snapshot interval in seconds (for time-to-warning extrapolation).
    snapshot_interval_s: f64,
    /// ASIC chip ID per chain for nonce rate calculations.
    chain_chip_ids: HashMap<u8, u16>,
}

impl ChipHealthTracker {
    /// Create a tracker initialized from tuning profiles.
    ///
    /// Each chip starts with a clean health record, using the profile's
    /// operating frequency as the baseline.
    pub fn new(profiles: &HashMap<u8, TuningProfile>) -> Self {
        let mut chain_chip_ids = HashMap::new();
        for profile in profiles.values() {
            let chip_id = crate::chip_id_from_type(&profile.chip_type).unwrap_or(0x1387);
            chain_chip_ids.insert(profile.chain_id, chip_id);
        }
        Self::new_with_chain_chip_ids(profiles, chain_chip_ids)
    }

    /// Create a tracker initialized from tuning profiles with a specific chip ID.
    pub fn new_with_chip_id(profiles: &HashMap<u8, TuningProfile>, chip_id: u16) -> Self {
        let chain_chip_ids = profiles.values().map(|tp| (tp.chain_id, chip_id)).collect();
        Self::new_with_chain_chip_ids(profiles, chain_chip_ids)
    }

    pub fn new_with_chain_chip_ids(
        profiles: &HashMap<u8, TuningProfile>,
        chain_chip_ids: HashMap<u8, u16>,
    ) -> Self {
        let mut chips = HashMap::new();

        for tp in profiles.values() {
            for chip in &tp.chips {
                chips.insert(
                    (tp.chain_id, chip.chip_index),
                    ChipHealthData {
                        ema_error_rate: 0.0,
                        prev_ema_error_rate: 0.0,
                        backoff_count: 0,
                        snapshot_count: 0,
                        current_freq_mhz: chip.operating_mhz,
                        profile_freq_mhz: chip.operating_mhz,
                        hashrate_ratio_ema: 1.0,
                    },
                );
            }
        }

        Self {
            chips,
            snapshot_interval_s: 60.0,
            chain_chip_ids,
        }
    }

    /// Update chip health with a new stats snapshot.
    ///
    /// Call this from the background monitor each time a ChipStatsSnapshot
    /// is received. Updates the EMA error rate and trend for each chip.
    pub fn update(&mut self, snapshot: &ChipStatsSnapshot) {
        self.snapshot_interval_s = snapshot.window_duration_s;

        let chip_count = snapshot.chip_nonces.len().min(snapshot.chip_errors.len());

        for chip_idx in 0..chip_count {
            let key = (snapshot.chain_id, chip_idx as u8);
            if let Some(data) = self.chips.get_mut(&key) {
                let nonces = snapshot.chip_nonces[chip_idx];
                let errors = snapshot.chip_errors[chip_idx];
                let total = nonces + errors;

                let error_rate = if total > 0 {
                    errors as f64 / total as f64
                } else {
                    // No activity at all — could be dead chip or just idle
                    // Keep previous EMA to avoid false positives
                    data.ema_error_rate
                };

                // Save previous EMA for trend calculation
                data.prev_ema_error_rate = data.ema_error_rate;

                // Update EMA
                if data.snapshot_count == 0 {
                    // First snapshot: initialize directly
                    data.ema_error_rate = error_rate;
                } else {
                    data.ema_error_rate =
                        EMA_ALPHA * error_rate + (1.0 - EMA_ALPHA) * data.ema_error_rate;
                }

                // Update hashrate ratio EMA: actual nonces vs expected
                // BM1387: expected_nps = (freq_mhz × 1e6 × 114_cores) / (diff × 2^32)
                // Uses actual difficulty from snapshot instead of hardcoded 256.
                if nonces > 0 && data.current_freq_mhz > 0 && snapshot.window_duration_s > 0.0 {
                    let chip_id = self
                        .chain_chip_ids
                        .get(&snapshot.chain_id)
                        .copied()
                        .unwrap_or(0x1387);
                    let expected_nps = crate::chip_geometry::expected_nps_for_chip(
                        chip_id,
                        data.current_freq_mhz,
                        snapshot.current_difficulty,
                    );
                    let expected_nonces = expected_nps * snapshot.window_duration_s;
                    if expected_nonces > 0.0 {
                        let ratio = (nonces as f64 / expected_nonces).min(2.0);
                        if data.snapshot_count == 0 {
                            data.hashrate_ratio_ema = ratio;
                        } else {
                            data.hashrate_ratio_ema =
                                EMA_ALPHA * ratio + (1.0 - EMA_ALPHA) * data.hashrate_ratio_ema;
                        }
                    }
                }

                data.snapshot_count += 1;
            }
        }
    }

    /// Record that a chip was backed off by the background monitor.
    ///
    /// Called when the autotuner reduces a chip's frequency due to sustained errors.
    pub fn record_backoff(&mut self, chain_id: u8, chip_index: u8, new_freq_mhz: u16) {
        if let Some(data) = self.chips.get_mut(&(chain_id, chip_index)) {
            data.backoff_count += 1;
            data.current_freq_mhz = new_freq_mhz;
            tracing::debug!(
                chain_id,
                chip_index,
                new_freq_mhz,
                backoff_count = data.backoff_count,
                "Chip back-off recorded"
            );
        }
    }

    /// Update the current operating frequency without incrementing back-off count.
    pub fn set_current_frequency(&mut self, chain_id: u8, chip_index: u8, freq_mhz: u16) {
        if let Some(data) = self.chips.get_mut(&(chain_id, chip_index)) {
            data.current_freq_mhz = freq_mhz;
        }
    }

    /// Get health status for all tracked chips.
    pub fn all_statuses(&self) -> Vec<ChipHealthStatus> {
        let mut statuses: Vec<ChipHealthStatus> = self
            .chips
            .iter()
            .map(|(&(chain_id, chip_index), data)| self.compute_status(chain_id, chip_index, data))
            .collect();

        // Sort by chain_id, then chip_index for consistent ordering
        statuses.sort_by(|a, b| {
            a.chain_id
                .cmp(&b.chain_id)
                .then_with(|| a.chip_index.cmp(&b.chip_index))
        });

        statuses
    }

    /// Get chips that are in Warning or Critical state.
    pub fn unhealthy_chips(&self) -> Vec<ChipHealthStatus> {
        let mut unhealthy: Vec<ChipHealthStatus> = self
            .chips
            .iter()
            .filter_map(|(&(chain_id, chip_index), data)| {
                let status = self.compute_status(chain_id, chip_index, data);
                match status.status {
                    ChipHealthLevel::Warning
                    | ChipHealthLevel::Critical
                    | ChipHealthLevel::Dead => Some(status),
                    _ => None,
                }
            })
            .collect();

        unhealthy.sort_by(|a, b| {
            a.health_score
                .partial_cmp(&b.health_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        unhealthy
    }

    /// Compute health status for a single chip.
    fn compute_status(
        &self,
        chain_id: u8,
        chip_index: u8,
        data: &ChipHealthData,
    ) -> ChipHealthStatus {
        let health_score = compute_health_score(data);
        let trend = compute_trend(data);
        let estimated_days =
            estimate_days_to_warning(health_score, trend, self.snapshot_interval_s);
        let error_rate_pct = data.ema_error_rate * 100.0;
        let status = classify_health(data);

        ChipHealthStatus {
            chain_id,
            chip_index,
            health_score,
            trend,
            estimated_days_to_warning: estimated_days,
            error_rate_pct,
            freq_mhz: data.current_freq_mhz,
            backoff_count: data.backoff_count,
            hashrate_ratio: data.hashrate_ratio_ema,
            status,
        }
    }
}

/// Compute health score for a chip.
///
/// Formula:
/// - Start at 1.0
/// - Subtract: error_rate * 0.5 (each 1% error = -0.005 health, so 100% error = -0.5)
///   Note: error_rate is a fraction (0.0-1.0), so we multiply by the scaling factor directly
/// - Subtract: backoff_count * 0.1 (each back-off = -0.1 health)
/// - Subtract: (profile_freq - current_freq) / profile_freq * 0.3 (frequency loss penalty)
/// - Clamp to 0.0-1.0
fn compute_health_score(data: &ChipHealthData) -> f64 {
    let mut score = 1.0;

    // Error rate penalty: each 1% error lowers health by 0.005.
    score -= data.ema_error_rate * 0.5;

    // Back-off penalty
    score -= data.backoff_count as f64 * 0.1;

    // Frequency loss penalty
    if data.profile_freq_mhz > 0 && data.current_freq_mhz < data.profile_freq_mhz {
        let freq_loss =
            (data.profile_freq_mhz - data.current_freq_mhz) as f64 / data.profile_freq_mhz as f64;
        score -= freq_loss * 0.3;
    }

    score.clamp(0.0, 1.0)
}

/// Compute health trend from EMA difference.
///
/// Positive = improving (error rate decreasing), negative = degrading.
/// The trend value represents the per-snapshot change in error rate EMA.
fn compute_trend(data: &ChipHealthData) -> f64 {
    if data.snapshot_count < 2 {
        return 0.0;
    }
    let current_score = compute_health_score(data);
    let previous_error_penalty = data.prev_ema_error_rate * 0.5;
    let previous_score = {
        let mut score = 1.0 - previous_error_penalty;
        score -= data.backoff_count as f64 * 0.1;
        if data.profile_freq_mhz > 0 && data.current_freq_mhz < data.profile_freq_mhz {
            let freq_loss = (data.profile_freq_mhz - data.current_freq_mhz) as f64
                / data.profile_freq_mhz as f64;
            score -= freq_loss * 0.3;
        }
        score.clamp(0.0, 1.0)
    };
    current_score - previous_score
}

/// Estimate days until health drops below warning threshold.
///
/// Uses linear extrapolation from the current trend. Returns None if
/// the chip is already below warning, or if the trend is stable/improving.
fn estimate_days_to_warning(
    health_score: f64,
    trend: f64,
    snapshot_interval_s: f64,
) -> Option<f64> {
    // Only estimate if health is above warning and trending downward
    if health_score <= WARNING_THRESHOLD || trend >= 0.0 {
        return None;
    }

    // trend is negative (health declining)
    // health_score + trend_per_snapshot * N = WARNING_THRESHOLD
    // N = (WARNING_THRESHOLD - health_score) / trend (trend is negative, so N is positive)
    let snapshots_to_warning = (WARNING_THRESHOLD - health_score) / trend;
    let seconds_to_warning = snapshots_to_warning * snapshot_interval_s;
    let days = seconds_to_warning / 86400.0;

    if days > 0.0 && days < 365.0 {
        Some(days)
    } else {
        None
    }
}

/// Classify chip health level based on error rate, back-off history, and
/// hashrate ratio.
///
/// `hashrate_ratio_ema` (actual nonces / expected nonces, EMA) is folded in as
/// an additive signal so a chip whose output has collapsed is never reported as
/// `Healthy` even when error rate and back-off count are clean. See the
/// `HASHRATE_RATIO_*` constants for the thresholds and why they cannot cause
/// false positives on idle/not-yet-active chips.
fn classify_health(data: &ChipHealthData) -> ChipHealthLevel {
    let error_rate_pct = data.ema_error_rate * 100.0;
    let hashrate_ratio = data.hashrate_ratio_ema;

    // Dead: chip frequency backed off to 0, or very high error rate with many back-offs
    if data.current_freq_mhz == 0 {
        return ChipHealthLevel::Dead;
    }

    // Critical: frequent back-offs, very high error rate, or a severely
    // collapsed hashrate ratio (producing well under a quarter of expected nonces).
    if data.backoff_count >= 3 || error_rate_pct > 5.0 || hashrate_ratio < HASHRATE_RATIO_CRITICAL {
        return ChipHealthLevel::Critical;
    }

    // Warning: sustained errors, multiple back-offs, or a hashrate ratio that
    // has fallen well below the frequency-derived expectation.
    if data.backoff_count >= 2 || error_rate_pct > 1.0 || hashrate_ratio < HASHRATE_RATIO_WARNING {
        return ChipHealthLevel::Warning;
    }

    // Watch: elevated error rate, single back-off, or a mildly depressed
    // hashrate ratio.
    if data.backoff_count >= 1 || error_rate_pct > 0.1 || hashrate_ratio < HASHRATE_RATIO_WATCH {
        return ChipHealthLevel::Watch;
    }

    // Healthy: low error rate, no back-offs, hashrate ratio at/near expected.
    ChipHealthLevel::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
    use std::time::Instant;

    fn make_chip(index: u8, operating: u16) -> ChipProfile {
        ChipProfile {
            chip_index: index,
            max_stable_mhz: operating + 30,
            operating_mhz: operating,
            grade: ChipGrade::B,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }
    }

    fn make_test_profiles() -> HashMap<u8, TuningProfile> {
        let mut profiles = HashMap::new();
        let chips = vec![make_chip(0, 650), make_chip(1, 625), make_chip(2, 600)];
        let stats = TuningProfile::compute_stats(&chips, 15.0);
        profiles.insert(
            6,
            TuningProfile {
                version: 1,
                chip_type: "BM1387".to_string(),
                chain_id: 6,
                chip_count: 3,
                voltage_mv: 9100,
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
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );
        profiles
    }

    fn make_snapshot(chain_id: u8, nonces: Vec<u64>, errors: Vec<u64>) -> ChipStatsSnapshot {
        ChipStatsSnapshot {
            chain_id,
            measurement_epoch: 0,
            chip_nonces: nonces,
            chip_errors: errors,
            window_duration_s: 60.0,
            timestamp: Instant::now(),
            board_temp_c: None,
            chip_hw_errors: None,
            chip_timeouts: None,
            chip_duplicates: None,
            current_difficulty: 256,
            chip_temps_c: None,
            psu_power_w: None,
        }
    }

    #[test]
    fn test_health_score_perfect() {
        let data = ChipHealthData {
            ema_error_rate: 0.0,
            prev_ema_error_rate: 0.0,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let score = compute_health_score(&data);
        assert!(
            (score - 1.0).abs() < 0.001,
            "Perfect chip should score 1.0, got {}",
            score
        );
    }

    #[test]
    fn test_health_score_with_errors() {
        let data = ChipHealthData {
            ema_error_rate: 0.01, // 1% error rate
            prev_ema_error_rate: 0.005,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let score = compute_health_score(&data);
        // 1.0 - 0.01 * 0.5 = 0.995
        assert!(
            (score - 0.995).abs() < 0.001,
            "1% error chip should score ~0.995, got {}",
            score
        );
    }

    #[test]
    fn test_health_score_with_backoffs() {
        let data = ChipHealthData {
            ema_error_rate: 0.0,
            prev_ema_error_rate: 0.0,
            backoff_count: 3,
            snapshot_count: 10,
            current_freq_mhz: 575,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let score = compute_health_score(&data);
        // 1.0 - 0 - 3*0.1 - (75/650)*0.3 = 1.0 - 0.3 - 0.0346 = 0.665
        assert!(
            (score - 0.665).abs() < 0.01,
            "Backed-off chip should score ~0.665, got {}",
            score
        );
    }

    #[test]
    fn test_health_score_clamped() {
        let data = ChipHealthData {
            ema_error_rate: 0.5, // 50% error rate
            prev_ema_error_rate: 0.4,
            backoff_count: 10,
            snapshot_count: 20,
            current_freq_mhz: 200,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let score = compute_health_score(&data);
        assert!(
            score >= 0.0,
            "Health score should be clamped to >= 0.0, got {}",
            score
        );
    }

    #[test]
    fn test_trend_detection() {
        // Degrading: error rate increasing
        let data = ChipHealthData {
            ema_error_rate: 0.02,
            prev_ema_error_rate: 0.01,
            backoff_count: 0,
            snapshot_count: 5,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let trend = compute_trend(&data);
        assert!(
            trend < 0.0,
            "Increasing error rate should give negative trend, got {}",
            trend
        );

        // Improving: error rate decreasing
        let data2 = ChipHealthData {
            ema_error_rate: 0.005,
            prev_ema_error_rate: 0.01,
            backoff_count: 0,
            snapshot_count: 5,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let trend2 = compute_trend(&data2);
        assert!(
            trend2 > 0.0,
            "Decreasing error rate should give positive trend, got {}",
            trend2
        );

        // Too few snapshots
        let data3 = ChipHealthData {
            ema_error_rate: 0.01,
            prev_ema_error_rate: 0.0,
            backoff_count: 0,
            snapshot_count: 1,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        let trend3 = compute_trend(&data3);
        assert_eq!(trend3, 0.0, "Insufficient data should give zero trend");
    }

    #[test]
    fn test_days_to_warning() {
        // Health 0.8, negative trend of -0.01 per snapshot, 60s intervals
        let days = estimate_days_to_warning(0.8, -0.01, 60.0);
        assert!(days.is_some());
        // (0.5 - 0.8) / -0.01 = 30 snapshots * 60s = 1800s = 0.0208 days
        let d = days.unwrap();
        assert!((d - 0.0208).abs() < 0.01, "Expected ~0.02 days, got {}", d);

        // Already below warning
        assert!(estimate_days_to_warning(0.3, -0.01, 60.0).is_none());

        // Positive trend (improving)
        assert!(estimate_days_to_warning(0.8, 0.01, 60.0).is_none());

        // Zero trend (stable)
        assert!(estimate_days_to_warning(0.8, 0.0, 60.0).is_none());
    }

    #[test]
    fn test_classify_health() {
        // Healthy
        let data = ChipHealthData {
            ema_error_rate: 0.0005, // 0.05%
            prev_ema_error_rate: 0.0,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Healthy);

        // Watch: elevated error rate
        let data = ChipHealthData {
            ema_error_rate: 0.005, // 0.5%
            prev_ema_error_rate: 0.001,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Watch);

        // Watch: single back-off
        let data = ChipHealthData {
            ema_error_rate: 0.0005,
            prev_ema_error_rate: 0.0,
            backoff_count: 1,
            snapshot_count: 10,
            current_freq_mhz: 625,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Watch);

        // Warning: multiple back-offs
        let data = ChipHealthData {
            ema_error_rate: 0.005,
            prev_ema_error_rate: 0.001,
            backoff_count: 2,
            snapshot_count: 10,
            current_freq_mhz: 600,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Warning);

        // Critical: many back-offs
        let data = ChipHealthData {
            ema_error_rate: 0.02,
            prev_ema_error_rate: 0.01,
            backoff_count: 3,
            snapshot_count: 10,
            current_freq_mhz: 575,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Critical);

        // Dead: frequency at 0
        let data = ChipHealthData {
            ema_error_rate: 0.5,
            prev_ema_error_rate: 0.4,
            backoff_count: 5,
            snapshot_count: 10,
            current_freq_mhz: 0,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&data), ChipHealthLevel::Dead);
    }

    #[test]
    fn test_classify_health_hashrate_ratio() {
        // Baseline: a chip with a clean error rate and no back-offs but a
        // collapsed hashrate ratio must NOT be reported as Healthy.
        let make = |hashrate_ratio_ema: f64| ChipHealthData {
            ema_error_rate: 0.0,
            prev_ema_error_rate: 0.0,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema,
        };

        // Ratio at/near expected → Healthy (unchanged behavior).
        assert_eq!(classify_health(&make(1.0)), ChipHealthLevel::Healthy);
        assert_eq!(classify_health(&make(0.80)), ChipHealthLevel::Healthy);

        // Mildly depressed → Watch.
        assert_eq!(classify_health(&make(0.70)), ChipHealthLevel::Watch);

        // Well below expectation → Warning.
        assert_eq!(classify_health(&make(0.40)), ChipHealthLevel::Warning);

        // Severely collapsed → Critical, despite a clean error/back-off record.
        let collapsed = make(0.10);
        assert_ne!(
            classify_health(&collapsed),
            ChipHealthLevel::Healthy,
            "a chip producing ~10% of expected nonces must never be Healthy"
        );
        assert_eq!(classify_health(&collapsed), ChipHealthLevel::Critical);

        // The hashrate signal only ever ESCALATES: a chip already Critical from
        // error rate stays Critical even with a perfect ratio.
        let high_error = ChipHealthData {
            ema_error_rate: 0.06, // 6% > 5% critical threshold
            prev_ema_error_rate: 0.05,
            backoff_count: 0,
            snapshot_count: 10,
            current_freq_mhz: 650,
            profile_freq_mhz: 650,
            hashrate_ratio_ema: 1.0,
        };
        assert_eq!(classify_health(&high_error), ChipHealthLevel::Critical);
    }

    #[test]
    fn test_tracker_new() {
        let profiles = make_test_profiles();
        let tracker = ChipHealthTracker::new(&profiles);

        assert_eq!(tracker.chips.len(), 3);
        assert!(tracker.chips.contains_key(&(6, 0)));
        assert!(tracker.chips.contains_key(&(6, 1)));
        assert!(tracker.chips.contains_key(&(6, 2)));
    }

    #[test]
    fn test_tracker_update() {
        let profiles = make_test_profiles();
        let mut tracker = ChipHealthTracker::new(&profiles);

        // Healthy snapshot: lots of nonces, no errors
        let snapshot = make_snapshot(6, vec![100, 95, 90], vec![0, 0, 0]);
        tracker.update(&snapshot);

        let statuses = tracker.all_statuses();
        assert_eq!(statuses.len(), 3);
        for s in &statuses {
            assert_eq!(s.status, ChipHealthLevel::Healthy);
            assert!(s.health_score > 0.99);
        }

        // Degrading snapshot: chip 2 has errors
        let snapshot2 = make_snapshot(6, vec![100, 95, 80], vec![0, 0, 20]);
        tracker.update(&snapshot2);

        let statuses2 = tracker.all_statuses();
        // Chip 2 should have elevated error rate
        let chip2 = statuses2.iter().find(|s| s.chip_index == 2).unwrap();
        assert!(chip2.error_rate_pct > 0.0, "Chip 2 should show errors");
        assert!(chip2.health_score < 1.0, "Chip 2 health should be degraded");
    }

    #[test]
    fn test_tracker_record_backoff() {
        let profiles = make_test_profiles();
        let mut tracker = ChipHealthTracker::new(&profiles);

        tracker.record_backoff(6, 1, 600);
        let statuses = tracker.all_statuses();
        let chip1 = statuses.iter().find(|s| s.chip_index == 1).unwrap();
        assert_eq!(chip1.backoff_count, 1);
        assert_eq!(chip1.freq_mhz, 600);
        assert_eq!(chip1.status, ChipHealthLevel::Watch);
    }

    #[test]
    fn test_unhealthy_chips_filter() {
        let profiles = make_test_profiles();
        let mut tracker = ChipHealthTracker::new(&profiles);

        // All healthy initially
        assert!(tracker.unhealthy_chips().is_empty());

        // Make chip 0 critical with many back-offs
        tracker.record_backoff(6, 0, 625);
        tracker.record_backoff(6, 0, 600);
        tracker.record_backoff(6, 0, 575);

        let unhealthy = tracker.unhealthy_chips();
        assert_eq!(unhealthy.len(), 1);
        assert_eq!(unhealthy[0].chip_index, 0);
        assert_eq!(unhealthy[0].status, ChipHealthLevel::Critical);
    }

    #[test]
    fn test_all_statuses_ordering() {
        let mut profiles = make_test_profiles();
        // Add a second chain
        let chips = vec![make_chip(0, 640), make_chip(1, 615)];
        let stats = TuningProfile::compute_stats(&chips, 15.0);
        profiles.insert(
            7,
            TuningProfile {
                version: 1,
                chip_type: "BM1387".to_string(),
                chain_id: 7,
                chip_count: 2,
                voltage_mv: 9100,
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
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        let tracker = ChipHealthTracker::new(&profiles);
        let statuses = tracker.all_statuses();

        assert_eq!(statuses.len(), 5);
        // Should be sorted: chain 6 chips 0,1,2, then chain 7 chips 0,1
        assert_eq!(statuses[0].chain_id, 6);
        assert_eq!(statuses[0].chip_index, 0);
        assert_eq!(statuses[3].chain_id, 7);
        assert_eq!(statuses[3].chip_index, 0);
        assert_eq!(statuses[4].chain_id, 7);
        assert_eq!(statuses[4].chip_index, 1);
    }

    #[test]
    fn test_health_status_serialization() {
        let status = ChipHealthStatus {
            chain_id: 6,
            chip_index: 0,
            health_score: 0.95,
            trend: 0.001,
            estimated_days_to_warning: None,
            error_rate_pct: 0.05,
            freq_mhz: 650,
            backoff_count: 0,
            hashrate_ratio: 1.0,
            status: ChipHealthLevel::Healthy,
        };

        let json = serde_json::to_string(&status).expect("serialize failed");
        let deserialized: ChipHealthStatus =
            serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.chain_id, 6);
        assert_eq!(deserialized.status, ChipHealthLevel::Healthy);
        assert!((deserialized.health_score - 0.95).abs() < 0.001);
    }
}
