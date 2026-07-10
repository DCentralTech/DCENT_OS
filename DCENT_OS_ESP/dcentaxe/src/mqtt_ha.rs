// DCENT_axe — Home Assistant MQTT auto-discovery (host-pure payload builder)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Home Assistant MQTT auto-discovery — the host-pure, unit-tested CORE.
//!
//! Given the device identity (derived from the MAC) + a telemetry snapshot, this
//! module produces:
//!   1. the retained HA **discovery** config topics + JSON payloads
//!      (`homeassistant/<component>/<device_id>/<object>/config`), and
//!   2. the periodic **state** topic + JSON payload the discovery
//!      `value_template`s read from.
//!
//! Everything here is pure data transformation over plain inputs — NO esp-idf,
//! NO network — so it host-compiles and unit-tests under
//! `cargo test -p dcentaxe-core` (re-included via `#[path]` in
//! `dcentaxe-core/src/lib.rs`, the same single-source-of-truth pattern used by
//! `config.rs`/`ota_signature.rs`). The esp-idf transport that connects to a
//! broker and publishes these payloads lives in `dcentaxe/src/mqtt.rs` and stays
//! thin so this builder carries all the testable logic.
//!
//! HA-discovery shape reference: the Antminer-side DCENT_OS implementation at
//! `DCENT_OS_Antminer/dcentrald/dcentrald-api/src/mqtt.rs`. This is a fresh
//! esp-idf-runtime implementation — the TOPIC/PAYLOAD *shape* is shared, the code
//! is not.
//!
//! SCOPE NOTE (honesty): the read-only telemetry surface (`sensor` /
//! `binary_sensor`, including the `device_class=energy` kWh meter) is ALWAYS
//! published — it is publish-only and never affects mining. The optional
//! operator-CONTROL surface (HA `number` / `select` / `climate` COMMAND
//! entities: target watts, autotuner mode, target chip temperature) is
//! **default-OFF** and is only advertised + subscribed when
//! `mqtt.commands_enabled` is set. Every commanded value is CLAMPED here to the
//! SAME safety envelope the local REST/autotuner setters enforce (the
//! target-watts / target-temp bounds mirror `chip_profiles_bitaxe`'s
//! `validate_autotune_target` limits, pinned equal by a `dcentaxe-core` test;
//! the downstream autotuner re-clamps freq/voltage against the board V/F caps and
//! the thermal fan curve keeps the home PWM floor). A remote HA publish can
//! therefore never open an un-clamped path or bypass a fail-closed gate — the
//! Antminer `MqttCommandSink` clamp contract, ported host-pure to ESP.

use serde_json::{json, Value};

/// HA `device` block `manufacturer`. Pinned by a host test.
pub const MANUFACTURER: &str = "D-Central";

/// Stable HA device identity — everything the discovery `device` block needs.
/// All fields are owned so the builder is borrow-free for the caller.
#[derive(Debug, Clone)]
pub struct HaDevice {
    /// Stable HA device id, e.g. `dcentaxe_27F6AB` (see [`device_id_from_mac`]).
    pub device_id: String,
    /// Friendly device name, e.g. `DCENT_axe Gamma`.
    pub name: String,
    /// Hardware model string, e.g. `Gamma / BM1370`.
    pub model: String,
    /// Firmware version (`CARGO_PKG_VERSION`).
    pub sw_version: String,
    /// Optional dashboard URL surfaced in HA, e.g. `http://dcentaxe.local`.
    pub configuration_url: Option<String>,
}

/// Live telemetry snapshot for the periodic STATE payload. Sourced from the same
/// fields `/api/system/info` already exposes (hashrate / chip temp / power / fan
/// RPM / accepted+rejected shares / uptime).
#[derive(Debug, Clone, Default)]
pub struct HaState {
    pub hashrate_ghs: f32,
    pub chip_temp_c: f32,
    pub power_w: f32,
    pub fan_rpm: u32,
    pub accepted: u64,
    pub rejected: u64,
    pub uptime_s: u64,
    /// Lifetime energy consumed, in kWh, integrated from the reported input
    /// power over the publish interval (see [`EnergyAccumulator`]). Feeds the
    /// `device_class=energy` / `state_class=total_increasing` HA sensor the HA
    /// Energy dashboard requires. Monotonic within a boot; resets to 0 on reboot
    /// (HA treats a decrease as a meter reset for `total_increasing`).
    pub energy_kwh: f64,
    /// Whether the runtime `board_version` was recognized by the firmware table.
    pub board_version_recognized: bool,
    /// Coarse operator support state: `supported`, `experimental`, or `unknown`.
    pub support_status: String,
}

/// Derive the stable HA device id from a MAC string (`"aa:bb:cc:dd:ee:ff"` or
/// `"AABBCCDDEEFF"`). Uses the last 3 octets (6 hex chars), uppercased, so two
/// units on the same broker never collide. An empty / hex-less MAC yields a
/// deterministic `dcentaxe_unknown` fallback (never panics).
pub fn device_id_from_mac(mac: &str) -> String {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.is_empty() {
        return "dcentaxe_unknown".to_string();
    }
    let suffix = &hex[hex.len().saturating_sub(6)..];
    format!("dcentaxe_{}", suffix.to_ascii_uppercase())
}

/// `dcentaxe/<device_id>` — the per-device topic root the publisher writes to.
pub fn topic_prefix(device_id: &str) -> String {
    format!("dcentaxe/{device_id}")
}

/// The retained/periodic STATE topic the discovery `value_template`s read.
pub fn state_topic(device_id: &str) -> String {
    format!("{}/state", topic_prefix(device_id))
}

/// The availability (LWT) topic — `online` while connected, `offline` via LWT.
pub fn availability_topic(device_id: &str) -> String {
    format!("{}/availability", topic_prefix(device_id))
}

/// One HA discovery message: the retained `config` topic plus the serialized JSON
/// discovery payload HA consumes to auto-create the entity.
#[derive(Debug, Clone, PartialEq)]
pub struct HaDiscoveryEntity {
    /// `homeassistant/<component>/<device_id>/<object_id>/config`.
    pub config_topic: String,
    /// The serialized JSON discovery config payload.
    pub payload: String,
}

/// Coerce a possibly non-finite f32 to a finite f64 for JSON (NaN/Inf -> 0.0).
/// `serde_json` cannot represent NaN/Inf; without this guard a NaN sensor reading
/// would either serialize as `null` or fail — HA would then show "unavailable"
/// noise. We publish a finite 0.0 instead (honest "no data yet").
fn finite(v: f32) -> f64 {
    if v.is_finite() {
        v as f64
    } else {
        0.0
    }
}

/// `finite` for an f64 (the energy accumulator is f64). NaN/Inf -> 0.0.
fn finite_f64(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Build the shared HA `device` block all entities reference.
fn device_block(device: &HaDevice) -> Value {
    let mut block = json!({
        "identifiers": [device.device_id],
        "name": device.name,
        "manufacturer": MANUFACTURER,
        "model": device.model,
        "sw_version": device.sw_version,
    });
    if let Some(url) = &device.configuration_url {
        if !url.is_empty() {
            block["configuration_url"] = json!(url);
        }
    }
    block
}

/// Build the full set of HA discovery entities for this device (publish-only).
///
/// Emits one `sensor` per "Bitcoin space heater" metric — hashrate (GH/s), ASIC
/// temperature (°C), input power (W), fan RPM, accepted shares, rejected shares,
/// uptime — plus one `binary_sensor` for mining-active. Every entity reads the
/// shared [`state_topic`] and is gated on [`availability_topic`]; each carries a
/// device-namespaced `unique_id` so HA keys them stably.
///
/// The `value_template`s reference exactly the keys [`build_state_payload`]
/// emits — a host test pins that contract so the two can never drift.
pub fn build_ha_discovery_entities(device: &HaDevice) -> Vec<HaDiscoveryEntity> {
    let did = &device.device_id;
    let state_t = state_topic(did);
    let avail_t = availability_topic(did);
    let device = device_block(device);

    let mut entities: Vec<HaDiscoveryEntity> = Vec::new();

    // (object_id, name, value_template, unit, device_class, state_class, icon)
    let sensors: &[(&str, &str, &str, &str, Option<&str>, Option<&str>, &str)] = &[
        (
            "hashrate",
            "Hashrate",
            "{{ value_json.hashrate_ghs | round(2) }}",
            "GH/s",
            None,
            Some("measurement"),
            "mdi:pickaxe",
        ),
        (
            "temperature",
            "ASIC Temperature",
            "{{ value_json.temp_c | round(1) }}",
            "\u{00B0}C",
            Some("temperature"),
            Some("measurement"),
            "mdi:thermometer",
        ),
        (
            "power",
            "Power",
            "{{ value_json.power_w | round(1) }}",
            "W",
            Some("power"),
            Some("measurement"),
            "mdi:flash",
        ),
        // Energy meter — REQUIRED by the HA Energy dashboard (device_class=energy
        // + state_class=total_increasing + a kWh unit). This is the space-heater
        // ROI surface: HA graphs kWh/day and $/day from it. The value is the
        // pure watt-seconds integral in [`EnergyAccumulator`], published as kWh.
        (
            "energy",
            "Energy",
            "{{ value_json.energy_kwh | round(4) }}",
            "kWh",
            Some("energy"),
            Some("total_increasing"),
            "mdi:lightning-bolt",
        ),
        (
            "fan_rpm",
            "Fan Speed",
            "{{ value_json.fan_rpm }}",
            "RPM",
            None,
            Some("measurement"),
            "mdi:fan",
        ),
        (
            "accepted",
            "Accepted Shares",
            "{{ value_json.accepted }}",
            "",
            None,
            Some("total_increasing"),
            "mdi:check-circle",
        ),
        (
            "rejected",
            "Rejected Shares",
            "{{ value_json.rejected }}",
            "",
            None,
            Some("total_increasing"),
            "mdi:close-circle",
        ),
        (
            "uptime",
            "Uptime",
            "{{ value_json.uptime_s }}",
            "s",
            Some("duration"),
            None,
            "mdi:clock-outline",
        ),
    ];

    for (object_id, name, value_template, unit, device_class, state_class, icon) in sensors {
        let unique_id = format!("{did}_{object_id}");
        let config_topic = format!("homeassistant/sensor/{did}/{object_id}/config");

        let mut payload = json!({
            "name": name,
            "unique_id": unique_id,
            "state_topic": state_t,
            "availability_topic": avail_t,
            "value_template": value_template,
            "device": device,
            "icon": icon,
        });
        if !unit.is_empty() {
            payload["unit_of_measurement"] = json!(unit);
        }
        if let Some(dc) = device_class {
            payload["device_class"] = json!(dc);
        }
        if let Some(sc) = state_class {
            payload["state_class"] = json!(sc);
        }

        entities.push(HaDiscoveryEntity {
            config_topic,
            payload: payload.to_string(),
        });
    }

    // binary_sensor: mining active (derived from positive hashrate).
    let mining_uid = format!("{did}_mining");
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/binary_sensor/{did}/mining/config"),
        payload: json!({
            "name": "Mining Active",
            "unique_id": mining_uid,
            "state_topic": state_t,
            "availability_topic": avail_t,
            "value_template": "{{ 'ON' if value_json.hashrate_ghs > 0 else 'OFF' }}",
            "device_class": "running",
            "device": device,
            "icon": "mdi:pickaxe",
        })
        .to_string(),
    });

    entities
}

