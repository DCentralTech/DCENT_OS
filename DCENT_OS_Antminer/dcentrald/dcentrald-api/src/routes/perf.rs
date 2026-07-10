//! W9.4 — J/TH calibration loop REST endpoints.
//!
//! Routes:
//!   - `POST /api/perf/calibrate` — accept an external wattmeter reading
//!     while the miner is running at a known operating point. The handler
//!     records `(measured_wall_watts, current_hashrate_ths, now_ms)` into
//!     the persistent `PowerCalibration` with `operator_confirmed = true`,
//!     then bakes the multiplier so the daemon's modeled wall watts snap
//!     to the operator's reading. Subsequent reads of
//!     `GET /api/perf/efficiency` return `source = operator` until the
//!     operator clears the calibration.
//!   - `GET /api/perf/efficiency` — return the canonical J/TH report with
//!     `(j_per_th, source, confidence, measured_at)`. The source ladder is
//!     `operator` (best) → `pmbus` → `model` (worst). Confidence is
//!     classified by `EfficiencyConfidence::classify`.
//!
//! The route module is intentionally thin: the underlying state lives in
//! `state.power_calibration` (an `Arc<RwLock<PowerCalibration>>`) and the
//! live power estimate watch channel `state.power_rx`. Calibration
//! persistence reuses `persist_power_calibration` from `rest.rs` so the
//! existing `POST /api/config/power-calibration` and the new
//! `POST /api/perf/calibrate` write to the same `[power.calibration]`
//! TOML section. They are not redundant: `power-calibration` is a
//! generic wall-meter anchor for the power model, and `perf/calibrate`
//! is the operator-supplied J/TH source-of-truth for
//! `TuneTarget::EfficiencyJTH`. Both share the same persisted struct,
//! the perf endpoint additionally records `operator_confirmed = true`
//! and the hashrate snapshot at calibration time.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use dcentrald_api_types::perf_efficiency::{
    EfficiencyConfidence, EfficiencyReport, EfficiencySource,
};

use crate::rest::{, PowerTelemetryProjection};
use crate::AppState;

