//! A03 — rolling-metrics CSV ring with 3-tier (5s / 1m / 5m) rolling averages.
//!
//! Read-only LuxOS `/metrics` parity surface. LuxOS exposes a 3-tier
//! granularity ring; DCENT had Prometheus scrape + a live snapshot but no
//! rotating CSV ring of rolling averages. This module adds that surface so
//! fleet tooling can poll trailing-window averages we did not previously
//! expose.
//!
//! Source (goldmine finding):
//!
//!   — CAND-01 (HIGH): "Rolling metrics CSV ring (5s/1m/5m) ... LuxOS exposes
//!     /metrics with 3-tier granularity ring; DCENT has Prometheus scrape +
//!     live snapshot but no rotating CSV ring. Enables fleet tools that poll
//!     rolling averages."
//!
//! Design notes:
//!   * The pure ring + average math lives in [`RollingMetrics`] (HAL-free,
//!     fully host-testable). It reuses the existing
//!     `dcentrald_api_types::metrics_csv` ring + CSV encoder ( tel-A,
//!     itself sourced from the LuxOS `5s/1m/5m.csv` layout).
//!   * The read-only routes self-record one sample (from already-published
//!     read-only telemetry) on each scrape, then return the trailing-window
//!     averages. A daemon timer MAY additionally push samples via
//!     [`ingest_sample`] — wiring that is optional and purely additive.
//!   * This module NEVER touches any live mining / voltage / thermal / PSU
//!     dispatch path. It only reads the published watch channels.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use dcentrald_api_types::metrics_csv::{
    csv_header, MetricsPowerSource, MetricsRing, MetricsSample,
};
use dcentrald_autotuner::{LivePowerEstimate, PowerAuthorityKind};

use crate::AppState;

/// Trailing-window widths exposed by the rolling-metrics surface (ms).
pub const WINDOW_5S_MS: u64 = 5_000;
pub const WINDOW_1M_MS: u64 = 60_000;
pub const WINDOW_5M_MS: u64 = 300_000;

/// Ring capacity. Sized to hold >=5 minutes of samples even at a fast (~1 Hz)
/// scrape cadence, with slack. Oldest samples roll off (LuxOS ring semantics).
pub const ROLLING_RING_CAPACITY: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorRateSource {
    Unavailable,
    DaemonIngest,
    Mixed,
}

/// One trailing-window average bucket.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RollingAverageBucket {
    /// Window width in seconds (5 / 60 / 300).
    pub window_s: u64,
    /// Number of samples that fell inside the window.
    pub sample_count: usize,
    /// Mean total hashrate over the window (TH/s).
    pub avg_hashrate_ths: f32,
    /// Mean wall power over the window (W).
    pub avg_wall_watts: f32,
    /// Number of samples that carried a positive wall-power value.
    pub wall_power_sample_count: usize,
    /// Positive wall-power samples sourced from measured telemetry.
    pub wall_power_measured_sample_count: usize,
    /// Positive wall-power samples sourced from modeled telemetry.
    pub wall_power_modeled_sample_count: usize,
    /// Samples in the window where wall power was unavailable.
    pub wall_power_unavailable_sample_count: usize,
    /// Mean of the per-sample max chip temperature over the window (°C).
    pub avg_max_chip_temp_c: f32,
    /// Mean hardware-error fraction over the window (0.0..=1.0).
    pub avg_error_rate: f32,
    /// True when at least one sample in this window carried real hardware-error
    /// data. `avg_error_rate == 0.0` with this false means unavailable, not clean.
    pub avg_error_rate_available: bool,
    /// Number of samples in this window that carried real hardware-error data.
    pub error_rate_sample_count: usize,
    /// Provenance for `avg_error_rate` in this window.
    pub error_rate_source: ErrorRateSource,
    /// Mean of the per-sample max fan PWM over the window (0..=100).
    pub avg_max_fan_pwm: f32,
    /// Accepted shares observed within the window (summed deltas).
    pub accepted_shares: u32,
    /// Rejected shares observed within the window (summed deltas).
    pub rejected_shares: u32,
}

/// Top-level rolling-metrics response: the three trailing-window buckets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RollingMetricsResponse {
    /// Unix epoch ms at which the buckets were computed.
    pub now_ms: u64,
    /// Total samples currently retained in the ring.
    pub total_samples: usize,
    /// 5-second trailing average.
    pub w5s: RollingAverageBucket,
    /// 1-minute trailing average.
    pub w1m: RollingAverageBucket,
    /// 5-minute trailing average.
    pub w5m: RollingAverageBucket,
}

