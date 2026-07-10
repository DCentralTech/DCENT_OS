//! W13-A Item 1 — autotuner runtime command wiring tests.
//!
//! Pre-W13-A, `PUT /api/profiles/silicon/active` was acknowledged-only:
//! the operator's mode change was persisted in the silicon-profiles
//! registry but the live `dcentrald-autotuner` actor never saw it. This
//! suite drives the wiring end-to-end:
//!   1. Build a minimal AppState with an `autotuner_command_tx` mpsc
//!      channel hooked up to a real `AutoTuner` instance.
//!   2. Call the `PUT /api/profiles/silicon/active` route handler.
//!   3. Tick the autotuner's runtime-command channel once.
//!   4. Assert the autotuner's
//!      `active_silicon_profile_id(model_snake, hashboard)` accessor
//!      returns the freshly selected profile id.
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
use dcentrald_autotuner::{
    config::AutoTunerConfig,
    profile::{ChipGrade, ProfileStats},
    tuner::TunerState,
    AutoTuner, AutoTunerCommand, ChainHardwareIdentity, ChipProfile, FreqCommand, PowerCalibration,
    SiliconPreset, TuningProfile,
};
use dcentrald_silicon_profiles::registry::{
    self, global, ProfileBundle, ProfileMetadata, ProfileSourceMetadata,
};
use dcentrald_silicon_profiles::{Profile, ProfileSource};

