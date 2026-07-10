//!  tel-A — 3-tier metrics CSV format + in-memory ring (HAL-free).
//!
//! Source RE evidence: .
//!
//! LuxOS writes per-miner telemetry to three rotating CSVs:
//! - `5s.csv` — 5-second-resolution samples, ring of last 12 (1 minute)
//! - `1m.csv` — 1-minute-resolution samples, ring of last 60 (1 hour)
//! - `5m.csv` — 5-minute-resolution samples, ring of last 288 (24 hours)
//!
//! Each tier is a fixed-capacity ring; the oldest sample is dropped on
//! insert when full. The CSV row schema is identical across tiers (only
//! the time resolution differs).
//!
//! This module owns the **ring + row encoder** — pure logic, no
//! filesystem. The runtime adapter inside `dcentrald-diagnostics` snapshots
//! the ring on each tier's timer tick and writes the CSV bytes to the
//! configured paths under `/data/metrics/{5s,1m,5m}.csv`.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Standard ring capacities matching LuxOS's tier semantics.
pub const RING_5S_CAPACITY: usize = 12;
/// 1-minute resolution × 60 = 1 hour of history.
pub const RING_1M_CAPACITY: usize = 60;
/// 5-minute resolution × 288 = 24 hours of history.
pub const RING_5M_CAPACITY: usize = 288;

/// Source class for the power value carried in a metrics CSV sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetricsPowerSource {
    /// No positive wall-power value was available for this sample.
    #[default]
    Unavailable,
    /// PSU PMBus telemetry.
    Pmbus,
    /// Board/current monitor telemetry, for example INA226 or platform ADC.
    Adc,
    /// Runtime estimate anchored to an operator wall-meter calibration.
    WallCalibratedEstimate,
    /// Runtime chip/frequency/voltage model without a live measurement anchor.
    Estimated,
    /// Positive value existed, but the producer could not classify the source.
    Unknown,
}

impl MetricsPowerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Pmbus => "pmbus",
            Self::Adc => "adc",
            Self::WallCalibratedEstimate => "wall_calibrated_estimate",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_measured(self) -> bool {
        matches!(self, Self::Pmbus | Self::Adc)
    }
}

/// One sample row. Field order is the canonical CSV column order; do not
/// reorder without coordinating dashboards / Grafana imports. New provenance
/// fields are appended so the original LuxOS-style columns retain their
/// positions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricsSample {
    /// Unix epoch milliseconds at which this sample was taken.
    pub timestamp_ms: u64,
    /// Hashrate in TH/s, summed across all chains.
    pub hashrate_ths: f32,
    /// Wall-side power in watts. `0` means unavailable; non-zero values must be
    /// interpreted with the appended power provenance fields.
    pub wall_watts: u32,
    /// Highest observed chip temperature in °C.
    pub max_chip_temp_c: f32,
    /// Highest observed PCB temperature in °C.
    ///
    /// TEL-2 (2026-06-20): the runtime currently exposes a SINGLE per-chain
    /// temperature (`ChainState::temp_c`), so until separate chip and PCB
    /// sensor domains are published, the producer (`metrics_export.rs`) mirrors
    /// that one observed value into BOTH `max_chip_temp_c` and `max_pcb_temp_c`
    /// — i.e. these two columns carry the SAME value. The value is real (not
    /// fabricated), but consumers must not treat the two columns as independent
    /// sensors. This mirrors the equivalent disclosure already made on the
    /// CGMiner-compat surface (`cgminer.rs`, `temp_chip{N}` == `temp{N}`).
    pub max_pcb_temp_c: f32,
    /// Highest observed fan speed PWM (0..=100).
    pub max_fan_pwm: u8,
    /// Hardware error rate as a fraction (0.0..=1.0).
    pub error_rate: f32,
    /// Number of accepted shares since the previous sample.
    pub accepted_shares_delta: u32,
    /// Number of rejected shares since the previous sample.
    pub rejected_shares_delta: u32,
    /// Source class for `wall_watts`.
    #[serde(default)]
    pub power_source: MetricsPowerSource,
    /// True when `wall_watts` is modeled rather than directly measured.
    #[serde(default)]
    pub power_modeled: bool,
    /// True when the modeled value is shaped by an operator wall-meter calibration.
    #[serde(default)]
    pub power_calibrated: bool,
}

impl MetricsSample {
    /// Canonical CSV row encoder. Columns are in `FIELD_ORDER` (see
    /// `csv_header`).
    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{:.2},{},{:.1},{:.1},{},{:.4},{},{},{},{},{}",
            self.timestamp_ms,
            self.hashrate_ths,
            self.wall_watts,
            self.max_chip_temp_c,
            self.max_pcb_temp_c,
            self.max_fan_pwm,
            self.error_rate,
            self.accepted_shares_delta,
            self.rejected_shares_delta,
            self.power_source.as_str(),
            self.power_modeled,
            self.power_calibrated,
        )
    }
}

/// CSV header line. Stable; consumers can rely on column positions.
pub fn csv_header() -> &'static str {
    "timestamp_ms,hashrate_ths,wall_watts,max_chip_temp_c,max_pcb_temp_c,max_fan_pwm,error_rate,accepted_shares_delta,rejected_shares_delta,power_source,power_modeled,power_calibrated"
}

/// Fixed-capacity FIFO ring of `MetricsSample`. Oldest entry is dropped
/// on insert when full.
#[derive(Debug, Clone)]
pub struct MetricsRing {
    capacity: usize,
    samples: VecDeque<MetricsSample>,
}

