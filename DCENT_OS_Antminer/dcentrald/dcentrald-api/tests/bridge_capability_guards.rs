// CE-052: fail-closed RuntimeCapability guards on the control BRIDGES.
//
// The gRPC / MQTT / CGMiner control bridges historically reused the REST write
// CORE (validate-and-write pool config, clamped fan envelope, restart action)
// but skipped the fail-closed runtime-capability gate their REST twins enforce
// (`post_pools` → PoolsRw, `post_fan` → PowerControl, reboot → Reboot,
// target-watts → AsicOptions, target-temp → ConfigRw, locate → Identify).
//
// These tests pin that the guard now runs FIRST — BEFORE any filesystem write /
// HAL open / restart flag / channel dispatch — so an Unknown-identity unit is
// rejected before a side effect can occur, while a granted beta-anchor identity
// (BM1387 + `exact`) is allowed through the guard.
//
// Host-runnable (no live hardware): the guard verdict is computed from
// `AppState` alone. On the ALLOW side the bridges proceed PAST the guard and
// then fail at the HAL/IO layer on a non-miner host — which is exactly the
// discriminator we assert: the returned error (if any) is NOT the capability
// guard error.
#![cfg(unix)]

use std::sync::Arc;

use dcentrald_api::rest::{
    app_state_mqtt_command_sink, bridge_guard_asic_options, bridge_guard_identify,
    grpc_bridge_reboot, grpc_bridge_set_fan, grpc_bridge_set_pools,
};
use dcentrald_api::{
    build_minimal_app_state, ApiConfig, AppState, MinimalAppStateInputs, NetworkBlockConfig,
};
use dcentrald_api_types::OperatingMode;

fn base_inputs(chip: &str) -> MinimalAppStateInputs {
    MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode: OperatingMode::Standard,
        firmware_version: "ce052-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/ce052-profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: chip.to_string(),
        external_state_rx: None,
    }
}

/// Unknown hardware identity: chip "test" resolves to no chip-id / no ASIC
/// profile, so the antminer capability descriptor lands
/// `IdentityConfidence::Unknown` + `CapabilitySupportTier::Unknown` and grants
/// NO mutating runtime capabilities.
fn unknown_identity_state() -> Arc<AppState> {
    build_minimal_app_state(base_inputs("test"))
}

/// Granted identity: BM1387 (the S9 beta anchor in the public support matrix) +
/// operator-confirmed `exact` identity confidence → `CapabilitySupportTier::Beta`
/// + `IdentityConfidence::Exact` → all mutating runtime caps are granted
/// (PoolsRw / PowerControl / Reboot / AsicOptions / ConfigRw / Identify).
fn granted_identity_state() -> Arc<AppState> {
    let state = build_minimal_app_state(base_inputs("BM1387"));
    {
        let mut hw = state.hardware_info.lock().unwrap();
        hw.identification.confidence = "exact".to_string();
    }
    state
}

/// The guard's rejection message (from `runtime_capability_guard_error`) always
/// contains this phrase. A downstream HAL/IO error on the ALLOW side does not.
fn is_capability_denied(message: &str) -> bool {
    message.contains("requires runtime capability")
}

fn sample_pools() -> Vec<(String, String, String, u32)> {
    vec![(
        "stratum+tcp://pool.example.com:3333".to_string(),
        "worker".to_string(),
        "x".to_string(),
        0,
    )]
}

// ── set_pools (PoolsRw) ────────────────────────────────────────────────────

#[tokio::test]
async fn set_pools_rejected_on_unknown_identity_before_write() {
    let state = unknown_identity_state();
    // `GrpcPoolBridgeOk` is not `Debug`, so match rather than `expect_err`.
    match grpc_bridge_set_pools(&state, sample_pools()).await {
        Ok(_) => panic!("Unknown identity must be denied PoolsRw before the config write"),
        Err(err) => assert!(
            is_capability_denied(&err),
            "expected capability-guard rejection, got: {err}"
        ),
    }
}

#[tokio::test]
async fn set_pools_passes_guard_on_granted_identity() {
    let state = granted_identity_state();
    // Granted identity clears the capability guard; the bridge then proceeds to
    // the validate-and-write core (which may Ok or fail at the IO layer on a
    // non-miner host). Either way it must NOT be the capability rejection.
    if let Err(err) = grpc_bridge_set_pools(&state, sample_pools()).await {
        assert!(
            !is_capability_denied(&err),
            "granted identity must clear the PoolsRw guard, got guard error: {err}"
        );
    }
}

// ── set_fan (PowerControl) ─────────────────────────────────────────────────

#[test]
fn set_fan_rejected_on_unknown_identity_before_hal() {
    let state = unknown_identity_state();
    let err = grpc_bridge_set_fan(&state, 50)
        .expect_err("Unknown identity must be denied PowerControl before the HAL fan write");
    assert!(
        is_capability_denied(&err),
        "expected capability-guard rejection, got: {err}"
    );
}

