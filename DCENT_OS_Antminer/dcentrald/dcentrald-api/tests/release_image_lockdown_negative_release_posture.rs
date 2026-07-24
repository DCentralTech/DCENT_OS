//!  B1 (release half): CI authenticated negative tests for the
//! `DCENT_RELEASE_IMAGE=1` lockdown posture (matrix §7 #1).
//!
//! Companion to `release_image_lockdown_negative.rs` (which covers
//! the DEV/LAB half). The two test files run in SEPARATE cargo test
//! binaries (one per `tests/*.rs`) so each one observes the
//! `RELEASE_IMAGE_STATE` atomic cache exactly once — the dev binary
//! sees the natural "marker absent" state, this binary sees a
//! "marker present" state provisioned by `scripts/ci/release-image
//! -negative-test.sh` BEFORE it starts.
//!
//! Posture provisioning (out-of-process):
//!   1. The CI script writes `/etc/dcentos/release-image` (needs
//!      sudo on the CI runner — `ubuntu-latest` has passwordless
//!      sudo).
//!   2. The CI script sets `DCENT_RELEASE_IMAGE_TEST_MARKER=1` so
//!      the tests fail-loud if the marker provisioning silently
//!      regressed.
//!   3. The CI script runs `cargo test --test
//!      release_image_lockdown_negative_release_posture`.
//!   4. On exit (success OR failure) the CI script removes the
//!      marker so subsequent test runs (and the dev half) see a
//!      clean host.
//!
//! Tests in this file (4, all RELEASE-posture / `is_release_image()
//! == true`):
//!   1. `release_image_skip_password_post_returns_403` — the gate
//!      kicks the freedom-first opt-out out of the setup-flow
//!      mutation set, the pre-setup-safe predicate then returns
//!      false, and the middleware 403s with "Password setup
//!      required". The stub handler MUST NOT be reached.
//!   2. `release_image_skip_safety_post_returns_403` — exact parallel
//!      for the safety opt-out.
//!   3. `release_image_auth_setup_still_works_returns_200` —
//!      negative-control: the gate is NARROW. The real
//!      `/api/auth/setup` endpoint (operator setting a password)
//!      is STILL classified as a setup-flow mutation on a release
//!      image. Otherwise the unit could never be onboarded.
//!   4. `release_image_unknown_setup_path_still_403` — second
//!      negative-control: the gate does not relax any other surface.
//!      An unknown `/api/setup/...` path stays rejected.
//!
//! If `DCENT_RELEASE_IMAGE_TEST_MARKER` is unset / not `1`, every
//! test in this file is a no-op (returns early with a printed
//! marker so CI logs make the skip visible). This lets a developer
//! run `cargo test -p dcentrald-api` locally without root and have
//! the dev half run + the release half explicitly skip — instead
//! of failing.
//!
//! Run locally on Linux (release half — needs sudo for the marker):
//!   bash scripts/ci/release-image-negative-test.sh
//!
//! Run inside CI (`.github/workflows/dcentrald-api-tests.yml`): the
//! workflow invokes the script as a separate step.

#![cfg(unix)]

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{post, Router};
use http_body_util::BodyExt;
use tower::ServiceExt;

use dcentrald_api::auth::auth_middleware;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

async fn stub_ok_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"reached":"stub"}"#,
    )
}

fn build_router() -> Router {
    Router::new()
        .route("/api/setup/skip-password", post(stub_ok_handler))
        .route("/api/setup/skip-safety", post(stub_ok_handler))
        .route("/api/auth/setup", post(stub_ok_handler))
        .route("/api/setup/unknown", post(stub_ok_handler))
        .layer(axum::middleware::from_fn(auth_middleware))
}

fn lan_remote() -> SocketAddr {
    "203.0.113.50:8080".parse().expect("parse lan addr")
}

fn same_origin_post(uri: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("host", "dcent.local")
        .header("origin", "http://dcent.local")
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo(lan_remote()));
    req
}

