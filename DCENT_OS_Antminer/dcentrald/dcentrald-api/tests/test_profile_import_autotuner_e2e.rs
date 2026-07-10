//! W13-A Item 2 — profile-import → silicon-profiles registry →
//! autotuner end-to-end pipeline test (closes R6 #5, deferred since
//! wave 9).
//!
//! Pipeline:
//!   1. POST `/api/profiles/silicon/import` — operator imports a JSON
//!      bundle via the multipart endpoint. Handler validates, writes
//!      to `<root>/operator/<filename>.json`, and reloads the
//!      registry.
//!   2. Assert the silicon-profiles registry's
//!      `lookup_bundle(model, hashboard, chip)` returns the imported
//!      bundle.
//!   3. PUT `/api/profiles/silicon/active` — operator selects the
//!      newly imported bundle as the active profile for the chain.
//!      Handler records the selection in the registry's
//!      `active_by_chain` map AND forwards an
//!      `AutoTunerCommand::ApplySiliconProfile` to the live
//!      autotuner via `autotuner_command_tx`.
//!   4. Tick the autotuner one iteration.
//!   5. Assert the autotuner's `active_silicon_profile_id` matches
//!      the imported profile id, AND that the registry's
//!      `get_active_bundle_for_chain` resolves to the imported
//!      bundle's frequency/voltage targets.
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
use tokio::sync::mpsc;
use tower::ServiceExt;

use dcentrald_api::routes::profiles;
use dcentrald_api_types::chip_init::ChipFamily;
use dcentrald_api_types::power_profile_preset::MinerModel;
use dcentrald_asic::drivers::{MinerProfile, PicType};
use dcentrald_asic::dspic::{DSPIC_MAX_VOLTAGE_MV, DSPIC_MIN_VOLTAGE_MV};
use dcentrald_autotuner::{
    config::AutoTunerConfig,
    profile::{ChipGrade, ProfileStats},
    tuner::TunerState,
    AutoTuner, AutoTunerCommand, ChipProfile, FreqCommand, PowerCalibration, TuningProfile,
};
use dcentrald_silicon_profiles::registry::{
    self, global, ProfileBundle, ProfileMetadata, ProfileSourceMetadata,
};
use dcentrald_silicon_profiles::{Profile, ProfileSource};

// ---------------------------------------------------------------------------
// Test harness (mirrors test_autotuner_runtime.rs)
// ---------------------------------------------------------------------------

/// Serialize the tests in this binary because they all mutate the
/// process-global `dcentrald_silicon_profiles::registry::global()` and
/// the `DCENTRALD_PROFILE_DIR` env var.
///
/// Callers MUST acquire this with `.lock().unwrap_or_else(|e|
/// e.into_inner())`, NOT `.lock().unwrap()`. A single test that panics
/// (e.g. on a failing assertion) while holding the guard poisons the
/// `Mutex`; with bare `.unwrap()` every subsequent test then panics
/// with `PoisonError`, turning one real failure into a cascade of
/// false failures and masking the root cause. Recovering the guard via
/// `into_inner()` keeps each test's pass/fail independent.
fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn temp_profile_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("dcentrald-w13a-e2e-{}-{}-{}", label, pid, nanos));
    std::fs::create_dir_all(&root).expect("create profile root");
    std::fs::create_dir_all(root.join("operator")).ok();
    std::fs::create_dir_all(root.join("baked")).ok();
    std::env::set_var("DCENTRALD_PROFILE_DIR", &root);
    root
}

fn reset_registry() {
    let mut g = global().write().expect("registry lock");
    *g = registry::ProfileRegistry::new();
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
            vendor: "test-w13a-e2e".into(),
            firmware_version: "1.0.0".into(),
            extracted_from_sha256: "0".repeat(64),
            extraction_date: "2026-05-05".into(),
            extracted_by: Some("w13a-e2e-test".into()),
        },
        source_class,
        presets: vec![Profile {
            step: 0,
            freq_mhz: 545,
            voltage_v: 1.34,
            wall_watts: Some(900),
            hashrate_ths: Some(10.0),
            source: source_class,
        }],
        metadata: ProfileMetadata::default(),
    }
}