impl MetricsRing {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring capacity must be > 0");
        Self {
            capacity,
            samples: VecDeque::with_capacity(capacity),
        }
    }

    /// Convenience: 5s ring with default capacity.
    pub fn ring_5s() -> Self {
        Self::new(RING_5S_CAPACITY)
    }

    /// Convenience: 1m ring.
    pub fn ring_1m() -> Self {
        Self::new(RING_1M_CAPACITY)
    }

    /// Convenience: 5m ring.
    pub fn ring_5m() -> Self {
        Self::new(RING_5M_CAPACITY)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.samples.len() == self.capacity
    }

    /// Insert a new sample. Drops the oldest if at capacity.
    pub fn push(&mut self, sample: MetricsSample) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Iterate samples oldest-to-newest.
    pub fn iter(&self) -> impl Iterator<Item = &MetricsSample> {
        self.samples.iter()
    }

    /// Render the entire ring as a CSV body (header NOT included). One
    /// row per sample, oldest first. Newline-separated; trailing newline.
    pub fn to_csv_body(&self) -> String {
        let mut out = String::new();
        for s in self.samples.iter() {
            out.push_str(&s.to_csv_row());
            out.push('\n');
        }
        out
    }

    /// Render the ring with the canonical header prepended.
    pub fn to_csv_with_header(&self) -> String {
        let mut out = String::from(csv_header());
        out.push('\n');
        out.push_str(&self.to_csv_body());
        out
    }

    /// Clear all samples but keep capacity.
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_at(t: u64) -> MetricsSample {
        MetricsSample {
            timestamp_ms: t,
            hashrate_ths: 100.5,
            wall_watts: 3000,
            max_chip_temp_c: 65.0,
            max_pcb_temp_c: 50.0,
            max_fan_pwm: 30,
            error_rate: 0.001,
            accepted_shares_delta: 5,
            rejected_shares_delta: 0,
            power_source: MetricsPowerSource::Pmbus,
            power_modeled: false,
            power_calibrated: false,
        }
    }

    #[test]
    fn csv_header_columns_match_to_csv_row_count() {
        let s = sample_at(0);
        let header_cols = csv_header().split(',').count();
        let row_cols = s.to_csv_row().split(',').count();
        assert_eq!(header_cols, row_cols, "header/row column count mismatch");
        assert_eq!(header_cols, 12);
    }

    #[test]
    fn to_csv_row_renders_canonical_format() {
        let s = sample_at(1_700_000_000_000);
        let row = s.to_csv_row();
        assert!(row.starts_with("1700000000000,100.50,3000,65.0,50.0,30,0.0010,5,0"));
        assert!(row.ends_with(",pmbus,false,false"));
    }

    #[test]
    fn ring_capacities_match_luxos_constants() {
        // Pin LuxOS-derived capacity values so a refactor doesn't
        // accidentally change the 1-minute / 1-hour / 24-hour windows.
        assert_eq!(RING_5S_CAPACITY, 12); // 1 minute @ 5s
        assert_eq!(RING_1M_CAPACITY, 60); // 1 hour @ 1m
        assert_eq!(RING_5M_CAPACITY, 288); // 24 hours @ 5m
    }

    #[test]
    fn ring_drops_oldest_on_overflow() {
        let mut r = MetricsRing::new(3);
        r.push(sample_at(1));
        r.push(sample_at(2));
        r.push(sample_at(3));
        assert!(r.is_full());
        r.push(sample_at(4));
        // Oldest (timestamp 1) dropped.
        let timestamps: Vec<u64> = r.iter().map(|s| s.timestamp_ms).collect();
        assert_eq!(timestamps, vec![2, 3, 4]);
    }

    #[test]
    fn ring_5s_holds_one_minute_worth() {
        let mut r = MetricsRing::ring_5s();
        for t in 0..15u64 {
            r.push(sample_at(t * 5_000));
        }
        assert_eq!(r.len(), 12);
        // 3 oldest dropped.
        assert_eq!(r.iter().next().unwrap().timestamp_ms, 3 * 5_000);
    }

    #[test]
    fn ring_iteration_is_oldest_first() {
        let mut r = MetricsRing::new(5);
        r.push(sample_at(10));
        r.push(sample_at(20));
        r.push(sample_at(30));
        let timestamps: Vec<u64> = r.iter().map(|s| s.timestamp_ms).collect();
        assert_eq!(timestamps, vec![10, 20, 30]);
    }

    #[test]
    fn to_csv_body_omits_header() {
        let mut r = MetricsRing::new(2);
        r.push(sample_at(1));
        r.push(sample_at(2));
        let body = r.to_csv_body();
        assert!(!body.contains("timestamp_ms"));
        assert_eq!(body.lines().count(), 2);
    }

    #[test]
    fn to_csv_with_header_includes_header() {
        let mut r = MetricsRing::new(2);
        r.push(sample_at(1));
        let csv = r.to_csv_with_header();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), csv_header());
        assert!(lines.next().unwrap().starts_with("1,"));
    }

    #[test]
    fn clear_resets_length_but_keeps_capacity() {
        let mut r = MetricsRing::new(3);
        r.push(sample_at(1));
        r.push(sample_at(2));
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.capacity(), 3);
    }

    #[test]
    fn empty_ring_renders_empty_csv_body() {
        let r = MetricsRing::new(5);
        assert_eq!(r.to_csv_body(), "");
    }

    #[test]
    fn sample_round_trips_through_serde_json() {
        let s = sample_at(123);
        let json = serde_json::to_string(&s).unwrap();
        let back: MetricsSample = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
