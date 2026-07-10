//! GROUP C (W8 parity) — integration tests for the persistent audit-log
//! read-back endpoint `GET /api/audit-log`.
//!
//! Drives the real `routes::audit_log::router()` in-process via
//! `tower::ServiceExt::oneshot` so the test proves the route is actually
//! MOUNTED (not just that the pure reader works), reads the persistent NDJSON
//! file from disk (redirected via `DCENTOS_AUDIT_LOG_PATH`), and serves a
//! redacted, paginated, newest-first response.
//!
//! The W8 gap these tests close: a record persisted BEFORE a reboot (when the
//! in-memory `/api/history/audit` ring is empty) must still be readable.
//!
//! Linux/CI only — `dcentrald-api` pulls Unix-only HAL crates.

#![cfg(unix)]

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use dcentrald_api::routes::audit_log;
use dcentrald_api_types::audit_log::{AuditEvent, AuditRecord};

/// Serialize tests in this file: they share the process-global
/// `DCENTOS_AUDIT_LOG_PATH` env var.
fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Unique temp audit-log path + redirect the reader at it via env var.
fn temp_audit_log(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!(
        "dcentrald-auditlog-{}-{}-{}.log",
        label, pid, nanos
    ));
    let _ = std::fs::remove_file(&path);
    std::env::set_var("DCENTOS_AUDIT_LOG_PATH", &path);
    path
}

fn write_ndjson(path: &PathBuf, records: &[AuditRecord]) {
    let mut f = std::fs::File::create(path).expect("create audit log");
    for r in records {
        f.write_all(r.to_ndjson_line().unwrap().as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
    }
    f.flush().unwrap();
}

fn rec(ts: u64, event: AuditEvent) -> AuditRecord {
    AuditRecord::new(ts, "operator", event)
}

fn build_router() -> axum::Router {
    use dcentrald_api::{
        build_minimal_app_state, ApiConfig, MinimalAppStateInputs, NetworkBlockConfig,
    };
    let state = build_minimal_app_state(MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode: dcentrald_api_types::OperatingMode::Standard,
        firmware_version: "groupc-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    });
    audit_log::router().with_state(state)
}

async fn get(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("parse json")
    };
    (status, value)
}

#[tokio::test]
async fn audit_log_route_is_mounted_and_reads_persistent_file() {
    let _guard = test_lock().lock().unwrap();
    let path = temp_audit_log("mounted");
    // Persist a record, then read it back as if after a reboot (ring empty).
    write_ndjson(
        &path,
        &[
            rec(
                1_000,
                AuditEvent::ModeChange {
                    from: "standard".into(),
                    to: "home".into(),
                },
            ),
            rec(
                2_000,
                AuditEvent::SysupgradeCommitted {
                    version: "0.6.0".into(),
                },
            ),
        ],
    );

    let (status, body) = get(build_router(), "/api/audit-log").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"].as_u64(), Some(2));
    assert_eq!(body["redacted"].as_bool(), Some(true));
    let events = body["events"].as_array().expect("events array");
    assert_eq!(events.len(), 2);
    // Newest first.
    assert_eq!(events[0]["timestamp_ms"].as_u64(), Some(2_000));
    assert_eq!(events[1]["timestamp_ms"].as_u64(), Some(1_000));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn audit_log_route_paginates() {
    let _guard = test_lock().lock().unwrap();
    let path = temp_audit_log("paginate");
    let records: Vec<AuditRecord> = (0..10)
        .map(|i| {
            rec(
                i as u64,
                AuditEvent::Free {
                    category: "test".into(),
                    message: format!("event {i}"),
                },
            )
        })
        .collect();
    write_ndjson(&path, &records);

    // Newest first: 9,8,7,... offset 2 limit 3 → 7,6,5.
    let (status, body) = get(build_router(), "/api/audit-log?offset=2&limit=3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"].as_u64(), Some(10));
    assert_eq!(body["returned"].as_u64(), Some(3));
    let events = body["events"].as_array().unwrap();
    assert_eq!(events[0]["timestamp_ms"].as_u64(), Some(7));
    assert_eq!(events[2]["timestamp_ms"].as_u64(), Some(5));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn audit_log_route_redacts_pool_credentials_from_disk() {
    let _guard = test_lock().lock().unwrap();
    let path = temp_audit_log("redact");
    write_ndjson(
        &path,
        &[rec(
            1,
            AuditEvent::PoolSwitch {
                from: None,
                to: "stratum+tcp://wkr:topsecret@pool.example:3333".into(),
            },
        )],
    );

    let (status, body) = get(build_router(), "/api/audit-log").await;
    assert_eq!(status, StatusCode::OK);
    let serialized = body.to_string();
    assert!(
        !serialized.contains("topsecret"),
        "credential leaked through /api/audit-log: {serialized}"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn audit_log_route_missing_file_is_empty_not_error() {
    let _guard = test_lock().lock().unwrap();
    let path = temp_audit_log("missing");
    let _ = std::fs::remove_file(&path); // ensure absent

    let (status, body) = get(build_router(), "/api/audit-log").await;
    // Fresh install, no events yet — must degrade gracefully, never 500.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"].as_u64(), Some(0));
    assert_eq!(body["returned"].as_u64(), Some(0));
    assert!(body["events"].as_array().unwrap().is_empty());
}