// ---------------------------------------------------------------------------
// Test harness
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
    let root = std::env::temp_dir().join(format!("dcentrald-w13a-{}-{}-{}", label, pid, nanos));
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
            vendor: "test-w13a".into(),
            firmware_version: "1.0.0".into(),
            extracted_from_sha256: "0".repeat(64),
            extraction_date: "2026-05-05".into(),
            extracted_by: Some("w13a-test".into()),
        },
        source_class,
        presets: vec![Profile {
            step: 0,
            freq_mhz: 500,
            voltage_v: 1.40,
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
        firmware_version: "w13a-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    }
}

/// CE-103/CE-121: grant the S9 / BM1387 Beta anchor at `exact` confidence so
/// the `PUT /api/profiles/silicon/active` route's `AsicOptions` capability guard
/// passes. Without this the route fails closed with 409 on the default
/// Unknown-identity state.
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

/// Build a runtime-ready test AutoTuner forced into BackgroundAdjust.
fn build_test_autotuner_runtime_ready() -> (AutoTuner, mpsc::Sender<AutoTunerCommand>) {
    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        AutoTunerConfig::default(),
        500,
        "BM1387".to_string(),
        "pic".to_string(),
        calibration,
    );
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutoTunerCommand>(8);
    tuner.set_command_receiver(cmd_rx);
    tuner.force_state_for_test(TunerState::BackgroundAdjust);
    (tuner, cmd_tx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// W13-A Item 1 — `PUT /api/profiles/silicon/active` reaches the live
/// autotuner via the `autotuner_command_tx` mpsc channel.
#[tokio::test]
async fn test_put_active_wires_through_to_live_autotuner() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = temp_profile_root("put-active-runtime");
    reset_registry();

    // Hydrate the registry with a single S9 bundle.
    let mut bundle = make_bundle(
        MinerModel::AntminerS9,
        "BHB-S9-generic",
        ChipFamily::Bm1387,
        ProfileSource::OperatorConfirmed,
    );
    // BM1387 voltage envelope is chain-rail 7.5..10.0 V, not chip-rail.
    bundle.presets[0].voltage_v = 9.1;
    write_bundle_to_disk(&root, "operator", "test", &bundle);
    reload_from_disk(&root);

    // Build the autotuner test rig + AppState with cmd_tx wired.
    let (mut tuner, cmd_tx) = build_test_autotuner_runtime_ready();
    let state =
        dcentrald_api::build_minimal_app_state_with_autotuner_tx(make_minimal_inputs(), cmd_tx);
    grant_beta_identity(&state);
    let app = profiles::router().with_state(state);

    // Issue PUT /api/profiles/silicon/active.
    let body = serde_json::json!({
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, resp_body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", resp_body);

    // Registry was updated.
    let reg = global().read().unwrap();
    assert_eq!(
        reg.get_active_profile_for_chain(MinerModel::AntminerS9, "BHB-S9-generic"),
        Some("antminer_s9__BHB-S9-generic__bm1387__operator_confirmed")
    );
    drop(reg);

    // Drive the autotuner's command channel one step.
    let (freq_tx, _freq_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(8);
    let ticked = tuner.tick_runtime_commands_for_test(&freq_tx).await;
    assert!(ticked, "expected one command to have been waiting");

    // Autotuner now reflects the new selection.
    assert_eq!(
        tuner.active_silicon_profile_id("antminer_s9", "BHB-S9-generic"),
        Some("antminer_s9__BHB-S9-generic__bm1387__operator_confirmed"),
        "autotuner did not record the silicon profile selection"
    );
    assert_eq!(tuner.active_silicon_profile_count(), 1);
}

/// W13-A Item 1 — when `autotuner_command_tx` is `None` (proxy mode,
/// hybrid fallback, or autotuner not started), the runtime hop is
/// reported as `unavailable` but the registry selection is still
/// durable. This is the next-cycle / next-restart fallback contract.
#[tokio::test]
async fn test_put_active_durable_when_runtime_channel_absent() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = temp_profile_root("put-active-no-channel");
    reset_registry();

    let mut bundle = make_bundle(
        MinerModel::AntminerS19jProA,
        "BHB42601",
        ChipFamily::Bm1362,
        ProfileSource::VendorExtracted,
    );
    bundle.presets[0].voltage_v = 1.34;
    write_bundle_to_disk(&root, "vendor", "test", &bundle);
    reload_from_disk(&root);

    // AppState without an autotuner command channel.
    let state = dcentrald_api::build_minimal_app_state(make_minimal_inputs());
    grant_beta_identity(&state);
    let app = profiles::router().with_state(state);

    let body = serde_json::json!({
        "model": "antminer_s19j_pro_a",
        "hashboard": "BHB42601",
        "profile_id": "antminer_s19j_pro_a__BHB42601__bm1362__vendor_extracted",
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/profiles/silicon/active")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, resp_body) = body_to_value(resp).await;
    assert_eq!(status, StatusCode::OK, "body={:?}", resp_body);

    // runtime should report `channel_available: false` / `unavailable`.
    let runtime = resp_body.get("runtime").expect("runtime field");
    assert_eq!(
        runtime.get("channel_available").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        runtime.get("status").and_then(Value::as_str),
        Some("unavailable")
    );

    // Registry still records the selection — durable next-cycle path.
    let reg = global().read().unwrap();
    assert_eq!(
        reg.get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601"),
        Some("antminer_s19j_pro_a__BHB42601__bm1362__vendor_extracted")
    );
}

/// W13-A Item 1 — when the autotuner is NOT in a runtime-ready state
/// (i.e. still characterizing), the
/// `AutoTunerCommand::ApplySiliconProfile` handler returns Deferred.
/// The selection is still recorded in `active_silicon_profile_ids` so
/// the next iteration picks it up.
#[tokio::test]
async fn test_apply_silicon_profile_deferred_when_not_ready() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());

    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        AutoTunerConfig::default(),
        500,
        "BM1387".to_string(),
        "pic".to_string(),
        calibration,
    );
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutoTunerCommand>(8);
    tuner.set_command_receiver(cmd_rx);
    // Default state is Idle — NOT runtime-ready.
    assert_eq!(tuner.active_silicon_profile_count(), 0);

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(AutoTunerCommand::ApplySiliconProfile {
            miner_model: "antminer_s9".to_string(),
            hashboard: "BHB-S9-generic".to_string(),
            profile_id: "antminer_s9__BHB-S9-generic__bm1387__live_confirmed".to_string(),
            presets: vec![],
            ack_tx,
        })
        .await
        .expect("send");

    let (freq_tx, _freq_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(8);
    assert!(tuner.tick_runtime_commands_for_test(&freq_tx).await);

    let result = ack_rx.await.expect("ack");
    assert!(matches!(
        result.status,
        dcentrald_autotuner::AutoTunerCommandStatus::Deferred
    ));
    assert!(!result.applied_runtime);
    assert_eq!(
        tuner.active_silicon_profile_id("antminer_s9", "BHB-S9-generic"),
        Some("antminer_s9__BHB-S9-generic__bm1387__live_confirmed"),
        "deferred state must still record the selection for next cycle"
    );
}

// ---------------------------------------------------------------------------
// W15-A: live preset-table consumption inside the tuning loop
//
//  W13-A wired `AutoTunerCommand::ApplySiliconProfile` from the
// REST handler into the autotuner actor and recorded the selection in
// `active_silicon_profile_ids`, but explicitly deferred live preset-
// table consumption inside the tuning loop. W15-A closes that gap:
// at the top of each `background_monitor` iteration tick the autotuner
// derives per-chain freq/voltage targets from the active profile's
// preset table (selected per `TunerMode`), clamps by the existing
// safety bounds (min/max freq, min/max voltage, fw=0x86 refusal), and
// applies via the existing `FreqCommand::SetChainFreq`/`SetVoltage`
// rails. The integration tests below exercise this path against the
// real `AutoTuner` actor without spinning up the full HAL probe
// pipeline.
// ---------------------------------------------------------------------------

/// Build a minimal 3-chip BM1387 (S9) `TuningProfile` for chain 6 so
/// the silicon-profile target apply path has chain ids to iterate.
fn make_s9_profile(chain_id: u8) -> TuningProfile {
    TuningProfile {
        version: 4,
        chip_type: "BM1387".to_string(),
        chain_id,
        chip_count: 3,
        voltage_mv: 9100,
        tuned_at: "0".to_string(),
        ambient_temp_c: None,
        optimal_voltage_mv: None,
        estimated_power_w: 0.0,
        estimated_efficiency_jth: 0.0,
        equilibrium_temp_c: None,
        thermal_refinement_duration_s: None,
        calibrated_c_eff: None,
        chips: (0..3u8)
            .map(|i| ChipProfile {
                chip_index: i,
                max_stable_mhz: 650,
                operating_mhz: 625,
                grade: ChipGrade::B,
                error_rate: 0.0,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect(),
        stats: ProfileStats {
            avg_freq_mhz: 625.0,
            min_freq_mhz: 625,
            max_freq_mhz: 625,
            grade_a: 0,
            grade_b: 3,
            grade_c: 0,
            grade_d: 0,
            tuning_duration_s: 10.0,
            estimated_hashrate_ghs: 0.0,
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
        },
        // W13.C3: SKU + flag denormalisation. Test fixture default.
        hashboard_sku: None,
        hashboard_sku_flags: None,
    }
}

/// Drain pending `FreqCommand` items off `freq_rx` and split them into
/// `(SetChainFreq{chain_id, freq_mhz}, SetVoltage{chain_id, voltage_mv})`
/// pairs so a test can assert on the W15-A applied targets without
/// depending on `Debug`-formatted enum variants.
fn drain_freq_commands(
    freq_rx: &mut mpsc::Receiver<FreqCommand>,
) -> (Vec<(u8, u16)>, Vec<(u8, u16)>) {
    let mut chain_freqs = Vec::new();
    let mut voltages = Vec::new();
    while let Ok(cmd) = freq_rx.try_recv() {
        match cmd {
            FreqCommand::SetChainFreq {
                chain_id,
                freq_mhz,
                ack_tx,
            } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(()));
                }
                chain_freqs.push((chain_id, freq_mhz));
            }
            FreqCommand::SetVoltage {
                chain_id,
                voltage_mv,
                ack_tx,
            } => {
                if let Some(tx) = ack_tx {
                    let _ = tx.send(Ok(voltage_mv));
                }
                voltages.push((chain_id, voltage_mv));
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
    (chain_freqs, voltages)
}

/// Bridge the dispatcher mock: in W15-A's
/// `apply_active_silicon_profile_targets` the autotuner emits
/// `SetChainFreq` and waits on the oneshot ack before continuing. A
/// passive `try_recv` won't unblock the autotuner. This task receives
/// FreqCommands as they arrive, ACKs them success, and forwards a
/// summary record into the test's collector.
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
    drop(last_freq);
}

/// Build a runtime-ready BM1387 autotuner with one S9 chain (id=6)
/// pre-installed and an attached preset table for `(antminer_s9,
/// BHB-S9-generic)`.
async fn build_w15a_tuner_with_active_profile(
    presets: Vec<SiliconPreset>,
) -> (AutoTuner, mpsc::Sender<AutoTunerCommand>) {
    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        AutoTunerConfig::default(),
        500,
        "BM1387".to_string(),
        "pic".to_string(),
        calibration,
    );
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutoTunerCommand>(8);
    tuner.set_command_receiver(cmd_rx);
    tuner.force_state_for_test(TunerState::BackgroundAdjust);
    tuner.install_profile_for_test(6, make_s9_profile(6));

    // Send an ApplySiliconProfile through the real command channel so
    // the autotuner's recording path is exercised end-to-end.
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(AutoTunerCommand::ApplySiliconProfile {
            miner_model: "antminer_s9".to_string(),
            hashboard: "BHB-S9-generic".to_string(),
            profile_id: "antminer_s9__BHB-S9-generic__bm1387__live_confirmed".to_string(),
            presets,
            ack_tx,
        })
        .await
        .expect("send");
    let (freq_tx_drain, mut freq_rx_drain) = mpsc::channel::<FreqCommand>(8);
    assert!(tuner.tick_runtime_commands_for_test(&freq_tx_drain).await);
    drop(freq_tx_drain);
    let _ = ack_rx.await.expect("ack");
    let _ = drain_freq_commands(&mut freq_rx_drain);

    (tuner, cmd_tx)
}

/// W15-A test 1: with an active silicon profile installed, ticking
/// `apply_active_silicon_profile_targets` emits per-chain freq + voltage
/// targets matching the preset table for the current `TunerMode`.
///
/// This test exercises the `select_preset_for_mode` Hashrate →
/// highest-step branch, so it explicitly opts the tuner into
/// `TuneTarget::Hashrate`. NOTE: the autotuner's *default* mode is
/// `TuneTarget::Efficiency`, not `Hashrate` — that is a load-bearing
/// home-miner safety default (home miners pay per kWh, so the J/TH
/// branch wins over the TH/s leaderboard; see
/// `config.rs::TuneTarget::default`, pinned by the W1.3 test). The
/// earlier "default = Hashrate" assumption in this test was stale, so
/// the test now selects Hashrate the same way Hacker mode does at
/// runtime instead of relying on a default that no longer exists.
#[tokio::test]
async fn test_tuner_loop_applies_active_profile_targets() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let presets = vec![
        SiliconPreset {
            step: 0,
            freq_mhz: 600,
            voltage_v: 9.0,
        },
        SiliconPreset {
            step: 1,
            freq_mhz: 650,
            voltage_v: 9.1,
        },
        SiliconPreset {
            step: 2,
            freq_mhz: 700,
            voltage_v: 9.2,
        },
    ];
    let (mut tuner, _cmd_tx) = build_w15a_tuner_with_active_profile(presets).await;
    // Opt into the Hashrate (max-step) branch — the default is
    // Efficiency (J/TH safety default), which would select step 0.
    tuner.set_target_mode_for_test(dcentrald_autotuner::config::TuneTarget::Hashrate);

    let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
    let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));

    tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
    drop(freq_tx);
    dispatcher.await.expect("dispatcher");

    let mut events = Vec::new();
    while let Ok(ev) = out_rx.try_recv() {
        events.push(ev);
    }
    let chain_freqs: Vec<(u8, u16)> = events
        .iter()
        .filter_map(|(c, f, v)| if v.is_none() { Some((*c, *f)) } else { None })
        .collect();
    let voltages: Vec<_> = events
        .iter()
        .filter_map(|(c, _, v)| v.map(|mv| (*c, mv)))
        .collect();

    // `TuneTarget::Hashrate` (set above) → max-step preset (700 MHz, 9.2 V → 9200 mV).
    assert!(
        chain_freqs.iter().any(|(c, f)| *c == 6 && *f == 700),
        "expected chain 6 freq 700 MHz, got {:?}",
        chain_freqs
    );
    assert!(
        voltages.iter().any(|(c, mv)| *c == 6 && *mv == 9200),
        "expected chain 6 voltage 9200 mV, got {:?}",
        voltages
    );
    assert_eq!(
        tuner.last_applied_silicon_target_for_test(6),
        Some((700, 9200))
    );
}

