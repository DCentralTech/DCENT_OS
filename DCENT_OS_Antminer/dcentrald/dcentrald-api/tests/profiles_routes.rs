//! Integration tests for the W8-D profile-import REST endpoints.
//!
//! These tests drive the silicon-profile router in-process via
//! `tower::ServiceExt::oneshot` so they don't bind a real TCP socket
//! and don't need the daemon's full HAL/AppState wiring. The
//! registry is the process-wide `OnceLock<RwLock<ProfileRegistry>>`
//! exposed by `dcentrald_silicon_profiles::registry::global`; tests
//! redirect the disk root via the `DCENTRALD_PROFILE_DIR` environment
//! variable that the handler module honors.
//!
//! Test count: 6 (per W8-D spec).
//! Test 1: list returns ≥24 (post-W8-B baked + vendor migration)
//! Test 2: get-by-id round-trip
//! Test 3: import validates + writes to disk
//! Test 4: import rejects SECURE_BOOT_SET-tainted bundle
//! Test 5: delete LiveConfirmed → 403 live_confirmed_immutable
//! Test 6: reload after disk change picks up new file
//!
//! Linux/CI only — `dcentrald-api` pulls Unix-only HAL crates.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use dcentrald_api::routes::profiles;
use dcentrald_api::AppState;
use dcentrald_api_types::chip_init::ChipFamily;
use dcentrald_api_types::power_profile_preset::MinerModel;
use dcentrald_api_types::OperatingMode;
use dcentrald_silicon_profiles::registry::{
    self, global, ProfileBundle, ProfileMetadata, ProfileSourceMetadata,
};
use dcentrald_silicon_profiles::{Profile, ProfileSource};

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Serialize all tests in this file. The endpoints touch a
/// process-global registry and a process-global env var, so parallel
/// execution would create flaky cross-test interference. Each test
/// takes this lock for its full duration and resets the env var on
/// drop.
fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Generate a unique temp profile root + redirect the handler module
/// at it via the `DCENTRALD_PROFILE_DIR` env var. Returns the root.
fn temp_profile_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("dcentrald-w8d-{}-{}-{}", label, pid, nanos));
    std::fs::create_dir_all(&root).expect("create profile root");
    std::fs::create_dir_all(root.join("operator")).ok();
    std::fs::create_dir_all(root.join("baked")).ok();
    std::env::set_var("DCENTRALD_PROFILE_DIR", &root);
    root
}

/// Reset the global registry to empty for a clean test slate.
fn reset_registry() {
    let mut g = global().write().expect("registry lock");
    *g = registry::ProfileRegistry::new();
}

/// Hydrate the global registry from the configured disk root.
fn reload_from_disk(root: &std::path::Path) {
    let mut g = global().write().expect("registry lock");
    let _ = g.reload(root);
}

fn write_bundle_to_disk(root: &std::path::Path, subdir: &str, name: &str, bundle: &ProfileBundle) {
    let dir = root.join(subdir);
    std::fs::create_dir_all(&dir).expect("create subdir");
    let path = dir.join(format!("{}.json", name));
    let bytes = serde_json::to_vec_pretty(bundle).expect("serialize bundle");
    std::fs::write(&path, bytes).expect("write bundle");
}

fn make_bundle(
    model: MinerModel,
    hashboard: &str,
    chip: ChipFamily,
    source_class: ProfileSource,
) -> ProfileBundle {
    ProfileBundle {
        schema_version: 1,
        miner_model: model,
        hashboard: hashboard.to_string(),
        chip,
        source: ProfileSourceMetadata {
            vendor: "test-w8d".into(),
            firmware_version: "1.0.0".into(),
            extracted_from_sha256: "0".repeat(64),
            extraction_date: "2026-05-04".into(),
            extracted_by: Some("w8d-test".into()),
        },
        source_class,
        presets: vec![Profile {
            step: 0,
            freq_mhz: 500,
            voltage_v: 8.0,
            wall_watts: Some(900),
            hashrate_ths: Some(10.0),
            source: source_class,
        }],
        metadata: ProfileMetadata::default(),
    }
}

