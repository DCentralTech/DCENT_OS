//! W13.D1 — `/api/boot/phase` + `/api/boot/timeline` routes.
//!
//! - `GET /api/boot/phase` — current cold-boot substate (read-only).
//!   Returns `404 NOT FOUND` when the boot-phase tracker was never started
//!   (`started_at_unix_ms` is `None` — the common case on a healthy,
//!   already-running unit where no cold-boot orchestrator published a phase).
//!   404 lets the dashboard synthesize the TRUE state from `/api/status` and
//!   hide the boot strip instead of showing a permanent false "Booting".
//!   Returns the real phase (200) once an orchestrator has published. Per
//!   .
//! - `GET /api/boot/timeline` — bounded ring of recent transitions.
//!   Dev-mode only — gated on `ApiConfig::expose_boot_timeline`. Returns
//!   `404 NOT FOUND` when disabled (default) so LAN scanners can't
//!   fingerprint per-boot timing.
//!
//! The current phase comes from the `BootPhaseTracker` watch channel held
//! in `AppState::boot_phase_tracker`. Cold-boot orchestrators publish into
//! the tracker via `tracker.publish(BootPhase::...)`. W13.D1 ships only
//! the API surface — orchestrator wiring deferred to W14+.
//!
//! Cross-references:
//!   - See `~/
//!   - See `~/
//!   -

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use dcentrald_api_types::boot_phase::{BootPhaseResponse, BootTimelineResponse};

use crate::AppState;

/// Build the boot-phase sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/boot/phase", get(get_boot_phase))
        .route("/api/boot/timeline", get(get_boot_timeline))
}

async fn get_boot_phase(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (phase, started_at) = state.boot_phase_tracker.current();

    // TRUTH CONTRACT: do not assert a boot phase the daemon never measured.
    //
    // `started_at_unix_ms` is `None` until a cold-boot orchestrator calls
    // `tracker.publish(...)`. On a healthy already-running unit (the common
    // case — orchestrator wiring is only present on the cold-boot path), the
    // tracker is never started, so reporting the default `Generic::Booting`
    // here would paint a permanent false "Booting" strip on a fully-mining
    // S9 / S19j Pro. Instead return 404 so the dashboard's tested fallback
    // synthesizes the TRUE state from `/api/status` (mining/idle) and hides
    // the banner..
    //
    // When the tracker IS active (started_at is Some), return the real phase
    // — the cold-boot orchestration UI is intentional and accurate.
    if started_at.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }

    Json(BootPhaseResponse {
        phase,
        started_at_unix_ms: started_at,
        // Reaching this handler means the daemon is up and the API
        // server is responding — is_live is true. The
        // hybrid-mode-no-api / dcentrald-down fallbacks live in
        // `server.py` (synthetic last-known response).
        is_live: true,
    })
    .into_response()
}

async fn get_boot_timeline(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if !state.config.expose_boot_timeline {
        return (StatusCode::NOT_FOUND, "boot timeline not exposed").into_response();
    }
    let entries = state.boot_phase_tracker.timeline_snapshot();
    Json(BootTimelineResponse { entries }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use dcentrald_api_types::boot_phase::{BootPhase, Cv1835BootPhase, GenericBootPhase};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state(expose_timeline: bool) -> Arc<AppState> {
        let mut api_config = crate::ApiConfig::default();
        api_config.expose_boot_timeline = expose_timeline;
        let s = crate::build_minimal_app_state(crate::MinimalAppStateInputs {
            api_config,
            pool_url: "stratum+tcp://example.com:3333".into(),
            pool_protocol: "sv1".into(),
            mode: dcentrald_api_types::OperatingMode::Standard,
            firmware_version: "13.0.0".into(),
            fan_pwm: 10,
            network_block: crate::NetworkBlockConfig::default(),
            profile_path: "/tmp/profiles".into(),
            control_board_label: "am3-aml".into(),
            chip_type_label: "BM1362".into(),
            external_state_rx: None,
        });
        s
    }

    #[tokio::test]
    async fn boot_phase_endpoint_404_when_tracker_never_started() {
        // TRUTH CONTRACT (INST-1): the tracker is never started on a healthy
        // already-running unit, so the endpoint must 404 (not paint a false
        // "Booting" strip). 404 lets the dashboard synthesize the true state
        // from /api/status and hide the banner.
        let state = test_state(false);
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/boot/phase")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn boot_phase_endpoint_reflects_published_cv1835_phase() {
        let state = test_state(false);
        state
            .boot_phase_tracker
            .publish(BootPhase::Cv1835(Cv1835BootPhase::BootMiscCtrlTripleWrite));
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/boot/phase")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Tracker IS started (publish above set started_at) → real phase + 200.
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["phase"]["kind"], "cv1835");
        assert_eq!(parsed["phase"]["phase"], "boot_misc_ctrl_triple_write");
        assert_eq!(parsed["is_live"], true);
        assert!(parsed["started_at_unix_ms"].is_number());
    }

    #[tokio::test]
    async fn boot_timeline_404_when_not_exposed() {
        let state = test_state(false);
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/boot/timeline")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn boot_timeline_returns_entries_when_exposed() {
        let state = test_state(true);
        state
            .boot_phase_tracker
            .publish(BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        state
            .boot_phase_tracker
            .publish(BootPhase::Cv1835(Cv1835BootPhase::BootPicDcDcEnable));
        state
            .boot_phase_tracker
            .publish(BootPhase::Generic(GenericBootPhase::Mining));
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/boot/timeline")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["phase"]["phase"], "boot_psu_init");
        assert_eq!(entries[2]["phase"]["kind"], "generic");
        assert_eq!(entries[2]["phase"]["phase"], "mining");
        // Active entry has no end timestamp.
        assert!(entries[2]["ended_at_unix_ms"].is_null());
        assert!(entries[0]["ended_at_unix_ms"].is_number());
    }
}
