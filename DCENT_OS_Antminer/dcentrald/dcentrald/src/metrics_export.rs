//! Runtime adapter for the HAL-free LuxOS-style metrics CSV contract.
//!
//! This samples existing daemon watch-channel state only. It does not poll
//! hardware, sockets, pool clients, or dispatcher internals.

use std::path::{Path, PathBuf};

use dcentrald_api::MinerState;
use dcentrald_api_types::metrics_csv::{MetricsPowerSource, MetricsRing, MetricsSample};
use dcentrald_autotuner::{LivePowerEstimate, PowerAuthorityKind};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub const METRICS_DIR_ENV: &str = "DCENTOS_METRICS_DIR";
pub const DEFAULT_METRICS_DIR: &str = "/data/metrics";

pub fn metrics_storage_dir() -> PathBuf {
    std::env::var_os(METRICS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if Path::new("/data").exists() {
                PathBuf::from(DEFAULT_METRICS_DIR)
            } else {
                PathBuf::from("/tmp/dcent/metrics")
            }
        })
}

#[derive(Debug, Clone, Copy)]
struct MetricsPowerProjection {
    wall_watts: u32,
    source: MetricsPowerSource,
    modeled: bool,
    calibrated: bool,
}

fn round_watts_for_metrics(watts: f64) -> u32 {
    watts.max(0.0).round().min(u32::MAX as f64) as u32
}

fn project_metrics_power(power: &LivePowerEstimate) -> MetricsPowerProjection {
    if !power.wall_watts.is_finite() || power.wall_watts <= 0.0 {
        return MetricsPowerProjection {
            wall_watts: 0,
            source: MetricsPowerSource::Unavailable,
            modeled: false,
            calibrated: false,
        };
    }

    let authority = PowerAuthorityKind::from_source(&power.source, power.calibrated);
    let source = match authority {
        PowerAuthorityKind::Pmbus => MetricsPowerSource::Pmbus,
        PowerAuthorityKind::Adc => MetricsPowerSource::Adc,
        PowerAuthorityKind::WallCalibratedEstimate => MetricsPowerSource::WallCalibratedEstimate,
        PowerAuthorityKind::Estimated => MetricsPowerSource::Estimated,
        PowerAuthorityKind::Unknown => MetricsPowerSource::Unknown,
    };

    MetricsPowerProjection {
        wall_watts: round_watts_for_metrics(power.wall_watts),
        source,
        modeled: !source.is_measured(),
        calibrated: power.calibrated,
    }
}

#[derive(Debug)]
pub struct MetricsCsvExporter {
    dir: PathBuf,
    ring_5s: MetricsRing,
    ring_1m: MetricsRing,
    ring_5m: MetricsRing,
    last_accepted: u64,
    last_rejected: u64,
    ticks: u64,
}

impl MetricsCsvExporter {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            ring_5s: MetricsRing::ring_5s(),
            ring_1m: MetricsRing::ring_1m(),
            ring_5m: MetricsRing::ring_5m(),
            last_accepted: 0,
            last_rejected: 0,
            ticks: 0,
        }
    }

    pub fn tick(
        &mut self,
        timestamp_ms: u64,
        state: &MinerState,
        power: &LivePowerEstimate,
    ) -> std::io::Result<()> {
        let sample = metrics_sample_from_runtime(
            timestamp_ms,
            state,
            power,
            self.last_accepted,
            self.last_rejected,
        );
        self.last_accepted = state.accepted;
        self.last_rejected = state.rejected;
        self.ticks = self.ticks.saturating_add(1);

        self.ring_5s.push(sample);
        if self.ticks.is_multiple_of(12) {
            self.ring_1m.push(sample);
        }
        if self.ticks.is_multiple_of(60) {
            self.ring_5m.push(sample);
        }
        self.persist()
    }

    fn persist(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        write_csv_atomically(&self.dir.join("5s.csv"), &self.ring_5s)?;
        write_csv_atomically(&self.dir.join("1m.csv"), &self.ring_1m)?;
        write_csv_atomically(&self.dir.join("5m.csv"), &self.ring_5m)
    }
}