/// Build a router + its `AppState` at the given operating mode. The state's
/// default hardware identity is Unknown, so the CE-103/CE-121 mutation guards
/// fail closed (409) until `grant_beta_identity` is called on the returned
/// `AppState`. Both the router and the state share the same `Arc`, so mutating
/// the returned state is visible to the in-flight handler.
fn build_router_with_mode(mode: OperatingMode) -> (axum::Router, Arc<AppState>) {
    use dcentrald_api::{
        build_minimal_app_state, ApiConfig, MinimalAppStateInputs, NetworkBlockConfig,
    };

    let state = build_minimal_app_state(MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode,
        firmware_version: "w8d-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    });
    let router = profiles::router().with_state(state.clone());
    (router, state)
}

/// Build a router + `AppState` in Standard mode (the common case).
fn build_router() -> (axum::Router, Arc<AppState>) {
    build_router_with_mode(OperatingMode::Standard)
}

/// Set a Beta-anchor hardware identity (S9 / BM1387 at `exact` confidence) so
/// the CE-103/CE-121 capability guard grants the `ConfigRw` + `AsicOptions`
/// mutating capabilities. Mirrors the daemon identity resolver's S9 anchor —
/// see `rest::antminer_beta_anchor` / `antminer_grants_mutating_capabilities`.
fn grant_beta_identity(state: &Arc<AppState>) {
    let mut hw = state.hardware_info.lock().expect("hardware_info lock");
    hw.chip_type = "BM1387".to_string();
    hw.identification = dcentrald_api::HardwareIdentification::from_evidence(
        vec![
            dcentrald_api::HardwareIdentityEvidence::declared_asic_board_target("am1-s9", "BM1387"),
            dcentrald_api::HardwareIdentityEvidence::measured_asic_enumeration(
                0x1387,
                "BM1387",
                dcentrald_api::HardwareCompositionToken::new(1, "test:am1-s9"),
            ),
        ],
        Some("test S9 enumeration evidence".to_string()),
    );
}

async fn body_to_value(resp: axum::response::Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    if bytes.is_empty() {
        return (status, Value::Null);
    }
    let value = serde_json::from_slice(&bytes).expect("parse json");
    (status, value)
}

fn multipart_body(field_name: &str, payload: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----dcentrald-w8d-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{}\"; filename=\"profile.json\"\r\n",
            field_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    body.extend_from_slice(payload);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    (content_type, body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test 1 — `GET /api/profiles/silicon` lists every profile bundle
/// loaded into the registry. After hydrating the registry from the
/// W8-B-migrated baked + vendor JSON files (24 expected; 9 baked + 15
/// vendor), the endpoint returns the same count.
#[tokio::test]
async fn test_list_profiles_returns_post_w8b_migration_bundles() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("list");
    reset_registry();

    // Synthesize a 24-bundle catalog matching the W8-B post-migration
    // shape (9 baked LiveConfirmed + 15 vendor VendorExtracted).
    let baked = [
        (MinerModel::AntminerS9, ChipFamily::Bm1387, "BHB-S9-generic"),
        (
            MinerModel::AntminerS17,
            ChipFamily::Bm1397,
            "BHB-S17-generic",
        ),
        (MinerModel::AntminerS19, ChipFamily::Bm1398, "BHB42501"),
        (
            MinerModel::AntminerS19jProA,
            ChipFamily::Bm1362,
            "BHB-S19jPro-generic",
        ),
        (MinerModel::AntminerS19kPro, ChipFamily::Bm1366, "BHB56902"),
        (MinerModel::AntminerS21, ChipFamily::Bm1368, "BHB68606"),
        (
            MinerModel::AntminerS21Pro,
            ChipFamily::Bm1370,
            "BHB-S21-Pro",
        ),
        (
            MinerModel::AntminerL3Plus,
            ChipFamily::Bm1485,
            "BHB-L3-Plus",
        ),
        (MinerModel::AntminerL7, ChipFamily::Bm1489, "BHB-L7"),
    ];
    for (model, chip, hb) in baked.iter() {
        let bundle = make_bundle(*model, hb, *chip, ProfileSource::LiveConfirmed);
        write_bundle_to_disk(
            &root,
            "baked",
            &format!("{:?}-{}", model, hb).to_lowercase(),
            &bundle,
        );
    }
    for i in 0..15 {
        let hb = format!("BHB426{:02}", i);
        let bundle = make_bundle(
            MinerModel::AntminerS19jProA,
            &hb,
            ChipFamily::Bm1362,
            ProfileSource::VendorExtracted,
        );
        write_bundle_to_disk(&root, "vendor", &format!("vendor-{}", i), &bundle);
    }

    reload_from_disk(&root);
    assert_eq!(global().read().unwrap().len(), 24, "expected 24 bundles");

    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profiles/silicon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", body);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 24, "expected 24 summaries, got {}", arr.len());

    // Spot-check shape of one summary.
    let first = &arr[0];
    for field in [
        "id",
        "miner_model",
        "hashboard",
        "chip",
        "source_class",
        "preset_count",
    ] {
        assert!(first.get(field).is_some(), "missing field {}", field);
    }
}

/// Test 2 — `GET /api/profiles/silicon/<id>` returns the full
/// `ProfileBundle` for the requested id.
#[tokio::test]
async fn test_get_profile_by_id_returns_full_bundle() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("get");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS9,
        "BHB-S9-generic",
        ChipFamily::Bm1387,
        ProfileSource::OperatorConfirmed,
    );
    write_bundle_to_disk(&root, "operator", "test", &bundle);
    reload_from_disk(&root);

    let id = "antminer_s9__BHB-S9-generic__bm1387__operator_confirmed";
    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/profiles/silicon/{}", id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", body);
    assert_eq!(body.get("schema_version").and_then(Value::as_u64), Some(1));
    assert_eq!(
        body.get("hashboard").and_then(Value::as_str),
        Some("BHB-S9-generic")
    );
    assert_eq!(body.get("chip").and_then(Value::as_str), Some("bm1387"));
    assert_eq!(
        body.get("source_class").and_then(Value::as_str),
        Some("operator_confirmed")
    );

    // Unknown id → 404.
    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profiles/silicon/antminer_s9__nope__bm1387__live_confirmed")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Test 3 — `POST /api/profiles/silicon/import` validates the bundle
