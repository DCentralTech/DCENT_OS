// DCENT_axe — MQTT publisher (esp-idf transport)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Thin, fail-soft, default-OFF MQTT publisher for Home Assistant integration.
//!
//! All the testable logic (HA discovery topics/payloads + the state payload)
//! lives in the host-pure [`crate::mqtt_ha`] module. This file is ONLY the
//! esp-idf transport: it owns the broker connection and the publish cadence.
//!
//! ## Invariants (do not regress)
//! - **Default-OFF.** Nothing runs unless `config.mqtt.enabled` is true AND a
//!   broker host is configured.
//! - **Fail-soft.** Every broker/network error is logged + retried with backoff.
//!   MQTT NEVER touches mining or the safety paths and NEVER blocks them — it
//!   runs on its own thread and only ever READS a telemetry snapshot.
//! - **No HTTP handler.** MQTT is outbound, so `MAX_URI_HANDLERS` is unchanged.
//! - **panic=abort safe.** We snapshot-then-drop every `SharedState` lock (never
//!   hold one across a publish) and use `unwrap_or_else(|e| e.into_inner())`, so
//!   a fault on this thread can never poison a `Mutex` another thread unwraps.
//! - **Publish-only.** We never subscribe to a command topic, matching the
//!   read-only entity set `mqtt_ha` advertises (no over-claimed control surface).
//!
//! Field-delivery status: implemented + host-unit-tested (the payload builder)
//! and xtensa-built; live broker delivery is not yet field-proven. See README /
//!  for the honest claim wording.

use crate::mqtt_ha::{
    build_publish_plan, command_state_echo, command_subscribe_topics, device_id_from_mac, lwt_spec,
    parse_command, state_topic, CommandTopics, EnergyAccumulator, HaCommand, HaDevice, HaState,
    MqttPublishOp, MqttQos, PublishPhase,
};
use crate::shared::SharedState;
use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EventPayload, LwtConfiguration, MqttClientConfiguration, QoS,
};
use esp_idf_svc::sys;
use log::{info, warn};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Map the host-pure [`MqttQos`] (used by the publish plan) onto the esp-idf
/// `QoS` enum. Keeps `mqtt_ha` free of any esp-idf dependency so it host-tests.
fn esp_qos(q: MqttQos) -> QoS {
    match q {
        MqttQos::AtMostOnce => QoS::AtMostOnce,
        MqttQos::AtLeastOnce => QoS::AtLeastOnce,
    }
}

/// Worker thread stack. The publish loop serializes a small JSON state object
/// (heap-allocated by serde_json), so this is comfortable headroom.
const MQTT_TASK_STACK: usize = 8 * 1024;
/// esp-mqtt client RX/TX buffers — kept small for the ~300 KB RAM budget. The
/// discovery configs + state payloads are well under 1 KiB each.
const MQTT_BUFFER_SIZE: usize = 1024;
/// Reconnect backoff ceiling.
const RECONNECT_BACKOFF_MAX_S: u64 = 60;
/// Never publish faster than this (avoids a busy loop on a bad config value).
const MIN_PUBLISH_INTERVAL_S: u16 = 5;