/// W15-A test 2: when no active profile is recorded, the
/// silicon-profile target apply path is a no-op and the legacy
/// config-driven path keeps owning per-chain targets.
#[tokio::test]
async fn test_tuner_loop_falls_back_to_config_when_no_active_profile() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());

    let calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let mut tuner = AutoTuner::new(
        AutoTunerConfig::default(),
        500,
        "BM1387".to_string(),
        "pic".to_string(),
        calibration,
    );
    tuner.force_state_for_test(TunerState::BackgroundAdjust);
    tuner.install_profile_for_test(6, make_s9_profile(6));

    let (freq_tx, mut freq_rx) = mpsc::channel::<FreqCommand>(32);
    tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
    drop(freq_tx);
    let (chain_freqs, voltages) = drain_freq_commands(&mut freq_rx);

    assert!(
        chain_freqs.is_empty(),
        "no active profile must produce zero chain-freq writes; got {:?}",
        chain_freqs
    );
    assert!(
        voltages.is_empty(),
        "no active profile must produce zero voltage writes; got {:?}",
        voltages
    );
    assert!(tuner.last_applied_silicon_target_for_test(6).is_none());
}

/// W15-A test 3: a preset row with extreme freq/voltage values is
/// clamped to the autotuner's safety bounds (config min/max freq,
/// chain-rail voltage ceiling for BM1387 = 9440 mV).
#[tokio::test]
async fn test_profile_targets_clamped_by_safety_bounds() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());

    // 1500 MHz / 15.0 V — extreme values that must be clamped.
    let presets = vec![SiliconPreset {
        step: 0,
        freq_mhz: 1500,
        voltage_v: 15.0,
    }];
    let (mut tuner, _cmd_tx) = build_w15a_tuner_with_active_profile(presets).await;

    let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
    let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));

    tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
    drop(freq_tx);
    dispatcher.await.expect("dispatcher");

    let mut events = Vec::new();
    while let Ok(ev) = out_rx.try_recv() {
        events.push(ev);
    }
    let voltages: Vec<_> = events
        .iter()
        .filter_map(|(c, _, v)| v.map(|mv| (*c, mv)))
        .collect();

    // Default config max_freq_mhz for BM1387 is well below 1500 MHz —
    // the apply path must clamp to <= max_freq_mhz, not pass 1500
    // through.
    let target = tuner
        .last_applied_silicon_target_for_test(6)
        .expect("freq applied");
    assert!(
        target.0 <= AutoTunerConfig::default().max_freq_mhz,
        "freq {} must be clamped to <= max_freq_mhz {}",
        target.0,
        AutoTunerConfig::default().max_freq_mhz
    );
    // BM1387 chain-rail max is 9440 mV; 15.0 V (=15000 mV) raw must be
    // clamped down.
    assert_eq!(
        target.1, 9440,
        "voltage must be clamped to BM1387 chain-rail ceiling 9440 mV"
    );
    assert!(
        voltages.iter().any(|(c, mv)| *c == 6 && *mv == 9440),
        "dispatcher should have received clamped voltage, got {:?}",
        voltages
    );
}