/// JSON, writes it to `<root>/operator/<filename>.json`, and reloads
/// the registry. The new bundle is then visible via
/// `GET /api/profiles/silicon`.
#[tokio::test]
async fn test_import_profile_validates_then_writes() {
    let _guard = test_lock().lock().unwrap();
    let _root = temp_profile_root("import");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    let payload = serde_json::to_vec(&bundle).expect("serialize");
    let (content_type, body) = multipart_body("profile", &payload);

    let (app, state) = build_router();
    grant_beta_identity(&state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/import")
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CREATED, "body={:?}", body);
    let id = body.get("id").and_then(Value::as_str).expect("id");
    assert!(
        id.contains("antminer_s19j_pro_a") && id.contains("BHB42601"),
        "id={}",
        id
    );
    let path = body.get("path").and_then(Value::as_str).expect("path");
    assert!(
        std::path::Path::new(path).exists(),
        "expected on-disk file at {}",
        path
    );

    // Reload + list → bundle is visible.
    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profiles/silicon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (_status, body) = body_to_value(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected exactly 1 profile after import");
}

/// Test 4 — `POST /api/profiles/silicon/import` rejects bundles
/// flagged with `metadata.secure_boot_set_seen=true`. The endpoint
/// surfaces the blocklist rejection from
/// `dcentrald_silicon_profiles::registry::validate` as a 400.
#[tokio::test]
async fn test_import_rejects_secure_boot_set_tainted() {
    let _guard = test_lock().lock().unwrap();
    let _root = temp_profile_root("secure-boot");
    reset_registry();

    let mut bundle = make_bundle(
        MinerModel::AntminerS21,
        "BHB68606",
        ChipFamily::Bm1368,
        ProfileSource::VendorExtracted,
    );
    bundle.metadata.secure_boot_set_seen = true;
    let payload = serde_json::to_vec(&bundle).expect("serialize");
    let (content_type, body) = multipart_body("profile", &payload);

    let (app, state) = build_router();
    grant_beta_identity(&state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/import")
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = body.get("error").and_then(Value::as_str).expect("error");
    assert!(
        err.contains("SECURE_BOOT_SET") || err.contains("secure_boot_set"),
        "error did not cite SECURE_BOOT_SET: {}",
        err
    );
}

/// Test 5 — `DELETE /api/profiles/silicon/<id>` for a `LiveConfirmed`
/// bundle returns 403 with `error: "live_confirmed_immutable"`.
#[tokio::test]
async fn test_delete_live_confirmed_returns_403() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("delete-live");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS9,
        "BHB-S9-generic",
        ChipFamily::Bm1387,
        ProfileSource::LiveConfirmed,
    );
    write_bundle_to_disk(&root, "baked", "live", &bundle);
    reload_from_disk(&root);

    let id = "antminer_s9__BHB-S9-generic__bm1387__live_confirmed";
    let (app, state) = build_router();
    grant_beta_identity(&state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/profiles/silicon/{}", id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let err = body.get("error").and_then(Value::as_str).expect("error");
    assert_eq!(err, "live_confirmed_immutable");
}

/// Test 6 — `POST /api/profiles/silicon/reload` re-reads the on-disk
/// directory. After dropping a new bundle into `<root>/operator/`,
/// the reload picks it up and the list endpoint shows it.
#[tokio::test]
async fn test_reload_after_disk_change() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("reload");
    reset_registry();
    reload_from_disk(&root);
    assert_eq!(global().read().unwrap().len(), 0, "registry not empty");

    // Drop a bundle directly to disk (no import endpoint).
    let bundle = make_bundle(
        MinerModel::AntminerS19,
        "BHB42501",
        ChipFamily::Bm1398,
        ProfileSource::VendorExtracted,
    );
    write_bundle_to_disk(&root, "vendor", "drop-in", &bundle);

    let (app, state) = build_router();
    grant_beta_identity(&state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/reload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", body);
    assert_eq!(body.get("loaded").and_then(Value::as_u64), Some(1));
    assert_eq!(body.get("skipped").and_then(Value::as_u64), Some(0));

    // List → bundle is visible.
    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profiles/silicon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (_status, body) = body_to_value(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected the dropped-in bundle to appear");
}

// ---------------------------------------------------------------------------
// CE-103 / CE-121 — capability + mode fail-closed guards
// ---------------------------------------------------------------------------

/// Count JSON files that landed under `<root>/operator/`. Used to prove the
/// guards reject BEFORE any disk write on the import path.
fn operator_json_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join("operator"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0)
}

/// CE-103 — `POST /api/profiles/silicon/import-json` with the default
/// (Unknown-identity) state fails closed with 409 (unknown_hardware) and
/// writes nothing to disk.
#[tokio::test]
async fn test_import_json_unknown_identity_returns_409_no_write() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("import-json-409");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    let body = serde_json::json!({ "bundle": serde_json::to_value(&bundle).unwrap() });

    // Default state → Unknown identity → no ConfigRw grant.
    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/import-json")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "expected fail-closed 409");
    assert_eq!(
        operator_json_count(&root),
        0,
        "guard must reject before writing to disk"
    );
}