fn make_minimal_inputs() -> dcentrald_api::MinimalAppStateInputs {
    use dcentrald_api::{ApiConfig, MinimalAppStateInputs, NetworkBlockConfig};
    MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode: dcentrald_api_types::OperatingMode::Standard,
        firmware_version: "w13a-e2e".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    }
}

/// CE-103/CE-121: grant the S9 / BM1387 Beta anchor at `exact` confidence so
/// the profile-mutation capability guards (`ConfigRw` on import, `AsicOptions`
/// on set-active) pass. Without this the import/set-active endpoints fail closed
/// with 409 on the default Unknown-identity state.
fn grant_beta_identity(state: &Arc<dcentrald_api::AppState>) {
    let mut hw = state.hardware_info.lock().expect("hardware_info lock");
    hw.chip_type = "BM1387".to_string();
    hw.identification.confidence = "exact".to_string();
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
    let boundary = "----dcentrald-w13a-e2e-boundary";
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

fn build_test_autotuner_runtime_ready() -> (AutoTuner, mpsc::Sender<AutoTunerCommand>) {
    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        AutoTunerConfig::default(),
        500,
        "BM1362".to_string(),
        "dspic".to_string(),
        calibration,
    );
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutoTunerCommand>(8);
    tuner.set_command_receiver(cmd_rx);
    tuner.force_state_for_test(TunerState::BackgroundAdjust);
    (tuner, cmd_tx)
}

#[derive(Debug, Clone, Copy)]
struct W30ChipCase {
    label: &'static str,
    model: MinerModel,
    hashboard: &'static str,
    chip: ChipFamily,
    chip_id: u16,
}

fn model_slug(model: MinerModel) -> String {
    serde_json::to_value(model)
        .expect("serialize model")
        .as_str()
        .expect("model slug")
        .to_string()
}

fn make_w30_bundle(case: W30ChipCase, miner_profile: &MinerProfile) -> ProfileBundle {
    let mut bundle = make_bundle(
        case.model,
        case.hashboard,
        case.chip,
        ProfileSource::OperatorConfirmed,
    );
    // Deliberately above the per-chip-family max so the apply path must clamp.
    bundle.presets[0].freq_mhz = 1000;
    // Use the dcentrald-asic safe/default chain-rail voltage for this family.
    bundle.presets[0].voltage_v = miner_profile.default_voltage_mv as f32 / 1000.0;
    bundle.presets[0].wall_watts = Some(
        miner_profile
            .total_hashrate_ths(miner_profile.default_freq_mhz)
            .round() as u32,
    );
    bundle.presets[0].hashrate_ths =
        Some(miner_profile.total_hashrate_ths(miner_profile.default_freq_mhz) as f32);
    bundle
}

