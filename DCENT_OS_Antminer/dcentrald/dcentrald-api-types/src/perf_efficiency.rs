//! W9.4 — J/TH efficiency contract DTOs (HAL-free).
//!
//! These types are the wire shape for `GET /api/perf/efficiency` and the
//! `POST /api/perf/calibrate` response body. Kept HAL-free so the
//! dashboard, fleet manager, dcent-toolbox, and external CI can depend on
//! the contract without pulling in `dcentrald-hal`, async runtimes, or
//! Linux-only sysfs paths.
//!
//! # Source-of-truth ladder
//!
//! J/TH can come from three sources, ordered from most to least
//! authoritative:
//!
//! 1. `Operator` — operator measured wall watts at a known operating point
//!    via an external wattmeter and submitted them via
//!    `POST /api/perf/calibrate`. This is the production source of truth
//!    used by `TuneTarget::EfficiencyJTH`.
//! 2. `Pmbus` — derived from APW12 / smart-PSU read_power telemetry. Real
//!    measurement, but limited to platforms with PMBus-capable PSUs.
//! 3. `Model` — pure C_eff voltage/frequency model with no live anchor.
//!    This is the cold-boot fallback before the operator has run a
//!    wattmeter calibration AND no PMBus telemetry is available.

use serde::{Deserialize, Serialize};

/// J/TH source enum surfaced to the dashboard. Matches the `source`
/// discriminant on `GET /api/perf/efficiency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EfficiencySource {
    /// Operator-confirmed via external wattmeter calibration.
    Operator,
    /// Derived from PMBus / smart-PSU telemetry (APW12 read_power, etc.).
    Pmbus,
    /// Modeled from C_eff * V^2 / k with no live measurement anchor.
    Model,
}

impl EfficiencySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Pmbus => "pmbus",
            Self::Model => "model",
        }
    }
}

/// Confidence band attached to the J/TH report.
///
/// - `High` — operator-confirmed within the last 7 days, or live PMBus.
/// - `Medium` — operator-confirmed but stale (>7 days), or PMBus-derived
///   without recent operator confirmation.
/// - `Low` — pure model output, cold boot, or operating point has drifted
///   substantially from where the calibration was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EfficiencyConfidence {
    High,
    Medium,
    Low,
}

impl EfficiencyConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Derive the confidence label from `(source, age_ms)` where `age_ms`
    /// is the milliseconds since the calibration / measurement was
    /// recorded. Pure function, host-safe.
    pub fn classify(source: EfficiencySource, age_ms: Option<u64>) -> Self {
        const SEVEN_DAYS_MS: u64 = 7 * 24 * 60 * 60 * 1000;
        match source {
            EfficiencySource::Operator => match age_ms {
                Some(age) if age <= SEVEN_DAYS_MS => Self::High,
                Some(_) => Self::Medium,
                None => Self::Medium,
            },
            EfficiencySource::Pmbus => Self::High,
            EfficiencySource::Model => Self::Low,
        }
    }
}

/// Wire shape for `GET /api/perf/efficiency`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfficiencyReport {
    /// Joules per terahash (lower is better). `null` when neither a
    /// modeled estimate nor live telemetry is available (e.g. cold boot
    /// before the autotuner has produced any chip profiles).
    pub j_per_th: Option<f64>,
    /// Source enum (`operator` / `pmbus` / `model`).
    pub source: EfficiencySource,
    /// Confidence band (`high` / `medium` / `low`).
    pub confidence: EfficiencyConfidence,
    /// Unix-epoch milliseconds when the underlying measurement /
    /// calibration was recorded. `None` for pure modeled output.
    pub measured_at_ms: Option<u64>,
    /// When `source = operator`, the operator-supplied wattmeter reading
    /// in watts. Surfaced so the dashboard can show the receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_wall_watts: Option<f64>,
    /// When `source = operator`, the hashrate (TH/s) at the moment of
    /// calibration. Used to compute the operator-confirmed J/TH
    /// (`j_per_th = operator_wall_watts / operator_hashrate_ths`, guarding
    /// `operator_hashrate_ths > 0` against div-by-zero).
    ///
    /// UNIT: terahash/s — NOT GH/s. Assigning a GH/s reading here is the 1000x
    /// GHS↔THS hazard (gap-swarm G62): it would deflate J/TH by 1000x and
    /// mis-drive the `TuneTarget::EfficiencyJTH` objective. Convert any GH/s
    /// source via `ghs_to_ths` before populating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_hashrate_ths: Option<f64>,
    /// Whether `TuneTarget::EfficiencyJTH` is currently the active runtime
    /// objective on the autotuner. Pure metadata for the dashboard badge.
    #[serde(default)]
    pub jth_target_active: bool,
}

impl EfficiencyReport {
    /// Construct an "unknown" report — used at cold boot before any
    /// estimate is ready. Always returns `Low` confidence, `Model`
    /// source, no measurement timestamp.
    pub fn unknown() -> Self {
        Self {
            j_per_th: None,
            source: EfficiencySource::Model,
            confidence: EfficiencyConfidence::Low,
            measured_at_ms: None,
            operator_wall_watts: None,
            operator_hashrate_ths: None,
            jth_target_active: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_operator_within_7_days_is_high() {
        let high = EfficiencyConfidence::classify(EfficiencySource::Operator, Some(1_000));
        assert_eq!(high, EfficiencyConfidence::High);
    }

    #[test]
    fn confidence_operator_after_7_days_is_medium() {
        // 8 days
        let stale_ms = 8 * 24 * 60 * 60 * 1000;
        let med = EfficiencyConfidence::classify(EfficiencySource::Operator, Some(stale_ms));
        assert_eq!(med, EfficiencyConfidence::Medium);
    }

    #[test]
    fn confidence_pmbus_is_high() {
        assert_eq!(
            EfficiencyConfidence::classify(EfficiencySource::Pmbus, None),
            EfficiencyConfidence::High
        );
    }

    #[test]
    fn confidence_model_is_low() {
        assert_eq!(
            EfficiencyConfidence::classify(EfficiencySource::Model, None),
            EfficiencyConfidence::Low
        );
    }

    #[test]
    fn source_strings_round_trip() {
        assert_eq!(EfficiencySource::Operator.as_str(), "operator");
        assert_eq!(EfficiencySource::Pmbus.as_str(), "pmbus");
        assert_eq!(EfficiencySource::Model.as_str(), "model");
    }

    #[test]
    fn unknown_report_is_low_confidence_model_source() {
        let r = EfficiencyReport::unknown();
        assert_eq!(r.source, EfficiencySource::Model);
        assert_eq!(r.confidence, EfficiencyConfidence::Low);
        assert!(r.j_per_th.is_none());
        assert!(r.measured_at_ms.is_none());
        assert!(!r.jth_target_active);
    }

    #[test]
    fn report_serializes_with_lowercase_enum() {
        let r = EfficiencyReport {
            j_per_th: Some(75.5),
            source: EfficiencySource::Operator,
            confidence: EfficiencyConfidence::High,
            measured_at_ms: Some(1_700_000_000_000),
            operator_wall_watts: Some(1310.0),
            operator_hashrate_ths: Some(13.5),
            jth_target_active: true,
        };
        let s = serde_json::to_string(&r).expect("json");
        assert!(s.contains("\"source\":\"operator\""));
        assert!(s.contains("\"confidence\":\"high\""));
        assert!(s.contains("\"jth_target_active\":true"));
    }
}