/// CE-121 — `PUT /api/profiles/silicon/active` with the default state fails
/// closed with 409 (AsicOptions not granted).
#[tokio::test]
async fn test_set_active_unknown_identity_returns_409() {
    let _guard = test_lock().lock().unwrap();
    let _root = temp_profile_root("set-active-409");
    reset_registry();

    let (app, _state) = build_router();
    let put_body = serde_json::json!({
        "model": "antminer_s9",
        "hashboard": "BHB-S9-generic",
        "profile_id": "antminer_s9__BHB-S9-generic__bm1387__operator_confirmed",
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/profiles/silicon/active")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&put_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "expected fail-closed 409");
}

/// CE-103 — `DELETE /api/profiles/silicon/:id` with the default state fails
/// closed with 409 before the LiveConfirmed-immutable / not-found checks.
#[tokio::test]
async fn test_delete_unknown_identity_returns_409() {
    let _guard = test_lock().lock().unwrap();
    let _root = temp_profile_root("delete-409");
    reset_registry();

    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(
                    "/api/profiles/silicon/antminer_s9__BHB-S9-generic__bm1387__operator_confirmed",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "expected fail-closed 409");
}

/// CE-103 — `POST /api/profiles/silicon/reload` with the default state fails
/// closed with 409.
#[tokio::test]
async fn test_reload_unknown_identity_returns_409() {
    let _guard = test_lock().lock().unwrap();
    let _root = temp_profile_root("reload-409");
    reset_registry();

    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/reload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "expected fail-closed 409");
}

/// CE-121 — even with a Beta-anchor identity granted, `import` is refused in
/// Home mode (StandardOrHigher endpoint) with 403 and writes nothing to disk.
#[tokio::test]
async fn test_import_home_mode_returns_403_no_write() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("import-home-403");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    let payload = serde_json::to_vec(&bundle).expect("serialize");
    let (content_type, body) = multipart_body("profile", &payload);

    // Capability passes (identity granted) so the *mode* guard is what rejects.
    let (app, state) = build_router_with_mode(OperatingMode::Home);
    grant_beta_identity(&state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/profiles/silicon/import")
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _body) = body_to_value(resp).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "Home mode must refuse import"
    );
    assert_eq!(
        operator_json_count(&root),
        0,
        "mode guard must reject before writing to disk"
    );
}

/// CE-103 — reads stay open: `GET /api/profiles/silicon` returns 200 even with
/// no hardware identity resolved (Home dashboard must keep listing profiles).
#[tokio::test]
async fn test_read_list_stays_open_without_identity() {
    let _guard = test_lock().lock().unwrap();
    let root = temp_profile_root("read-open");
    reset_registry();
    reload_from_disk(&root);

    let (app, _state) = build_router();
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/profiles/silicon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "reads must stay open in any mode");
    assert!(body.as_array().is_some(), "list returns a JSON array");
}