/// Read the device MAC (last 3 octets feed the stable HA device id).
fn device_mac_string() -> String {
    let mut mac = [0u8; 6];
    // SAFETY: esp_efuse_mac_get_default fills exactly 6 bytes into our buffer.
    let err = unsafe { sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if err == sys::ESP_OK {
        mac.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    } else {
        String::new()
    }
}

/// Spawn the MQTT publisher thread IFF `config.mqtt.enabled` and a broker host is
/// configured. Safe to call unconditionally at boot — it returns immediately when
/// MQTT is off (the default).
pub fn spawn_publisher(state: SharedState) {
    let cfg = state
        .config
        .lock()
        .map(|c| c.mqtt.clone())
        .unwrap_or_else(|e| e.into_inner().mqtt.clone());

    if !cfg.enabled {
        info!("MQTT publisher disabled (mqtt.enabled=false)");
        return;
    }
    if cfg.broker_host.trim().is_empty() {
        warn!("MQTT enabled but broker host is empty — not starting publisher");
        return;
    }

    let device_id = device_id_from_mac(&device_mac_string());
    info!(
        "MQTT publisher starting: broker {}:{} (tls={}) device_id={}",
        cfg.broker_host.trim(),
        cfg.broker_port,
        cfg.tls,
        device_id
    );

    let _ = std::thread::Builder::new()
        .name("mqtt".into())
        .stack_size(MQTT_TASK_STACK)
        .spawn(move || run_loop(state, device_id))
        .map_err(|e| warn!("failed to spawn MQTT thread: {e}"));
}

/// Reconnecting publish loop. Each session is a fresh connect + retained
/// discovery publish + periodic state publishes; on any error we back off and
/// reconnect. This loop never exits while MQTT stays enabled.
fn run_loop(state: SharedState, device_id: String) {
    let mut backoff = 2u64;
    // ONE energy accumulator for the whole boot: it lives HERE (not inside
    // `run_session`) so the lifetime kWh total is monotonic ACROSS reconnects and
    // only resets on reboot — exactly the HA `total_increasing` energy contract.
    let mut energy = EnergyAccumulator::new();
    loop {
        match run_session(&state, &device_id, &mut energy) {
            Ok(()) => {
                // A clean return means MQTT was disabled mid-run — stop quietly.
                info!("MQTT publisher stopping (disabled at runtime)");
                return;
            }
            Err(e) => warn!("MQTT session ended: {e} — reconnecting in {backoff}s"),
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX_S);
    }
}

/// One broker session: connect, publish retained discovery, then publish state on
/// the configured cadence until an error or MQTT is disabled. Returns `Ok(())`
/// only when MQTT was disabled at runtime (clean stop); any transport problem is
/// `Err` so `run_loop` reconnects.
fn run_session(
    state: &SharedState,
    device_id: &str,
    energy: &mut EnergyAccumulator,
) -> Result<(), String> {
    // Re-read config each session so edits (broker move, disable) take effect on
    // the next reconnect without a reboot.
    let cfg = state
        .config
        .lock()
        .map(|c| c.mqtt.clone())
        .unwrap_or_else(|e| e.into_inner().mqtt.clone());
    if !cfg.enabled {
        return Ok(());
    }

    let host = cfg.broker_host.trim();
    if host.is_empty() {
        return Err("broker host empty".to_string());
    }
    let scheme = if cfg.tls { "mqtts" } else { "mqtt" };
    let url = format!("{scheme}://{host}:{}", cfg.broker_port);

    let device = build_device(state, device_id);
    let state_t = state_topic(device_id);

    // The publish SEQUENCE + the LWT now come from the host-pure, host-tested
    // plan in `mqtt_ha` (`build_publish_plan` / `lwt_spec`). This transport only
    // owns the connection + the loop cadence; the wire output (topics/payloads/
    // retain/QoS) is exactly what this fn used to inline. The plan is proven
    // host-side against an in-process mock broker — that is NOT live-broker proof
    // (live delivery stays operator/broker-gated).
    let lwt_spec = lwt_spec(device_id);

    // Borrows below must outlive the new_cb() call (the C client copies them
    // synchronously at construction). They all live for the rest of this fn.
    let client_id = device_id.to_string();
    let username = cfg.username.clone();
    let password = cfg.password.clone();
    let lwt = LwtConfiguration {
        topic: &lwt_spec.topic,
        payload: lwt_spec.payload.as_bytes(),
        qos: esp_qos(lwt_spec.qos),
        retain: lwt_spec.retain,
    };
    let conf = MqttClientConfiguration {
        client_id: Some(&client_id),
        username: (!username.is_empty()).then_some(username.as_str()),
        password: (!password.is_empty()).then_some(password.as_str()),
        keep_alive_interval: Some(Duration::from_secs(30)),
        buffer_size: MQTT_BUFFER_SIZE,
        out_buffer_size: MQTT_BUFFER_SIZE,
        lwt: Some(lwt),
        ..Default::default()
    };

    // new_cb pumps the connection internally (no extra pump thread needed); the
    // callback is publish-only, so it just drops inbound events.
    let mut client =
        EspMqttClient::new_cb(&url, &conf, |_event| {}).map_err(|e| format!("connect: {e}"))?;

    // Whether the operator opted into the HA COMMAND surface (number/select/
    // climate). DEFAULT-OFF: when false the plan advertises ONLY the read-only
    // telemetry entities and never publishes a `command_topic`.
    let commands_enabled = cfg.commands_enabled;

    // On-connect plan: retained discovery configs (so a freshly started HA still
    // auto-creates the entities) -> availability `online` -> the first state
    // payload (so entities show a value immediately). All ops are load-bearing
    // on connect, so any enqueue failure ends the session and reconnects.
    let connect_plan = build_publish_plan(
        &device,
        &snapshot_state(state, energy.energy_kwh()),
        PublishPhase::OnConnect,
        commands_enabled,
    );
    let discovery_count = connect_plan
        .iter()
        .filter(|op| op.topic.starts_with("homeassistant/"))
        .count();
    for op in &connect_plan {
        client
            .enqueue(&op.topic, esp_qos(op.qos), op.retain, op.payload.as_bytes())
            .map_err(|e| format!("on-connect publish to {}: {e}", op.topic))?;
    }
    info!(
        "MQTT/HA discovery published ({discovery_count} entities) — miner will appear in Home Assistant"
    );

    let interval_s = cfg.publish_interval_s.max(MIN_PUBLISH_INTERVAL_S) as u64;
    let interval = Duration::from_secs(interval_s);

    // The on-connect plan already shipped the first state, so the first periodic
    // tick must NOT re-publish it — it only refreshes availability. From the
    // second tick on, the full periodic plan runs. This keeps the publish stream
    // byte-identical to the prior inline loop (which published state, then the
    // availability heartbeat, then slept).
    let mut first_tick = true;
    loop {
        // Stop cleanly if MQTT was disabled at runtime.
        let still_enabled = state
            .config
            .lock()
            .map(|c| c.mqtt.enabled)
            .unwrap_or_else(|e| e.into_inner().mqtt.enabled);
        if !still_enabled {
            return Ok(());
        }

        // Snapshot once per tick: the state payload carries the cumulative energy
        // integrated so far, and we reuse its `power_w` to advance the accumulator
        // for the interval we are about to sleep.
        let snap = snapshot_state(state, energy.energy_kwh());

        // Periodic plan: the state payload (load-bearing — reconnect on failure)
        // then a cheap retained availability heartbeat (best-effort, matching the
        // prior inline behavior).
        let tick_plan =
            build_publish_plan(&device, &snap, PublishPhase::Periodic, commands_enabled);
        for op in &tick_plan {
            // First tick: the on-connect plan already published this state, so
            // skip the duplicate (still refresh availability below).
            if first_tick && op.topic == state_t {
                continue;
            }
            let r = client.enqueue(&op.topic, esp_qos(op.qos), op.retain, op.payload.as_bytes());
            if op.topic == state_t {
                r.map_err(|e| format!("state publish: {e}"))?;
            } else {
                let _ = r; // availability refresh — best-effort heartbeat
            }
        }
        first_tick = false;

        // Integrate this interval's energy from the just-read input power BEFORE
        // sleeping, so the NEXT state publish reports the updated cumulative kWh.
        // `add_sample` is fail-benign (non-finite/negative power is ignored), so a
        // garbage reading can never corrupt or decrease the monotonic total.
        energy.add_sample(snap.power_w, interval_s as f64);
        std::thread::sleep(interval);
    }
}

/// Build the stable HA device identity from the current config (name/model/url
/// from board config + hostname/IP). `device_id` is MAC-derived and stable.
fn build_device(state: &SharedState, device_id: &str) -> HaDevice {
    let (name, model, hostname) = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let board = cfg.board_config();
        let model_name = board.model.name();
        (
            format!("DCENT_axe {model_name}"),
            format!("{model_name} / {}", board.asic_model),
            cfg.hostname.clone(),
        )
    };

    // Prefer the live IP; fall back to <hostname>.local; else omit.
    let configuration_url = {
        let ip = state
            .telemetry
            .lock()
            .map(|t| t.device_ip.clone())
            .unwrap_or_else(|e| e.into_inner().device_ip.clone());
        if !ip.is_empty() {
            Some(format!("http://{ip}"))
        } else if !hostname.is_empty() {
            Some(format!("http://{hostname}.local"))
        } else {
            None
        }
    };

    HaDevice {
        device_id: device_id.to_string(),
        name,
        model,
        sw_version: env!("CARGO_PKG_VERSION").to_string(),
        configuration_url,
    }
}