/// POST body for `/api/perf/calibrate`.
#[derive(Debug, Deserialize)]
pub(crate) struct CalibrateRequest {
    /// Operator-measured wall watts (e.g. Kill-A-Watt, P3 P4400, smart
    /// plug with metering). Required when `enabled` is true / unset.
    /// Range-checked to [50.0, 5000.0] to reject obvious typos.
    pub measured_wall_watts: Option<f64>,
    /// Optional override of the hashrate snapshot used as the divisor.
    /// When omitted, the handler reads the live `MinerState.hashrate_ghs`
    /// and converts to TH/s. Useful for fleet calibration where the
    /// caller already knows the steady-state hashrate.
    #[serde(default)]
    pub hashrate_ghs: Option<f64>,
    /// When `Some(false)`, clear the operator-confirmed calibration and
    /// return the daemon to modeled output. Default true.
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/perf/calibrate", post(post_calibrate))
        .route("/api/perf/efficiency", get(get_efficiency))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn calibration_power_estimates(
    projection: &PowerTelemetryProjection,
    measured_wall_watts: f64,
) -> (f64, f64) {
    if projection.live_power_available && projection.wall_watts > 0 && projection.board_watts > 0 {
        (projection.wall_watts as f64, projection.board_watts as f64)
    } else {
        // Cold path: live telemetry is absent, so do not calibrate against
        // static fallback watts. Anchor the model directly to the operator
        // meter and use an APW12-class board-watt estimate until telemetry
        // catches up.
        (measured_wall_watts, measured_wall_watts * 0.93)
    }
}

/// Whether the projection is backed by a MEASURED power source. Classifies via
/// the shared [`dcentrald_autotuner::PowerAuthorityKind`] model
/// (`PowerTelemetryProjection::is_measured`) instead of re-matching the
/// `source_detail` string against a single literal — the old `== "pmbus_measured"`
/// compare silently downgraded ADC-measured power (`source_detail =
/// "adc_measured"`) to a modeled estimate.
fn measured_power_source(projection: &PowerTelemetryProjection) -> bool {
    projection.is_measured()
}

fn efficiency_report_from_projection(
    calibration: &dcentrald_autotuner::PowerCalibration,
    projection: &PowerTelemetryProjection,
    jth_target_active: bool,
    now_ms: u64,
) -> EfficiencyReport {
    if let Some(operator_jth) = calibration.operator_confirmed_jth() {
        let age_ms = calibration
            .updated_at_ms
            .map(|then| now_ms.saturating_sub(then));
        let confidence = EfficiencyConfidence::classify(EfficiencySource::Operator, age_ms);
        return EfficiencyReport {
            j_per_th: Some(operator_jth),
            source: EfficiencySource::Operator,
            confidence,
            measured_at_ms: calibration.updated_at_ms,
            operator_wall_watts: calibration.,
            operator_hashrate_ths: calibration.confirmed_hashrate_ths,
            jth_target_active,
        };
    }

    if projection.live_power_available
        && projection.efficiency_jth.is_finite()
        && projection.efficiency_jth > 0.0
    {
        let source = if measured_power_source(projection) {
            EfficiencySource::Pmbus
        } else {
            EfficiencySource::Model
        };
        return EfficiencyReport {
            j_per_th: Some(projection.efficiency_jth),
            source,
            confidence: EfficiencyConfidence::classify(source, None),
            measured_at_ms: if source == EfficiencySource::Pmbus {
                Some(now_ms)
            } else {
                None
            },
            operator_wall_watts: None,
            operator_hashrate_ths: None,
            jth_target_active,
        };
    }

    let mut r = EfficiencyReport::unknown();
    r.jth_target_active = jth_target_active;
    r
}

/// `POST /api/perf/calibrate` — bake an external wattmeter reading into
/// the active profile as `OperatorConfirmed`.
async fn post_calibrate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CalibrateRequest>,
) -> impl IntoResponse {
    // Disable / clear path.
    if matches!(body.enabled, Some(false)) {
        if let Err(e) = crate::rest::persist_power_calibration(None) {
            return Json(serde_json::json!({
                "status": "error",
                "message": e,
            }));
        }
        if let Ok(mut guard) = state.power_calibration.write() {
            *guard = dcentrald_autotuner::PowerCalibration::default();
        }
        crate::rest::push_rest_audit_free(
            &state,
            "perf_calibrate",
            "Operator J/TH calibration cleared",
        );
        return Json(serde_json::json!({
            "status": "ok",
            "message": "Operator J/TH calibration cleared. Efficiency falls back to PMBus or modeled estimate.",
            "enabled": false,
        }));
    }

    // Validate watts.
    let measured_wall_watts = body.measured_wall_watts.unwrap_or(0.0);
    if !(50.0..=5000.0).contains(&measured_wall_watts) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "measured_wall_watts must be between 50W and 5000W. Use a real wall meter — typos here will mistune the autotuner cost function.",
        }));
    }

    // Resolve hashrate (TH/s). Use override when supplied; else live snapshot.
    let live_state = state.state_rx.borrow().clone();
    let live_hashrate_ghs = live_state.hashrate_ghs;
    let hashrate_ghs = body.hashrate_ghs.unwrap_or(live_hashrate_ghs);
    let hashrate_ths = hashrate_ghs / 1000.0;
    if !(0.05..=10_000.0).contains(&hashrate_ths) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Live hashrate is not stable yet. Let the miner run for a few minutes at the operating point you want to anchor before calibrating.",
            "live_hashrate_ths": hashrate_ths,
        }));
    }

    // Snapshot the live power estimate so we can compute the multiplier.
    let live_power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|info| info.clone())
        .unwrap_or_default();
    let power_projection = (&live_power, &live_state, &hardware);
    let (estimated_wall_watts, estimated_board_watts) =
        calibration_power_estimates(&power_projection, measured_wall_watts);

    let multiplier = (measured_wall_watts / estimated_wall_watts).clamp(0.5, 1.5);

    let calibration = dcentrald_autotuner::PowerCalibration {
        enabled: true,
        multiplier,
        : Some(measured_wall_watts),
        estimated_wall_watts: Some(estimated_wall_watts),
        estimated_board_watts: Some(estimated_board_watts),
        updated_at_ms: Some(now_ms()),
        operator_confirmed: true,
        confirmed_hashrate_ths: Some(hashrate_ths),
    };

    if let Err(e) = crate::rest::persist_power_calibration(Some(&calibration)) {
        return Json(serde_json::json!({
            "status": "error",
            "message": e,
        }));
    }

    if let Ok(mut guard) = state.power_calibration.write() {
        *guard = calibration.clone();
    }

    let j_per_th = calibration.operator_confirmed_jth().unwrap_or(0.0);

    crate::rest::push_rest_audit_free(
        &state,
        "perf_calibrate",
        format!(
            "Operator J/TH calibration: {:.1} W / {:.2} TH/s = {:.1} J/TH (multiplier {:.4})",
            measured_wall_watts, hashrate_ths, j_per_th, multiplier
        ),
    );

    Json(serde_json::json!({
        "status": "ok",
        "message": "Operator J/TH calibration saved. EfficiencyJTH tuner mode now uses this as the source of truth.",
        "enabled": true,
        "operator_confirmed": true,
        "measured_wall_watts": measured_wall_watts,
        "hashrate_ths": hashrate_ths,
        "j_per_th": j_per_th,
        "multiplier": calibration.effective_multiplier(),
        "measured_at_ms": calibration.updated_at_ms,
        "source": "operator",
    }))
}