/// W15-A test 4: swapping the active profile between iterations causes
/// the autotuner to re-apply per-chain targets on the next tick.
#[tokio::test]
async fn test_profile_swap_between_iterations() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());

    // First profile: step 0 = 550 MHz / 9.0 V.
    let first = vec![SiliconPreset {
        step: 0,
        freq_mhz: 550,
        voltage_v: 9.0,
    }];
    let (mut tuner, cmd_tx) = build_w15a_tuner_with_active_profile(first).await;

    // First iteration: applies step 0 = 550 MHz / 9000 mV.
    {
        let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
        let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));
        tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
        drop(freq_tx);
        dispatcher.await.expect("dispatcher");
    }
    assert_eq!(
        tuner.last_applied_silicon_target_for_test(6),
        Some((550, 9000))
    );

    // Operator selects a NEW profile: step 0 = 650 MHz / 9.2 V.
    let second = vec![SiliconPreset {
        step: 0,
        freq_mhz: 650,
        voltage_v: 9.2,
    }];
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(AutoTunerCommand::ApplySiliconProfile {
            miner_model: "antminer_s9".to_string(),
            hashboard: "BHB-S9-generic".to_string(),
            profile_id: "antminer_s9__BHB-S9-generic__bm1387__operator_confirmed".to_string(),
            presets: second,
            ack_tx,
        })
        .await
        .expect("send");
    let (freq_tx_drain, mut freq_rx_drain) = mpsc::channel::<FreqCommand>(8);
    assert!(tuner.tick_runtime_commands_for_test(&freq_tx_drain).await);
    drop(freq_tx_drain);
    let _ = ack_rx.await.expect("ack");
    let _ = drain_freq_commands(&mut freq_rx_drain);

    // Next iteration: applies step 0 of the new profile = 650 MHz / 9200 mV.
    {
        let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
        let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));
        tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
        drop(freq_tx);
        dispatcher.await.expect("dispatcher");
    }
    assert_eq!(
        tuner.last_applied_silicon_target_for_test(6),
        Some((650, 9200))
    );
}

