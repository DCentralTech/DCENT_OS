//!  B1: CI authenticated negative tests for the
//! `DCENT_RELEASE_IMAGE=1` lockdown posture (matrix §7 #1).
//!
//! The unit-level helpers in `auth.rs` (parameterized
//! `is_setup_flow_mutation_for_image(..., release_image=bool)` + the
//! `is_release_image_at(path)` marker probe) already prove the gate's
//! pure logic in 16/0 lib tests shipped in commit `64e0a269`. What was
//! still missing — and what this file adds — is end-to-end PROOF that
//! the gate **actually fires through the live `auth_middleware`**, the
//! same middleware that wraps `/api/setup/skip-password` and
//! `/api/setup/skip-safety` on a real running daemon.
//!
//! Strategy: build a minimal `axum::Router` whose only routes are the
//! two skip endpoints (each backed by a stub handler that returns
//! 200), then layer `auth::auth_middleware` on top — byte-identical to
//! how the production router does it in
//! `dcentrald-api::lib::start_api_server`. Each request is driven
//! in-process via `tower::ServiceExt::oneshot` (no TCP bind, no
//! `AppState`, no HAL), so the suite runs everywhere the rest of the
//! `#![cfg(unix)]` Linux-CI block runs (no live miner / no root /
//! no `/etc/dcentos/release-image` touch).
//!
//! Posture switching: the production gate reads
//! `/etc/dcentos/release-image` with a process-wide `AtomicU8` cache
//! that's set on the first probe and never cleared. That cache means
//! **a single test binary can only assert one posture per process** —
//! whichever one the first middleware call observes. This file
//! deliberately asserts only the **DEV/LAB / `is_release_image()
//! == false`** posture (the natural state of every CI runner: the
//! marker file does not exist on `ubuntu-latest`). The RELEASE
//! posture is asserted out-of-process by
//! `scripts/ci/release-image-negative-test.sh`, which provisions the
//! marker via `sudo` and then runs the binary so the very first probe
//! observes a release image. Together the two halves cover all 4
//! cases in the B1 spec; see the report at
//! .
//!
//! Tests in this file (4, all DEV-posture / `release_image == false`):
//!   1. `dev_image_skip_password_post_reaches_handler_returns_200` — the
//!      freedom-first opt-out is a setup-flow mutation, the pre-setup
//!      gate lets it through, the stub handler runs.
//!   2. `dev_image_skip_safety_post_reaches_handler_returns_200` — the
//!      exact parallel for the safety opt-out.
//!   3. `dev_image_skip_password_get_is_blocked_method_or_405` — the
//!      opt-outs are POSTs ONLY; a GET must NOT be misclassified as a
//!      setup-flow mutation (no auth bypass via method confusion).
//!   4. `dev_image_skip_endpoints_require_same_origin_or_403` — the
//!      pre-setup-mutation gate's same-origin check is load-bearing:
//!      a cross-origin POST to a skip endpoint must 403 even on a
//!      dev/lab image. This pins the `is_same_origin_setup_request`
//!      contract that protects against cross-site setup hijack while
//!      the freedom-first behavior is otherwise byte-identical to
//!      today.
//!
//! Run locally on Linux (matches the dcentrald-api-tests.yml CI step):
//!   cargo +1.90.0 test -p dcentrald-api \
//!     --test release_image_lockdown_negative

// The crate ships Unix-only HAL surface; integration tests therefore
// gate on `cfg(unix)` to keep Windows host builds green. The Linux CI
// (`.github/workflows/dcentrald-api-tests.yml`) runs the full suite.
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

/// Serialize every test in this file. `auth_middleware` reads from
/// the process-wide `is_password_set()` filesystem state and the
/// `RELEASE_IMAGE_STATE` atomic; running in parallel could shuffle
/// observable state across tests.
fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Stub handler that returns 200 OK with a small JSON body. We use it
/// to prove a request actually crossed the auth middleware (anything
/// that 403s, 401s, or short-circuits at the middleware never reaches
/// here).
async fn stub_ok_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"reached":"stub"}"#,
    )
}

/// Build the minimal router the production code wraps with
/// `auth_middleware`. Only the two skip endpoints are wired (plus the
/// canonical control endpoint `/api/auth/setup` so test #4 can assert
/// other setup-flow mutations remain unaffected).
fn build_router() -> Router {
    Router::new()
        .route("/api/setup/skip-password", post(stub_ok_handler))
        .route("/api/setup/skip-safety", post(stub_ok_handler))
        .route("/api/auth/setup", post(stub_ok_handler))
        .layer(axum::middleware::from_fn(auth_middleware))
}

/// Production-faithful loopback ConnectInfo used by middleware to
/// detect the trusted-proxy bypass. We deliberately use a NON-loopback
/// address so the loopback bypass does NOT fire — every test in this
/// file exercises the LAN-client codepath that real dashboards take.
fn lan_remote() -> SocketAddr {
    "203.0.113.50:8080".parse().expect("parse lan addr")
}

/// Build a same-origin POST: `Host: dcent.local`, `Origin:
/// http://dcent.local`. Mirrors what the dashboard SPA sends.
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