fn make_w30_tuning_profile(chain_id: u8, miner_profile: &MinerProfile) -> TuningProfile {
    let chip_type = format!("BM{:04X}", miner_profile.chip_id);
    TuningProfile {
        version: 4,
        chip_type,
        chain_id,
        chip_count: miner_profile.chips_per_chain,
        voltage_mv: miner_profile.default_voltage_mv,
        tuned_at: "0".to_string(),
        ambient_temp_c: None,
        optimal_voltage_mv: None,
        estimated_power_w: 0.0,
        estimated_efficiency_jth: 0.0,
        equilibrium_temp_c: None,
        thermal_refinement_duration_s: None,
        calibrated_c_eff: None,
        chips: (0..miner_profile.chips_per_chain)
            .map(|i| ChipProfile {
                chip_index: i,
                max_stable_mhz: miner_profile.max_freq_mhz,
                operating_mhz: miner_profile.default_freq_mhz,
                grade: ChipGrade::B,
                error_rate: 0.0,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect(),
        stats: ProfileStats {
            avg_freq_mhz: miner_profile.default_freq_mhz as f64,
            min_freq_mhz: miner_profile.default_freq_mhz,
            max_freq_mhz: miner_profile.default_freq_mhz,
            grade_a: 0,
            grade_b: miner_profile.chips_per_chain as u16,
            grade_c: 0,
            grade_d: 0,
            tuning_duration_s: 10.0,
            estimated_hashrate_ghs: miner_profile
                .chain_hashrate_ghs(miner_profile.default_freq_mhz),
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
        },
        // W13.C3: SKU + flag denormalisation. Test fixture default.
        hashboard_sku: None,
        hashboard_sku_flags: None,
    }
}

fn voltage_bounds_for_profile(
    miner_profile: &MinerProfile,
    config: &AutoTunerConfig,
) -> (u16, u16) {
    match miner_profile.pic_type {
        PicType::DsPic33EP => (
            config.min_voltage_mv.max(DSPIC_MIN_VOLTAGE_MV),
            DSPIC_MAX_VOLTAGE_MV,
        ),
        PicType::Pic16F1704 => (config.min_voltage_mv, 9440),
        PicType::NoPic => (config.min_voltage_mv, 15000),
    }
}

fn build_w30_tuner_for_case(
    case: W30ChipCase,
    miner_profile: &MinerProfile,
    chain_id: u8,
) -> (AutoTuner, mpsc::Sender<AutoTunerCommand>, AutoTunerConfig) {
    let mut config = AutoTunerConfig::default();
    config.max_freq_mhz = miner_profile.max_freq_mhz;
    if miner_profile.pic_type == PicType::DsPic33EP {
        config.min_voltage_mv = config.min_voltage_mv.max(DSPIC_MIN_VOLTAGE_MV);
    }

    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        config.clone(),
        miner_profile.default_freq_mhz,
        format!("BM{:04X}", case.chip_id),
        format!("{:?}", miner_profile.pic_type).to_ascii_lowercase(),
        calibration,
    );
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutoTunerCommand>(8);
    tuner.set_command_receiver(cmd_rx);
    tuner.force_state_for_test(TunerState::BackgroundAdjust);
    tuner.install_profile_for_test(chain_id, make_w30_tuning_profile(chain_id, miner_profile));
    (tuner, cmd_tx, config)
}

async fn run_dispatcher_mock(
    mut freq_rx: mpsc::Receiver<FreqCommand>,
    out_tx: tokio::sync::mpsc::UnboundedSender<(u8, u16, Option<u16>)>,
) {
    let mut last_freq: std::collections::HashMap<u8, u16> = std::collections::HashMap::new();
    while let Some(cmd) = freq_rx.recv().await {
        match cmd {
            FreqCommand::SetChainFreq {
                chain_id,
                freq_mhz,
                ack_tx,
            } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(()));
                }
                last_freq.insert(chain_id, freq_mhz);
                let _ = out_tx.send((chain_id, freq_mhz, None));
            }
            FreqCommand::SetVoltage {
                chain_id,
                voltage_mv,
                ack_tx,
            } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(voltage_mv));
                }
                let prev = *last_freq.get(&chain_id).unwrap_or(&0);
                let _ = out_tx.send((chain_id, prev, Some(voltage_mv)));
            }
            FreqCommand::UpdateWorkTime { .. } => {}
            FreqCommand::SetChipFreq { ack_tx, .. } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(0));
                }
            }
            FreqCommand::SetFrequencyLimit { ack_tx, .. }
            | FreqCommand::SetChipFrequencyLimit { ack_tx, .. } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(()));
                }
            }
            FreqCommand::VerifyVoltage { ack_tx, .. } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(None));
                }
            }
            FreqCommand::Barrier { ack_tx } => {
                let _ = ack_tx.send(());
            }
            FreqCommand::BeginMeasurement { ack_tx, .. } => {
                let _ = ack_tx.send(None);
            }
            FreqCommand::PrepareI2cQuietWindow { ack_tx } => {
                let _ = ack_tx.send(Ok(()));
            }
        }
    }
}