async fn drain(resp: axum::response::Response) -> (StatusCode, Vec<u8>) {
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

/// Gate: this whole test binary only runs meaningfully when the CI
/// wrapper script has provisioned the release-image marker
/// (`/etc/dcentos/release-image`) and set
/// `DCENT_RELEASE_IMAGE_TEST_MARKER=1` so we can fail-loud if the
/// marker is missing despite the env claim.
///
/// Returns `true` when posture provisioning is in place; `false`
/// when the caller should skip (developer running `cargo test`
/// locally without root). Skips print a one-line note so the skip is
/// auditable in CI logs.
fn release_posture_provisioned_or_skip(test_name: &str) -> bool {
    let env_claims_provisioned = std::env::var("DCENT_RELEASE_IMAGE_TEST_MARKER")
        .map(|v| v == "1")
        .unwrap_or(false);

    if !env_claims_provisioned {
        eprintln!(
            "[{}] SKIP — DCENT_RELEASE_IMAGE_TEST_MARKER != 1; \
             release-image lockdown integration tests must be run via \
             scripts/ci/release-image-negative-test.sh (provisions the \
             /etc/dcentos/release-image marker with narrow sudo operations before launching \
             this test binary)",
            test_name,
        );
        return false;
    }

    let marker = std::path::Path::new("/etc/dcentos/release-image");
    assert!(
        marker.exists(),
        "DCENT_RELEASE_IMAGE_TEST_MARKER=1 but `{}` is missing — the CI \
         wrapper did not provision the marker before launching this test \
         binary; refusing to run the release-posture assertions against a \
         dev-posture host (would produce confusing 200 results)",
        marker.display(),
    );

    // Sanity: the gate caches the posture on first probe. We re-import
    // is_release_image() through the public surface to force at least
    // one probe BEFORE the first middleware call so that — even if the
    // test order or runner internals reorder things — the cache reflects
    // the present-marker state from here onward.
    assert!(
        dcentrald_api::auth::is_release_image(),
        "marker present at `{}` but is_release_image() returned false; \
         possible cache-state regression",
        marker.display(),
    );

    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test 1 — RELEASE image: `POST /api/setup/skip-password` is 403'd
/// by the middleware with "Password setup required". The gate
/// declassifies the opt-out as a setup-flow mutation, the
/// pre-setup-safe predicate then returns false, and the middleware
/// short-circuits with a 403 before the stub handler runs.
#[tokio::test]
async fn release_image_skip_password_post_returns_403() {
    let _guard = test_lock().lock().unwrap();
    if !release_posture_provisioned_or_skip("release_image_skip_password_post_returns_403") {
        return;
    }

    let app = build_router();
    let req = same_origin_post("/api/setup/skip-password");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "release image MUST 403 the password opt-out; got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
    assert!(
        !body
            .windows(b"\"reached\":\"stub\"".len())
            .any(|w| w == b"\"reached\":\"stub\""),
        "release-image opt-out must not reach the stub handler, body was {}",
        String::from_utf8_lossy(&body),
    );
}

/// Test 2 — RELEASE image: `POST /api/setup/skip-safety` is 403'd
/// by the middleware. Exact parallel of test 1 for the safety
/// opt-out; pins that BOTH opt-outs are locked down by the gate.
#[tokio::test]
async fn release_image_skip_safety_post_returns_403() {
    let _guard = test_lock().lock().unwrap();
    if !release_posture_provisioned_or_skip("release_image_skip_safety_post_returns_403") {
        return;
    }

    let app = build_router();
    let req = same_origin_post("/api/setup/skip-safety");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "release image MUST 403 the safety opt-out; got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
}

/// Test 3 — RELEASE image: `POST /api/auth/setup` STILL reaches the
/// stub handler (200). Negative control — the gate is narrow. The
/// operator MUST still be able to set a password and onboard a
/// release unit; the gate ONLY removes the passwordless escape hatch.
#[tokio::test]
async fn release_image_auth_setup_still_works_returns_200() {
    let _guard = test_lock().lock().unwrap();
    if !release_posture_provisioned_or_skip("release_image_auth_setup_still_works_returns_200") {
        return;
    }

    let app = build_router();
    let req = same_origin_post("/api/auth/setup");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "release image MUST still allow the password-setting onboarding \
         path; got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
    assert!(
        body.windows(b"\"reached\":\"stub\"".len())
            .any(|w| w == b"\"reached\":\"stub\""),
        "onboarding path must reach the stub handler, body was {}",
        String::from_utf8_lossy(&body),
    );
}

/// Test 4 — RELEASE image: an unknown `/api/setup/...` path stays
/// 403. Second negative control — the release gate does not widen
/// any other surface; unknown setup paths are still not pre-setup
/// mutations.
#[tokio::test]
async fn release_image_unknown_setup_path_still_403() {
    let _guard = test_lock().lock().unwrap();
    if !release_posture_provisioned_or_skip("release_image_unknown_setup_path_still_403") {
        return;
    }

    let app = build_router();
    let req = same_origin_post("/api/setup/unknown");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "unknown setup path must 403 on a release image; got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
}