/// `GET /api/perf/efficiency` — canonical J/TH report with source enum +
/// confidence band. The dashboard EarningsPage uses this to render the
/// J/TH headline with a green/amber/grey-italic source tag.
async fn get_efficiency(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let calibration = state
        .power_calibration
        .read()
        .map(|cal| cal.clone())
        .unwrap_or_default();
    let live_power = state.power_rx.borrow().clone();
    let miner = state.state_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|info| info.clone())
        .unwrap_or_default();
    let power_projection = (&live_power, &miner, &hardware);
    let autotuner = state.autotuner_status_rx.borrow().clone();

    // The autotuner publishes the active runtime objective via
    // `AutotunerRuntimeStatus.policy.active_objective` (see
    // `dcentrald_autotuner::AutotunerPolicyStatus`). We use that to set
    // `jth_target_active` so the dashboard can show a JTH badge near the
    // J/TH headline. Falls back to `false` when the policy snapshot is
    // unavailable (cold boot, autotuner disabled).
    let jth_target_active = autotuner
        .policy
        .as_ref()
        .and_then(|p| p.active_objective.as_deref())
        .map(|obj| obj == "efficiency_jth")
        .unwrap_or(false);

    let report = efficiency_report_from_projection(
        &calibration,
        &power_projection,
        jth_target_active,
        now_ms(),
    );

    Json(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_autotuner::PowerCalibration;

    fn projection(
        source_detail: &'static str,
        live_power_available: bool,
        efficiency_jth: f64,
    ) -> PowerTelemetryProjection {
        let source = match source_detail {
            "pmbus_measured" => "pmbus",
            "adc_measured" => "adc",
            "wall_calibrated_estimate" => "estimated",
            "live_runtime_model" => "estimated",
            _ => "static_model_fallback",
        };
        PowerTelemetryProjection {
            board_watts: 1218,
            wall_watts: 1310,
            efficiency_jth,
            btu_h: 4470.0,
            source: source.to_string(),
            source_detail,
            live_power_available,
            modeled: !matches!(source_detail, "pmbus_measured" | "adc_measured"),
            calibrated: false,
            calibration_multiplier: None,
            note: "test projection",
        }
    }

    #[test]
    fn calibrate_request_default_enabled_is_none() {
        let body: CalibrateRequest = serde_json::from_str("{}").unwrap();
        assert!(body.enabled.is_none());
        assert!(body.measured_wall_watts.is_none());
        assert!(body.hashrate_ghs.is_none());
    }

    #[test]
    fn operator_confirmed_jth_round_trip() {
        let cal = PowerCalibration {
            enabled: true,
            multiplier: 1.0,
            : Some(1310.0),
            estimated_wall_watts: Some(1310.0),
            estimated_board_watts: Some(1218.0),
            updated_at_ms: Some(1_700_000_000_000),
            operator_confirmed: true,
            confirmed_hashrate_ths: Some(13.5),
        };
        let jth = cal
            .operator_confirmed_jth()
            .expect("operator confirmed jth");
        assert!((jth - 97.04).abs() < 0.5, "jth={}", jth);
    }

    #[test]
    fn operator_confirmed_jth_returns_none_when_disabled() {
        let cal = PowerCalibration {
            enabled: false,
            operator_confirmed: true,
            : Some(1310.0),
            confirmed_hashrate_ths: Some(13.5),
            ..Default::default()
        };
        assert!(cal.operator_confirmed_jth().is_none());
    }

    #[test]
    fn operator_confirmed_jth_returns_none_when_not_operator_confirmed() {
        // enabled=true (e.g. via /api/config/power-calibration) but not
        // operator-confirmed via /api/perf/calibrate. The J/TH endpoint
        // must NOT mislabel this as `source = operator`.
        let cal = PowerCalibration {
            enabled: true,
            multiplier: 1.05,
            : Some(1310.0),
            confirmed_hashrate_ths: None,
            ..Default::default()
        };
        assert!(cal.operator_confirmed_jth().is_none());
    }

    #[test]
    fn operator_confirmed_jth_rejects_zero_hashrate() {
        let cal = PowerCalibration {
            enabled: true,
            operator_confirmed: true,
            : Some(1310.0),
            confirmed_hashrate_ths: Some(0.0),
            ..Default::default()
        };
        assert!(cal.operator_confirmed_jth().is_none());
    }

    #[test]
    fn calibration_uses_operator_meter_when_projection_is_static_fallback() {
        let static_projection = projection("static_power_fallback_from_miner_state", false, 80.0);

        let (wall, board) = calibration_power_estimates(&static_projection, 1310.0);

        assert_eq!(wall, 1310.0);
        assert!((board - 1218.3).abs() < 0.01, "board={board}");
    }

    #[test]
    fn calibration_uses_live_projection_when_available() {
        let live_projection = projection("live_runtime_model", true, 80.0);

        let (wall, board) = calibration_power_estimates(&live_projection, 1500.0);

        assert_eq!(wall, 1310.0);
        assert_eq!(board, 1218.0);
    }

    #[test]
    fn efficiency_static_fallback_stays_unknown_even_with_modeled_jth() {
        let static_projection = projection("static_power_fallback_from_miner_state", false, 80.0);

        let report = efficiency_report_from_projection(
            &PowerCalibration::default(),
            &static_projection,
            true,
            1_700_000_000_000,
        );

        assert_eq!(report.source, EfficiencySource::Model);
        assert_eq!(report.confidence, EfficiencyConfidence::Low);
        assert_eq!(report.j_per_th, None);
        assert_eq!(report.measured_at_ms, None);
        assert!(report.jth_target_active);
    }

    #[test]
    fn efficiency_measured_projection_reports_pmbus_high_confidence() {
        let measured_projection = projection("pmbus_measured", true, 97.0);

        let report = efficiency_report_from_projection(
            &PowerCalibration::default(),
            &measured_projection,
            false,
            1_700_000_000_000,
        );

        assert_eq!(report.source, EfficiencySource::Pmbus);
        assert_eq!(report.confidence, EfficiencyConfidence::High);
        assert_eq!(report.j_per_th, Some(97.0));
        assert_eq!(report.measured_at_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn efficiency_live_runtime_model_reports_low_confidence_model() {
        let live_model_projection = projection("live_runtime_model", true, 97.0);

        let report = efficiency_report_from_projection(
            &PowerCalibration::default(),
            &live_model_projection,
            false,
            1_700_000_000_000,
        );

        assert_eq!(report.source, EfficiencySource::Model);
        assert_eq!(report.confidence, EfficiencyConfidence::Low);
        assert_eq!(report.j_per_th, Some(97.0));
        assert_eq!(report.measured_at_ms, None);
    }

    #[test]
    fn efficiency_wall_calibrated_estimate_reports_low_confidence_model() {
        let calibrated_projection = projection("wall_calibrated_estimate", true, 97.0);

        let report = efficiency_report_from_projection(
            &PowerCalibration::default(),
            &calibrated_projection,
            false,
            1_700_000_000_000,
        );

        assert_eq!(report.source, EfficiencySource::Model);
        assert_eq!(report.confidence, EfficiencyConfidence::Low);
        assert_eq!(report.j_per_th, Some(97.0));
        assert_eq!(report.measured_at_ms, None);
    }

    #[test]
    fn adc_measured_projection_classifies_as_measured() {
        // PROVENANCE PIN: ADC (board current monitor) is a MEASURED source per
        // `PowerAuthorityKind::is_measured()`. The old `== "pmbus_measured"`
        // string compare silently downgraded it to a modeled/Model estimate;
        // it must now classify as measured (the pmbus-class measured bucket,
        // High confidence) — not Low-confidence Model.
        let adc_projection = projection("adc_measured", true, 97.0);
        assert!(
            adc_projection.is_measured(),
            "adc_measured must classify as measured via PowerAuthorityKind"
        );

        let report = efficiency_report_from_projection(
            &PowerCalibration::default(),
            &adc_projection,
            false,
            1_700_000_000_000,
        );

        assert_eq!(report.source, EfficiencySource::Pmbus);
        assert_eq!(report.confidence, EfficiencyConfidence::High);
        assert_eq!(report.j_per_th, Some(97.0));
        assert_eq!(report.measured_at_ms, Some(1_700_000_000_000));
    }
}
