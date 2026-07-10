//! CE-103 / CE-121 — capability + mode fail-closed guards on the V/F profile
//! `POST /api/profile/upload` route (LuxOS `:8080` parity save/restore).
//!
//! The paired `GET /api/profile/download` (read) stays OPEN in every mode and
//! at any identity confidence, so the Home dashboard / fleet tooling can always
//! read the saved profile. Only the mutating `upload` is gated.
//!
//! Mirrors the harness in `profiles_routes.rs`: drives the router in-process via
//! `tower::ServiceExt::oneshot`, and grants a Beta-anchor hardware identity
//! (S9 / BM1387 at `exact` confidence) so the capability guard passes.
//!
//! Linux/CI only — `dcentrald-api` pulls Unix-only HAL crates.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use dcentrald_api::routes::vf_profile;
use dcentrald_api::AppState;
use dcentrald_api_types::OperatingMode;

/// Unique scratch dir under the OS temp dir (no external tempfile crate). Used
/// as the `profile_path` so the `vf_profile_active.json` write lands somewhere
/// disposable and can be existence-checked for the fail-closed assertions.
fn scratch_dir(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "dcent_vf_guard_{}_{}_{}",
        tag,
        std::process::id(),
        nanos
    ));
    p.to_string_lossy().into_owned()
}

/// Build an `AppState` at the given mode + `profile_path`. Default hardware
/// identity is Unknown so the CE-103/CE-121 guard fails closed until
/// `grant_beta_identity` is called.
fn build_state(mode: OperatingMode, profile_path: &str) -> Arc<AppState> {
    use dcentrald_api::{
        build_minimal_app_state, ApiConfig, MinimalAppStateInputs, NetworkBlockConfig,
    };

    build_minimal_app_state(MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode,
        firmware_version: "vf-guard-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: profile_path.to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    })
}

/// Fresh router bound to `state` (oneshot consumes a router, so rebuild per
/// request against the same shared state).
fn router(state: &Arc<AppState>) -> axum::Router {
    vf_profile::router().with_state(state.clone())
}

/// Grant the S9 / BM1387 Beta anchor at `exact` confidence so the capability
/// guard grants `ConfigRw`.
fn grant_beta_identity(state: &Arc<AppState>) {
    let mut hw = state.hardware_info.lock().expect("hardware_info lock");
    hw.chip_type = "BM1387".to_string();
    hw.identification.confidence = "exact".to_string();
}

async fn status_of(resp: axum::response::Response) -> StatusCode {
    resp.status()
}

/// The persisted V/F profile filename (mirrors `vf_profile::VF_PROFILE_FILENAME`).
fn vf_file(profile_path: &str) -> std::path::PathBuf {
    std::path::Path::new(profile_path).join(vf_profile::VF_PROFILE_FILENAME)
}

/// CE-103 — `POST /api/profile/upload` with the default (Unknown-identity)
/// state fails closed with 409 and writes no `vf_profile_active.json`.
#[tokio::test]
async fn test_upload_unknown_identity_returns_409_no_write() {
    let dir = scratch_dir("upload-409");
    let state = build_state(OperatingMode::Standard, &dir);

    let resp = router(&state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profile/upload")
                .header("content-type", "application/json")
                .body(Body::from(br#"{"freq_mhz":525}"#.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        status_of(resp).await,
        StatusCode::CONFLICT,
        "expected fail-closed 409"
    );
    assert!(
        !vf_file(&dir).exists(),
        "guard must reject before writing vf_profile_active.json"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// CE-121 — even with a Beta identity granted, `upload` is refused in Home mode
/// (StandardOrHigher endpoint) with 403 and writes nothing.
#[tokio::test]
async fn test_upload_home_mode_returns_403_no_write() {
    let dir = scratch_dir("upload-home-403");
    let state = build_state(OperatingMode::Home, &dir);
    grant_beta_identity(&state);

    let resp = router(&state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profile/upload")
                .header("content-type", "application/json")
                .body(Body::from(br#"{"freq_mhz":525}"#.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        status_of(resp).await,
        StatusCode::FORBIDDEN,
        "Home mode must refuse upload"
    );
    assert!(
        !vf_file(&dir).exists(),
        "mode guard must reject before writing vf_profile_active.json"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// CE-121 — with a Beta identity in Standard mode the upload succeeds (200) and
/// the saved profile round-trips byte-identical through `download`.
#[tokio::test]
async fn test_granted_standard_upload_then_download_byte_identical() {
    let dir = scratch_dir("upload-rt");
    let state = build_state(OperatingMode::Standard, &dir);
    grant_beta_identity(&state);

    // Deliberately irregular whitespace so a parse→reserialize path would
    // change the bytes; verbatim storage must not.
    let original: &[u8] = br#"{ "schema": 1,
  "voltage_mv": 1320,   "freq_mhz": 525,
            "per_chip": [500, 505, 510] }"#;

    let resp = router(&state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profile/upload")
                .header("content-type", "application/json")
                .body(Body::from(original.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "granted upload must succeed");

    // Download stays open and returns the ORIGINAL bytes unchanged.
    let resp = router(&state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profile/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let back = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        back.as_ref(),
        original,
        "download must be byte-identical to upload"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// CE-103 — the read path stays OPEN: `GET /api/profile/download` with no
/// identity resolved and no profile saved returns 404 `no_profile` (the honest
/// "nothing saved yet" response), NOT the 409 fail-closed of the mutation guard.
#[tokio::test]
async fn test_download_stays_open_without_identity() {
    let dir = scratch_dir("download-open");
    let state = build_state(OperatingMode::Home, &dir);

    let resp = router(&state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profile/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // 404 (no profile) — decisively NOT 409 (which would mean the read got
    // gated by the mutation guard).
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "download must stay open (no_profile 404), not fail-closed 409"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body.get("error").and_then(|v| v.as_str()),
        Some("no_profile")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