/// Snapshot the live telemetry into the host-pure [`HaState`]. Snapshot-then-drop
/// each lock so we never hold one across a publish (panic=abort lock-safety).
/// `energy_kwh` is the cumulative lifetime energy from the caller-owned
/// [`EnergyAccumulator`] (integrated across ticks in `run_session`), so the meter
/// stays monotonic — it is NOT re-derived here per tick.
fn snapshot_state(state: &SharedState, energy_kwh: f64) -> HaState {
    let (hashrate_ghs, accepted, rejected) = {
        let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
        let snap = stats.snapshot();
        (
            snap.hashrate_1m_ghs as f32,
            snap.accepted_shares,
            snap.rejected_shares,
        )
    };
    let (chip_temp_c, power_w, fan_rpm, uptime_s) = {
        let t = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
        (t.chip_temp_c, t.power_w, t.fan_rpm, t.uptime_secs)
    };
    let (board_version_recognized, support_status) = {
        let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
        (
            cfg.board_version_recognized(),
            cfg.support_status().to_string(),
        )
    };

    HaState {
        hashrate_ghs,
        chip_temp_c,
        power_w,
        fan_rpm,
        accepted,
        rejected,
        uptime_s,
        energy_kwh,
        board_version_recognized,
        support_status,
    }
}