/// Pure, HAL-free 3-tier rolling-average ring.
///
/// Holds a time-stamped ring of `MetricsSample`s and computes trailing-window
/// averages on demand. No filesystem, no async — fully host-testable.
#[derive(Debug, Clone)]
pub struct RollingMetrics {
    ring: MetricsRing,
    error_rate_available: VecDeque<bool>,
    /// Previously-seen cumulative accepted/rejected share counters, used to
    /// derive per-sample deltas from the cumulative `MinerState` counters.
    last_accepted: u64,
    last_rejected: u64,
    seeded: bool,
}

impl Default for RollingMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RollingMetrics {
    pub fn new() -> Self {
        Self {
            ring: MetricsRing::new(ROLLING_RING_CAPACITY),
            error_rate_available: VecDeque::with_capacity(ROLLING_RING_CAPACITY),
            last_accepted: 0,
            last_rejected: 0,
            seeded: false,
        }
    }

    /// Record a fully-formed sample (deltas already populated). Used by an
    /// external timer-driven ingestor if the daemon wires one later.
    fn record(&mut self, sample: MetricsSample) {
        self.record_with_error_rate_availability(sample, true);
    }

    pub fn record_with_error_rate_availability(
        &mut self,
        sample: MetricsSample,
        available: bool,
    ) {
        if self.error_rate_available.len() == self.ring.capacity() {
            self.error_rate_available.pop_front();
        }
        self.error_rate_available.push_back(available);
        self.ring.push(sample);
    }