#[test]
fn set_fan_passes_guard_on_granted_identity() {
    let state = granted_identity_state();
    // Granted clears the guard; the HAL fan write then fails on a non-miner host
    // (no fan-control UIO) — a non-capability error.
    if let Err(err) = grpc_bridge_set_fan(&state, 50) {
        assert!(
            !is_capability_denied(&err),
            "granted identity must clear the PowerControl guard, got guard error: {err}"
        );
    }
}

// ── reboot (Reboot) ────────────────────────────────────────────────────────

#[test]
fn reboot_rejected_on_unknown_identity_before_restart() {
    // Plain `#[test]`: on denial the guard returns BEFORE `trigger_daemon_restart`
    // (which would `tokio::spawn`), so no runtime is needed and no restart flag
    // is written.
    let state = unknown_identity_state();
    let err = grpc_bridge_reboot(&state)
        .expect_err("Unknown identity must be denied Reboot before the restart flag");
    assert!(
        is_capability_denied(&err),
        "expected capability-guard rejection, got: {err}"
    );
}

#[tokio::test]
async fn reboot_passes_guard_on_granted_identity() {
    // Needs a tokio runtime: the granted path clears the guard and reaches
    // `trigger_daemon_restart`, which `tokio::spawn`s a 2s-delayed init.d
    // restart. The spawned task is cancelled when this test's current-thread
    // runtime is dropped on return (well under the 2s delay), so no restart is
    // actually executed.
    let state = granted_identity_state();
    let result = grpc_bridge_reboot(&state);
    assert!(
        result.is_ok(),
        "granted identity must clear the Reboot guard, got: {result:?}"
    );
    // The restart flag is a harmless temp file on the test host; remove it.
    let _ = std::fs::remove_file("/tmp/dcentrald_restart");
}

// ── AsicOptions guard (set_tuner_mode + mqtt:target_watts) ──────────────────

#[test]
fn asic_options_guard_denies_unknown_allows_granted() {
    let unknown = unknown_identity_state();
    let err = bridge_guard_asic_options(&unknown, "grpc:set_tuner_mode")
        .expect_err("Unknown identity must be denied AsicOptions");
    assert!(is_capability_denied(&err), "unexpected message: {err}");

    let granted = granted_identity_state();
    bridge_guard_asic_options(&granted, "grpc:set_tuner_mode")
        .expect("granted beta-anchor identity must be allowed AsicOptions");
}

// ── Identify guard (locate_device) ─────────────────────────────────────────

#[test]
fn identify_guard_denies_unknown_allows_granted() {
    let unknown = unknown_identity_state();
    let err = bridge_guard_identify(&unknown, "grpc:locate_device")
        .expect_err("Unknown identity must be denied Identify");
    assert!(is_capability_denied(&err), "unexpected message: {err}");

    let granted = granted_identity_state();
    bridge_guard_identify(&granted, "grpc:locate_device")
        .expect("granted beta-anchor identity must be allowed Identify");
}

// ── MQTT sink (target_watts=AsicOptions, target_temp_c=ConfigRw, fan) ───────

#[tokio::test]
async fn mqtt_sink_rejected_on_unknown_identity_before_side_effect() {
    let sink = app_state_mqtt_command_sink(unknown_identity_state());

    let watts_err = sink
        .set_target_watts(500)
        .await
        .expect_err("Unknown identity must be denied target-watts (AsicOptions) before persist");
    assert!(is_capability_denied(&watts_err), "unexpected: {watts_err}");

    let temp_err = sink
        .set_target_temp_c(60.0)
        .await
        .expect_err("Unknown identity must be denied target-temp (ConfigRw) before write");
    assert!(is_capability_denied(&temp_err), "unexpected: {temp_err}");

    let fan_err = sink
        .set_fan_pwm(50)
        .await
        .expect_err("Unknown identity must be denied fan-pwm (PowerControl) before HAL");
    assert!(is_capability_denied(&fan_err), "unexpected: {fan_err}");
}

#[tokio::test]
async fn mqtt_sink_passes_guard_on_granted_identity() {
    let sink = app_state_mqtt_command_sink(granted_identity_state());

    // Each setpoint clears its guard on the granted identity, then fails at the
    // persist/dispatch/HAL layer on a non-miner host — a non-capability error.
    if let Err(err) = sink.set_target_watts(500).await {
        assert!(!is_capability_denied(&err), "watts guard error: {err}");
    }
    if let Err(err) = sink.set_target_temp_c(60.0).await {
        assert!(!is_capability_denied(&err), "temp guard error: {err}");
    }
    if let Err(err) = sink.set_fan_pwm(50).await {
        assert!(!is_capability_denied(&err), "fan guard error: {err}");
    }
}
