//! `POST /api/autotuner/quota` — hashrate split-quota planner.
//!
//! ePIC UMC OS V1.18.2 shipped "hashrate split quota controls
//! (API/Web)" (see
//! ).
//! This is the DCENT_OS analog endpoint. It is the API surface for the
//! `dcentrald::autotune::tuner_mode::TunerMode::HashrateQuota` variant:
//! the operator supplies EITHER a `fraction` (0.01..=1.0 of the rated
//! max hashrate) OR an `absolute_ths` ceiling, and the handler returns
//! the resolved TH/s, the equivalent wattage target, and the exact
//! `[autotune]` TOML block to persist.
//!
//! ## Why this is a *planner* (read-only), not a mutator
//!
//! It mirrors the existing `GET /api/autotuner/target` philosophy:
//! report the resolved mapping + the canonical TOML; the operator (or
//! `dcent tune --hashrate-quota`) persists it and restarts the daemon.
//! The endpoint never touches the running tuner — so it cannot move
//! voltage/frequency/fan at all, and there is no clamp surface to
//! bypass here.
//!
//! ## Safety contract (load-bearing — keep in lock-step with the Rust
//! `TunerMode::HashrateQuota` resolution in
//! `dcentrald/src/autotune/tuner_mode.rs`)
//!
//! 1. The quota is a **cap**: the resolved TH/s is `min(req, rated)`
//!    and the derived wattage is clamped to `rated_max_watts`. A quota
//!    can NEVER request an overclock.
//! 2. `home_mode` defaults to `true` and is forwarded verbatim into
//!    the emitted `[autotune]` block; the actual fan ≤30 PWM /
//!    14_500 mV / ±5 MHz-slew clamps live in the daemon's
//!    `PowerTargetController` (the `HashrateQuota` variant delegates to
//!    it) and are NOT re-implemented here. This endpoint emits the
//!    same `mode = "hashrate-quota"` block the Rust side deserializes;
//!    the controller enforces the clamps at tick time regardless of
//!    this payload.
//! 3. Invalid input (fraction out of `[0.01, 1.0]`, non-positive
//!    `absolute_ths`/`rated_*`, neither field set) returns HTTP 400
//!    with a reason — it never silently produces a 0 W (disabled) or
//!    over-rated target.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde::Deserialize;

use crate::AppState;

/// Lower bound on the quota fraction. Mirrors
/// `tuner_mode::HASHRATE_QUOTA_MIN_FRACTION`.
const HASHRATE_QUOTA_MIN_FRACTION: f64 = 0.01;
/// Conservative S9-class defaults — mirror
/// `tuner_mode::DEFAULT_RATED_MAX_{THS,WATTS}`.
const DEFAULT_RATED_MAX_THS: f64 = 14.0;
const DEFAULT_RATED_MAX_WATTS: u32 = 1400;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/autotuner/quota", post(post_autotuner_quota))
}

/// POST body for `/api/autotuner/quota`. Supply exactly one of
/// `fraction` / `absolute_ths`. The `rated_*` fields default to a
/// conservative S9-class envelope when omitted.
#[derive(Debug, Deserialize)]
pub(crate) struct QuotaRequest {
    /// Fraction of `rated_max_ths` (0.01..=1.0). Mutually exclusive
    /// with `absolute_ths`; if both set, `absolute_ths` wins.
    #[serde(default)]
    pub fraction: Option<f64>,
    /// Explicit absolute TH/s ceiling. Mutually exclusive with
    /// `fraction`.
    #[serde(default)]
    pub absolute_ths: Option<f64>,
    /// Rated max hashrate (the 100% reference for `fraction`).
    #[serde(default)]
    pub rated_max_ths: Option<f64>,
    /// Rated max wall/chip wattage (the `ths→watts` slope endpoint).
    #[serde(default)]
    pub rated_max_watts: Option<u32>,
    /// Home / space-heater posture. Defaults to `true` (a quota cap is
    /// a home demand-response feature). Forwarded into the emitted
    /// `[autotune]` block.
    #[serde(default)]
    pub home_mode: Option<bool>,
}

fn bad(reason: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "status": "error", "message": reason })),
    )
        .into_response()
}

/// Resolve a quota request to `(resolved_ths, target_watts,
/// home_mode)`. Pure — no AppState read, no daemon contact. Returns
/// `Err(reason)` for any invalid input. This is the byte-for-byte
/// arithmetic twin of `TunerMode::step_hashrate_quota`'s resolution
/// (minus the PI delegation, which is daemon-side).
pub(crate) fn resolve_quota(
    fraction: Option<f64>,
    absolute_ths: Option<f64>,
    rated_max_ths: f64,
    rated_max_watts: u32,
) -> Result<(f64, u32), String> {
    if !rated_max_ths.is_finite() || rated_max_ths <= 0.0 {
        return Err("rated_max_ths must be > 0 TH/s".to_string());
    }
    if rated_max_watts == 0 {
        return Err("rated_max_watts must be > 0".to_string());
    }
    let resolved_ths = match (absolute_ths, fraction) {
        (Some(abs), _) => {
            if !abs.is_finite() || abs <= 0.0 {
                return Err("absolute_ths must be > 0 TH/s".to_string());
            }
            abs.min(rated_max_ths)
        }
        (None, Some(frac)) => {
            if !frac.is_finite() || !(HASHRATE_QUOTA_MIN_FRACTION..=1.0).contains(&frac) {
                return Err(format!(
                    "fraction must be in [{:.2}, 1.0], got {}",
                    HASHRATE_QUOTA_MIN_FRACTION, frac
                ));
            }
            frac * rated_max_ths
        }
        (None, None) => {
            return Err("supply exactly one of `fraction` or `absolute_ths`".to_string());
        }
    };
    let watts_per_ths = rated_max_watts as f64 / rated_max_ths;
    let target_watts = (resolved_ths * watts_per_ths).clamp(1.0, rated_max_watts as f64);
    Ok((resolved_ths, target_watts.round() as u32))
}