    /// Record from cumulative share counters, computing the accepted/rejected
    /// deltas against the previously-seen cumulative values. The first call
    /// seeds the baseline and records a 0-delta sample (so a counter reset /
    /// fresh boot never reports a spurious burst).
    pub fn record_cumulative(
        &mut self,
        mut sample: MetricsSample,
        accepted_total: u64,
        rejected_total: u64,
    ) {
        if self.seeded {
            // S4-2: clamp the u64 delta to u32::MAX before the cast. A raw `as u32`
            // silently truncates a >4.29e9 single-interval jump (only reachable via a
            // counter reload), and the sibling metrics_export.rs:177 already clamps —
            // match it so the two writers of this u32 field stay consistent.
            sample.accepted_shares_delta = accepted_total
                .saturating_sub(self.last_accepted)
                .min(u32::MAX as u64) as u32;
            sample.rejected_shares_delta = rejected_total
                .saturating_sub(self.last_rejected)
                .min(u32::MAX as u64) as u32;
        } else {
            sample.accepted_shares_delta = 0;
            sample.rejected_shares_delta = 0;
            self.seeded = true;
        }
        self.last_accepted = accepted_total;
        self.last_rejected = rejected_total;
        self.record_with_error_rate_availability(sample, false);
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// CSV body (header + rows) of the underlying ring — LuxOS `/metrics`
    /// parity. Oldest sample first.
    pub fn to_csv(&self) -> String {
        self.ring.to_csv_with_header()
    }

    pub fn to_csv_with_error_rate_provenance(&self) -> String {
        let mut out = String::from(csv_header());
        out.push_str(",error_rate_available,error_rate_source\n");
        for (sample, available) in self.ring.iter().zip(self.error_rate_available.iter()) {
            out.push_str(&sample.to_csv_row());
            out.push(',');
            out.push_str(if *available { "true" } else { "false" });
            out.push(',');
            out.push_str(if *available {
                "daemon_ingest"
            } else {
                "unavailable"
            });
            out.push('\n');
        }
        out
    }

    /// Compute the trailing-window average bucket ending at `now_ms`.
    pub fn average(&self, now_ms: u64, window_ms: u64) -> RollingAverageBucket {
        let cutoff = now_ms.saturating_sub(window_ms);
        let mut n = 0u32;
        let mut power_n = 0u32;
        let mut power_measured_n = 0u32;
        let mut power_modeled_n = 0u32;
        let mut power_unavailable_n = 0u32;
        let mut hr = 0f64;
        let mut watts = 0f64;
        let mut chip = 0f64;
        let mut err = 0f64;
        let mut err_n = 0u32;
        let mut fan = 0f64;
        let mut acc = 0u32;
        let mut rej = 0u32;
        for (s, err_available) in self.ring.iter().zip(self.error_rate_available.iter()) {
            if s.timestamp_ms >= cutoff && s.timestamp_ms <= now_ms {
                n += 1;
                hr += s.hashrate_ths as f64;
                if s.wall_watts > 0 && s.power_source != MetricsPowerSource::Unavailable {
                    power_n += 1;
                    watts += s.wall_watts as f64;
                    if s.power_source.is_measured() {
                        power_measured_n += 1;
                    } else if s.power_modeled {
                        power_modeled_n += 1;
                    }
                } else {
                    power_unavailable_n += 1;
                }
                chip += s.max_chip_temp_c as f64;
                if *err_available {
                    err_n += 1;
                    err += s.error_rate as f64;
                }
                fan += s.max_fan_pwm as f64;
                acc = acc.saturating_add(s.accepted_shares_delta);
                rej = rej.saturating_add(s.rejected_shares_delta);
            }
        }
        let div = if n == 0 { 1.0 } else { n as f64 };
        let power_div = if power_n == 0 { 1.0 } else { power_n as f64 };
        let err_div = if err_n == 0 { 1.0 } else { err_n as f64 };
        let error_rate_source = if err_n == 0 {
            ErrorRateSource::Unavailable
        } else if err_n == n {
            ErrorRateSource::DaemonIngest
        } else {
            ErrorRateSource::Mixed
        };
        RollingAverageBucket {
            window_s: window_ms / 1000,
            sample_count: n as usize,
            avg_hashrate_ths: (hr / div) as f32,
            avg_wall_watts: (watts / power_div) as f32,
            wall_power_sample_count: power_n as usize,
            wall_power_measured_sample_count: power_measured_n as usize,
            wall_power_modeled_sample_count: power_modeled_n as usize,
            wall_power_unavailable_sample_count: power_unavailable_n as usize,
            avg_max_chip_temp_c: (chip / div) as f32,
            avg_error_rate: (err / err_div) as f32,
            avg_error_rate_available: err_n > 0,
            error_rate_sample_count: err_n as usize,
            error_rate_source,
            avg_max_fan_pwm: (fan / div) as f32,
            accepted_shares: acc,
            rejected_shares: rej,
        }
    }

    /// Build the full 3-tier response at `now_ms`.
    pub fn response(&self, now_ms: u64) -> RollingMetricsResponse {
        RollingMetricsResponse {
            now_ms,
            total_samples: self.ring.len(),
            w5s: self.average(now_ms, WINDOW_5S_MS),
            w1m: self.average(now_ms, WINDOW_1M_MS),
            w5m: self.average(now_ms, WINDOW_5M_MS),
        }
    }
}

/// Process-global rolling-metrics ring. Populated on each scrape of the
/// read-only endpoints from already-published telemetry (and optionally by a
/// daemon timer via [`ingest_sample`]). Purely additive: never touches any
/// live mining / voltage / thermal / PSU path.
static ROLLING: LazyLock<Mutex<RollingMetrics>> =
    LazyLock::new(|| Mutex::new(RollingMetrics::new()));

/// Optional external ingest hook (e.g. a daemon timer). Additive — the
/// read-only routes also self-record, so wiring this is not required.
pub fn ingest_sample(sample: MetricsSample) {
    ingest_sample_with_error_rate_availability(sample, true);
}

pub fn ingest_sample_with_error_rate_availability(sample: MetricsSample, available: bool) {
    let mut g = ROLLING.lock().unwrap_or_else(|e| e.into_inner());
    g.record_with_error_rate_availability(sample, available);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy)]
struct MetricsPowerProjection {
    wall_watts: u32,
    source: MetricsPowerSource,
    modeled: bool,
    calibrated: bool,
}

fn (power: &LivePowerEstimate) -> MetricsPowerProjection {
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
        wall_watts: power.wall_watts.max(0.0).round().min(u32::MAX as f64) as u32,
        source,
        modeled: !source.is_measured(),
        calibrated: power.calibrated,
    }
}