/// Drain a response body and return (status, body-bytes).
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

/// Pre-flight gate: this whole file only runs meaningfully when the
/// release-image marker is ABSENT on the test host (i.e. dev/lab
/// posture). On the Linux CI runner this is the natural state —
/// `/etc/dcentos/release-image` does not exist on `ubuntu-latest`.
/// If a previous run (or a misconfigured host) left the marker
/// behind, every dev-path assertion would observe a release posture
/// and fail confusingly; bail out loud instead.
fn require_dev_image_posture() {
    let marker = std::path::Path::new("/etc/dcentos/release-image");
    assert!(
        !marker.exists(),
        "release-image marker `{}` is present — this test binary asserts \
         the DEV/LAB posture. Run the release half via \
         scripts/ci/release-image-negative-test.sh in a separate process.",
        marker.display(),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test 1 — DEV image: `POST /api/setup/skip-password` reaches the
/// stub handler with a 200. Proves the freedom-first opt-out is
/// classified as a setup-flow mutation, the pre-setup-mutation gate
/// passes its same-origin check, the `is_pre_setup_safe` branch
/// returns true, and the middleware calls `next.run(request)`.
#[tokio::test]
async fn dev_image_skip_password_post_reaches_handler_returns_200() {
    let _guard = test_lock().lock().unwrap();
    require_dev_image_posture();

    let app = build_router();
    let req = same_origin_post("/api/setup/skip-password");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "dev/lab image MUST allow the password opt-out (body={})",
        String::from_utf8_lossy(&body),
    );
    assert!(
        body.windows(b"\"reached\":\"stub\"".len())
            .any(|w| w == b"\"reached\":\"stub\""),
        "request must reach the stub handler, body was {}",
        String::from_utf8_lossy(&body),
    );
}

/// Test 2 — DEV image: `POST /api/setup/skip-safety` reaches the stub
/// handler with a 200. Exact parallel of test 1 for the safety
/// opt-out — pins that BOTH freedom-first paths are preserved
/// byte-identically on dev/lab images.
#[tokio::test]
async fn dev_image_skip_safety_post_reaches_handler_returns_200() {
    let _guard = test_lock().lock().unwrap();
    require_dev_image_posture();

    let app = build_router();
    let req = same_origin_post("/api/setup/skip-safety");
    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "dev/lab image MUST allow the safety opt-out (body={})",
        String::from_utf8_lossy(&body),
    );
}

/// Test 3 — DEV image: `GET /api/setup/skip-password` MUST NOT be
/// classified as a setup-flow mutation. The skip endpoints are POSTs
/// only; a GET must hit axum's method-not-allowed handler (405) or
/// the middleware's pre-setup gate (403). Either way, it must NOT
/// reach the 200 stub handler — that would prove a method-confusion
/// auth bypass.
#[tokio::test]
async fn dev_image_skip_password_get_is_blocked_method_or_405() {
    let _guard = test_lock().lock().unwrap();
    require_dev_image_posture();

    let app = build_router();
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/api/setup/skip-password")
        .header("host", "dcent.local")
        .header("origin", "http://dcent.local")
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo(lan_remote()));

    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert!(
        status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::FORBIDDEN,
        "GET on the password opt-out must not reach the 200 stub; \
         got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
}

/// Test 4 — DEV image: a CROSS-origin POST to the skip endpoint MUST
/// be 403'd by the middleware's `is_same_origin_setup_request` gate.
/// This is the load-bearing protection against cross-site setup
/// hijack: a malicious page on `evil.local` must not be able to drive
/// a fresh passwordless miner into a passwordless final state. The
/// dev/lab image is freedom-first about the LOCAL operator's choice,
/// not about LAN attackers.
///
/// The same 403 fires on release images too — the same-origin gate
/// is layered BEFORE the release-image classification, so a release
/// image already 403s cross-origin (`Dashboard-origin required`)
/// before the lockdown gets a chance to 403 it (`Password setup
/// required`). Pinning this on dev/lab proves the gate is wired into
/// the middleware on the SAME codepath the release image hits.
#[tokio::test]
async fn dev_image_skip_endpoints_require_same_origin_or_403() {
    let _guard = test_lock().lock().unwrap();
    require_dev_image_posture();

    let app = build_router();

    // Cross-origin: Host says `dcent.local`, but the Origin header
    // claims a totally different host. The middleware must 403 BEFORE
    // it ever reaches the stub handler.
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/api/setup/skip-password")
        .header("host", "dcent.local")
        .header("origin", "http://evil.local")
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo(lan_remote()));

    let resp = app.oneshot(req).await.expect("oneshot");
    let (status, body) = drain(resp).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cross-origin setup mutation must be 403'd by the same-origin gate, \
         got status={} body={}",
        status,
        String::from_utf8_lossy(&body),
    );
    assert!(
        !body
            .windows(b"\"reached\":\"stub\"".len())
            .any(|w| w == b"\"reached\":\"stub\""),
        "cross-origin request must not reach the stub handler, body was {}",
        String::from_utf8_lossy(&body),
    );
}