async fn assert_w30_chip_family_e2e(case: W30ChipCase) {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _root = temp_profile_root(case.label);
    reset_registry();

    let miner_profile = MinerProfile::for_chip(case.chip_id).expect("dcentrald-asic profile");
    let chain_id = *miner_profile
        .chain_ids
        .first()
        .expect("profile has chain ids");
    let (mut tuner, cmd_tx, config) = build_w30_tuner_for_case(case, miner_profile, chain_id);
    let state =
        dcentrald_api::build_minimal_app_state_with_autotuner_tx(make_minimal_inputs(), cmd_tx);
    grant_beta_identity(&state);

    let bundle = make_w30_bundle(case, miner_profile);
    let payload = serde_json::to_vec(&bundle).expect("serialize");
    let (content_type, body) = multipart_body("profile", &payload);
    let app = profiles::router().with_state(state.clone());
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
    let (status, import_resp) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CREATED, "body={:?}", import_resp);

    let imported_id = import_resp
        .get("id")
        .and_then(Value::as_str)
        .expect("id")
        .to_string();
    let model = model_slug(case.model);
    assert!(
        imported_id.contains(&model) && imported_id.contains(case.hashboard),
        "id={}",
        imported_id
    );

    let app = profiles::router().with_state(state.clone());
    let put_body = serde_json::json!({
        "model": model,
        "hashboard": case.hashboard,
        "profile_id": imported_id,
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
    let (status, put_resp) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", put_resp);

    let (runtime_tx, _runtime_rx) = mpsc::channel::<FreqCommand>(8);
    assert!(
        tuner.tick_runtime_commands_for_test(&runtime_tx).await,
        "expected one ApplySiliconProfile command for {}",
        case.label
    );
    assert_eq!(
        tuner.active_silicon_profile_id(&model_slug(case.model), case.hashboard),
        Some(imported_id.as_str()),
        "autotuner did not record active profile for {}",
        case.label
    );

    let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
    let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));
    tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
    drop(freq_tx);
    dispatcher.await.expect("dispatcher");

    let mut events = Vec::new();
    while let Ok(event) = out_rx.try_recv() {
        events.push(event);
    }
    let target = tuner
        .last_applied_silicon_target_for_test(chain_id)
        .expect("silicon target applied");
    let (min_voltage_mv, max_voltage_mv) = voltage_bounds_for_profile(miner_profile, &config);

    assert!(
        (config.min_freq_mhz..=miner_profile.max_freq_mhz).contains(&target.0),
        "{} frequency {} MHz outside {}..={} MHz",
        case.label,
        target.0,
        config.min_freq_mhz,
        miner_profile.max_freq_mhz
    );
    assert_eq!(
        target.0, miner_profile.max_freq_mhz,
        "{} should clamp the 1000 MHz synthetic preset to dcentrald-asic max_freq_mhz",
        case.label
    );
    assert!(
        (min_voltage_mv..=max_voltage_mv).contains(&target.1),
        "{} voltage {} mV outside {}..={} mV",
        case.label,
        target.1,
        min_voltage_mv,
        max_voltage_mv
    );
    assert_eq!(
        target.1, miner_profile.default_voltage_mv,
        "{} should apply the dcentrald-asic default chain-rail voltage",
        case.label
    );
    assert!(
        events
            .iter()
            .any(|(chain, freq, voltage)| *chain == chain_id
                && *freq == target.0
                && voltage.is_none()),
        "{} missing SetChainFreq event; got {:?}",
        case.label,
        events
    );
    assert!(
        events.iter().any(|(chain, _, voltage)| {
            *chain == chain_id && voltage.map(|mv| mv == target.1).unwrap_or(false)
        }),
        "{} missing SetVoltage event; got {:?}",
        case.label,
        events
    );
}

/// W30 - BM1366 / S19k Pro coverage for the profile-import ->
/// active-selection -> autotuner target-apply path.
#[tokio::test]
async fn test_w30_bm1366_s19k_profile_import_applies_safe_targets() {
    assert_w30_chip_family_e2e(W30ChipCase {
        label: "bm1366-s19k",
        model: MinerModel::AntminerS19kPro,
        hashboard: "BHB-S19kPro-W30",
        chip: ChipFamily::Bm1366,
        chip_id: 0x1366,
    })
    .await;
}