/// Build the periodic STATE payload — the JSON the discovery `value_template`s
/// read. Keys MUST match the templates in [`build_ha_discovery_entities`]
/// (host-test pinned). Non-finite floats are coerced to 0.0 ([`finite`]).
pub fn build_state_payload(s: &HaState) -> String {
    json!({
        "hashrate_ghs": finite(s.hashrate_ghs),
        "temp_c": finite(s.chip_temp_c),
        "power_w": finite(s.power_w),
        "fan_rpm": s.fan_rpm,
        "accepted": s.accepted,
        "rejected": s.rejected,
        "uptime_s": s.uptime_s,
        // Lifetime energy (kWh) for the HA Energy dashboard. Coerced finite +
        // clamped non-negative so a corrupt accumulator can never publish a
        // decreasing/NaN total (which would break `total_increasing`).
        "energy_kwh": finite_f64(s.energy_kwh).max(0.0),
        // Convenience boolean for the mining binary_sensor (the template reads
        // hashrate_ghs directly, but this keeps the payload self-describing).
        "mining": finite(s.hashrate_ghs) > 0.0,
        "board_version_recognized": s.board_version_recognized,
        "support_status": if s.support_status.is_empty() {
            "unknown"
        } else {
            s.support_status.as_str()
        },
    })
    .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Publish PLAN (host-pure publish SEQUENCE + LWT model)
//
// The transport (`dcentaxe/src/mqtt.rs`) used to inline the publish ORDER —
// which discovery configs go out, then availability, then state, with which
// retain/QoS — directly in the esp-idf loop, so that sequencing was NOT
// host-testable. This block extracts it into pure data the transport iterates,
// so an in-process mock broker can assert the full sequence host-side.
//
// HONESTY: this proves the publish SEQUENCE in-host against a mock broker; it
// does NOT make MQTT live-broker-proven on hardware (that stays operator/broker
// -gated). The wire output (topics/payloads/retain/QoS) is exactly what the
// inline transport published before the extraction.
// ─────────────────────────────────────────────────────────────────────────────

/// Availability payload published (retained) on connect and refreshed each tick.
pub const AVAIL_ONLINE: &str = "online";
/// Availability payload the broker publishes (retained) via LWT on a dirty drop.
pub const AVAIL_OFFLINE: &str = "offline";

/// MQTT QoS level for a publish op / LWT. Kept transport-agnostic (no esp-idf
/// `QoS` dependency) so the plan stays host-pure. The transport maps this onto
/// `esp_idf_svc::mqtt::client::QoS`. Mirrors exactly the two levels the publisher
/// uses today: the high-cadence state payload is `AtMostOnce` (fire-and-forget),
/// retained discovery + availability are `AtLeastOnce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttQos {
    /// QoS 0 — fire-and-forget (the high-cadence state payload).
    AtMostOnce,
    /// QoS 1 — at-least-once (retained discovery + availability).
    AtLeastOnce,
}

/// One publish operation the transport must perform, as pure data:
/// `(topic, payload, retain, qos)`. No esp-idf, no network — the transport maps
/// `qos` to the esp-idf `QoS` enum and calls `enqueue(topic, qos, retain, bytes)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttPublishOp {
    /// Destination topic.
    pub topic: String,
    /// UTF-8 payload (JSON for discovery/state; `online` for availability).
    pub payload: String,
    /// Retain flag — `true` for discovery configs + availability, `false` for state.
    pub retain: bool,
    /// Quality of Service for this publish.
    pub qos: MqttQos,
}

/// The Last-Will-and-Testament: the retained `offline` message the broker
/// publishes on the availability topic when our connection drops uncleanly — the
/// mirror of the `online` we publish on connect. Set at client construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LwtSpec {
    /// Availability topic (same one the `online` op publishes to).
    pub topic: String,
    /// LWT payload (`offline`).
    pub payload: String,
    /// Retain flag (`true`, so subscribers see `offline` even if they connect late).
    pub retain: bool,
    /// LWT QoS.
    pub qos: MqttQos,
}

/// Which publish phase the plan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPhase {
    /// First publish burst on every (re)connect: the retained HA discovery configs
    /// (one per entity) THEN the availability `online` message THEN the first
    /// state payload (so HA shows a value immediately, not "unknown").
    OnConnect,
    /// Steady-state per-tick publish: the state payload THEN an availability
    /// `online` refresh (a cheap retained heartbeat — the existing design).
    Periodic,
}

/// The retained `offline` LWT spec for this device.
pub fn lwt_spec(device_id: &str) -> LwtSpec {
    LwtSpec {
        topic: availability_topic(device_id),
        payload: AVAIL_OFFLINE.to_string(),
        retain: true,
        qos: MqttQos::AtLeastOnce,
    }
}