/// Build a `MetricsSample` from the latest already-published telemetry. Reads
/// only the read-only watch channels — no hardware probe, no side effects.
/// Returns the sample plus the cumulative accepted/rejected counters so the
/// caller can compute per-sample share deltas.
fn sample_from_state(state: &AppState, ts_ms: u64) -> (MetricsSample, u64, u64) {
    let (hashrate_ths, max_chip_temp_c, max_fan_pwm, accepted, rejected) = {
        let ms = state.state_rx.borrow();
        let max_chip = ms.chains.iter().map(|c| c.temp_c).fold(0.0f32, f32::max);
        (
            (ms.hashrate_ghs / 1000.0) as f32,
            max_chip,
            ms.fans.pwm,
            ms.accepted,
            ms.rejected,
        )
    };
    let power_projection = (&state.power_rx.borrow());
    let sample = MetricsSample {
        timestamp_ms: ts_ms,
        hashrate_ths,
        wall_watts: power_projection.wall_watts,
        max_chip_temp_c,
        // No distinct PCB sensor is exposed in the published MinerState; the
        // chain temp already carries the honest board/die value. Left 0.0
        // rather than fabricating a second reading.
        max_pcb_temp_c: 0.0,
        max_fan_pwm,
        // A hardware-error fraction is not exposed in the read-only MinerState
        // snapshot. The rolling bucket marks this sample unavailable so the
        // legacy numeric 0.0 is not mistaken for observed clean hardware.
        error_rate: 0.0,
        // Deltas are filled in by `record_cumulative`.
        accepted_shares_delta: 0,
        rejected_shares_delta: 0,
        power_source: power_projection.source,
        power_modeled: power_projection.modeled,
        power_calibrated: power_projection.calibrated,
    };
    (sample, accepted, rejected)
}

/// Build the `/api/metrics/rolling*` sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/metrics/rolling", get(get_rolling))
        .route("/api/metrics/rolling.csv", get(get_rolling_csv))
}

async fn get_rolling(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ts = now_ms();
    let (sample, acc, rej) = sample_from_state(&state, ts);
    let resp = {
        let mut g = ROLLING.lock().unwrap_or_else(|e| e.into_inner());
        g.record_cumulative(sample, acc, rej);
        g.response(ts)
    };
    Json(resp)
}