pub fn metrics_sample_from_runtime(
    timestamp_ms: u64,
    state: &MinerState,
    power: &LivePowerEstimate,
    previous_accepted: u64,
    previous_rejected: u64,
) -> MetricsSample {
    let max_temp_c = state
        .chains
        .iter()
        .map(|chain| chain.temp_c)
        .reduce(f32::max)
        .unwrap_or(0.0);
    let power_projection = project_metrics_power(power);
    let hw_errors: u64 = state.chains.iter().map(|chain| chain.errors as u64).sum();
    // P1-3 (D-6): hardware-error rate as a fraction of *diff1 work done*, NOT of
    // pool shares. Pool shares (accepted + rejected) are only the rare nonces
    // that cleared the pool's share target, so a healthy unit with a steady
    // trickle of CRC errors but few submitted shares read ~97 % "hw error" (the
    // `.100` audit). cgminer's "Device Hardware%" convention is
    //   hw_errors / (hw_errors + diff1_work_done)
    // where diff1_work_done is the difficulty-1-equivalent work the silicon has
    // actually completed. We approximate accumulated diff1 work from the
    // observed hashrate over uptime: diff1 = (H/s × seconds) / 2^32. Both inputs
    // are existing watch-channel fields (no new plumbing).
    const TWO_POW_32: f64 = 4_294_967_296.0;
    let diff1_work = (state.hashrate_ghs.max(0.0) * 1.0e9 * state.uptime_s as f64) / TWO_POW_32;
    let error_rate = if diff1_work <= 0.0 {
        // No proven work yet (cold boot / not hashing): don't fabricate a high
        // error reading from a single early CRC glitch against a zero denominator.
        0.0
    } else {
        (hw_errors as f64 / (hw_errors as f64 + diff1_work)).clamp(0.0, 1.0) as f32
    };

    MetricsSample {
        timestamp_ms,
        hashrate_ths: (state.hashrate_ghs.max(0.0) / 1000.0) as f32,
        wall_watts: power_projection.wall_watts,
        max_chip_temp_c: max_temp_c,
        // Current MinerState exposes one per-chain temperature. Preserve that
        // single observed value in both CSV columns until the runtime publishes
        // separate chip and PCB sensor domains.
        max_pcb_temp_c: max_temp_c,
        max_fan_pwm: state.fans.pwm,
        error_rate,
        accepted_shares_delta: state
            .accepted
            .saturating_sub(previous_accepted)
            .min(u32::MAX as u64) as u32,
        rejected_shares_delta: state
            .rejected
            .saturating_sub(previous_rejected)
            .min(u32::MAX as u64) as u32,
        power_source: power_projection.source,
        power_modeled: power_projection.modeled,
        power_calibrated: power_projection.calibrated,
    }
}

fn write_csv_atomically(path: &Path, ring: &MetricsRing) -> std::io::Result<()> {
    let tmp = path.with_extension("csv.tmp");
    std::fs::write(&tmp, ring.to_csv_with_header())?;
    std::fs::rename(&tmp, path)
}