/// Build the ORDERED publish plan the transport must perform for `phase`.
///
/// Pure: no esp-idf, no network. The transport iterates this list in order,
/// calling `client.enqueue(topic, qos, retain, payload)` for each op, and sets
/// the [`LwtSpec`] at client construction.
///
/// - [`PublishPhase::OnConnect`] → retained discovery configs (one per entity,
///   retain=true, QoS1) **then** availability `online` (retain=true, QoS1)
///   **then** the first state payload (retain=false, QoS0).
/// - [`PublishPhase::Periodic`] → the state payload (retain=false, QoS0) **then**
///   an availability `online` refresh (retain=true, QoS1).
///
/// The discovery ops are byte-for-byte the topics/payloads of
/// [`build_ha_discovery_entities`]; the state op is exactly
/// [`build_state_payload`]; the availability/LWT payloads are
/// [`AVAIL_ONLINE`]/[`AVAIL_OFFLINE`]. So this extracts the SEQUENCE without
/// changing the wire output.
pub fn build_publish_plan(
    device: &HaDevice,
    state: &HaState,
    phase: PublishPhase,
    commands_enabled: bool,
) -> Vec<MqttPublishOp> {
    let did = &device.device_id;
    let avail_online = MqttPublishOp {
        topic: availability_topic(did),
        payload: AVAIL_ONLINE.to_string(),
        retain: true,
        qos: MqttQos::AtLeastOnce,
    };
    let state_op = MqttPublishOp {
        topic: state_topic(did),
        payload: build_state_payload(state),
        retain: false,
        qos: MqttQos::AtMostOnce,
    };

    match phase {
        PublishPhase::OnConnect => {
            let mut entities = build_ha_discovery_entities(device);
            // Operator-CONTROL entities are advertised ONLY when the operator
            // opted in (default-OFF). They are retained like the read-only
            // configs so a restarted HA re-creates them.
            if commands_enabled {
                entities.extend(build_command_entities(device));
            }
            let mut ops = Vec::with_capacity(entities.len() + 2);
            for e in entities {
                ops.push(MqttPublishOp {
                    topic: e.config_topic,
                    payload: e.payload,
                    retain: true,
                    qos: MqttQos::AtLeastOnce,
                });
            }
            ops.push(avail_online);
            ops.push(state_op);
            ops
        }
        PublishPhase::Periodic => vec![state_op, avail_online],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HA Energy sensor — pure watt-seconds accumulator
//
// The HA Energy dashboard REQUIRES a `device_class=energy` +
// `state_class=total_increasing` sensor in a kWh-family unit. Neither firmware
// emitted one, so the space-heater ROI story (kWh/day, $/day) could not be told
// in HA. This is the pure, host-tested integrator: the transport feeds it the
// reported input power once per publish interval and reads back `energy_kwh`.
// ─────────────────────────────────────────────────────────────────────────────

/// Watt-seconds integrator producing a monotonic lifetime energy total (kWh).
///
/// Pure: no esp-idf, no clock — the caller supplies the elapsed seconds (the
/// publish interval), so it host-tests deterministically. The total only ever
/// increases (each sample adds `power_w * elapsed_s`, and non-finite/negative
/// inputs are ignored), so it satisfies HA's `total_increasing` contract.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnergyAccumulator {
    watt_seconds: f64,
}

impl EnergyAccumulator {
    /// A fresh, zeroed accumulator.
    pub fn new() -> Self {
        Self { watt_seconds: 0.0 }
    }

    /// Integrate one power sample over `elapsed_s` seconds. FAIL-BENIGN: a
    /// non-finite / negative power reading, or a non-positive / non-finite
    /// elapsed, is IGNORED — a garbage sensor reading can never corrupt the
    /// monotonic total or make it decrease (which would break `total_increasing`
    /// in HA). Returns the running kWh total for convenience.
    pub fn add_sample(&mut self, power_w: f32, elapsed_s: f64) -> f64 {
        let p = power_w as f64;
        if p.is_finite() && p >= 0.0 && elapsed_s.is_finite() && elapsed_s > 0.0 {
            self.watt_seconds += p * elapsed_s;
        }
        self.energy_kwh()
    }

    /// The accumulated energy in kWh (watt-seconds / 3_600_000).
    pub fn energy_kwh(&self) -> f64 {
        self.watt_seconds / 3_600_000.0
    }

    /// The raw accumulated watt-seconds (exposed for tests / diagnostics).
    pub fn watt_seconds(&self) -> f64 {
        self.watt_seconds
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Operator-CONTROL surface (default-OFF) — HA command entities + clamp contract
//
// This is the ESP port of the Antminer `MqttCommandSink` clamp contract
// (`dcentrald-api/src/mqtt.rs`). HA gets a `number` (target watts), a `select`
// (autotuner mode), and a `climate` (space-heater target chip temperature).
// Every inbound `*/set` publish is parsed + CLAMPED here to the SAME envelope
// the local REST/autotuner setters enforce, so a remote HA command can never
// open an un-clamped path. The transport (`mqtt.rs`) applies the parsed command
// through the SAME autotuner state the REST `/api/mining/autotune` handler uses
// (which re-validates via `chip_profiles_bitaxe::validate_autotune_target` and
// re-clamps freq/voltage against the board V/F caps), and echoes the APPLIED
// (clamped) value back to the entity's `/state` topic.
//
// HONESTY: this is host-pure + mock-broker-tested (parse/clamp/echo/subscribe);
// it is NOT live-broker-proven on hardware (that stays operator/broker-gated).
// ─────────────────────────────────────────────────────────────────────────────

/// Min target power (W) advertised to HA AND enforced by [`parse_command`]'s
/// clamp. A single Bitaxe-class board idles well under this; below it there is no
/// meaningful watt-target.
pub const CMD_TARGET_WATTS_MIN: f32 = 5.0;
/// Max target power (W) advertised to HA AND enforced by the clamp. Pinned EQUAL
/// to `chip_profiles_bitaxe::MAX_AUTOTUNE_TARGET_WATTS` by a `dcentaxe-core`
/// cross-module test, so the advertised HA ceiling can never drift above the
/// validator's board-power budget.
pub const CMD_TARGET_WATTS_MAX: f32 = 200.0;
/// Min target chip temperature (°C). Pinned EQUAL to
/// `chip_profiles_bitaxe::MIN_AUTOTUNE_TARGET_TEMP_C`.
pub const CMD_TARGET_TEMP_MIN_C: f32 = 40.0;
/// Max target chip temperature (°C). Pinned EQUAL to
/// `chip_profiles_bitaxe::MAX_AUTOTUNE_TARGET_TEMP_C` — strictly below the
/// thermal-shutdown ceiling, so a remote HA setpoint can never park the target
/// at a dangerous temperature.
pub const CMD_TARGET_TEMP_MAX_C: f32 = 95.0;

/// Canonical autotuner-mode API strings (mirror `shared::AutotuneMode::as_api_str`;
/// pinned by a `dcentaxe-core` source-text guard). These are the HA `select`
/// options AND the accepted `*/set` payloads.
pub const AUTOTUNE_MODE_MAX_HASHRATE: &str = "max_hashrate";
pub const AUTOTUNE_MODE_BEST_EFFICIENCY: &str = "best_efficiency";
pub const AUTOTUNE_MODE_TARGET_WATTS: &str = "target_watts";
pub const AUTOTUNE_MODE_TARGET_TEMP: &str = "target_temp";
/// All four canonical modes, in HA `select` option order.
pub const AUTOTUNE_MODES: [&str; 4] = [
    AUTOTUNE_MODE_MAX_HASHRATE,
    AUTOTUNE_MODE_BEST_EFFICIENCY,
    AUTOTUNE_MODE_TARGET_WATTS,
    AUTOTUNE_MODE_TARGET_TEMP,
];

/// The per-device command/state topic names for the operator-control entities.
#[derive(Debug, Clone)]
pub struct CommandTopics {
    pub target_watts_set: String,
    pub target_watts_state: String,
    pub autotune_mode_set: String,
    pub autotune_mode_state: String,
    pub target_temp_set: String,
    pub target_temp_state: String,
}

impl CommandTopics {
    pub fn new(device_id: &str) -> Self {
        let p = topic_prefix(device_id);
        Self {
            target_watts_set: format!("{p}/target_watts/set"),
            target_watts_state: format!("{p}/target_watts/state"),
            autotune_mode_set: format!("{p}/autotune_mode/set"),
            autotune_mode_state: format!("{p}/autotune_mode/state"),
            target_temp_set: format!("{p}/target_temp/set"),
            target_temp_state: format!("{p}/target_temp/state"),
        }
    }

    /// The topics the transport must SUBSCRIBE to (replayed on every connect —
    /// with a clean session the broker forgets subscriptions on disconnect).
    pub fn subscribe_topics(&self) -> [&str; 3] {
        [
            self.target_watts_set.as_str(),
            self.autotune_mode_set.as_str(),
            self.target_temp_set.as_str(),
        ]
    }
}

/// Convenience: the three command `*/set` topics the transport subscribes to.
pub fn command_subscribe_topics(device_id: &str) -> [String; 3] {
    let t = CommandTopics::new(device_id);
    [t.target_watts_set, t.autotune_mode_set, t.target_temp_set]
}

/// A parsed + ALREADY-CLAMPED HA command. Construction guarantees the value is
/// inside the advertised safety envelope, so the transport can apply it directly
/// (it still re-validates as defense-in-depth, matching the Antminer sink).
#[derive(Debug, Clone, PartialEq)]
pub enum HaCommand {
    /// Target power (W), clamped to `[CMD_TARGET_WATTS_MIN, CMD_TARGET_WATTS_MAX]`.
    TargetWatts(f32),
    /// Autotuner mode — one of the canonical [`AUTOTUNE_MODES`] strings.
    AutotuneMode(&'static str),
    /// Target chip temperature (°C), clamped to
    /// `[CMD_TARGET_TEMP_MIN_C, CMD_TARGET_TEMP_MAX_C]`.
    TargetTempC(f32),
}

/// Normalize + validate an autotuner-mode payload. Mirrors
/// `shared::AutotuneMode::from_api_str` (trim, lowercase, `-`→`_`). Returns the
/// canonical `&'static str` on match, or `None` (FAIL-CLOSED — an unknown mode
/// leaves HA's last-known state untouched, never applies a garbage mode).
pub fn parse_autotune_mode(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "max_hashrate" => Some(AUTOTUNE_MODE_MAX_HASHRATE),
        "best_efficiency" => Some(AUTOTUNE_MODE_BEST_EFFICIENCY),
        "target_watts" => Some(AUTOTUNE_MODE_TARGET_WATTS),
        "target_temp" => Some(AUTOTUNE_MODE_TARGET_TEMP),
        _ => None,
    }
}

/// Parse an inbound MQTT publish into a CLAMPED [`HaCommand`], or `None` if the
/// topic is not a command topic or the payload is unusable.
///
/// Pure + esp-idf-free so the whole command path is unit-testable without a
/// broker. FAIL-CLOSED: an out-of-range NUMBER is CLAMPED into the envelope (the
/// truthful accepted value is echoed back), but a non-finite / unparseable /
/// empty payload — or an unknown mode — yields `None`, so HA's last-known
/// (truthful) state is left untouched rather than applying garbage.
pub fn parse_command(topics: &CommandTopics, topic: &str, payload: &[u8]) -> Option<HaCommand> {
    let text = std::str::from_utf8(payload).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    if topic == topics.target_watts_set {
        let v: f32 = text.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        Some(HaCommand::TargetWatts(
            v.clamp(CMD_TARGET_WATTS_MIN, CMD_TARGET_WATTS_MAX),
        ))
    } else if topic == topics.target_temp_set {
        let v: f32 = text.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        Some(HaCommand::TargetTempC(
            v.clamp(CMD_TARGET_TEMP_MIN_C, CMD_TARGET_TEMP_MAX_C),
        ))
    } else if topic == topics.autotune_mode_set {
        parse_autotune_mode(text).map(HaCommand::AutotuneMode)
    } else {
        None
    }
}

/// The `/state` topic + APPLIED (clamped) value string to echo back to HA after
/// a command is applied, so the entity reflects the truthful accepted value
/// (e.g. `100 W` → `200 W`, `500 °C` → `95 °C`). Numbers echo as trimmed
/// integers (the HA `number`/`climate` entities use integer steps).
pub fn command_state_echo(topics: &CommandTopics, cmd: &HaCommand) -> (String, String) {
    match cmd {
        HaCommand::TargetWatts(w) => (
            topics.target_watts_state.clone(),
            format!("{}", w.round() as i64),
        ),
        HaCommand::AutotuneMode(m) => (topics.autotune_mode_state.clone(), (*m).to_string()),
        HaCommand::TargetTempC(t) => (
            topics.target_temp_state.clone(),
            format!("{}", t.round() as i64),
        ),
    }
}

/// Build the operator-CONTROL discovery entities (a `number` for target watts, a
/// `select` for autotuner mode, a `climate` "Space Heater" for target chip
/// temperature). Advertised ONLY when `mqtt.commands_enabled` (the caller gates
/// this via [`build_publish_plan`]). Each advertises the SAME `min`/`max` the
/// [`parse_command`] clamp enforces, so HA's slider/box can't even request an
/// out-of-envelope value (and the sink re-clamps regardless).
pub fn build_command_entities(device: &HaDevice) -> Vec<HaDiscoveryEntity> {
    let did = &device.device_id;
    let t = CommandTopics::new(did);
    let avail_t = availability_topic(did);
    let device_block = device_block(device);

    let mut entities = Vec::with_capacity(3);

    // number: target power (W) — clamped, routed to the autotuner TargetWatts mode.
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/number/{did}/target_watts_set/config"),
        payload: json!({
            "name": "Target Power",
            "unique_id": format!("{did}_target_watts_set"),
            "command_topic": t.target_watts_set,
            "state_topic": t.target_watts_state,
            "availability_topic": avail_t,
            "min": CMD_TARGET_WATTS_MIN,
            "max": CMD_TARGET_WATTS_MAX,
            "step": 5,
            "mode": "box",
            "unit_of_measurement": "W",
            "device_class": "power",
            "icon": "mdi:flash",
            "device": device_block,
        })
        .to_string(),
    });

    // select: autotuner mode — one of the 4 canonical modes; unknown => rejected.
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/select/{did}/autotune_mode_set/config"),
        payload: json!({
            "name": "Autotuner Mode",
            "unique_id": format!("{did}_autotune_mode_set"),
            "command_topic": t.autotune_mode_set,
            "state_topic": t.autotune_mode_state,
            "availability_topic": avail_t,
            "options": AUTOTUNE_MODES,
            "icon": "mdi:tune-variant",
            "device": device_block,
        })
        .to_string(),
    });

    // climate: space-heater target CHIP temperature. current_temperature reads
    // the MEASURED chip temp from the periodic state publish (honest, not a
    // fabricated room temp); the setpoint is clamped to the thermal envelope.
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/climate/{did}/heater/config"),
        payload: json!({
            "name": "Space Heater",
            "unique_id": format!("{did}_heater"),
            "availability_topic": avail_t,
            "temperature_command_topic": t.target_temp_set,
            "temperature_state_topic": t.target_temp_state,
            "current_temperature_topic": state_topic(did),
            "current_temperature_template": "{{ value_json.temp_c | round(1) }}",
            "min_temp": CMD_TARGET_TEMP_MIN_C,
            "max_temp": CMD_TARGET_TEMP_MAX_C,
            "temp_step": 1,
            "temperature_unit": "C",
            "modes": ["heat"],
            "icon": "mdi:radiator",
            "device": device_block,
        })
        .to_string(),
    });

    entities
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sample_device() -> HaDevice {
        HaDevice {
            device_id: device_id_from_mac("aa:bb:cc:dd:ee:ff"),
            name: "DCENT_axe Gamma".to_string(),
            model: "Gamma / BM1370".to_string(),
            sw_version: "0.3.0".to_string(),
            configuration_url: Some("http://dcentaxe.local".to_string()),
        }
    }

    fn parse(payload: &str) -> Value {
        serde_json::from_str(payload).expect("discovery/state payload must be valid JSON")
    }

    /// Extract every `value_json.<key>` reference from a Jinja2 template string.
    fn value_json_keys(template: &str) -> Vec<String> {
        let needle = "value_json.";
        let mut keys = Vec::new();
        let bytes = template.as_bytes();
        let mut i = 0;
        while let Some(pos) = template[i..].find(needle) {
            let start = i + pos + needle.len();
            let mut end = start;
            while end < bytes.len() {
                let c = bytes[end];
                if c == b'_' || c.is_ascii_alphanumeric() {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start {
                keys.push(template[start..end].to_string());
            }
            i = start.max(i + pos + 1);
        }
        keys
    }

    // ── device id derivation: stable, uppercase, last-6-hex, panic-free ──────
    #[test]
    fn device_id_from_mac_derivation() {
        assert_eq!(device_id_from_mac("aa:bb:cc:dd:ee:ff"), "dcentaxe_27F6AB");
        // Lowercase + no-separator input yields the same id (case/format robust).
        assert_eq!(device_id_from_mac("32304127f6ab"), "dcentaxe_27F6AB");
        // Short MAC: uses whatever hex is available, never panics on the slice.
        assert_eq!(device_id_from_mac("ab"), "dcentaxe_AB");
        // Empty / hex-less MAC: deterministic fallback, no panic.
        assert_eq!(device_id_from_mac(""), "dcentaxe_unknown");
        assert_eq!(device_id_from_mac("::::"), "dcentaxe_unknown");
        // Deterministic: same input -> same id.
        assert_eq!(
            device_id_from_mac("DE:AD:BE:EF:00:11"),
            device_id_from_mac("deadbeef0011")
        );
    }

    // ── discovery config topics are well-formed ──────────────────────────────
    #[test]
    fn discovery_topics_well_formed() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        assert!(!entities.is_empty(), "must emit at least one entity");
        for e in &entities {
            let parts: Vec<&str> = e.config_topic.split('/').collect();
            assert_eq!(
                parts.len(),
                5,
                "topic must be 5 segments: {}",
                e.config_topic
            );
            assert_eq!(parts[0], "homeassistant");
            assert!(
                parts[1] == "sensor" || parts[1] == "binary_sensor",
                "component must be sensor|binary_sensor, got {}",
                parts[1]
            );
            assert_eq!(parts[2], d.device_id, "node id must be the device id");
            assert!(!parts[3].is_empty(), "object id must be non-empty");
            assert_eq!(parts[4], "config", "topic must end in /config");
        }
    }

    // ── unique_ids unique across entities + device-namespaced ────────────────
    #[test]
    fn unique_ids_unique_and_namespaced() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);

        let mut uids: Vec<String> = entities
            .iter()
            .map(|e| {
                parse(&e.payload)["unique_id"]
                    .as_str()
                    .expect("every entity must carry a unique_id")
                    .to_string()
            })
            .collect();
        let total = uids.len();
        uids.sort();
        uids.dedup();
        assert_eq!(
            uids.len(),
            total,
            "unique_ids must be unique across entities"
        );

        for e in &entities {
            let uid = parse(&e.payload)["unique_id"].as_str().unwrap().to_string();
            assert!(
                uid.starts_with(&format!("{}_", d.device_id)),
                "unique_id {uid} must be namespaced by the device id"
            );
            // config topic is likewise namespaced by the device id.
            assert!(e.config_topic.contains(&d.device_id));
        }
    }

    // ── unique_ids are STABLE across rebuilds (no nondeterminism) ────────────
    #[test]
    fn discovery_is_stable_across_builds() {
        let d = sample_device();
        assert_eq!(
            build_ha_discovery_entities(&d),
            build_ha_discovery_entities(&d),
            "discovery output must be deterministic"
        );
    }

    // ── device block correctness (manufacturer/identifiers/sw_version/model) ──
    #[test]
    fn device_block_correct() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        for e in &entities {
            let p = parse(&e.payload);
            let dev = &p["device"];
            assert_eq!(dev["manufacturer"], MANUFACTURER);
            assert_eq!(dev["manufacturer"], "D-Central");
            assert_eq!(dev["model"], "Gamma / BM1370");
            assert_eq!(dev["sw_version"], "0.3.0");
            assert!(
                !dev["sw_version"].as_str().unwrap().is_empty(),
                "sw_version must be present"
            );
            let ids = dev["identifiers"].as_array().expect("identifiers array");
            assert!(
                ids.iter().any(|v| v == &json!(d.device_id)),
                "identifiers must contain the device id"
            );
            assert_eq!(dev["configuration_url"], "http://dcentaxe.local");
        }
    }

    // ── state JSON keys match EVERY value_template's value_json.<key> ─────────
    #[test]
    fn state_keys_match_value_templates() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        let state = parse(&build_state_payload(&HaState {
            hashrate_ghs: 1234.5,
            chip_temp_c: 61.2,
            power_w: 18.0,
            fan_rpm: 4200,
            accepted: 7,
            rejected: 1,
            uptime_s: 3600,
            energy_kwh: 12.5,
            board_version_recognized: true,
            support_status: "supported".to_string(),
        }));
        let obj = state.as_object().expect("state payload is a JSON object");
        assert_eq!(state["board_version_recognized"], json!(true));
        assert_eq!(state["support_status"], json!("supported"));

        let mut checked = 0;
        for e in &entities {
            let p = parse(&e.payload);
            let tmpl = p["value_template"].as_str().expect("value_template");
            for key in value_json_keys(tmpl) {
                assert!(
                    obj.contains_key(&key),
                    "value_template of {} references value_json.{key}, missing from state payload",
                    e.config_topic
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 8,
            "expected to check >=8 template->state key refs"
        );
    }

    // ── covers the required "space heater" metric set ────────────────────────
    #[test]
    fn covers_required_metrics() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        let object_ids: Vec<String> = entities
            .iter()
            .map(|e| e.config_topic.split('/').nth(3).unwrap().to_string())
            .collect();
        for required in [
            "hashrate",
            "temperature",
            "power",
            "fan_rpm",
            "accepted",
            "rejected",
            "uptime",
        ] {
            assert!(
                object_ids.iter().any(|id| id == required),
                "discovery must cover the `{required}` metric"
            );
        }
        // device_class / unit sanity on the headline metrics.
        let entity = |dom: &str, id: &str| {
            entities
                .iter()
                .find(|e| {
                    e.config_topic == format!("homeassistant/{dom}/{}/{id}/config", d.device_id)
                })
                .map(|e| parse(&e.payload))
                .unwrap_or_else(|| panic!("missing {dom}/{id}"))
        };
        assert_eq!(
            entity("sensor", "temperature")["device_class"],
            "temperature"
        );
        assert_eq!(
            entity("sensor", "temperature")["unit_of_measurement"],
            "\u{00B0}C"
        );
        assert_eq!(entity("sensor", "power")["device_class"], "power");
        assert_eq!(entity("sensor", "hashrate")["unit_of_measurement"], "GH/s");
        assert_eq!(entity("binary_sensor", "mining")["device_class"], "running");
    }

    // ── no panic + finite output on empty / NaN / Inf fields ─────────────────
    #[test]
    fn no_panic_on_empty_or_nan_fields() {
        // NaN/Inf telemetry must serialize as finite numbers, never crash.
        let state = parse(&build_state_payload(&HaState {
            hashrate_ghs: f32::NAN,
            chip_temp_c: f32::INFINITY,
            power_w: f32::NEG_INFINITY,
            fan_rpm: 0,
            accepted: 0,
            rejected: 0,
            uptime_s: 0,
            energy_kwh: f64::NAN,
            board_version_recognized: false,
            support_status: String::new(),
        }));
        for key in ["hashrate_ghs", "temp_c", "power_w", "energy_kwh"] {
            let v = state[key].as_f64().expect("numeric");
            assert!(v.is_finite(), "{key} must be finite, got {v}");
            assert_eq!(v, 0.0, "non-finite {key} must coerce to 0.0");
        }
        assert_eq!(state["mining"], json!(false), "NaN hashrate => not mining");
        assert_eq!(state["board_version_recognized"], json!(false));
        assert_eq!(
            state["support_status"],
            json!("unknown"),
            "empty support status must publish as an honest unknown"
        );

        // Default state + a device built from an empty MAC must not panic.
        let d = HaDevice {
            device_id: device_id_from_mac(""),
            name: String::new(),
            model: String::new(),
            sw_version: String::new(),
            configuration_url: None,
        };
        let entities = build_ha_discovery_entities(&d);
        assert!(!entities.is_empty());
        // configuration_url omitted when None / empty.
        let any = parse(&entities[0].payload);
        assert!(any["device"].get("configuration_url").is_none());
        // build_state_payload on the all-zero default never panics.
        let _ = build_state_payload(&HaState::default());
    }

    // ── discovery payloads NEVER embed broker/pool credentials ───────────────
    // Mirrors the Antminer-side security pin: the discovery surface is built from
    // constants + Jinja2 templates only, so it must never leak a credential token
    // onto the retained config topics that every MQTT subscriber can read.
    #[test]
    fn discovery_payloads_never_embed_credentials() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        let forbidden = ["password", "user:", "@broker", "hunter2"];
        for e in &entities {
            let whole = format!("{}\n{}", e.config_topic, e.payload);
            for needle in &forbidden {
                assert!(
                    !whole.contains(needle),
                    "discovery surface for {} leaked forbidden token {needle:?}",
                    e.config_topic
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Publish-PLAN + in-process mock-broker e2e
    //
    // These pin the publish SEQUENCE the esp-idf transport drives — what gets
    // published, in what order, with which retain/QoS, plus the LWT — WITHOUT a
    // real broker or network. This is the host-side proof that the transport's
    // sequencing matches the discovery/state contract; it does NOT make MQTT
    // live-broker-proven on hardware (that stays operator/broker-gated).
    // ═════════════════════════════════════════════════════════════════════════

    use std::collections::HashMap;

    fn busy_snapshot() -> HaState {
        HaState {
            hashrate_ghs: 1234.5,
            chip_temp_c: 61.2,
            power_w: 18.0,
            fan_rpm: 4200,
            accepted: 7,
            rejected: 1,
            uptime_s: 3600,
            energy_kwh: 1.5,
            board_version_recognized: true,
            support_status: "supported".to_string(),
        }
    }

    /// In-process MOCK BROKER: records every published op + the registered LWT,
    /// and models retained storage (topic -> last retained payload). No network.
    #[derive(Default)]
    struct MockBroker {
        published: Vec<MqttPublishOp>,
        lwt: Option<LwtSpec>,
        retained: HashMap<String, String>,
        subscribed: Vec<String>,
    }

    impl MockBroker {
        /// The transport registers the LWT at client construction — model that.
        fn connect_with_lwt(&mut self, lwt: LwtSpec) {
            self.lwt = Some(lwt);
        }

        /// The transport subscribes to a command topic (replayed each connect).
        fn subscribe(&mut self, topic: &str) {
            self.subscribed.push(topic.to_string());
        }

        fn is_subscribed(&self, topic: &str) -> bool {
            self.subscribed.iter().any(|t| t == topic)
        }

        /// A single publish: stash retained payloads + append to the wire log.
        fn publish(&mut self, op: &MqttPublishOp) {
            if op.retain {
                self.retained.insert(op.topic.clone(), op.payload.clone());
            }
            self.published.push(op.clone());
        }

        /// Drive an ordered plan into the broker, in order.
        fn drive(&mut self, ops: &[MqttPublishOp]) {
            for op in ops {
                self.publish(op);
            }
        }

        fn published_on(&self, topic: &str) -> Vec<&MqttPublishOp> {
            self.published.iter().filter(|o| o.topic == topic).collect()
        }

        fn discovery_ops(&self) -> Vec<&MqttPublishOp> {
            self.published
                .iter()
                .filter(|o| o.topic.starts_with("homeassistant/"))
                .collect()
        }
    }

    /// The HA component of a discovery config topic
    /// (`homeassistant/<component>/...`).
    fn component_of(op: &MqttPublishOp) -> &str {
        op.topic.split('/').nth(1).unwrap_or("")
    }

    /// A READ-ONLY telemetry entity (sensor / binary_sensor) — never a command
    /// surface.
    fn is_readonly_component(op: &MqttPublishOp) -> bool {
        matches!(component_of(op), "sensor" | "binary_sensor")
    }

    /// Assert a READ-ONLY discovery payload is a valid HA discovery JSON with the
    /// fields HA needs to auto-create the entity, and that it advertises NO
    /// writable/command surface. Returns the parsed value for further use.
    fn assert_valid_discovery(op: &MqttPublishOp, state_t: &str) -> Value {
        assert!(op.retain, "discovery config {} must be retained", op.topic);
        assert_eq!(
            op.qos,
            MqttQos::AtLeastOnce,
            "discovery config {} must be QoS1",
            op.topic
        );
        let p = parse(&op.payload);
        assert!(
            p["unique_id"].as_str().is_some_and(|s| !s.is_empty()),
            "discovery {} needs a unique_id",
            op.topic
        );
        assert!(
            p["device"]["identifiers"].is_array(),
            "discovery {} needs a device block with identifiers",
            op.topic
        );
        assert_eq!(
            p["state_topic"],
            json!(state_t),
            "discovery {} state_topic must be the shared state topic",
            op.topic
        );
        assert!(
            p["value_template"].as_str().is_some_and(|s| !s.is_empty()),
            "discovery {} needs a value_template",
            op.topic
        );
        // A read-only sensor advertises unit/device_class; binary_sensor advertises
        // a device_class. Either way it must carry at least one of them so HA can
        // type the entity (never a bare value).
        assert!(
            p.get("device_class").is_some()
                || p.get("unit_of_measurement").is_some()
                || p.get("state_class").is_some(),
            "discovery {} must carry device_class/unit/state_class",
            op.topic
        );
        // Read-only invariant: a sensor/binary_sensor must NEVER advertise a
        // writable/command surface (that stays exclusive to the number/select/
        // climate command entities, which are validated separately).
        for forbidden in ["command_topic", "command_template", "payload_press"] {
            assert!(
                p.get(forbidden).is_none(),
                "read-only discovery {} must NOT advertise control field {forbidden}",
                op.topic
            );
        }
        p
    }

    /// Assert a COMMAND discovery payload (number / select / climate) is a valid,
    /// retained HA command entity that pins the CLAMP CONTRACT: it advertises the
    /// SAME safety envelope [`parse_command`] enforces, subscribes on a
    /// `command_topic` (or `temperature_command_topic` for climate), and never
    /// advertises a bare unclamped writable range. Returns the parsed value.
    fn assert_valid_command_entity(op: &MqttPublishOp) -> Value {
        assert!(op.retain, "command config {} must be retained", op.topic);
        assert_eq!(op.qos, MqttQos::AtLeastOnce, "command config must be QoS1");
        let p = parse(&op.payload);
        assert!(
            p["unique_id"].as_str().is_some_and(|s| !s.is_empty()),
            "command {} needs a unique_id",
            op.topic
        );
        assert!(
            p["device"]["identifiers"].is_array(),
            "command {} needs a device block",
            op.topic
        );
        match component_of(op) {
            "number" => {
                assert!(
                    p["command_topic"]
                        .as_str()
                        .is_some_and(|s| s.ends_with("/set")),
                    "number command {} must subscribe on a /set command_topic",
                    op.topic
                );
                assert!(
                    p.get("state_topic").is_some(),
                    "number command {} must echo on a state_topic",
                    op.topic
                );
                // The clamp contract: min/max are advertised AND bound the value.
                let min = p["min"].as_f64().expect("number needs min");
                let max = p["max"].as_f64().expect("number needs max");
                assert!(min < max, "number {} min<max", op.topic);
                assert_eq!(
                    min, CMD_TARGET_WATTS_MIN as f64,
                    "target-watts min must equal the clamp floor"
                );
                assert_eq!(
                    max, CMD_TARGET_WATTS_MAX as f64,
                    "target-watts max must equal the clamp ceiling"
                );
            }
            "select" => {
                assert!(
                    p["command_topic"]
                        .as_str()
                        .is_some_and(|s| s.ends_with("/set")),
                    "select command {} must subscribe on a /set command_topic",
                    op.topic
                );
                let opts = p["options"].as_array().expect("select needs options");
                let opts: Vec<&str> = opts.iter().filter_map(|v| v.as_str()).collect();
                // Options are EXACTLY the canonical modes parse_command accepts.
                assert_eq!(
                    opts,
                    AUTOTUNE_MODES.to_vec(),
                    "select options must be the canonical autotuner modes"
                );
            }
            "climate" => {
                assert!(
                    p["temperature_command_topic"]
                        .as_str()
                        .is_some_and(|s| s.ends_with("/set")),
                    "climate command {} must subscribe on a temperature_command_topic",
                    op.topic
                );
                let min = p["min_temp"].as_f64().expect("climate needs min_temp");
                let max = p["max_temp"].as_f64().expect("climate needs max_temp");
                assert_eq!(
                    min, CMD_TARGET_TEMP_MIN_C as f64,
                    "target-temp min == clamp floor"
                );
                assert_eq!(
                    max, CMD_TARGET_TEMP_MAX_C as f64,
                    "target-temp max == clamp ceiling"
                );
            }
            other => panic!("unexpected command component {other} for {}", op.topic),
        }
        p
    }

    /// FULL on-connect-through-first-state sequence, end-to-end against the mock
    /// broker: discovery (retained) -> availability online (+ matching offline
    /// LWT) -> state, with the high-value state-key↔value_template cross-check,
    /// then a periodic tick proving discovery is not re-published.
    #[test]
    fn e2e_publish_sequence_against_mock_broker() {
        let d = sample_device();
        let snap = busy_snapshot();
        let state_t = state_topic(&d.device_id);
        let avail_t = availability_topic(&d.device_id);

        let mut broker = MockBroker::default();
        // (b) The transport sets the LWT at client construction.
        broker.connect_with_lwt(lwt_spec(&d.device_id));
        // On connect: drive the on-connect plan (discovery -> online -> state).
        // commands_enabled=false: this test pins the READ-ONLY publish sequence.
        broker.drive(&build_publish_plan(
            &d,
            &snap,
            PublishPhase::OnConnect,
            false,
        ));

        // (a) every advertised entity has a retained, valid HA discovery config.
        let discovery = broker.discovery_ops();
        assert!(!discovery.is_empty(), "must publish discovery configs");
        for op in &discovery {
            assert_valid_discovery(op, &state_t);
        }
        // ALL the entities the feature advertises are present (sensors + binary).
        let published_objects: Vec<String> = discovery
            .iter()
            .map(|o| o.topic.split('/').nth(3).unwrap().to_string())
            .collect();
        for required in [
            "hashrate",
            "temperature",
            "power",
            "fan_rpm",
            "accepted",
            "rejected",
            "uptime",
            "mining",
        ] {
            assert!(
                published_objects.iter().any(|o| o == required),
                "on-connect plan must advertise the `{required}` entity"
            );
        }

        // (b) availability online published retained on the availability topic,
        //     and the LWT is the matching retained `offline` on the SAME topic.
        let online = broker.published_on(&avail_t);
        assert!(
            online
                .iter()
                .any(|o| o.payload == AVAIL_ONLINE && o.retain && o.qos == MqttQos::AtLeastOnce),
            "on-connect must publish a retained `online` availability message"
        );
        assert_eq!(
            broker.retained.get(&avail_t).map(String::as_str),
            Some("online")
        );
        let lwt = broker.lwt.as_ref().expect("transport must register an LWT");
        assert_eq!(lwt.topic, avail_t, "LWT must target the availability topic");
        assert_eq!(lwt.payload, AVAIL_OFFLINE, "LWT payload must be `offline`");
        assert!(lwt.retain, "LWT must be retained");
        assert_eq!(lwt.qos, MqttQos::AtLeastOnce);

        // (c) the first state payload is published (once) on the state_topic the
        //     value_templates read, non-retained, QoS0 — and every entity's
        //     value_template references a key the state payload actually emits.
        let state_pubs = broker.published_on(&state_t);
        assert_eq!(
            state_pubs.len(),
            1,
            "on-connect plan publishes the first state exactly once"
        );
        let state_op = state_pubs[0];
        assert!(!state_op.retain, "state payload must be non-retained");
        assert_eq!(state_op.qos, MqttQos::AtMostOnce, "state payload is QoS0");
        let state_json = parse(&state_op.payload);
        let state_obj = state_json
            .as_object()
            .expect("state payload is a JSON object");

        let mut crosschecked = 0;
        for op in &discovery {
            let p = parse(&op.payload);
            let tmpl = p["value_template"].as_str().unwrap();
            for key in value_json_keys(tmpl) {
                assert!(
                    state_obj.contains_key(&key),
                    "entity {} value_template reads value_json.{key}, missing from the published state payload",
                    op.topic
                );
                crosschecked += 1;
            }
        }
        assert!(
            crosschecked >= 8,
            "expected >=8 template->state key cross-checks, got {crosschecked}"
        );

        // (d) a second periodic tick publishes ONLY the state (+ availability
        //     refresh) — never the retained discovery configs again.
        let before = broker.published.len();
        broker.drive(&build_publish_plan(
            &d,
            &snap,
            PublishPhase::Periodic,
            false,
        ));
        let tick = &broker.published[before..];
        assert!(
            tick.iter().all(|o| !o.topic.starts_with("homeassistant/")),
            "periodic tick must NOT re-publish retained discovery configs"
        );
        assert_eq!(
            tick.iter().filter(|o| o.topic == state_t).count(),
            1,
            "periodic tick publishes the state exactly once"
        );
        assert!(
            tick.iter()
                .any(|o| o.topic == avail_t && o.payload == AVAIL_ONLINE && o.retain),
            "periodic tick refreshes the retained availability heartbeat"
        );
    }

    // ── negative path: NaN / empty fields never panic, plan stays valid ───────
    #[test]
    fn e2e_plan_is_panic_safe_on_nan_and_empty_fields() {
        // Empty MAC -> fallback device id; NaN/Inf telemetry. Must not panic and
        // must still yield a fully valid, drivable plan.
        let d = HaDevice {
            device_id: device_id_from_mac(""),
            name: String::new(),
            model: String::new(),
            sw_version: String::new(),
            configuration_url: None,
        };
        let snap = HaState {
            hashrate_ghs: f32::NAN,
            chip_temp_c: f32::INFINITY,
            power_w: f32::NEG_INFINITY,
            fan_rpm: 0,
            accepted: 0,
            rejected: 0,
            uptime_s: 0,
            energy_kwh: f64::NEG_INFINITY,
            board_version_recognized: false,
            support_status: String::new(),
        };
        let state_t = state_topic(&d.device_id);

        let mut broker = MockBroker::default();
        broker.connect_with_lwt(lwt_spec(&d.device_id));
        // Exercise BOTH read-only and command-enabled plans for panic-safety.
        broker.drive(&build_publish_plan(
            &d,
            &snap,
            PublishPhase::OnConnect,
            true,
        ));
        broker.drive(&build_publish_plan(&d, &snap, PublishPhase::Periodic, true));

        // Every JSON-bearing payload (discovery configs + state) still parses.
        for op in &broker.published {
            if op.topic.starts_with("homeassistant/") || op.topic == state_t {
                let _ = serde_json::from_str::<Value>(&op.payload)
                    .expect("plan must only emit valid JSON payloads");
            }
        }
        // The state payload coerced the non-finite fields to a finite 0.0.
        let state_op = broker
            .published
            .iter()
            .find(|o| o.topic == state_t)
            .expect("state must be published");
        let st = parse(&state_op.payload);
        for key in ["hashrate_ghs", "temp_c", "power_w"] {
            let v = st[key].as_f64().expect("numeric");
            assert!(v.is_finite(), "{key} must be finite");
            assert_eq!(v, 0.0, "non-finite {key} must coerce to 0.0");
        }
        // Default snapshot + default-ish device must also be panic-free.
        let _ = build_publish_plan(
            &sample_device(),
            &HaState::default(),
            PublishPhase::OnConnect,
            true,
        );
        let _ = build_publish_plan(
            &sample_device(),
            &HaState::default(),
            PublishPhase::Periodic,
            true,
        );
    }

    // ── plan ORDER + retain/QoS per op are exactly the prior wire shape ───────
    #[test]
    fn on_connect_plan_order_and_flags() {
        let d = sample_device();
        let ops = build_publish_plan(&d, &busy_snapshot(), PublishPhase::OnConnect, false);

        // discovery configs first (all retained, QoS1), in build order.
        let n_disc = ops
            .iter()
            .take_while(|o| o.topic.starts_with("homeassistant/"))
            .count();
        assert!(n_disc >= 8, "expected >=8 discovery ops, got {n_disc}");
        for op in &ops[..n_disc] {
            assert!(op.retain && op.qos == MqttQos::AtLeastOnce);
        }
        // then availability `online` (retained, QoS1) ...
        let avail = &ops[n_disc];
        assert_eq!(avail.topic, availability_topic(&d.device_id));
        assert_eq!(avail.payload, AVAIL_ONLINE);
        assert!(avail.retain && avail.qos == MqttQos::AtLeastOnce);
        // ... then the first state (non-retained, QoS0) LAST.
        let last = ops.last().unwrap();
        assert_eq!(last.topic, state_topic(&d.device_id));
        assert!(!last.retain && last.qos == MqttQos::AtMostOnce);
        assert_eq!(
            ops.len(),
            n_disc + 2,
            "on-connect = discovery + online + state"
        );

        // The discovery ops in the plan are byte-identical to the entity builder
        // (extraction did not alter the wire payloads).
        let entities = build_ha_discovery_entities(&d);
        assert_eq!(entities.len(), n_disc);
        for (op, e) in ops[..n_disc].iter().zip(entities.iter()) {
            assert_eq!(op.topic, e.config_topic);
            assert_eq!(op.payload, e.payload);
        }
    }

    #[test]
    fn periodic_plan_is_state_then_availability_refresh() {
        let d = sample_device();
        let ops = build_publish_plan(&d, &busy_snapshot(), PublishPhase::Periodic, false);
        assert_eq!(ops.len(), 2, "periodic = state + availability refresh");
        // state first (non-retained, QoS0) ...
        assert_eq!(ops[0].topic, state_topic(&d.device_id));
        assert!(!ops[0].retain && ops[0].qos == MqttQos::AtMostOnce);
        assert_eq!(ops[0].payload, build_state_payload(&busy_snapshot()));
        // ... then availability online refresh (retained, QoS1). No discovery.
        assert_eq!(ops[1].topic, availability_topic(&d.device_id));
        assert_eq!(ops[1].payload, AVAIL_ONLINE);
        assert!(ops[1].retain && ops[1].qos == MqttQos::AtLeastOnce);
        assert!(
            !ops.iter().any(|o| o.topic.starts_with("homeassistant/")),
            "periodic plan must never carry discovery configs"
        );
    }

    #[test]
    fn lwt_mirrors_availability_online_topic() {
        let d = sample_device();
        let lwt = lwt_spec(&d.device_id);
        let online = build_publish_plan(&d, &busy_snapshot(), PublishPhase::OnConnect, false)
            .into_iter()
            .find(|o| o.payload == AVAIL_ONLINE)
            .expect("on-connect publishes availability online");
        assert_eq!(
            lwt.topic, online.topic,
            "LWT offline + availability online must share one topic"
        );
        assert_eq!(lwt.payload, AVAIL_OFFLINE);
        assert!(lwt.retain && lwt.qos == MqttQos::AtLeastOnce);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Energy accumulator (HA Energy-dashboard sensor source)
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn energy_accumulator_integrates_watt_seconds_to_kwh() {
        let mut acc = EnergyAccumulator::new();
        assert_eq!(acc.energy_kwh(), 0.0);
        // 3600 W for 1000 s == 3600 * 1000 Ws == 1.0 kWh.
        acc.add_sample(3600.0, 1000.0);
        assert!((acc.energy_kwh() - 1.0).abs() < 1e-9, "1 kWh expected");
        // Another 1800 W for 2000 s == 3.6e6 Ws == 1.0 kWh more -> 2.0 kWh total.
        let total = acc.add_sample(1800.0, 2000.0);
        assert!((total - 2.0).abs() < 1e-9);
        assert!((acc.energy_kwh() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn energy_accumulator_is_monotonic_and_fail_benign() {
        let mut acc = EnergyAccumulator::new();
        acc.add_sample(100.0, 30.0);
        let after_good = acc.energy_kwh();
        assert!(after_good > 0.0);
        // Garbage / negative / zero-elapsed samples are IGNORED — never decrease.
        acc.add_sample(f32::NAN, 30.0);
        acc.add_sample(f32::INFINITY, 30.0);
        acc.add_sample(-500.0, 30.0);
        acc.add_sample(100.0, 0.0);
        acc.add_sample(100.0, -30.0);
        acc.add_sample(100.0, f64::NAN);
        assert_eq!(
            acc.energy_kwh(),
            after_good,
            "non-finite/negative/zero-elapsed samples must not change the total"
        );
        // A valid sample still advances it (monotonic non-decreasing).
        acc.add_sample(50.0, 60.0);
        assert!(acc.energy_kwh() > after_good);
    }

    #[test]
    fn energy_sensor_is_advertised_with_energy_dashboard_contract() {
        let d = sample_device();
        let entities = build_ha_discovery_entities(&d);
        let energy = entities
            .iter()
            .find(|e| {
                e.config_topic == format!("homeassistant/sensor/{}/energy/config", d.device_id)
            })
            .map(|e| parse(&e.payload))
            .expect("an energy sensor must be advertised");
        // HA Energy dashboard REQUIRES device_class=energy + state_class=
        // total_increasing + a kWh-family unit.
        assert_eq!(energy["device_class"], "energy");
        assert_eq!(energy["state_class"], "total_increasing");
        assert_eq!(energy["unit_of_measurement"], "kWh");
        // Its value_template reads a key the state payload emits.
        let tmpl = energy["value_template"].as_str().unwrap();
        assert!(tmpl.contains("value_json.energy_kwh"));
        let state = parse(&build_state_payload(&busy_snapshot()));
        assert!(state.as_object().unwrap().contains_key("energy_kwh"));
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Command surface: parse + CLAMP + echo (Antminer MqttCommandSink port)
    // ═════════════════════════════════════════════════════════════════════════

    #[test]
    fn autotune_mode_parse_mirrors_from_api_str() {
        // The 4 canonical modes round-trip (case/dash-robust like from_api_str).
        assert_eq!(
            parse_autotune_mode("max_hashrate"),
            Some(AUTOTUNE_MODE_MAX_HASHRATE)
        );
        assert_eq!(
            parse_autotune_mode("BEST-EFFICIENCY"),
            Some(AUTOTUNE_MODE_BEST_EFFICIENCY)
        );
        assert_eq!(
            parse_autotune_mode("  Target_Watts "),
            Some(AUTOTUNE_MODE_TARGET_WATTS)
        );
        assert_eq!(
            parse_autotune_mode("target-temp"),
            Some(AUTOTUNE_MODE_TARGET_TEMP)
        );
        // Unknown modes FAIL CLOSED (None -> HA's last-known state untouched).
        assert_eq!(parse_autotune_mode("turbo"), None);
        assert_eq!(parse_autotune_mode(""), None);
        assert_eq!(parse_autotune_mode("max hashrate"), None);
    }

    #[test]
    fn target_watts_command_is_clamped_to_envelope_not_applied_raw() {
        let t = CommandTopics::new(&device_id_from_mac("aa:bb:cc:dd:ee:ff"));
        // Absurd power request clamps DOWN to the max envelope.
        assert_eq!(
            parse_command(&t, &t.target_watts_set, b"999999"),
            Some(HaCommand::TargetWatts(CMD_TARGET_WATTS_MAX)),
            "an over-budget watts command must clamp to the ceiling, not apply raw"
        );
        // Below-floor / negative clamps UP to the min.
        assert_eq!(
            parse_command(&t, &t.target_watts_set, b"0"),
            Some(HaCommand::TargetWatts(CMD_TARGET_WATTS_MIN))
        );
        assert_eq!(
            parse_command(&t, &t.target_watts_set, b"-40"),
            Some(HaCommand::TargetWatts(CMD_TARGET_WATTS_MIN))
        );
        // In-range passes through, and echoes as an integer on the state topic.
        let cmd = parse_command(&t, &t.target_watts_set, b"75").unwrap();
        assert_eq!(cmd, HaCommand::TargetWatts(75.0));
        assert_eq!(
            command_state_echo(&t, &cmd),
            (t.target_watts_state.clone(), "75".to_string())
        );
        // The clamped-echo of an over-budget request is the ceiling.
        let clamped = parse_command(&t, &t.target_watts_set, b"999999").unwrap();
        assert_eq!(
            command_state_echo(&t, &clamped),
            (t.target_watts_state.clone(), "200".to_string())
        );
    }

    #[test]
    fn target_temp_command_is_clamped_to_thermal_envelope() {
        let t = CommandTopics::new(&device_id_from_mac("deadbeef0011"));
        // A dangerous temp request clamps DOWN to the max chip-temp envelope.
        assert_eq!(
            parse_command(&t, &t.target_temp_set, b"500"),
            Some(HaCommand::TargetTempC(CMD_TARGET_TEMP_MAX_C))
        );
        // Too-cold clamps UP to the min (no stranded/empty eligible-point set).
        assert_eq!(
            parse_command(&t, &t.target_temp_set, b"10"),
            Some(HaCommand::TargetTempC(CMD_TARGET_TEMP_MIN_C))
        );
        let cmd = parse_command(&t, &t.target_temp_set, b"65").unwrap();
        assert_eq!(cmd, HaCommand::TargetTempC(65.0));
        assert_eq!(
            command_state_echo(&t, &cmd),
            (t.target_temp_state.clone(), "65".to_string())
        );
    }

    #[test]
    fn command_parse_is_fail_closed_on_garbage_and_foreign_topics() {
        let t = CommandTopics::new(&device_id_from_mac("aa:bb:cc:dd:ee:ff"));
        // Unparseable / non-finite / empty payloads yield None (state untouched).
        assert_eq!(parse_command(&t, &t.target_watts_set, b"loud-please"), None);
        assert_eq!(parse_command(&t, &t.target_watts_set, b""), None);
        assert_eq!(parse_command(&t, &t.target_watts_set, b"   "), None);
        assert_eq!(parse_command(&t, &t.target_temp_set, b"NaN"), None);
        assert_eq!(parse_command(&t, &t.target_temp_set, b"inf"), None);
        // A foreign / non-command topic is never parsed as a command.
        assert_eq!(parse_command(&t, &t.target_watts_state, b"50"), None);
        assert_eq!(parse_command(&t, "dcentaxe/other/state", b"50"), None);
        // An unknown autotuner mode is rejected (fail-closed).
        assert_eq!(parse_command(&t, &t.autotune_mode_set, b"turbo"), None);
        // A valid mode selection parses.
        assert_eq!(
            parse_command(&t, &t.autotune_mode_set, b"best_efficiency"),
            Some(HaCommand::AutotuneMode(AUTOTUNE_MODE_BEST_EFFICIENCY))
        );
    }

    #[test]
    fn command_entities_pin_the_clamp_contract() {
        let d = sample_device();
        let entities = build_command_entities(&d);
        // Exactly the three operator-control entities.
        let ids: Vec<String> = entities.iter().map(|e| e.config_topic.clone()).collect();
        assert!(ids.iter().any(|t| t
            == &format!(
                "homeassistant/number/{}/target_watts_set/config",
                d.device_id
            )));
        assert!(ids.iter().any(|t| t
            == &format!(
                "homeassistant/select/{}/autotune_mode_set/config",
                d.device_id
            )));
        assert!(ids
            .iter()
            .any(|t| t == &format!("homeassistant/climate/{}/heater/config", d.device_id)));
        // Each pins the advertised clamp envelope.
        for op in entities.iter().map(|e| MqttPublishOp {
            topic: e.config_topic.clone(),
            payload: e.payload.clone(),
            retain: true,
            qos: MqttQos::AtLeastOnce,
        }) {
            assert_valid_command_entity(&op);
        }
        // Command entities NEVER leak a credential (same pin as the read-only set).
        for e in &entities {
            for needle in ["password", "user:", "@broker", "hunter2"] {
                assert!(!e.payload.contains(needle));
            }
        }
    }

    #[test]
    fn command_plan_only_advertises_commands_when_enabled() {
        let d = sample_device();
        let snap = busy_snapshot();
        // Default-OFF: no number/select/climate configs on the wire.
        let ro = build_publish_plan(&d, &snap, PublishPhase::OnConnect, false);
        assert!(
            !ro.iter().any(|o| {
                o.topic.starts_with("homeassistant/number/")
                    || o.topic.starts_with("homeassistant/select/")
                    || o.topic.starts_with("homeassistant/climate/")
            }),
            "commands_enabled=false must advertise NO command entities"
        );
        // Opt-in: the three command entities appear (retained, QoS1), and the
        // read-only sensors are still present too.
        let on = build_publish_plan(&d, &snap, PublishPhase::OnConnect, true);
        for needle in [
            format!(
                "homeassistant/number/{}/target_watts_set/config",
                d.device_id
            ),
            format!(
                "homeassistant/select/{}/autotune_mode_set/config",
                d.device_id
            ),
            format!("homeassistant/climate/{}/heater/config", d.device_id),
        ] {
            assert!(
                on.iter().any(|o| o.topic == needle),
                "commands_enabled=true must advertise {needle}"
            );
        }
        assert!(
            on.iter()
                .any(|o| o.topic == format!("homeassistant/sensor/{}/energy/config", d.device_id)),
            "the read-only energy sensor is present regardless of commands_enabled"
        );
        // A periodic tick NEVER re-publishes command configs.
        let tick = build_publish_plan(&d, &snap, PublishPhase::Periodic, true);
        assert!(!tick.iter().any(|o| o.topic.starts_with("homeassistant/")));
    }

    /// FULL command round-trip against the mock broker: subscribe the /set topics
    /// on connect, deliver a raw operator command, prove it is CLAMPED, and prove
    /// the APPLIED (clamped) value is echoed retained on the /state topic HA reads.
    #[test]
    fn e2e_command_subscribe_and_clamped_echo_against_mock_broker() {
        let d = sample_device();
        let did = &d.device_id;
        let topics = CommandTopics::new(did);
        let mut broker = MockBroker::default();
        broker.connect_with_lwt(lwt_spec(did));

        // (1) On connect the transport subscribes to all 3 command /set topics.
        for topic in command_subscribe_topics(did) {
            broker.subscribe(&topic);
        }
        assert!(broker.is_subscribed(&topics.target_watts_set));
        assert!(broker.is_subscribed(&topics.autotune_mode_set));
        assert!(broker.is_subscribed(&topics.target_temp_set));
        // It never subscribes to a read-only telemetry topic.
        assert!(!broker.is_subscribed(&state_topic(did)));

        // (2) The on-connect plan advertises the command entities (opt-in).
        broker.drive(&build_publish_plan(
            &d,
            &busy_snapshot(),
            PublishPhase::OnConnect,
            true,
        ));
        let cmd_ops: Vec<&MqttPublishOp> = broker
            .discovery_ops()
            .into_iter()
            .filter(|o| {
                o.topic.starts_with("homeassistant/number/")
                    || o.topic.starts_with("homeassistant/select/")
                    || o.topic.starts_with("homeassistant/climate/")
            })
            .collect();
        assert_eq!(cmd_ops.len(), 3, "3 command entities advertised");
        for op in &cmd_ops {
            assert_valid_command_entity(op);
        }

        // (3) HA publishes a DANGEROUS raw setpoint on the /set topic. The
        //     transport parses + clamps it (never applies raw) and echoes the
        //     APPLIED value retained on the /state topic.
        let inbound = |broker: &mut MockBroker, topic: &str, payload: &[u8]| {
            if let Some(cmd) = parse_command(&topics, topic, payload) {
                let (state_t, applied) = command_state_echo(&topics, &cmd);
                broker.publish(&MqttPublishOp {
                    topic: state_t,
                    payload: applied,
                    retain: true,
                    qos: MqttQos::AtLeastOnce,
                });
            }
        };

        inbound(&mut broker, &topics.target_watts_set, b"999999");
        inbound(&mut broker, &topics.target_temp_set, b"500");
        inbound(&mut broker, &topics.autotune_mode_set, b"target_watts");
        // Garbage never touches the retained state (fail-closed).
        inbound(&mut broker, &topics.target_temp_set, b"loud-please");

        assert_eq!(
            broker
                .retained
                .get(&topics.target_watts_state)
                .map(String::as_str),
            Some("200"),
            "999999 W must be echoed as the clamped 200 W ceiling"
        );
        assert_eq!(
            broker
                .retained
                .get(&topics.target_temp_state)
                .map(String::as_str),
            Some("95"),
            "500 C must be echoed as the clamped 95 C ceiling"
        );
        assert_eq!(
            broker
                .retained
                .get(&topics.autotune_mode_state)
                .map(String::as_str),
            Some("target_watts")
        );
        // The garbage temp command left the (clamped 95) retained state untouched.
        assert_eq!(
            broker
                .retained
                .get(&topics.target_temp_state)
                .map(String::as_str),
            Some("95")
        );
    }
}