async fn get_rolling_csv(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ts = now_ms();
    let (sample, acc, rej) = sample_from_state(&state, ts);
    let csv = {
        let mut g = ROLLING.lock().unwrap_or_else(|e| e.into_inner());
        g.record_cumulative(sample, acc, rej);
        g.to_csv_with_error_rate_provenance()
    };
    (
        axum::http::StatusCode::OK,
        [("content-type", "text/csv; charset=utf-8")],
        csv,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: u64, hr_ths: f32, watts: u32, chip: f32, fan: u8) -> MetricsSample {
        MetricsSample {
            timestamp_ms: ts,
            hashrate_ths: hr_ths,
            wall_watts: watts,
            max_chip_temp_c: chip,
            max_pcb_temp_c: 0.0,
            max_fan_pwm: fan,
            error_rate: 0.0,
            accepted_shares_delta: 0,
            rejected_shares_delta: 0,
            power_source: if watts > 0 {
                MetricsPowerSource::Pmbus
            } else {
                MetricsPowerSource::Unavailable
            },
            power_modeled: false,
            power_calibrated: false,
        }
    }

    #[test]
    fn window_constants_are_5s_1m_5m() {
        assert_eq!(WINDOW_5S_MS, 5_000);
        assert_eq!(WINDOW_1M_MS, 60_000);
        assert_eq!(WINDOW_5M_MS, 300_000);
    }

    #[test]
    fn empty_ring_returns_zero_buckets_without_panic() {
        let r = RollingMetrics::new();
        let resp = r.response(1_000_000);
        assert_eq!(resp.total_samples, 0);
        for b in [&resp.w5s, &resp.w1m, &resp.w5m] {
            assert_eq!(b.sample_count, 0);
            assert_eq!(b.avg_hashrate_ths, 0.0);
            assert_eq!(b.avg_wall_watts, 0.0);
            assert_eq!(b.wall_power_sample_count, 0);
            assert_eq!(b.wall_power_measured_sample_count, 0);
            assert_eq!(b.wall_power_modeled_sample_count, 0);
            assert_eq!(b.wall_power_unavailable_sample_count, 0);
            assert_eq!(b.avg_error_rate, 0.0);
            assert!(!b.avg_error_rate_available);
            assert_eq!(b.error_rate_sample_count, 0);
            assert_eq!(b.error_rate_source, ErrorRateSource::Unavailable);
            assert_eq!(b.accepted_shares, 0);
        }
        assert_eq!(resp.w5s.window_s, 5);
        assert_eq!(resp.w1m.window_s, 60);
        assert_eq!(resp.w5m.window_s, 300);
    }

    #[test]
    fn windows_partition_samples_by_age() {
        // Pick a `now` larger than the widest age (400s) so every
        // `now - age` stays a positive u64 constant.
        let now = 1_000_000u64;
        let mut r = RollingMetrics::new();
        // inside 5s window (age 2s) — hashrate 100
        r.record(sample(now - 2_000, 100.0, 3000, 60.0, 30));
        // inside 1m but outside 5s (age 30s) — hashrate 80
        r.record(sample(now - 30_000, 80.0, 2800, 58.0, 30));
        // inside 5m but outside 1m (age 200s) — hashrate 60
        r.record(sample(now - 200_000, 60.0, 2600, 55.0, 25));
        // outside 5m (age 400s) — must be excluded everywhere
        r.record(sample(now - 400_000, 10.0, 1000, 40.0, 10));

        let w5s = r.average(now, WINDOW_5S_MS);
        assert_eq!(w5s.sample_count, 1);
        assert_eq!(w5s.avg_hashrate_ths, 100.0);
        assert_eq!(w5s.avg_wall_watts, 3000.0);
        assert_eq!(w5s.wall_power_sample_count, 1);
        assert_eq!(w5s.wall_power_measured_sample_count, 1);

        let w1m = r.average(now, WINDOW_1M_MS);
        assert_eq!(w1m.sample_count, 2); // 100 + 80
        assert_eq!(w1m.avg_hashrate_ths, 90.0);
        assert_eq!(w1m.avg_wall_watts, 2900.0);

        let w5m = r.average(now, WINDOW_5M_MS);
        assert_eq!(w5m.sample_count, 3); // 100 + 80 + 60
        assert_eq!(w5m.avg_hashrate_ths, 80.0);
        assert_eq!(w5m.avg_wall_watts, 2800.0);
        // The 400s-old sample is excluded from the widest window.
        assert!(w5m.avg_hashrate_ths > 10.0);
    }

    #[test]
    fn wall_power_average_uses_available_samples_only_and_counts_provenance() {
        let now = 1_000_000u64;
        let mut r = RollingMetrics::new();
        let measured = sample(now - 1_000, 100.0, 3000, 60.0, 30);
        let mut modeled = sample(now - 2_000, 100.0, 2500, 60.0, 30);
        modeled.power_source = MetricsPowerSource::WallCalibratedEstimate;
        modeled.power_modeled = true;
        modeled.power_calibrated = true;
        let unavailable = sample(now - 3_000, 100.0, 0, 60.0, 30);

        r.record(measured);
        r.record(modeled);
        r.record(unavailable);

        let bucket = r.average(now, WINDOW_5S_MS);

        assert_eq!(bucket.sample_count, 3);
        assert_eq!(bucket.wall_power_sample_count, 2);
        assert_eq!(bucket.wall_power_measured_sample_count, 1);
        assert_eq!(bucket.wall_power_modeled_sample_count, 1);
        assert_eq!(bucket.wall_power_unavailable_sample_count, 1);
        assert_eq!(bucket.avg_wall_watts, 2750.0);
    }

    #[test]
    fn scrape_only_error_rate_is_marked_unavailable_not_clean_zero() {
        let now = 1_000_000u64;
        let mut r = RollingMetrics::new();

        r.record_cumulative(sample(now - 1_000, 100.0, 3000, 60.0, 30), 10, 0);

        let bucket = r.average(now, WINDOW_5S_MS);
        assert_eq!(bucket.sample_count, 1);
        assert_eq!(bucket.avg_error_rate, 0.0);
        assert!(!bucket.avg_error_rate_available);
        assert_eq!(bucket.error_rate_sample_count, 0);
        assert_eq!(bucket.error_rate_source, ErrorRateSource::Unavailable);
    }

    #[test]
    fn daemon_ingested_error_rate_is_available_and_averaged() {
        let now = 1_000_000u64;
        let mut r = RollingMetrics::new();
        let mut s = sample(now - 1_000, 100.0, 3000, 60.0, 30);
        s.error_rate = 0.02;

        r.record(s);

        let bucket = r.average(now, WINDOW_5S_MS);
        assert!(bucket.avg_error_rate_available);
        assert_eq!(bucket.error_rate_sample_count, 1);
        assert_eq!(bucket.error_rate_source, ErrorRateSource::DaemonIngest);
        assert!((bucket.avg_error_rate - 0.02).abs() < f32::EPSILON);
    }

    #[test]
    fn mixed_error_rate_sources_do_not_average_unavailable_zeroes() {
        let now = 1_000_000u64;
        let mut r = RollingMetrics::new();
        let mut ingested = sample(now - 1_000, 100.0, 3000, 60.0, 30);
        ingested.error_rate = 0.03;

        r.record(ingested);
        r.record_cumulative(sample(now - 500, 100.0, 3000, 60.0, 30), 10, 0);

        let bucket = r.average(now, WINDOW_5S_MS);
        assert_eq!(bucket.sample_count, 2);
        assert!(bucket.avg_error_rate_available);
        assert_eq!(bucket.error_rate_sample_count, 1);
        assert_eq!(bucket.error_rate_source, ErrorRateSource::Mixed);
        assert!((bucket.avg_error_rate - 0.03).abs() < f32::EPSILON);
    }

    #[test]
    fn projected_power_suppresses_unavailable_live_estimate() {
        let power = LivePowerEstimate {
            board_watts: 1_200.0,
            wall_watts: 0.0,
            source: "estimated".to_string(),
            ..LivePowerEstimate::default()
        };

        let projection = (&power);

        assert_eq!(projection.wall_watts, 0);
        assert_eq!(projection.source, MetricsPowerSource::Unavailable);
        assert!(!projection.modeled);
        assert!(!projection.calibrated);
    }

    #[test]
    fn projected_power_marks_calibrated_model() {
        let power = LivePowerEstimate {
            wall_watts: 1_300.0,
            source: "estimated".to_string(),
            calibrated: true,
            ..LivePowerEstimate::default()
        };

        let projection = (&power);

        assert_eq!(projection.wall_watts, 1_300);
        assert_eq!(
            projection.source,
            MetricsPowerSource::WallCalibratedEstimate
        );
        assert!(projection.modeled);
        assert!(projection.calibrated);
    }

    #[test]
    fn record_cumulative_computes_share_deltas() {
        let mut r = RollingMetrics::new();
        // First record seeds the baseline → 0 deltas even though totals are
        // non-zero (counter could be mid-run).
        r.record_cumulative(sample(1_000, 100.0, 3000, 60.0, 30), 50, 2);
        // Second record: +5 accepted, +1 rejected.
        r.record_cumulative(sample(2_000, 100.0, 3000, 60.0, 30), 55, 3);
        let b = r.average(3_000, WINDOW_5M_MS);
        assert_eq!(b.accepted_shares, 5);
        assert_eq!(b.rejected_shares, 1);
    }

    #[test]
    fn record_cumulative_counter_reset_does_not_underflow() {
        let mut r = RollingMetrics::new();
        r.record_cumulative(sample(1_000, 100.0, 3000, 60.0, 30), 100, 10);
        // Counter went backwards (reboot) — saturating_sub keeps it at 0.
        r.record_cumulative(sample(2_000, 100.0, 3000, 60.0, 30), 5, 0);
        let b = r.average(3_000, WINDOW_5M_MS);
        assert_eq!(b.accepted_shares, 0);
        assert_eq!(b.rejected_shares, 0);
    }

    #[test]
    fn to_csv_has_header_and_one_row_per_sample() {
        let mut r = RollingMetrics::new();
        r.record(sample(1_000, 100.0, 3000, 60.0, 30));
        r.record(sample(2_000, 100.0, 3000, 60.0, 30));
        let csv = r.to_csv();
        let mut lines = csv.lines();
        assert!(lines.next().unwrap().starts_with("timestamp_ms,"));
        assert_eq!(csv.lines().count(), 3); // header + 2 rows
    }

    #[test]
    fn rolling_csv_appends_error_rate_availability_and_source() {
        let mut r = RollingMetrics::new();
        r.record_cumulative(sample(1_000, 100.0, 3000, 60.0, 30), 1, 0);
        let mut ingested = sample(2_000, 100.0, 3000, 60.0, 30);
        ingested.error_rate = 0.04;
        r.record_with_error_rate_availability(ingested, true);

        let csv = r.to_csv_with_error_rate_provenance();
        let lines: Vec<&str> = csv.lines().collect();

        assert!(lines[0].ends_with(",error_rate_available,error_rate_source"));
        assert!(lines[1].ends_with(",false,unavailable"));
        assert!(lines[2].ends_with(",true,daemon_ingest"));
    }

    #[test]
    fn ring_rolls_off_oldest_beyond_capacity() {
        let mut r = RollingMetrics::new();
        for i in 0..(ROLLING_RING_CAPACITY as u64 + 10) {
            r.record(sample(i, 100.0, 3000, 60.0, 30));
        }
        assert_eq!(r.len(), ROLLING_RING_CAPACITY);
    }
}