/// W15-A bonus safety test: a chain whose dsPIC reports fw=0x86
/// (post-PIC-RESET corruption signature) must NOT receive a
/// `SetVoltage` from the silicon-profile target apply path. Frequency
/// is still applied; voltage is suppressed per
/// .
#[tokio::test]
async fn test_fw86_dspic_chain_refuses_voltage_application() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let presets = vec![SiliconPreset {
        step: 0,
        freq_mhz: 600,
        voltage_v: 9.0,
    }];
    let (mut tuner, _cmd_tx) = build_w15a_tuner_with_active_profile(presets).await;
    tuner.set_chain_hardware_identity_for_test(
        6,
        ChainHardwareIdentity {
            eeprom_serial: None,
            eeprom_fingerprint: None,
            dspic_fw_byte: Some(0x86),
        },
    );

    let (freq_tx, freq_rx) = mpsc::channel::<FreqCommand>(32);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(u8, u16, Option<u16>)>();
    let dispatcher = tokio::spawn(run_dispatcher_mock(freq_rx, out_tx));

    tuner.tick_silicon_profile_targets_for_test(&freq_tx).await;
    drop(freq_tx);
    dispatcher.await.expect("dispatcher");

    let mut events = Vec::new();
    while let Ok(ev) = out_rx.try_recv() {
        events.push(ev);
    }
    let chain_freqs: Vec<(u8, u16)> = events
        .iter()
        .filter_map(|(c, f, v)| if v.is_none() { Some((*c, *f)) } else { None })
        .collect();
    let voltages: Vec<_> = events
        .iter()
        .filter_map(|(c, _, v)| v.map(|mv| (*c, mv)))
        .collect();

    assert!(
        chain_freqs.iter().any(|(c, f)| *c == 6 && *f == 600),
        "freq must still be applied on fw=0x86 chains; got {:?}",
        chain_freqs
    );
    assert!(
        voltages.is_empty(),
        "fw=0x86 chains MUST NOT receive a SetVoltage; got {:?}",
        voltages
    );
}