/// W30 - BM1368 / S21 coverage for the profile-import ->
/// active-selection -> autotuner target-apply path.
#[tokio::test]
async fn test_w30_bm1368_s21_profile_import_applies_safe_targets() {
    assert_w30_chip_family_e2e(W30ChipCase {
        label: "bm1368-s21",
        model: MinerModel::AntminerS21,
        hashboard: "BHB-S21-W30",
        chip: ChipFamily::Bm1368,
        chip_id: 0x1368,
    })
    .await;
}

/// W30 - BM1362 / S19j Pro Amlogic coverage for the profile-import ->
/// active-selection -> autotuner target-apply path.
#[tokio::test]
async fn test_w30_bm1362_s19j_profile_import_applies_safe_targets() {
    assert_w30_chip_family_e2e(W30ChipCase {
        label: "bm1362-s19j",
        model: MinerModel::AntminerS19jProA,
        hashboard: "BHB-S19jPro-W30",
        chip: ChipFamily::Bm1362,
        chip_id: 0x1362,
    })
    .await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// W13-A Item 2 — full pipeline: import → registry → set active →
/// autotuner sees it. Closes R6 #5 (deferred since wave 9).
#[tokio::test]
async fn test_profile_import_through_autotuner_pipeline() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _root = temp_profile_root("e2e-pipeline");
    reset_registry();

    // Build the autotuner test rig + AppState with cmd_tx wired.
    let (mut tuner, cmd_tx) = build_test_autotuner_runtime_ready();
    let state =
        dcentrald_api::build_minimal_app_state_with_autotuner_tx(make_minimal_inputs(), cmd_tx);
    grant_beta_identity(&state);

    // ---- Step 1: import a fixture bundle via POST /import. ----
    let bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    let payload = serde_json::to_vec(&bundle).expect("serialize");
    let (content_type, body) = multipart_body("profile", &payload);

    let app = profiles::router().with_state(state.clone());
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
    let (status, import_resp) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CREATED, "body={:?}", import_resp);

    let imported_id = import_resp
        .get("id")
        .and_then(Value::as_str)
        .expect("id")
        .to_string();
    assert!(
        imported_id.contains("antminer_s19j_pro_a") && imported_id.contains("BHB42601"),
        "id={}",
        imported_id
    );

    // ---- Step 2: assert registry was populated. ----
    {
        let reg = global().read().unwrap();
        let resolved = reg
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .expect("registry must have the imported bundle");
        assert_eq!(resolved.presets[0].freq_mhz, 545);
        assert!((resolved.presets[0].voltage_v - 1.34).abs() < 1e-3);
    }

    // ---- Step 3: PUT /active to authorize the autotuner to use it. ----
    let app = profiles::router().with_state(state.clone());
    let put_body = serde_json::json!({
        "model": "antminer_s19j_pro_a",
        "hashboard": "BHB42601",
        "profile_id": imported_id,
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
    let (status, put_resp) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", put_resp);

    // Registry's active-profile map now reflects the selection.
    {
        let reg = global().read().unwrap();
        assert_eq!(
            reg.get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601")
                .map(|s| s.to_string()),
            Some(imported_id.clone())
        );

        // get_active_bundle_for_chain resolves through the id.
        let active_bundle = reg
            .get_active_bundle_for_chain(MinerModel::AntminerS19jProA, "BHB42601")
            .expect("active bundle resolves");
        assert_eq!(active_bundle.presets[0].freq_mhz, 545);
        assert!((active_bundle.presets[0].voltage_v - 1.34).abs() < 1e-3);
    }

    // ---- Step 4: tick the autotuner, observe the selection. ----
    let (freq_tx, _freq_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(8);
    assert!(
        tuner.tick_runtime_commands_for_test(&freq_tx).await,
        "expected one ApplySiliconProfile command pending"
    );
    assert_eq!(
        tuner.active_silicon_profile_id("antminer_s19j_pro_a", "BHB42601"),
        Some(imported_id.as_str()),
        "autotuner did not record the imported profile selection"
    );
    assert_eq!(tuner.active_silicon_profile_count(), 1);
}

/// W13-A Item 2 — when the operator imports two distinct bundles for
/// the same chain (e.g. an operator-confirmed JSON replaces a vendor
/// JSON), the registry merges by source-class rank. Setting the new id
/// active wires through to the autotuner with the latest selection.
#[tokio::test]
async fn test_pipeline_resolves_higher_authority_over_vendor() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = temp_profile_root("e2e-rank");
    reset_registry();

    let (mut tuner, cmd_tx) = build_test_autotuner_runtime_ready();
    let state =
        dcentrald_api::build_minimal_app_state_with_autotuner_tx(make_minimal_inputs(), cmd_tx);
    grant_beta_identity(&state);

    // Pre-populate disk with a vendor bundle. After import of the
    // operator-confirmed bundle the registry merges by rank
    // (OperatorConfirmed rank 4 > VendorExtracted rank 3).
    let vendor = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::VendorExtracted,
    );
    let vendor_path = root.join("vendor").join("vendor.json");
    std::fs::create_dir_all(vendor_path.parent().unwrap()).unwrap();
    std::fs::write(&vendor_path, serde_json::to_vec_pretty(&vendor).unwrap()).unwrap();
    {
        let mut reg = global().write().unwrap();
        let _ = reg.reload(&root);
    }

    // Now import the operator-confirmed override.
    let mut operator = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    operator.presets[0].freq_mhz = 600;
    operator.presets[0].voltage_v = 1.42;
    operator.source.firmware_version = "operator-override".into();
    let payload = serde_json::to_vec(&operator).unwrap();
    let (content_type, body) = multipart_body("profile", &payload);

    let app = profiles::router().with_state(state.clone());
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
    let (status, import_resp) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::CREATED, "body={:?}", import_resp);
    let imported_id = import_resp
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    assert!(
        imported_id.ends_with("__operator_confirmed"),
        "id={}",
        imported_id
    );

    // Registry should now hold the operator-confirmed bundle (higher rank).
    {
        let reg = global().read().unwrap();
        let resolved = reg
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .expect("bundle present");
        assert_eq!(resolved.source_class, ProfileSource::OperatorConfirmed);
        assert_eq!(resolved.presets[0].freq_mhz, 600);
    }

    // Set active + tick autotuner.
    let app = profiles::router().with_state(state.clone());
    let put_body = serde_json::json!({
        "model": "antminer_s19j_pro_a",
        "hashboard": "BHB42601",
        "profile_id": imported_id,
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
    assert_eq!(resp.status(), StatusCode::OK);

    let (freq_tx, _freq_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(8);
    assert!(tuner.tick_runtime_commands_for_test(&freq_tx).await);
    assert_eq!(
        tuner.active_silicon_profile_id("antminer_s19j_pro_a", "BHB42601"),
        Some(imported_id.as_str())
    );
}

/// W13-A Item 2 — registry's `clear_active_profile_for_chain` allows
/// an operator to revert a selection. Verifies the tracking surface
/// exposed for future "clear active selection" UI work.
#[tokio::test]
async fn test_registry_clear_active_after_e2e_set() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = temp_profile_root("e2e-clear");
    reset_registry();

    let bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::OperatorConfirmed,
    );
    let path = root.join("operator").join("test.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    {
        let mut reg = global().write().unwrap();
        let _ = reg.reload(&root);
    }

    let id = "antminer_s19j_pro_a__BHB42601__bm1362__operator_confirmed";
    {
        let mut reg = global().write().unwrap();
        reg.set_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601", id)
            .unwrap();
    }
    {
        let reg = global().read().unwrap();
        assert_eq!(
            reg.get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601"),
            Some(id)
        );
    }

    {
        let mut reg = global().write().unwrap();
        let cleared = reg.clear_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601");
        assert_eq!(cleared.as_deref(), Some(id));
    }
    {
        let reg = global().read().unwrap();
        assert!(reg
            .get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601")
            .is_none());
    }
}