pub fn spawn_metrics_csv_task(
    shutdown: CancellationToken,
    state_rx: watch::Receiver<MinerState>,
    power_rx: watch::Receiver<LivePowerEstimate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut exporter = MetricsCsvExporter::new(metrics_storage_dir());
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let timestamp_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let state = state_rx.borrow().clone();
                    let power = power_rx.borrow().clone();
                    if let Err(error) = exporter.tick(timestamp_ms, &state, &power) {
                        warn!(error = %error, "Failed to persist metrics CSV snapshot");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_counts(accepted: u64, rejected: u64) -> MinerState {
        MinerState {
            hashrate_ghs: 12_500.0,
            hashrate_5s_ghs: 12_000.0,
            accepted,
            rejected,
            chains: vec![dcentrald_api::ChainState {
                id: 6,
                chips: 63,
                frequency_mhz: 550,
                voltage_mv: 8_800,
                temp_c: 61.5,
                temp_source: Some(dcentrald_api::ChainTempSource::BOARD_SENSOR.to_string()),
                hashrate_ghs: 12_500.0,
                errors: 1,
                status: "Mining".to_string(),
            }],
            fans: dcentrald_api::FanState {
                pwm: 30,
                rpm: 2_400,
                per_fan: Vec::new(),
            },
            pool: dcentrald_api::PoolState {
                url: "stratum+tcp://pool.example.com:3333".to_string(),
                worker: String::new(),
                status: "Alive".to_string(),
                difficulty: 4096.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: dcentrald_api::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: dcentrald_api::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: dcentrald_api::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: dcentrald_api::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: dcentrald_api::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: dcentrald_api::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: dcentrald_api::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: dcentrald_api::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: dcentrald_api::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 60,
            firmware_version: "test".to_string(),
            mode: dcentrald_api::OperatingMode::Standard,
        }
    }

    #[test]
    fn metrics_sample_from_runtime_suppresses_board_only_power() {
        let state = state_with_counts(12, 3);
        let power = LivePowerEstimate {
            board_watts: 1_234.0,
            wall_watts: 0.0,
            ..LivePowerEstimate::default()
        };

        let sample = metrics_sample_from_runtime(10_000, &state, &power, 10, 2);

        assert_eq!(sample.timestamp_ms, 10_000);
        assert!((sample.hashrate_ths - 12.5).abs() < f32::EPSILON);
        assert_eq!(sample.wall_watts, 0);
        assert_eq!(sample.power_source, MetricsPowerSource::Unavailable);
        assert!(!sample.power_modeled);
        assert!(!sample.power_calibrated);
        assert_eq!(sample.max_chip_temp_c, 61.5);
        assert_eq!(sample.max_pcb_temp_c, 61.5);
        assert_eq!(sample.max_fan_pwm, 30);
        assert_eq!(sample.accepted_shares_delta, 2);
        assert_eq!(sample.rejected_shares_delta, 1);
        assert!(sample.error_rate > 0.0);
    }

    #[test]
    fn metrics_sample_from_runtime_marks_measured_pmbus_power() {
        let state = state_with_counts(12, 3);
        let power = LivePowerEstimate {
            board_watts: 1_150.0,
            wall_watts: 1_240.0,
            source: "pmbus".to_string(),
            ..LivePowerEstimate::default()
        };

        let sample = metrics_sample_from_runtime(10_000, &state, &power, 10, 2);

        assert_eq!(sample.wall_watts, 1_240);
        assert_eq!(sample.power_source, MetricsPowerSource::Pmbus);
        assert!(!sample.power_modeled);
        assert!(!sample.power_calibrated);
    }

    #[test]
    fn metrics_sample_from_runtime_marks_calibrated_model_power() {
        let state = state_with_counts(12, 3);
        let power = LivePowerEstimate {
            board_watts: 1_100.0,
            wall_watts: 1_300.0,
            source: "estimated".to_string(),
            calibrated: true,
            calibration_multiplier: Some(1.04),
            ..LivePowerEstimate::default()
        };

        let sample = metrics_sample_from_runtime(10_000, &state, &power, 10, 2);

        assert_eq!(sample.wall_watts, 1_300);
        assert_eq!(
            sample.power_source,
            MetricsPowerSource::WallCalibratedEstimate
        );
        assert!(sample.power_modeled);
        assert!(sample.power_calibrated);
    }

    #[test]
    fn hw_error_rate_uses_diff1_work_not_pool_shares() {
        // P1-3 (D-6): a HEALTHY unit — high hashrate, an hour of uptime, a
        // handful of CRC errors, but only a few submitted pool shares. The old
        // denominator (accepted + rejected + hw_errors) made this read ~93-97 %.
        // The diff1-work denominator must keep it near zero.
        let mut state = state_with_counts(3, 1); // only 4 pool replies
        state.hashrate_ghs = 13_500.0; // ~13.5 TH/s
        state.uptime_s = 3_600; // 1 h
        state.chains[0].errors = 50; // 50 CRC errors

        let sample = metrics_sample_from_runtime(0, &state, &LivePowerEstimate::default(), 3, 1);

        // diff1 ≈ 13.5e12 * 3600 / 2^32 ≈ 1.13e7 work units;
        // 50 / (50 + 1.13e7) ≈ 4.4e-6. The OLD formula was 50/(50+4) ≈ 0.93.
        assert!(
            sample.error_rate < 0.001,
            "healthy unit hw_error_rate should be tiny, got {}",
            sample.error_rate
        );
    }

    #[test]
    fn hw_error_rate_zero_when_not_hashing() {
        // No proven diff1 work yet — a CRC glitch must not read as 100 % error.
        let mut state = state_with_counts(0, 0);
        state.hashrate_ghs = 0.0;
        state.uptime_s = 30;
        state.chains[0].errors = 5;

        let sample = metrics_sample_from_runtime(0, &state, &LivePowerEstimate::default(), 0, 0);

        assert_eq!(sample.error_rate, 0.0);
    }

    #[test]
    fn exporter_persists_all_three_csv_files() {
        let dir = std::env::temp_dir().join(format!(
            "dcent_metrics_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut exporter = MetricsCsvExporter::new(dir.clone());
        let power = LivePowerEstimate {
            wall_watts: 2_000.0,
            source: "pmbus".to_string(),
            ..LivePowerEstimate::default()
        };

        for tick in 0..60u64 {
            exporter
                .tick(tick * 5_000, &state_with_counts(tick, 0), &power)
                .expect("metrics tick");
        }

        for name in ["5s.csv", "1m.csv", "5m.csv"] {
            let body = std::fs::read_to_string(dir.join(name)).expect("read metrics csv");
            assert!(body.starts_with(dcentrald_api_types::metrics_csv::csv_header()));
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("5m.csv"))
                .expect("read 5m csv")
                .lines()
                .count(),
            2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