async fn post_autotuner_quota(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QuotaRequest>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/autotuner/quota", mode) {
        return resp.into_response();
    }

    let rated_max_ths = req.rated_max_ths.unwrap_or(DEFAULT_RATED_MAX_THS);
    let rated_max_watts = req.rated_max_watts.unwrap_or(DEFAULT_RATED_MAX_WATTS);
    let home_mode = req.home_mode.unwrap_or(true);

    let (resolved_ths, target_watts) = match resolve_quota(
        req.fraction,
        req.absolute_ths,
        rated_max_ths,
        rated_max_watts,
    ) {
        Ok(v) => v,
        Err(reason) => return bad(&reason).into_response(),
    };

    let fraction_of_rated = resolved_ths / rated_max_ths;

    // The canonical [autotune] block the operator (or `dcent tune
    // --hashrate-quota`) should persist to /data/dcentrald.toml. The
    // daemon's TunerMode::HashrateQuota delegates this to the gated
    // PowerTargetController — the clamps are enforced there, not here.
    let toml_block = format!(
        "[autotune]\nmode = \"hashrate-quota\"\n{}rated_max_ths = {}\nrated_max_watts = {}\nhome_mode = {}\n",
        match (req.absolute_ths, req.fraction) {
            (Some(abs), _) => format!("absolute_ths = {}\n", abs),
            (None, Some(frac)) => format!("fraction = {}\n", frac),
            (None, None) => String::new(),
        },
        rated_max_ths,
        rated_max_watts,
        home_mode,
    );

    Json(serde_json::json!({
        "status": "ok",
        "mode": "hashrate-quota",
        "resolved": {
            "hashrate_ths": resolved_ths,
            "fraction_of_rated": fraction_of_rated,
            "target_watts": target_watts,
            "rated_max_ths": rated_max_ths,
            "rated_max_watts": rated_max_watts,
            "home_mode": home_mode,
        },
        // Read-only planner: this endpoint does NOT mutate the running
        // tuner. The clamps (≤14500 mV / ≤30 PWM home fan / ±5 MHz
        // slew) are enforced by the daemon's PowerTargetController that
        // TunerMode::HashrateQuota delegates to — never bypassed.
        "applies": false,
        "needs_restart": true,
        "autotune_toml_block": toml_block,
        "note": "Quota is a CAP — resolved TH/s is min(requested, rated); \
                 derived watts clamped to rated_max_watts. Persist the \
                 block and restart dcentrald (or use `dcent tune \
                 --hashrate-quota`). HARD clamps enforced by the \
                 delegated PowerTargetController.",
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_resolves_to_proportional_watts() {
        // 50% of 100 TH/s @ 3000 W rated → 50 TH/s, 1500 W.
        let (ths, w) = resolve_quota(Some(0.5), None, 100.0, 3000).unwrap();
        assert!((ths - 50.0).abs() < 1e-9);
        assert_eq!(w, 1500);
    }

    #[test]
    fn absolute_ths_maps_linearly_and_beats_fraction() {
        // absolute_ths wins over fraction; 30/100 * 2000 = 600 W.
        let (ths, w) = resolve_quota(Some(0.9), Some(30.0), 100.0, 2000).unwrap();
        assert!((ths - 30.0).abs() < 1e-9);
        assert_eq!(w, 600);
    }

    #[test]
    fn quota_is_a_cap_never_an_overclock() {
        // Asking for 150 TH/s on a 100 TH/s / 1000 W miner resolves to
        // the rated ceiling, never above.
        let (ths, w) = resolve_quota(None, Some(150.0), 100.0, 1000).unwrap();
        assert!((ths - 100.0).abs() < 1e-9);
        assert_eq!(w, 1000); // clamped to rated_max_watts
    }

    #[test]
    fn fraction_clamped_to_rated_watts_ceiling() {
        // fraction can't exceed 1.0; 1.0 maps to exactly rated watts.
        let (ths, w) = resolve_quota(Some(1.0), None, 14.0, 1400).unwrap();
        assert!((ths - 14.0).abs() < 1e-9);
        assert_eq!(w, 1400);
    }

    #[test]
    fn rejects_neither_field() {
        assert!(resolve_quota(None, None, 14.0, 1400).is_err());
    }

    #[test]
    fn rejects_out_of_range_fraction() {
        assert!(resolve_quota(Some(0.0), None, 14.0, 1400).is_err());
        assert!(resolve_quota(Some(1.5), None, 14.0, 1400).is_err());
        assert!(resolve_quota(Some(f64::NAN), None, 14.0, 1400).is_err());
    }

    #[test]
    fn rejects_nonpositive_absolute_and_rated() {
        assert!(resolve_quota(None, Some(0.0), 14.0, 1400).is_err());
        assert!(resolve_quota(None, Some(-3.0), 14.0, 1400).is_err());
        assert!(resolve_quota(Some(0.5), None, 0.0, 1400).is_err());
        assert!(resolve_quota(Some(0.5), None, 14.0, 0).is_err());
    }

    #[test]
    fn small_fraction_never_resolves_to_zero_watt_disabled_target() {
        // The minimum legal fraction must still produce a >=1 W target
        // (PowerTargetController treats target_watts==0 as Disabled).
        let (_ths, w) = resolve_quota(Some(HASHRATE_QUOTA_MIN_FRACTION), None, 14.0, 1400).unwrap();
        assert!(w >= 1, "min-fraction quota must not disable the controller");
    }
}
