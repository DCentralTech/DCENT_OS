//! MQTT publisher with Home Assistant auto-discovery.
//!
//! Publishes miner stats to an MQTT broker for integration with
//! Home Assistant, Node-RED, Grafana, or any MQTT consumer.
//!
//! On connect, sends HA MQTT discovery configs so the miner appears
//! automatically as sensor entities (hashrate, temp, power, fan) and
//! a climate entity (space heater mode) in Home Assistant.
//!
//! Home Assistant MQTT auto-discovery is available when the MQTT publisher is
//! enabled.
//!
//! Usage:
//!   [mqtt]
//!   enabled = true
//!   broker = "mqtt://203.0.113.100:1883"
//!   topic_prefix = "dcentrald"
//!   discovery = true

use dcentrald_stratum::pool_api::sanitize_pool_url;
use rumqttc::{AsyncClient, Event, LastWill, MqttOptions, Packet, QoS};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

/// Availability payload published (retained) on connect and refreshed each tick.
pub const AVAILABILITY_ONLINE: &str = "online";
/// Availability payload published on graceful shutdown AND registered as the
/// broker-side LastWill, so a crash/power-loss flips the retained availability
/// to `offline` instead of leaving every HA entity showing a dead miner online.
pub const AVAILABILITY_OFFLINE: &str = "offline";

/// The shared availability topic every discovery entity gates on.
fn availability_topic(prefix: &str) -> String {
    format!("{}/availability", prefix)
}

/// The shared state topic every sensor value_template reads from.
fn state_topic(prefix: &str) -> String {
    format!("{}/state", prefix)
}

/// MqttConfig re-export (defined in dcentrald/src/config.rs).
/// We accept it as a parameter, not import the crate.
#[derive(Debug, Clone)]
pub struct MqttPublisherConfig {
    pub broker: String,
    pub topic_prefix: String,
    pub discovery: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub publish_interval_s: u16,
    /// HA discovery device-block identity. Threaded from the daemon (or
    /// best-effort detected via [`MqttDeviceIdentity::detect`]) so multi-unit
    /// fleets don't collide on identical device names/models/URLs.
    pub device: MqttDeviceIdentity,
}

/// Per-unit identity for the HA discovery device block. Every field is
/// optional: absent fields fall back to the generic values, so transient
/// bring-ups and connection tests need no identity plumbing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MqttDeviceIdentity {
    /// Platform/model string (e.g. the `/etc/dcentos/platform` stamp
    /// `zynq-bm3-am2`, or a human model like "Antminer S19j Pro").
    pub model: Option<String>,
    /// Management hostname (LAN/mDNS resolvable) for `configuration_url`.
    pub hostname: Option<String>,
    /// Live management IP — preferred over the hostname for
    /// `configuration_url` when known.
    pub management_ip: Option<String>,
}

impl MqttDeviceIdentity {
    /// Best-effort on-device detection for call sites that don't thread
    /// identity explicitly: the canonical platform stamp + the kernel
    /// hostname. Absent files (dev hosts, tests) leave the fields `None` so
    /// the device block falls back to the generic values.
    pub fn detect() -> Self {
        fn read_trimmed(path: &str) -> Option<String> {
            let contents = std::fs::read_to_string(path).ok()?;
            let trimmed = contents.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Self {
            model: read_trimmed("/etc/dcentos/platform"),
            hostname: read_trimmed("/proc/sys/kernel/hostname"),
            management_ip: None,
        }
    }

    /// The HA `configuration_url`: prefer the live management IP, then the
    /// hostname, then the generic mDNS fallback (which resolves to at most one
    /// unit on a multi-miner LAN — hence the preference order; mirrors the
    /// ESP-side `build_device` precedence).
    pub fn configuration_url(&self) -> String {
        let non_empty = |s: &&str| !s.trim().is_empty();
        if let Some(ip) = self.management_ip.as_deref().filter(non_empty) {
            return format!("http://{}", ip.trim());
        }
        if let Some(host) = self.hostname.as_deref().filter(non_empty) {
            return format!("http://{}", host.trim());
        }
        "http://dcentos.local".to_string()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// P2-7 (Omega): MQTT command subscriber — operator setpoints from Home
// Assistant, clamped through the SAME validated setters the local REST API
// uses. The publisher half (above/below) is publish-only; this half lets HA
// WRITE a few safe setpoints (fan PWM / target watts / target chip temp). EVERY
// commanded value is clamped by the sink before it touches hardware — a remote
// command can NEVER raise fans above the home PWM-30 cap or bypass a safety
// gate. The whole surface is default-OFF: a sink is only wired when `[mqtt]` is
// enabled AND the daemon supplies one.
// ───────────────────────────────────────────────────────────────────────────

/// Advertised minimum fan PWM for the HA `number` entity. The sink's real clamp
/// is the per-mode envelope in `rest::compute_commanded_fan_pwm`; this is the
/// discovery floor only.
pub const CMD_FAN_PWM_MIN: u8 = 0;
/// Advertised maximum fan PWM for the HA `number` entity — the home safety cap
/// (`== dcentrald_hal::fan::PWM_SAFETY_MAX`, pinned equal by a test in `rest`).
/// The sink re-clamps every command regardless, so a raw publish above this is
/// still clamped, never applied.
pub const CMD_FAN_PWM_MAX: u8 = 30;
/// Min target power (W) advertised to HA AND enforced by the sink before the
/// value is dispatched to the live autotuner `PowerTarget`.
pub const CMD_TARGET_WATTS_MIN: u32 = 100;
/// Max target power (W) advertised to HA AND enforced by the sink (the autotuner
/// runtime applies its own downstream voltage/PVT clamps).
pub const CMD_TARGET_WATTS_MAX: u32 = 6000;
/// Min target CHIP temperature (°C) advertised to HA AND enforced by the sink.
pub const CMD_TARGET_TEMP_MIN_C: u8 = 40;
/// Max target CHIP temperature (°C) — capped at the thermal RAMP threshold (60),
/// strictly BELOW hot (65) / dangerous (70) / critical (75), so a remote HA
/// setpoint can never park the PID target at or above the danger line (P2-7
/// review fix).
pub const CMD_TARGET_TEMP_MAX_C: u8 = 60;

/// The validated-setter sink the MQTT command subscriber routes operator
/// setpoints through. Implemented by the daemon (over `AppState`) so commands
/// reach the SAME clamped paths the REST API uses — the subscriber NEVER opens
/// a new unclamped path. Each method returns the actually-APPLIED (clamped)
/// value on success, or a human-readable rejection string.
///
/// SAFETY CONTRACT: implementations MUST clamp every value to the existing caps
/// before it touches hardware (fan PWM ≤ home cap, watts/temp to the envelope).
///
/// CE-242: proof that a space-heater watt-cap write was actually accepted by the
/// live autotuner. This is a WHITELIST — any unknown/absent status fails closed.
/// `"applied"` = live apply this cycle; `"deferred"` = the tuner already wrote
/// the new cap into its live config (applies next cycle). Every other outcome
/// (`rejected`/`unavailable`/`closed`/`closed_before_ack`/`ack_timeout`/malformed)
/// is NOT proof, so the MQTT/HA sink must not echo the new watts back to HA.
pub(crate) fn target_watts_cap_write_confirmed(runtime: &serde_json::Value) -> bool {
    let accepted = runtime
        .get("accepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let status = runtime.get("status").and_then(|v| v.as_str()).unwrap_or("");
    accepted && matches!(status, "applied" | "deferred")
}

/// CE-242: rejection string for an unconfirmed watt-cap write (config was
/// persisted, so it applies on next restart; the live write was not proven).
pub(crate) fn unconfirmed_target_watts_error(runtime: &serde_json::Value) -> String {
    let status = runtime
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = runtime
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("no acknowledgement detail");
    format!(
        "target watts persisted to config but the live watt-cap write is unconfirmed (status={status}): {message}"
    )
}

#[async_trait::async_trait]
pub trait MqttCommandSink: Send + Sync {
    /// Apply a fan PWM setpoint. MUST clamp to the live operating-mode envelope
    /// (load-bearing home PWM-30 hard cap). Returns the applied PWM.
    async fn set_fan_pwm(&self, requested_pwm: u32) -> Result<u8, String>;
    /// Apply a target-power setpoint (W). MUST clamp to the configured envelope.
    /// Returns the applied watts.
    async fn set_target_watts(&self, requested_watts: u32) -> Result<u32, String>;
    /// Apply a target chip-temperature setpoint (°C). MUST clamp to the thermal
    /// envelope. Returns the applied temperature.
    async fn set_target_temp_c(&self, requested_temp_c: f64) -> Result<u8, String>;
}

/// A parsed, range-unvalidated HA command (the sink does the clamping).
#[derive(Debug, Clone, PartialEq)]
enum MqttCommand {
    FanPwm(u32),
    TargetWatts(u32),
    TargetTempC(f64),
}

/// The per-entity command/state topic names derived from the configured prefix.
#[derive(Debug, Clone)]
struct CommandTopics {
    fan_pwm_set: String,
    fan_pwm_state: String,
    target_watts_set: String,
    target_watts_state: String,
    target_temp_set: String,
    target_temp_state: String,
}

impl CommandTopics {
    fn new(prefix: &str) -> Self {
        Self {
            fan_pwm_set: format!("{}/fan_pwm/set", prefix),
            fan_pwm_state: format!("{}/fan_pwm/state", prefix),
            target_watts_set: format!("{}/target_watts/set", prefix),
            target_watts_state: format!("{}/target_watts/state", prefix),
            target_temp_set: format!("{}/target_temp/set", prefix),
            target_temp_state: format!("{}/target_temp/state", prefix),
        }
    }
}

/// Parse an inbound MQTT publish into a command, or `None` if the topic is not a
/// command topic or the payload is not a finite number. Pure + HAL-free so the
/// dispatch path is unit-testable without a broker.
fn parse_command(topics: &CommandTopics, topic: &str, payload: &[u8]) -> Option<MqttCommand> {
    let value: f64 = std::str::from_utf8(payload).ok()?.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    if topic == topics.fan_pwm_set {
        Some(MqttCommand::FanPwm(
            value.round().clamp(0.0, u32::MAX as f64) as u32,
        ))
    } else if topic == topics.target_watts_set {
        Some(MqttCommand::TargetWatts(
            value.round().clamp(0.0, u32::MAX as f64) as u32,
        ))
    } else if topic == topics.target_temp_set {
        Some(MqttCommand::TargetTempC(value))
    } else {
        None
    }
}

/// Apply a parsed command through the sink and, on success, return the entity
/// state topic + the APPLIED (clamped) value string to echo back to HA. Returns
/// `None` when the sink rejects the command (logged), so HA's last-known
/// (truthful) state is left untouched.
async fn apply_command(
    sink: &dyn MqttCommandSink,
    topics: &CommandTopics,
    command: MqttCommand,
) -> Option<(String, String)> {
    match command {
        MqttCommand::FanPwm(requested) => match sink.set_fan_pwm(requested).await {
            Ok(applied) => {
                tracing::info!(
                    requested,
                    applied,
                    "MQTT/HA fan PWM command applied (clamped to safety envelope)"
                );
                Some((topics.fan_pwm_state.clone(), applied.to_string()))
            }
            Err(error) => {
                tracing::warn!(requested, %error, "MQTT/HA fan PWM command rejected");
                None
            }
        },
        MqttCommand::TargetWatts(requested) => match sink.set_target_watts(requested).await {
            Ok(applied) => {
                tracing::info!(
                    requested,
                    applied,
                    "MQTT/HA target-power command applied (clamped)"
                );
                Some((topics.target_watts_state.clone(), applied.to_string()))
            }
            Err(error) => {
                tracing::warn!(requested, %error, "MQTT/HA target-power command rejected");
                None
            }
        },
        MqttCommand::TargetTempC(requested) => match sink.set_target_temp_c(requested).await {
            Ok(applied) => {
                tracing::info!(
                    requested,
                    applied,
                    "MQTT/HA target-temp command applied (clamped)"
                );
                Some((topics.target_temp_state.clone(), applied.to_string()))
            }
            Err(error) => {
                tracing::warn!(requested, %error, "MQTT/HA target-temp command rejected");
                None
            }
        },
    }
}

/// One ordered MQTT operation in the per-connection bring-up plan — pure data
/// so [`build_on_connect_plan`] is unit-testable without a broker (the port of
/// the ESP `build_publish_plan` pattern).
#[derive(Debug, Clone, PartialEq)]
pub enum MqttConnectOp {
    /// Subscribe to a command topic. With `clean_session=true` the broker
    /// forgets subscriptions on every disconnect, so these MUST be replayed on
    /// every ConnAck or the HA command entities die silently after a broker
    /// blip while state keeps flowing.
    Subscribe { topic: String, qos: QoS },
    /// Publish a message (discovery config / availability / state).
    Publish {
        topic: String,
        payload: String,
        retain: bool,
        qos: QoS,
    },
}

/// Build the ORDERED per-connection bring-up plan, replayed on EVERY ConnAck
/// (initial connect AND every rumqttc auto-reconnect):
///
/// 1. command-topic subscribes (QoS 1) — first, so no inbound command window
///    is lost while the retained publishes below are in flight;
/// 2. retained HA discovery configs (QoS 1) — a restarted broker may have lost
///    them;
/// 3. retained availability `online` (QoS 1) — after discovery so a freshly
///    discovered entity never sits on a stale `offline`;
/// 4. the latest known state (QoS 0, not retained) — so entities show a value
///    immediately after a reconnect instead of "unknown" until the next tick.
pub fn build_on_connect_plan(
    prefix: &str,
    mac_id: &str,
    discovery: bool,
    commands_enabled: bool,
    device: &MqttDeviceIdentity,
    last_state: Option<&str>,
) -> Vec<MqttConnectOp> {
    let mut ops = Vec::new();

    if commands_enabled {
        let topics = CommandTopics::new(prefix);
        for topic in [
            topics.fan_pwm_set,
            topics.target_watts_set,
            topics.target_temp_set,
        ] {
            ops.push(MqttConnectOp::Subscribe {
                topic,
                qos: QoS::AtLeastOnce,
            });
        }
    }

    if discovery {
        for entity in build_ha_discovery_entities(prefix, mac_id, commands_enabled, device) {
            ops.push(MqttConnectOp::Publish {
                topic: entity.config_topic,
                payload: entity.payload.to_string(),
                retain: true,
                qos: QoS::AtLeastOnce,
            });
        }
    }

    ops.push(MqttConnectOp::Publish {
        topic: availability_topic(prefix),
        payload: AVAILABILITY_ONLINE.to_string(),
        retain: true,
        qos: QoS::AtLeastOnce,
    });

    if let Some(state) = last_state {
        ops.push(MqttConnectOp::Publish {
            topic: state_topic(prefix),
            payload: state.to_string(),
            retain: false,
            qos: QoS::AtMostOnce,
        });
    }

    ops
}

/// Build the publisher connection options: keep-alive, clean session, and the
/// retained `offline` LastWill on the SAME availability topic the retained
/// `online` is published to — the broker flips availability to `offline` on an
/// uncleanly dropped connection (crash/power-loss), so HA never shows a dead
/// miner as online forever. Extracted so the LWT contract is unit-testable
/// (mirrors the ESP `lwt_spec` pin).
pub fn build_publisher_mqtt_options(
    client_id: &str,
    host: &str,
    port: u16,
    topic_prefix: &str,
) -> MqttOptions {
    let mut mqttoptions = MqttOptions::new(client_id, host, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    // clean_session=true is safe ONLY because the on-connect plan replays the
    // command subscribes on every ConnAck (see `build_on_connect_plan`).
    mqttoptions.set_clean_session(true);
    mqttoptions.set_last_will(LastWill::new(
        availability_topic(topic_prefix),
        AVAILABILITY_OFFLINE,
        QoS::AtLeastOnce,
        true,
    ));
    mqttoptions
}

/// Run the MQTT publisher until shutdown.
///
/// Subscribes to the stats broadcast channel (same data as WebSocket)
/// and publishes to the MQTT broker at the configured interval.
pub async fn run_publisher(
    config: MqttPublisherConfig,
    mut stats_rx: broadcast::Receiver<String>,
    mac: String,
    shutdown: CancellationToken,
    command_sink: Option<Arc<dyn MqttCommandSink>>,
) -> anyhow::Result<()> {
    // P2-7 (Omega): per-entity HA command topics for the optional command
    // subscriber. The subscriber only activates when `command_sink` is `Some`
    // (the daemon supplies the validated-setter sink); every commanded value is
    // clamped by that sink, never applied raw.
    let command_topics = CommandTopics::new(&config.topic_prefix);

    // Parse broker URL
    let (host, port) = parse_broker_url(&config.broker)?;

    // MQTT-4: the broker URL can carry inline `user:pass@host` credentials. Never
    // emit it verbatim to any log line or error message — mask it through the same
    // credential-stripping helper the pool surfaces use, so a `mqtts://user:pass@`
    // URL is logged/reported as `mqtts://host:port` only.
    let broker_display = sanitize_pool_url(&config.broker);

    // MQTT-TLS-DOWNGRADE-1: an mqtts:// broker asks for TLS, but this build links
    // rumqttc WITHOUT a TLS transport. Silently using plaintext TCP would send the
    // credentials below in the clear — fail closed with a clear message instead.
    if broker_url_requires_tls(&config.broker) {
        anyhow::bail!(
            "MQTT broker '{}' requests TLS (mqtts://) but this build has no MQTT TLS \
             transport — refusing to send credentials over plaintext. Use mqtt:// on a \
             trusted LAN, or build with MQTT TLS support.",
            broker_display
        );
    }

    // Build MQTT client options (incl. the retained `offline` LastWill on the
    // availability topic — see `build_publisher_mqtt_options`).
    let mac_short = mac.replace(':', "");
    let mac_last6 = &mac_short[mac_short.len().saturating_sub(6)..];
    let client_id = format!("dcentrald_{}", mac_last6);

    let mut mqttoptions =
        build_publisher_mqtt_options(&client_id, &host, port, &config.topic_prefix);

    // Auth
    if let (Some(user), Some(pass)) = (&config.username, &config.password) {
        mqttoptions.set_credentials(user, pass);
    }

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 64);

    tracing::info!(
        broker = %broker_display,
        prefix = %config.topic_prefix,
        discovery = config.discovery,
        interval_s = config.publish_interval_s,
        client_id = %client_id,
        "MQTT publisher connecting"
    );

    // Latest stats payload, shared between the main publish loop (writer) and
    // the event-loop task (reader — the on-connect plan replays it after a
    // reconnect so HA entities show a value immediately, not "unknown").
    let (last_state_tx, last_state_rx) = watch::channel::<Option<String>>(None);

    // Whether a validated-setter command sink is wired (default-OFF: `None`
    // for transient/proxy bring-ups, so no command surface is opened).
    let commands_enabled = command_sink.is_some();

    // Spawn the event loop processor (handles MQTT protocol, reconnects). It
    // owns the per-connection bring-up: EVERY ConnAck (initial connect AND
    // every rumqttc auto-reconnect) replays the on-connect plan — command
    // subscribes + retained discovery + availability + latest state. Without
    // the replay, `clean_session=true` means a broker restart/network blip
    // silently kills the fan/watts/climate command entities while state keeps
    // flowing. When a command sink is present it ALSO dispatches inbound HA
    // command publishes (fan PWM / target watts / target temp) through the
    // sink's clamped setters and echoes the APPLIED (clamped) value back to
    // the entity's state topic, so Home Assistant reflects what was actually
    // applied (e.g. a fan PWM 100 request shows 30 on a home unit) — never the
    // raw request.
    let el_shutdown = shutdown.clone();
    let el_sink = command_sink.clone();
    let el_topics = command_topics.clone();
    let el_client = client.clone();
    let el_prefix = config.topic_prefix.clone();
    let el_mac_id = mac_last6.to_string();
    let el_discovery = config.discovery;
    let el_device = config.device.clone();
    tokio::spawn(async move {
        loop {
            if el_shutdown.is_cancelled() {
                break;
            }
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    let last_state = last_state_rx.borrow().clone();
                    let plan = build_on_connect_plan(
                        &el_prefix,
                        &el_mac_id,
                        el_discovery,
                        commands_enabled,
                        &el_device,
                        last_state.as_deref(),
                    );
                    let op_count = plan.len();
                    for op in plan {
                        let result = match &op {
                            MqttConnectOp::Subscribe { topic, qos } => {
                                el_client.subscribe(topic.as_str(), *qos).await
                            }
                            MqttConnectOp::Publish {
                                topic,
                                payload,
                                retain,
                                qos,
                            } => {
                                el_client
                                    .publish(
                                        topic.as_str(),
                                        *qos,
                                        *retain,
                                        payload.clone().into_bytes(),
                                    )
                                    .await
                            }
                        };
                        if let Err(e) = result {
                            tracing::warn!(error = %e, op = ?op, "MQTT on-connect plan op failed");
                        }
                    }
                    tracing::info!(
                        ops = op_count,
                        discovery = el_discovery,
                        commands_enabled,
                        "MQTT connected — (re)subscribed command topics and (re)published HA discovery/availability/state"
                    );
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let Some(ref sink) = el_sink else { continue };
                    // MQTT-RETAINED-CMD-1: ignore RETAINED command messages. A
                    // retained value on a */set topic is redelivered on every
                    // (re)subscribe, so acting on it would re-apply a stale
                    // (possibly broker-injected) setpoint on each reconnect and
                    // fight live local control. Only live operator publishes drive
                    // the clamped setters.
                    if publish.retain {
                        tracing::debug!(topic = %publish.topic, "ignoring retained MQTT command message");
                        continue;
                    }
                    let Some(command) = parse_command(&el_topics, &publish.topic, &publish.payload)
                    else {
                        continue;
                    };
                    if let Some((state_topic, applied)) =
                        apply_command(sink.as_ref(), &el_topics, command).await
                    {
                        // Retained so HA shows the applied setpoint after restart.
                        let _ = el_client
                            .publish(&state_topic, QoS::AtLeastOnce, true, applied.into_bytes())
                            .await;
                    }
                }
                Ok(_notification) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "MQTT eventloop error — reconnecting in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Subscribes + HA discovery + availability now run per-ConnAck in the
    // event-loop task above (the on-connect plan), so a broker reconnect
    // restores the command entities instead of silently killing them.
    if commands_enabled {
        tracing::info!(
            "MQTT command subscriber active — Home Assistant can set fan PWM / target watts / \
             target temp; every setpoint is clamped to the safety envelope (home fan PWM ≤ 30)"
        );
    }
    if config.discovery {
        tracing::info!("Home Assistant MQTT auto-discovery enabled — miner will appear in HA automatically on connect");
    }

    // Main publish loop
    let interval = Duration::from_secs(config.publish_interval_s as u64);
    let state_topic = state_topic(&config.topic_prefix);
    let mut publish_timer = tokio::time::interval(interval);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("MQTT publisher shutting down");
                // Publish offline availability (the LastWill covers the
                // non-graceful paths).
                let avail_topic = availability_topic(&config.topic_prefix);
                let _ = client.publish(&avail_topic, QoS::AtLeastOnce, true, AVAILABILITY_OFFLINE).await;
                break;
            }

            // Receive new stats from the broadcast channel (same as WebSocket)
            result = stats_rx.recv() => {
                match result {
                    Ok(stats_json) => {
                        let _ = last_state_tx.send(Some(stats_json));
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(lagged = n, "MQTT stats receiver lagged — using latest");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("MQTT stats channel closed — publisher stopping");
                        break;
                    }
                }
            }

            // Publish at configured interval
            _ = publish_timer.tick() => {
                let stats = last_state_tx.borrow().clone();
                if let Some(stats) = stats {
                    match client.publish(&state_topic, QoS::AtMostOnce, false, stats.into_bytes()).await {
                        Ok(_) => {
                            tracing::trace!("MQTT published to {}", state_topic);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, topic = %state_topic, "MQTT publish failed");
                        }
                    }

                    // Also publish availability
                    let avail_topic = availability_topic(&config.topic_prefix);
                    let _ = client.publish(&avail_topic, QoS::AtLeastOnce, true, AVAILABILITY_ONLINE).await;
                }
            }
        }
    }

    Ok(())
}

/// Attempt a one-shot MQTT connection test.
///
/// Returns the generated client ID after a successful CONNACK.
pub async fn test_connection(config: &MqttPublisherConfig) -> anyhow::Result<String> {
    let (host, port) = parse_broker_url(&config.broker)?;
    // MQTT-4: mask any inline `user:pass@` credentials before they reach an error
    // message (same masking as run_publisher).
    let broker_display = sanitize_pool_url(&config.broker);
    // MQTT-TLS-DOWNGRADE-1: same fail-closed posture as run_publisher — never
    // probe an mqtts:// broker over plaintext with credentials attached.
    if broker_url_requires_tls(&config.broker) {
        anyhow::bail!(
            "MQTT broker '{}' requests TLS (mqtts://) but this build has no MQTT TLS \
             transport — refusing a plaintext connection test with credentials.",
            broker_display
        );
    }
    let client_id = format!("dcentrald_test_{}", unique_suffix());

    let mut mqttoptions = MqttOptions::new(&client_id, &host, port);
    mqttoptions.set_keep_alive(Duration::from_secs(10));
    mqttoptions.set_clean_session(true);

    if let (Some(user), Some(pass)) = (&config.username, &config.password) {
        mqttoptions.set_credentials(user, pass);
    }

    let (_client, mut eventloop) = AsyncClient::new(mqttoptions, 8);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    while tokio::time::Instant::now() < deadline {
        let timeout = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(Duration::from_millis(500));

        match tokio::time::timeout(timeout, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => return Ok(client_id),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(anyhow::anyhow!("{}", error)),
            Err(_) => {}
        }
    }

    Err(anyhow::anyhow!(
        "timed out waiting for MQTT broker connection acknowledgement"
    ))
}

/// One Home Assistant MQTT discovery message: the retained `config` topic plus
/// the JSON config payload HA consumes to auto-create the entity. Built by the
/// PURE `build_ha_discovery_entities` helper so the schema (topic shape, payload
/// keys, advertised ranges) is unit-testable without a broker.
#[derive(Debug, Clone, PartialEq)]
pub struct HaDiscoveryEntity {
    /// `homeassistant/{domain}/{device_id}/{entity_id}/config`.
    pub config_topic: String,
    /// The serialized JSON discovery config payload.
    pub payload: serde_json::Value,
}

/// Build the full set of Home Assistant MQTT discovery entities for this device,
/// PURE + broker-free so the schema is unit-testable. The on-connect plan
/// (`build_on_connect_plan`) serializes + publishes each one (retained) to its
/// `config_topic` on every ConnAck.
///
/// Each entity config lives at
/// `homeassistant/{domain}/dcentrald_{mac}/{entity_id}/config`. Sensor /
/// binary_sensor entities are always emitted; the operator-writable command
/// entities (number fan PWM, number target watts, climate heater) are emitted
/// ONLY when `commands_enabled` (a validated-setter sink is wired). The
/// advertised command ranges mirror the daemon-side safety envelope (fan PWM
/// hard-capped at the home PWM-30 ceiling); the sink re-clamps every value
/// regardless, so a raw out-of-range publish can never drive hardware past the cap.
/// HA `json_attributes_template` for the power-derived sensors (power / BTU /
/// efficiency). On current production platforms `live_power_available` can be
/// true while the underlying wattage is MODELED, so the numeric value alone
/// can't tell an operator whether they're looking at a wall-meter reading or an
/// estimate. This renders the power-provenance fields from the shared state
/// payload as HA entity attributes so the provenance is visible right next to
/// the number. Every `value_json.*` path here must resolve against
/// `WsStatsMessage` (pinned by the schema-resolution test).
const POWER_PROVENANCE_ATTR_TEMPLATE: &str = "{{ {'power_source': value_json.power_source, 'power_source_detail': value_json.power_source_detail, 'power_modeled': value_json.power_modeled, 'power_calibrated': value_json.power_calibrated, 'power_note': value_json.power_note} | tojson }}";

pub fn build_ha_discovery_entities(
    prefix: &str,
    mac_id: &str,
    commands_enabled: bool,
    identity: &MqttDeviceIdentity,
) -> Vec<HaDiscoveryEntity> {
    let device_id = format!("dcentrald_{}", mac_id);
    let state_topic = format!("{}/state", prefix);
    let avail_topic = format!("{}/availability", prefix);

    // Shared device block — all entities belong to the same device. The
    // device NAME is MAC-suffixed and the model/configuration_url come from
    // the per-unit identity so multi-unit fleets don't collide on identical
    // device blocks. `identifiers` and every entity `unique_id` keep the
    // pre-existing `dcentrald_{mac}` scheme — changing those would orphan the
    // HA entities of every unit already running the beta.
    let device = json!({
        "identifiers": [&device_id],
        "name": format!("DCENT_OS Miner {}", mac_id),
        "manufacturer": "D-Central Technologies",
        "model": identity.model.as_deref().unwrap_or("Antminer"),
        "sw_version": env!("CARGO_PKG_VERSION"),
        "configuration_url": identity.configuration_url()
    });

    let mut entities: Vec<HaDiscoveryEntity> = Vec::new();

    // Sensor entities — value_templates match the WsStatsMessage JSON structure exactly:
    //   hashrate_ghs (GH/s), hashrate_5s_ghs, accepted, rejected, uptime_s,
    //   chains[].temp_c, fans.rpm, fans.pwm, pool.status,
    //   power_watts, wall_watts, efficiency_jth, btu_h, live_power_available.
    // Live-power-derived HA sensors fail closed to `none` until the daemon marks
    // live wall power available, so fallback/model startup values do not appear
    // as current power, heat, or efficiency readings.
    let sensors = vec![
        (
            "hashrate",
            "Hashrate",
            "{{ (value_json.hashrate_ghs / 1000) | round(2) }}",
            "TH/s",
            None::<&str>,
            "mdi:pickaxe",
        ),
        (
            "temperature",
            "Temperature",
            // MQTT-2: guard the empty-chains case. Indexing `chains[0]` when the
            // array is empty (or the key is absent) misbehaves in HA's Jinja2; the
            // `if value_json.chains` predicate skips the index entirely and falls
            // back to 0 rather than evaluating an out-of-range subscript.
            "{{ (value_json.chains[0].temp_c | default(0)) if value_json.chains else 0 }}",
            "\u{00B0}C",
            Some("temperature"),
            "mdi:thermometer",
        ),
        (
            "power",
            "Power",
            "{{ (value_json.wall_watts | round(0)) if value_json.live_power_available else none }}",
            "W",
            Some("power"),
            "mdi:flash",
        ),
        (
            "btu",
            "BTU/h",
            "{{ (value_json.btu_h | round(0)) if value_json.live_power_available else none }}",
            "BTU/h",
            None,
            "mdi:fire",
        ),
        (
            "fan_rpm",
            "Fan RPM",
            "{{ value_json.fans.rpm }}",
            "RPM",
            None,
            "mdi:fan",
        ),
        (
            "efficiency",
            "Efficiency",
            "{{ (value_json.efficiency_jth | round(1)) if value_json.live_power_available else none }}",
            "J/TH",
            None,
            "mdi:lightning-bolt",
        ),
        (
            "accepted",
            "Accepted Shares",
            "{{ value_json.accepted }}",
            "",
            None,
            "mdi:check-circle",
        ),
        (
            "rejected",
            "Rejected Shares",
            "{{ value_json.rejected }}",
            "",
            None,
            "mdi:close-circle",
        ),
        (
            "uptime",
            "Uptime",
            "{{ value_json.uptime_s | default(0) }}",
            "s",
            Some("duration"),
            "mdi:clock-outline",
        ),
    ];

    for (id, name, value_key, unit, device_class, icon) in &sensors {
        let unique_id = format!("{}_{}", device_id, id);
        let config_topic = format!("homeassistant/sensor/{}/{}/config", device_id, id);

        let mut payload = json!({
            "name": name,
            "unique_id": &unique_id,
            "state_topic": &state_topic,
            "availability_topic": &avail_topic,
            "value_template": value_key, // Already a full Jinja2 template string
            "device": &device,
            "icon": icon,
        });

        if !unit.is_empty() {
            payload["unit_of_measurement"] = json!(unit);
        }
        if let Some(dc) = device_class {
            payload["device_class"] = json!(dc);
            payload["state_class"] = json!("measurement");
        }

        // POWER-PROVENANCE: the power/BTU/efficiency sensors are derived from a
        // wattage that can be MODELED even when `live_power_available` is true.
        // Attach the provenance fields as HA attributes so a modeled reading is
        // never displayed as if it were a wall-meter measurement.
        if matches!(*id, "power" | "btu" | "efficiency") {
            payload["json_attributes_topic"] = json!(&state_topic);
            payload["json_attributes_template"] = json!(POWER_PROVENANCE_ATTR_TEMPLATE);
        }

        entities.push(HaDiscoveryEntity {
            config_topic,
            payload,
        });
    }

    // HA ENERGY: `device_class=energy` + `state_class=total_increasing` kWh
    // sensor — the entity the HA Energy dashboard requires (space-heater ROI:
    // kWh/day → $/day). Mirrors the ESP `mqtt_ha` energy sensor. Fed by the
    // daemon-side `websocket::EnergyAccumulator`, which integrates the SAME
    // gated wall watts the power sensor displays (0 W while live power is
    // unavailable, so the meter holds instead of creeping). NOT declared in
    // the measurement-sensor loop above because its state_class differs
    // (`total_increasing`, not `measurement`). Deliberately NOT gated to
    // `none` on live power: the total stays true while power is unavailable —
    // it simply stops increasing. Resets to 0 on daemon restart, which HA's
    // `total_increasing` contract treats as a meter reset (history kept).
    // Carries the same provenance attributes as the power sensor because the
    // integrated wattage can be modeled.
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/sensor/{}/energy/config", device_id),
        payload: json!({
            "name": "Energy",
            "unique_id": format!("{}_energy", device_id),
            "state_topic": &state_topic,
            "availability_topic": &avail_topic,
            "value_template": "{{ (value_json.energy_kwh | default(0)) | round(4) }}",
            "unit_of_measurement": "kWh",
            "device_class": "energy",
            "state_class": "total_increasing",
            "icon": "mdi:lightning-bolt-circle",
            "json_attributes_topic": &state_topic,
            "json_attributes_template": POWER_PROVENANCE_ATTR_TEMPLATE,
            "device": &device,
        }),
    });

    // Diagnostic provenance sensor: a plain text sensor whose value IS the
    // power-provenance note from the shared state payload. Categorized
    // `diagnostic` (filed under the device's diagnostics, not the main sensor
    // list) and intentionally NOT gated on live power — it is the
    // human-readable reason a power/BTU/efficiency sensor may read `unknown`.
    entities.push(HaDiscoveryEntity {
        config_topic: format!("homeassistant/sensor/{}/power_provenance/config", device_id),
        payload: json!({
            "name": "Power Provenance",
            "unique_id": format!("{}_power_provenance", device_id),
            "state_topic": &state_topic,
            "availability_topic": &avail_topic,
            "value_template": "{{ value_json.power_note }}",
            "entity_category": "diagnostic",
            "icon": "mdi:information-outline",
            "device": &device,
        }),
    });

    // Binary sensors — use actual WsStatsMessage fields
    let binary_sensors = vec![
        (
            "mining",
            "Mining Active",
            "{{ 'ON' if value_json.hashrate_ghs > 0 else 'OFF' }}",
            "running",
            "mdi:pickaxe",
        ),
        (
            "pool",
            "Pool Connected",
            // Must list the FULL canonical connected set (crate::rest::POOL_CONNECTED_STATUSES)
            // or HA falsely reports Pool Connected = OFF for Donating/Authorized/Mining
            // states while the miner is connected + hashing. Pinned by a drift-guard test.
            "{{ 'ON' if value_json.pool.status in ['Alive', 'Donating', 'Connected', 'Authorized', 'Active', 'Mining'] else 'OFF' }}",
            "connectivity",
            "mdi:lan-connect",
        ),
    ];

    for (id, name, value_key, device_class, icon) in &binary_sensors {
        let unique_id = format!("{}_{}", device_id, id);
        let config_topic = format!("homeassistant/binary_sensor/{}/{}/config", device_id, id);

        let payload = json!({
            "name": name,
            "unique_id": &unique_id,
            "state_topic": &state_topic,
            "availability_topic": &avail_topic,
            "value_template": value_key, // Full Jinja2 template
            "device_class": device_class,
            "device": &device,
            "icon": icon,
        });

        entities.push(HaDiscoveryEntity {
            config_topic,
            payload,
        });
    }

    // P2-7 (Omega): operator-writable command entities — only published when the
    // command subscriber is active (a validated-setter sink is wired). Each
    // `command_topic` is one the publisher SUBSCRIBES to; the matching
    // `state_topic` is where the daemon echoes the APPLIED (clamped) value.
    if commands_enabled {
        // number: fan PWM (0..=30 — the home safety cap; the sink re-clamps).
        entities.push(HaDiscoveryEntity {
            config_topic: format!("homeassistant/number/{}/fan_pwm_set/config", device_id),
            payload: json!({
                "name": "Fan PWM",
                "unique_id": format!("{}_fan_pwm_set", device_id),
                "command_topic": format!("{}/fan_pwm/set", prefix),
                "state_topic": format!("{}/fan_pwm/state", prefix),
                "availability_topic": &avail_topic,
                "min": CMD_FAN_PWM_MIN,
                "max": CMD_FAN_PWM_MAX,
                "step": 1,
                "mode": "slider",
                "unit_of_measurement": "%",
                "icon": "mdi:fan",
                "device": &device,
            }),
        });

        // number: target power (W) — clamped to the envelope, then dispatched to
        // the SAME live autotuner PowerTarget the REST API uses.
        entities.push(HaDiscoveryEntity {
            config_topic: format!("homeassistant/number/{}/target_watts_set/config", device_id),
            payload: json!({
                "name": "Target Power",
                "unique_id": format!("{}_target_watts_set", device_id),
                "command_topic": format!("{}/target_watts/set", prefix),
                "state_topic": format!("{}/target_watts/state", prefix),
                "availability_topic": &avail_topic,
                "min": CMD_TARGET_WATTS_MIN,
                "max": CMD_TARGET_WATTS_MAX,
                "step": 10,
                "mode": "box",
                "unit_of_measurement": "W",
                "device_class": "power",
                "icon": "mdi:flash",
                "device": &device,
            }),
        });

        // climate: space-heater target CHIP temperature. current_temperature is
        // the MEASURED chain temp from the periodic state publish (honest — not a
        // fabricated room temp); the setpoint is clamped to the thermal envelope.
        entities.push(HaDiscoveryEntity {
            config_topic: format!("homeassistant/climate/{}/heater/config", device_id),
            payload: json!({
                "name": "Space Heater",
                "unique_id": format!("{}_heater", device_id),
                "availability_topic": &avail_topic,
                "temperature_command_topic": format!("{}/target_temp/set", prefix),
                "temperature_state_topic": format!("{}/target_temp/state", prefix),
                "current_temperature_topic": &state_topic,
                // MQTT-2: same empty-chains guard as the temperature sensor — never
                // index chains[0] unless the array is non-empty.
                "current_temperature_template": "{{ (value_json.chains[0].temp_c | default(0)) if value_json.chains else 0 }}",
                "min_temp": CMD_TARGET_TEMP_MIN_C,
                "max_temp": CMD_TARGET_TEMP_MAX_C,
                "temp_step": 1,
                "temperature_unit": "C",
                "modes": ["heat"],
                "icon": "mdi:radiator",
                "device": &device,
            }),
        });
    }

    entities
}

/// Parse a broker URL like "mqtt://host:port" into (host, port).
pub fn parse_broker_url(url: &str) -> anyhow::Result<(String, u16)> {
    let url = url
        .trim_start_matches("mqtt://")
        .trim_start_matches("mqtts://")
        .trim_start_matches("tcp://");

    if let Some((host, port_str)) = url.rsplit_once(':') {
        if host.trim().is_empty() {
            return Err(anyhow::anyhow!("broker hostname is required"));
        }
        let port: u16 = port_str.parse().unwrap_or(1883);
        Ok((host.to_string(), port))
    } else {
        if url.trim().is_empty() {
            return Err(anyhow::anyhow!("broker hostname is required"));
        }
        Ok((url.to_string(), 1883))
    }
}

/// MQTT-TLS-DOWNGRADE-1: true when the broker URL scheme requests an encrypted
/// transport. `parse_broker_url` strips the scheme, so the connect sites check
/// this separately and FAIL CLOSED rather than silently downgrade an `mqtts://`
/// (or `ssl://`/`tls://`) broker to plaintext TCP — which would put the MQTT
/// username/password on the wire in the clear.
pub fn broker_url_requires_tls(url: &str) -> bool {
    let t = url.trim();
    t.starts_with("mqtts://") || t.starts_with("ssl://") || t.starts_with("tls://")
}

fn unique_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod command_subscriber_tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Mutex;

    #[test]
    fn ce242_cap_write_proof_accepts_applied_and_deferred() {
        assert!(target_watts_cap_write_confirmed(
            &json!({"accepted": true, "status": "applied"})
        ));
        assert!(target_watts_cap_write_confirmed(
            &json!({"accepted": true, "status": "deferred"})
        ));
    }

    #[test]
    fn ce242_cap_write_proof_fails_closed_otherwise() {
        // Real (accepted, status) pairs the dispatcher can emit that are NOT proof.
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": true, "status": "rejected"})
        ));
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": true, "status": "ack_timeout"})
        ));
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": false, "status": "unavailable"})
        ));
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": false, "status": "closed"})
        ));
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": false, "status": "closed_before_ack"})
        ));
        // accepted=false must fail even if status looks like success.
        assert!(!target_watts_cap_write_confirmed(
            &json!({"accepted": false, "status": "applied"})
        ));
        // Missing fields fail closed.
        assert!(!target_watts_cap_write_confirmed(&json!({})));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn mqtt_command_parser_never_panics_on_arbitrary_topic_or_payload(
            topic in ".{0,512}",
            payload in proptest::collection::vec(any::<u8>(), 0..512)
        ) {
            let topics = CommandTopics::new("dcentrald");
            let _ = parse_command(&topics, &topic, &payload);
        }
    }

    /// Test sink that routes fan PWM through the SAME production clamp the live
    /// `rest::grpc_bridge_set_fan` setter uses (`rest::compute_commanded_fan_pwm`
    /// in Home mode), so this pins the real load-bearing PWM-30 home cap — not a
    /// re-implemented clamp. Watts/temp mirror the published envelope constants.
    struct ClampProbeSink {
        applied_fan: Mutex<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl MqttCommandSink for ClampProbeSink {
        async fn set_fan_pwm(&self, requested_pwm: u32) -> Result<u8, String> {
            let pwm = crate::rest::compute_commanded_fan_pwm(
                crate::OperatingMode::Home,
                "custom",
                Some(requested_pwm.min(u8::MAX as u32) as u8),
                false,
            )?;
            self.applied_fan.lock().unwrap().push(pwm);
            Ok(pwm)
        }
        async fn set_target_watts(&self, requested_watts: u32) -> Result<u32, String> {
            Ok(requested_watts.clamp(CMD_TARGET_WATTS_MIN, CMD_TARGET_WATTS_MAX))
        }
        async fn set_target_temp_c(&self, requested_temp_c: f64) -> Result<u8, String> {
            Ok((requested_temp_c.round() as i64)
                .clamp(CMD_TARGET_TEMP_MIN_C as i64, CMD_TARGET_TEMP_MAX_C as i64)
                as u8)
        }
    }

    #[tokio::test]
    async fn ha_fan_pwm_100_is_clamped_to_home_cap_not_applied_raw() {
        let topics = CommandTopics::new("dcentrald");
        let sink = ClampProbeSink {
            applied_fan: Mutex::new(Vec::new()),
        };

        // An out-of-range HA command (fan PWM 100) arrives on the fan set topic.
        let command =
            parse_command(&topics, &topics.fan_pwm_set, b"100").expect("fan command parses");
        assert_eq!(command, MqttCommand::FanPwm(100));

        let outcome = apply_command(&sink, &topics, command).await;

        // The value the daemon echoes to HA is the CLAMPED 30 — never the raw 100.
        assert_eq!(
            outcome,
            Some((topics.fan_pwm_state.clone(), "30".to_string())),
            "fan PWM 100 must be clamped to the home PWM-30 cap, not applied raw"
        );
        // And the hardware-facing setter saw exactly 30.
        assert_eq!(*sink.applied_fan.lock().unwrap(), vec![30u8]);
    }

    #[test]
    fn parse_ignores_foreign_topics_and_non_numeric_payloads() {
        let topics = CommandTopics::new("dcentrald");
        // The publish-only state topic is NOT a command topic.
        assert!(parse_command(&topics, "dcentrald/state", b"100").is_none());
        // Garbage payloads never parse into a command.
        assert!(parse_command(&topics, &topics.fan_pwm_set, b"loud-please").is_none());
        assert!(parse_command(&topics, &topics.target_temp_set, b"").is_none());
    }

    #[tokio::test]
    async fn ha_target_watts_and_temp_clamp_to_envelope() {
        let topics = CommandTopics::new("m");
        let sink = ClampProbeSink {
            applied_fan: Mutex::new(Vec::new()),
        };

        // Absurd power request clamps DOWN to the max envelope.
        let command =
            parse_command(&topics, &topics.target_watts_set, b"999999").expect("watts parses");
        let (topic, applied) = apply_command(&sink, &topics, command)
            .await
            .expect("applied");
        assert_eq!(topic, topics.target_watts_state);
        assert_eq!(applied, CMD_TARGET_WATTS_MAX.to_string());

        // Dangerous temp request clamps DOWN to the max chip-temp envelope.
        let command = parse_command(&topics, &topics.target_temp_set, b"250").expect("temp parses");
        let (topic, applied) = apply_command(&sink, &topics, command)
            .await
            .expect("applied");
        assert_eq!(topic, topics.target_temp_state);
        assert_eq!(applied, CMD_TARGET_TEMP_MAX_C.to_string());
    }
}

#[cfg(test)]
mod ha_discovery_schema_tests {
    use super::*;

    /// Find the (single) entity whose config topic ends in `/{id}/config`.
    fn entity_by_id<'a>(
        entities: &'a [HaDiscoveryEntity],
        domain: &str,
        id: &str,
    ) -> &'a HaDiscoveryEntity {
        let suffix = format!("homeassistant/{}/dcentrald_AABBCC/{}/config", domain, id);
        entities
            .iter()
            .find(|e| e.config_topic == suffix)
            .unwrap_or_else(|| panic!("no {} entity {} (topic {})", domain, id, suffix))
    }

    #[test]
    fn core_sensor_entities_have_expected_schema_topics_and_state_topic() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            false,
            &MqttDeviceIdentity::default(),
        );

        // The 4 prompt-named core entities exist with the expected domain.
        // hashrate / temperature are sensors; pool / shares are below.
        let hashrate = entity_by_id(&entities, "sensor", "hashrate");
        let temperature = entity_by_id(&entities, "sensor", "temperature");
        let accepted = entity_by_id(&entities, "sensor", "accepted");
        let rejected = entity_by_id(&entities, "sensor", "rejected");
        let pool = entity_by_id(&entities, "binary_sensor", "pool");

        // Every sensor reads the SAME shared state topic the publisher writes to.
        for e in [hashrate, temperature, accepted, rejected, pool] {
            assert_eq!(
                e.payload["state_topic"], "dcentrald/state",
                "{} must subscribe to the shared state topic",
                e.config_topic
            );
            assert_eq!(
                e.payload["availability_topic"], "dcentrald/availability",
                "{} must carry the availability topic",
                e.config_topic
            );
        }

        // R-O1 drift guard: the Pool Connected binary_sensor template MUST cover the
        // full canonical connected set (is_pool_connected / POOL_CONNECTED_STATUSES),
        // else HA falsely shows "Pool Connected = OFF" during e.g. a Donating or
        // Authorized state while the miner is connected + hashing.
        let pool_tmpl = pool.payload["value_template"].as_str().unwrap();
        for status in crate::rest::POOL_CONNECTED_STATUSES {
            assert!(
                pool_tmpl.contains(status),
                "pool binary_sensor value_template must list canonical connected status '{}' (found: {})",
                status,
                pool_tmpl
            );
        }

        // hashrate sensor: TH/s unit + value_template reads hashrate_ghs.
        assert_eq!(hashrate.payload["unit_of_measurement"], "TH/s");
        assert!(hashrate.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("hashrate_ghs"));

        // temperature sensor: HA temperature device_class + reads chains[0].temp_c.
        assert_eq!(temperature.payload["device_class"], "temperature");
        assert_eq!(temperature.payload["state_class"], "measurement");
        assert!(temperature.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("chains[0].temp_c"));

        // shares: accepted + rejected read the matching WsStatsMessage counters.
        assert!(accepted.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("value_json.accepted"));
        assert!(rejected.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("value_json.rejected"));

        // pool connectivity binary sensor.
        assert_eq!(pool.payload["device_class"], "connectivity");
        assert!(pool.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("value_json.pool.status"));
    }

    #[test]
    fn every_entity_has_a_unique_config_topic_and_unique_id() {
        // With commands enabled we get the full superset (sensors + binary + 3 cmd).
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        let mut topics: Vec<&str> = entities.iter().map(|e| e.config_topic.as_str()).collect();
        let topic_count = topics.len();
        topics.sort_unstable();
        topics.dedup();
        assert_eq!(
            topics.len(),
            topic_count,
            "HA discovery config topics must be unique (HA keys entities by config topic)"
        );

        let mut uids: Vec<&str> = entities
            .iter()
            .map(|e| {
                e.payload["unique_id"]
                    .as_str()
                    .expect("every discovery entity must carry a unique_id")
            })
            .collect();
        let uid_count = uids.len();
        uids.sort_unstable();
        uids.dedup();
        assert_eq!(
            uids.len(),
            uid_count,
            "HA discovery unique_ids must be unique across all entities"
        );

        // unique_id + config topic are both namespaced by the device id (mac).
        for e in &entities {
            assert!(
                e.config_topic.contains("dcentrald_AABBCC"),
                "config topic {} must namespace the device id",
                e.config_topic
            );
            assert!(e.payload["unique_id"]
                .as_str()
                .unwrap()
                .starts_with("dcentrald_AABBCC_"));
        }
    }

    #[test]
    fn command_entities_only_appear_when_commands_enabled() {
        let read_only = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            false,
            &MqttDeviceIdentity::default(),
        );
        let with_commands = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        // No writable command/climate entity exists in the read-only schema.
        assert!(
            !read_only.iter().any(
                |e| e.config_topic.contains("/number/") || e.config_topic.contains("/climate/")
            ),
            "read-only discovery must NOT advertise any writable command entity"
        );
        assert!(read_only
            .iter()
            .all(|e| e.payload.get("command_topic").is_none()
                && e.payload.get("temperature_command_topic").is_none()));

        // Enabling commands adds exactly the 3 writable entities.
        assert_eq!(with_commands.len(), read_only.len() + 3);
    }

    /// SAFETY ENVELOPE PIN: the advertised fan-PWM command range must never
    /// exceed the home PWM-30 cap, and the advertised chip-temp range must stay
    /// strictly below the danger line. The sink re-clamps regardless, but the
    /// discovery schema must not even DISPLAY a higher ceiling to the operator.
    #[test]
    fn writable_command_ranges_pin_the_safety_envelope() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        let fan = entity_by_id(&entities, "number", "fan_pwm_set");
        assert_eq!(fan.payload["min"], CMD_FAN_PWM_MIN);
        assert_eq!(fan.payload["max"], CMD_FAN_PWM_MAX);
        // The advertised max IS the home cap — never higher.
        assert_eq!(
            fan.payload["max"].as_u64().unwrap(),
            30,
            "fan PWM HA slider must cap at the home PWM-30 safety ceiling"
        );

        let heater = entity_by_id(&entities, "climate", "heater");
        assert_eq!(heater.payload["min_temp"], CMD_TARGET_TEMP_MIN_C);
        assert_eq!(heater.payload["max_temp"], CMD_TARGET_TEMP_MAX_C);
        assert!(
            heater.payload["max_temp"].as_u64().unwrap() < 65,
            "climate setpoint max must stay strictly below the hot (65C) threshold"
        );

        // The heater current_temperature reads the MEASURED chain temp, never a
        // fabricated room temperature (truthfulness contract).
        assert_eq!(
            heater.payload["current_temperature_topic"],
            "dcentrald/state"
        );
        assert!(heater.payload["current_temperature_template"]
            .as_str()
            .unwrap()
            .contains("chains[0].temp_c"));
    }

    /// The command_topic the climate/number entities advertise is exactly the
    /// `*/set` topic the publisher SUBSCRIBES to, and the state_topic is the
    /// `*/state` echo topic — wired through `CommandTopics` so the discovery
    /// schema and the subscriber can never drift apart.
    #[test]
    fn writable_command_topics_match_the_subscriber_topics() {
        let entities =
            build_ha_discovery_entities("m", "AABBCC", true, &MqttDeviceIdentity::default());
        let topics = CommandTopics::new("m");

        // entity_by_id is hardcoded to AABBCC + the `homeassistant/...` shape, so
        // look these up directly by their distinct config-topic substrings.
        let fan = entities
            .iter()
            .find(|e| e.config_topic.contains("/number/") && e.config_topic.contains("fan_pwm_set"))
            .expect("fan command entity");
        assert_eq!(fan.payload["command_topic"], topics.fan_pwm_set);
        assert_eq!(fan.payload["state_topic"], topics.fan_pwm_state);

        let heater = entities
            .iter()
            .find(|e| e.config_topic.contains("/climate/"))
            .expect("climate entity");
        assert_eq!(
            heater.payload["temperature_command_topic"],
            topics.target_temp_set
        );
        assert_eq!(
            heater.payload["temperature_state_topic"],
            topics.target_temp_state
        );
    }

    /// MQTT-2 EMPTY-CHAINS GUARD PIN: both the temperature sensor and the climate
    /// heater read the chain temperature via `chains[0]`. A bare `chains[0]` index
    /// misbehaves in HA's Jinja2 when the array is empty (a unit reporting no chains
    /// yet, or a transient empty publish), so the templates MUST predicate the index
    /// on a non-empty `value_json.chains` and fall back to 0 — never evaluate an
    /// out-of-range subscript.
    #[test]
    fn chain_temperature_templates_guard_the_empty_chains_case() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        let temperature = entity_by_id(&entities, "sensor", "temperature");
        let temp_tmpl = temperature.payload["value_template"].as_str().unwrap();
        assert!(
            temp_tmpl.contains("if value_json.chains"),
            "temperature sensor template must guard the empty-chains case, got: {temp_tmpl}"
        );
        assert!(
            temp_tmpl.contains("else 0"),
            "temperature sensor template must fall back to 0 on empty chains, got: {temp_tmpl}"
        );

        let heater = entity_by_id(&entities, "climate", "heater");
        let heater_tmpl = heater.payload["current_temperature_template"]
            .as_str()
            .unwrap();
        assert!(
            heater_tmpl.contains("if value_json.chains"),
            "climate current_temperature_template must guard the empty-chains case, got: {heater_tmpl}"
        );
        assert!(
            heater_tmpl.contains("else 0"),
            "climate current_temperature_template must fall back to 0 on empty chains, got: {heater_tmpl}"
        );
    }

    /// POWER-PROVENANCE PIN: Home Assistant numeric power/heat/efficiency
    /// sensors must not publish static/model fallback values as if they were
    /// live readings. The shared state payload carries `live_power_available`;
    /// these templates fail closed to `none` until that flag is true.
    #[test]
    fn power_derived_templates_require_live_power_availability() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            false,
            &MqttDeviceIdentity::default(),
        );

        for (id, field) in [
            ("power", "wall_watts"),
            ("btu", "btu_h"),
            ("efficiency", "efficiency_jth"),
        ] {
            let entity = entity_by_id(&entities, "sensor", id);
            let tmpl = entity.payload["value_template"].as_str().unwrap();
            assert!(
                tmpl.contains(field),
                "{id} template must read {field}, got: {tmpl}"
            );
            assert!(
                tmpl.contains("if value_json.live_power_available"),
                "{id} template must gate on live power, got: {tmpl}"
            );
            assert!(
                tmpl.contains("else none"),
                "{id} template must fail closed to none without live power, got: {tmpl}"
            );
        }
    }

    /// POWER-PROVENANCE ATTRIBUTES PIN: the power/BTU/efficiency sensors must
    /// expose the power-provenance fields as HA entity attributes (via
    /// `json_attributes_topic` + `json_attributes_template`), so an operator
    /// can SEE when a wattage is modeled vs measured. The attribute template
    /// reads the shared state topic and surfaces source/detail/modeled/
    /// calibrated/note.
    #[test]
    fn power_derived_sensors_surface_provenance_attributes() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            false,
            &MqttDeviceIdentity::default(),
        );

        for id in ["power", "btu", "efficiency"] {
            let entity = entity_by_id(&entities, "sensor", id);
            assert_eq!(
                entity.payload["json_attributes_topic"], "dcentrald/state",
                "{id} must attach attributes from the shared state topic"
            );
            let tmpl = entity.payload["json_attributes_template"]
                .as_str()
                .unwrap_or_else(|| panic!("{id} missing json_attributes_template"));
            for field in [
                "power_source",
                "power_source_detail",
                "power_modeled",
                "power_calibrated",
                "power_note",
            ] {
                assert!(
                    tmpl.contains(field),
                    "{id} attribute template must expose {field}, got: {tmpl}"
                );
            }
        }

        // A diagnostic text sensor surfaces the human-readable provenance note.
        let provenance = entity_by_id(&entities, "sensor", "power_provenance");
        assert_eq!(provenance.payload["entity_category"], "diagnostic");
        assert!(provenance.payload["value_template"]
            .as_str()
            .unwrap()
            .contains("value_json.power_note"));
    }

    /// HA ENERGY PIN: the Energy dashboard requires a `device_class=energy` +
    /// `state_class=total_increasing` sensor in a kWh-family unit reading a
    /// monotonic total. Pins the schema so a template/field drift can't
    /// silently drop the miner from the HA Energy dashboard, and pins the
    /// read-only + provenance-attribute posture (the integrated wattage can be
    /// modeled, so the same attributes as the power sensor must travel along).
    #[test]
    fn energy_sensor_is_total_increasing_kwh_with_provenance_and_no_command_topic() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            false,
            &MqttDeviceIdentity::default(),
        );

        let energy = entity_by_id(&entities, "sensor", "energy");
        assert_eq!(energy.payload["device_class"], "energy");
        assert_eq!(energy.payload["state_class"], "total_increasing");
        assert_eq!(energy.payload["unit_of_measurement"], "kWh");
        assert_eq!(energy.payload["state_topic"], "dcentrald/state");
        assert_eq!(
            energy.payload["availability_topic"],
            "dcentrald/availability"
        );

        let tmpl = energy.payload["value_template"].as_str().unwrap();
        assert!(
            tmpl.contains("value_json.energy_kwh"),
            "energy template must read energy_kwh, got: {tmpl}"
        );
        // NOT gated to `none` on live power: a monotonic total stays true while
        // power is unavailable — it simply stops increasing.
        assert!(
            !tmpl.contains("live_power_available"),
            "energy total must not blank out on live-power gaps, got: {tmpl}"
        );

        // Provenance attributes travel with the total, like the power sensor.
        assert_eq!(energy.payload["json_attributes_topic"], "dcentrald/state");
        assert!(energy.payload["json_attributes_template"]
            .as_str()
            .unwrap()
            .contains("power_modeled"));

        // Read-only: never a command topic.
        assert!(energy.payload.get("command_topic").is_none());
    }

    /// CREDENTIAL-MASKING PIN: the discovery schema is built from constants +
    /// Jinja2 templates only — it must NEVER embed the operator's pool worker
    /// (a BTC payout address in V1 solo), pool password, or broker credentials.
    /// A regression that interpolated config secrets into a discovery payload
    /// would leak them to every MQTT subscriber on the (retained) config topic.
    #[test]
    fn discovery_payloads_never_embed_credentials_or_wallet() {
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        // Sample of secrets that MUST NOT appear in any discovery surface.
        let forbidden = [
            "bc1q",     // bech32 wallet / V1 solo worker prefix
            "password", // a credential key/value leaking into the schema
            "user:",    // user:pass@ embedded in a broker/pool URL
            "@",        // host-auth separator from a credentialed URL
        ];

        for e in &entities {
            let serialized = serde_json::to_string(&e.payload).expect("serializes");
            // The whole emitted surface = the config topic + the JSON payload.
            let whole = format!("{}\n{}", e.config_topic, serialized);
            for needle in &forbidden {
                assert!(
                    !whole.contains(needle),
                    "HA discovery surface for {} leaked forbidden token {:?}: {}",
                    e.config_topic,
                    needle,
                    whole
                );
            }
        }
    }
}

#[cfg(test)]
mod broker_tls_guard_tests {
    use super::*;

    /// FAIL-CLOSED PIN (MQTT-TLS-DOWNGRADE-1): every TLS-requesting scheme is
    /// detected so the connect sites refuse rather than silently downgrade an
    /// encrypted broker to plaintext TCP (which would put the username/password
    /// on the wire in the clear).
    #[test]
    fn tls_requesting_schemes_are_detected() {
        for url in [
            "mqtts://broker.example:8883",
            "ssl://broker.example:8883",
            "tls://broker.example:8883",
            "  mqtts://broker.example:8883  ", // surrounding whitespace trimmed
        ] {
            assert!(
                broker_url_requires_tls(url),
                "{} must be detected as TLS-requesting and fail closed",
                url
            );
        }
    }

    #[test]
    fn plaintext_schemes_do_not_trip_the_tls_guard() {
        for url in [
            "mqtt://broker.example:1883",
            "tcp://broker.example:1883",
            "broker.example:1883",
            "broker.example",
        ] {
            assert!(
                !broker_url_requires_tls(url),
                "{} is plaintext and must not trip the TLS guard",
                url
            );
        }
    }

    /// run_publisher (and test_connection) only attach credentials AFTER the
    /// TLS guard. This test pins the guard's role: an mqtts:// URL is refused so
    /// credentials are never sent in cleartext. We exercise the same predicate
    /// the connect sites gate on, and prove the parsed host/port still resolves
    /// (so the refusal is a deliberate fail-closed, not a parse accident).
    #[test]
    fn mqtts_broker_is_refused_before_any_credential_send() {
        let url = "mqtts://broker.example:8883";
        // The guard fires (connect site bails with an error before set_credentials).
        assert!(broker_url_requires_tls(url));
        // And it is NOT because the URL is unparseable — host/port are valid;
        // the refusal is a security decision, not a parse failure.
        let (host, port) = parse_broker_url(url).expect("mqtts url still parses host/port");
        assert_eq!(host, "broker.example");
        assert_eq!(port, 8883);
    }

    /// The plaintext default port is 1883 when no port is given (so a misparsed
    /// scheme can't silently land on an unexpected port).
    #[test]
    fn parse_broker_url_defaults_and_strips_scheme() {
        assert_eq!(
            parse_broker_url("mqtt://h").unwrap(),
            ("h".to_string(), 1883)
        );
        assert_eq!(
            parse_broker_url("mqtts://h:8883").unwrap(),
            ("h".to_string(), 8883)
        );
        // Empty hostname is a hard error, never a silent default.
        assert!(parse_broker_url("mqtt://").is_err());
        assert!(parse_broker_url("mqtt://:1883").is_err());
    }
}

#[cfg(test)]
mod broker_url_masking_tests {
    use super::*;

    /// MQTT-4 NEGATIVE REGRESSION: a broker URL can carry inline `user:pass@host`
    /// credentials, and `run_publisher` / `test_connection` emit the broker URL to a
    /// log line and to the TLS-refusal error message. Both call sites route the URL
    /// through `sanitize_pool_url` first, so the masked form that reaches the log/error
    /// must NOT contain the password, the user:pass separator, or the `@` host-auth
    /// separator — across every broker scheme (mqtt / mqtts / tcp).
    #[test]
    fn broker_credentials_are_stripped_before_logging() {
        for url in [
            "mqtt://relay:hunter2@broker.example:1883",
            "mqtts://relay:hunter2@broker.example:8883",
            "tcp://relay:hunter2@broker.example:1883",
        ] {
            let masked = sanitize_pool_url(url);
            assert!(
                !masked.contains("hunter2"),
                "masked broker {masked:?} (from {url:?}) must not leak the password"
            );
            assert!(
                !masked.contains(":hunter2@") && !masked.contains("relay:"),
                "masked broker {masked:?} (from {url:?}) must not leak user:pass@"
            );
            assert!(
                !masked.contains('@'),
                "masked broker {masked:?} (from {url:?}) must drop the host-auth separator"
            );
            // The host:port survives so the log line is still useful.
            assert!(
                masked.contains("broker.example"),
                "masked broker {masked:?} (from {url:?}) must keep the host"
            );
        }
    }

    /// A credential-free broker URL must round-trip unchanged (masking must not
    /// mangle the common case).
    #[test]
    fn credential_free_broker_url_is_unchanged() {
        assert_eq!(
            sanitize_pool_url("mqtt://broker.example:1883"),
            "mqtt://broker.example:1883"
        );
    }

    /// MQTT-4 EMISSION-SITE REGRESSION: the tests above exercise `sanitize_pool_url`
    /// in isolation, but the load-bearing property is that the ACTUAL error sink
    /// masks credentials. `test_connection` bails on an `mqtts://` broker (the
    /// TLS-refusal path) with a message built from the masked `broker_display`, and
    /// that `bail!` text IS the returned error's Display — so a credentialed broker
    /// URL must reach the caller with the host intact but the password and the
    /// `user:pass@` separator stripped. This proves the masking at the real emission
    /// site, not just in the helper (no tracing-subscriber capture needed).
    #[tokio::test]
    async fn mqtt_test_connection_error_masks_broker_credentials() {
        let config = MqttPublisherConfig {
            broker: "mqtts://relay:secretpass@broker.example:8883".to_string(),
            topic_prefix: "dcentrald".to_string(),
            discovery: false,
            username: None,
            password: None,
            publish_interval_s: 30,
            device: MqttDeviceIdentity::default(),
        };

        let err = test_connection(&config)
            .await
            .expect_err("an mqtts:// broker with no TLS transport must fail closed");
        let shown = err.to_string();

        assert!(
            !shown.contains("secretpass"),
            "test_connection error must not leak the broker password: {shown}"
        );
        assert!(
            !shown.contains(":secretpass@") && !shown.contains("relay:"),
            "test_connection error must not leak the user:pass@ separator: {shown}"
        );
        assert!(
            shown.contains("broker.example"),
            "test_connection error should still name the host so it stays useful: {shown}"
        );
    }
}

#[cfg(test)]
mod lwt_and_connect_plan_tests {
    use super::*;

    #[test]
    fn publisher_options_register_retained_offline_lastwill_on_availability_topic() {
        let options =
            build_publisher_mqtt_options("dcentrald_AABBCC", "broker.local", 1883, "dcentrald");

        // The LWT contract that keeps HA honest after a crash/power-loss: the
        // broker itself flips the SAME retained availability topic every
        // discovery entity gates on to `offline`. Topic drift here means HA
        // ignores the will and shows a dead miner as online forever.
        let will = options.last_will().expect("LastWill must be registered");
        assert_eq!(will.topic, "dcentrald/availability");
        assert_eq!(will.message.as_ref(), AVAILABILITY_OFFLINE.as_bytes());
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(
            will.retain,
            "the LWT must be retained or HA re-reads stale 'online'"
        );
    }

    #[test]
    fn on_connect_plan_orders_subscribes_then_discovery_then_availability_then_state() {
        let identity = MqttDeviceIdentity::default();
        let plan = build_on_connect_plan(
            "dcentrald",
            "AABBCC",
            true,
            true,
            &identity,
            Some(r#"{"hashrate_ghs":1.0}"#),
        );
        let topics = CommandTopics::new("dcentrald");
        let discovery_count =
            build_ha_discovery_entities("dcentrald", "AABBCC", true, &identity).len();

        // 1) Command subscribes FIRST (clean_session=true forgets them on every
        //    disconnect) so no inbound command window is lost.
        let expected_subscribes = [
            topics.fan_pwm_set.clone(),
            topics.target_watts_set.clone(),
            topics.target_temp_set.clone(),
        ];
        for (i, expected_topic) in expected_subscribes.iter().enumerate() {
            assert_eq!(
                plan[i],
                MqttConnectOp::Subscribe {
                    topic: expected_topic.clone(),
                    qos: QoS::AtLeastOnce,
                },
                "op {i} must re-subscribe {expected_topic}"
            );
        }

        // 2) Retained discovery configs next.
        for op in &plan[3..3 + discovery_count] {
            match op {
                MqttConnectOp::Publish {
                    topic, retain, qos, ..
                } => {
                    assert!(
                        topic.starts_with("homeassistant/"),
                        "expected discovery config publish, got topic {topic}"
                    );
                    assert!(*retain, "discovery configs must be retained");
                    assert_eq!(*qos, QoS::AtLeastOnce);
                }
                other => panic!("expected discovery publish, got {other:?}"),
            }
        }

        // 3) Retained availability `online` AFTER discovery so a freshly
        //    discovered entity never sits on a stale `offline`.
        match &plan[3 + discovery_count] {
            MqttConnectOp::Publish {
                topic,
                payload,
                retain,
                qos,
            } => {
                assert_eq!(topic, "dcentrald/availability");
                assert_eq!(payload, AVAILABILITY_ONLINE);
                assert!(*retain);
                assert_eq!(*qos, QoS::AtLeastOnce);
            }
            other => panic!("expected availability publish, got {other:?}"),
        }

        // 4) The latest state LAST, not retained (the periodic loop refreshes it).
        match plan.last().expect("plan is non-empty") {
            MqttConnectOp::Publish {
                topic,
                payload,
                retain,
                qos,
            } => {
                assert_eq!(topic, "dcentrald/state");
                assert_eq!(payload, r#"{"hashrate_ghs":1.0}"#);
                assert!(!*retain, "state snapshots must not be retained");
                assert_eq!(*qos, QoS::AtMostOnce);
            }
            other => panic!("expected state publish, got {other:?}"),
        }

        assert_eq!(plan.len(), 3 + discovery_count + 2);
    }

    #[test]
    fn on_connect_plan_without_commands_discovery_or_state_is_availability_only() {
        let plan = build_on_connect_plan(
            "dcentrald",
            "AABBCC",
            false,
            false,
            &MqttDeviceIdentity::default(),
            None,
        );
        assert_eq!(plan.len(), 1, "plan must degrade to availability-only");
        match &plan[0] {
            MqttConnectOp::Publish {
                topic,
                payload,
                retain,
                ..
            } => {
                assert_eq!(topic, "dcentrald/availability");
                assert_eq!(payload, AVAILABILITY_ONLINE);
                assert!(*retain);
            }
            other => panic!("expected availability publish, got {other:?}"),
        }
    }

    #[test]
    fn discovery_device_block_carries_per_unit_identity_with_stable_unique_ids() {
        let identity = MqttDeviceIdentity {
            model: Some("Antminer S19j Pro".to_string()),
            hostname: Some("miner-25".to_string()),
            management_ip: Some("203.0.113.25".to_string()),
        };
        let entities = build_ha_discovery_entities("dcentrald", "AABBCC", true, &identity);

        for entity in &entities {
            let device = &entity.payload["device"];
            assert_eq!(
                device["name"], "DCENT_OS Miner AABBCC",
                "device name must be MAC-suffixed so fleet units don't collide"
            );
            assert_eq!(device["model"], "Antminer S19j Pro");
            assert_eq!(
                device["configuration_url"], "http://203.0.113.25",
                "management IP must win over hostname for configuration_url"
            );
            assert_eq!(device["identifiers"][0], "dcentrald_AABBCC");
        }

        // The unique_id scheme is LOAD-BEARING: changing it orphans the HA
        // entities of every unit already running the beta.
        assert!(entities
            .iter()
            .any(|e| e.payload["unique_id"] == "dcentrald_AABBCC_hashrate"));

        // Fallback ladder: hostname, then the generic mDNS URL.
        let host_only = MqttDeviceIdentity {
            hostname: Some("miner-25".to_string()),
            ..Default::default()
        };
        assert_eq!(host_only.configuration_url(), "http://miner-25");
        assert_eq!(
            MqttDeviceIdentity::default().configuration_url(),
            "http://dcentos.local"
        );
    }
}

#[cfg(test)]
mod ws_stats_schema_contract_tests {
    use super::*;
    use crate::websocket::{WsChainStatus, WsFanStatus, WsPoolStatus, WsStatsMessage};

    /// A fully-populated stats frame, serialized exactly like the live
    /// publisher serializes it. Every field a discovery template references
    /// must resolve against this JSON — the pin that turns a silent
    /// field-rename breakage into a red test.
    fn fully_populated_stats_json() -> serde_json::Value {
        let msg = WsStatsMessage {
            msg_type: "stats".to_string(),
            timestamp: 1_700_000_000,
            hashrate_ghs: 13_500.0,
            hashrate_5s_ghs: 13_400.0,
            accepted: 42,
            rejected: 1,
            chains: vec![WsChainStatus {
                id: 6,
                chips: 63,
                frequency_mhz: 650,
                voltage_mv: 9_100,
                temp_c: 55.5,
                temp_source: Some("board_sensor".to_string()),
                hashrate_ghs: 4_500.0,
                errors: 0,
                status: "Mining".to_string(),
            }],
            fans: WsFanStatus {
                pwm: 30,
                rpm: 2_880,
                per_fan: Vec::new(),
            },
            pool: WsPoolStatus {
                url: "stratum+tcp://pool.example:3333".to_string(),
                status: "Connected".to_string(),
                difficulty: 512.0,
                last_share_s: 3,
                protocol: Some("V1".to_string()),
                encrypted: Some(false),
                donating: Some(false),
                donation_active_url: None,
                donation_active_worker: None,
                donation_pool_index: None,
                share_efficiency: None,
                auto_fallback_active: None,
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: None,
                sv2_session: None,
            },
            power_watts: 1_200.0,
            wall_watts: 1_300.0,
            efficiency_jth: 27.6,
            btu_h: 4_436.0,
            power_source: "estimated".to_string(),
            power_source_detail: "runtime_model".to_string(),
            live_power_available: false,
            power_modeled: true,
            power_note: "runtime model".to_string(),
            power_calibrated: false,
            power_calibration_multiplier: None,
            watt_cap: None,
            uptime_s: 3_600,
            energy_kwh: 1.5,
        };
        serde_json::to_value(&msg).expect("stats frame serializes")
    }

    /// Extract every `value_json.<path>` reference from a Jinja2 template.
    fn value_json_paths(template: &str) -> Vec<String> {
        let mut paths = Vec::new();
        let mut rest = template;
        while let Some(idx) = rest.find("value_json.") {
            let after = &rest[idx + "value_json.".len()..];
            let end = after
                .find(|c: char| {
                    !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '[' || c == ']')
                })
                .unwrap_or(after.len());
            paths.push(after[..end].trim_end_matches('.').to_string());
            rest = &after[end..];
        }
        paths
    }

    /// Resolve a dotted path (with `[n]` array indexes) against a JSON value.
    fn resolve_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        let mut current = root;
        for segment in path.split('.') {
            if segment.is_empty() {
                return None;
            }
            let (key, mut indexes) = match segment.find('[') {
                Some(i) => (&segment[..i], &segment[i..]),
                None => (segment, ""),
            };
            current = current.get(key)?;
            while let Some(close) = indexes.find(']') {
                let n: usize = indexes[1..close].parse().ok()?;
                current = current.get(n)?;
                indexes = &indexes[close + 1..];
            }
        }
        Some(current)
    }

    #[test]
    fn every_discovery_template_path_resolves_against_ws_stats_message() {
        let stats = fully_populated_stats_json();
        let entities = build_ha_discovery_entities(
            "dcentrald",
            "AABBCC",
            true,
            &MqttDeviceIdentity::default(),
        );

        let mut checked = 0usize;
        for entity in &entities {
            for field in [
                "value_template",
                "current_temperature_template",
                "json_attributes_template",
            ] {
                let Some(template) = entity.payload.get(field).and_then(|v| v.as_str()) else {
                    continue;
                };
                for path in value_json_paths(template) {
                    assert!(
                        resolve_path(&stats, &path).is_some(),
                        "discovery template path `value_json.{path}` (entity {}) does not \
                         resolve against the serialized WsStatsMessage — a websocket field \
                         rename just silently broke this HA entity",
                        entity.config_topic
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked >= 12,
            "expected to resolve at least 12 template paths, found {checked} — \
             the extractor or the discovery entity set changed"
        );
    }

    #[test]
    fn schema_pin_actually_fails_on_a_renamed_field() {
        // Guard against a tautological extractor/resolver: removing a field
        // (what a rename looks like to consumers) must make resolution fail.
        let mut stats = fully_populated_stats_json();
        stats
            .as_object_mut()
            .expect("stats frame is an object")
            .remove("hashrate_ghs");
        assert!(resolve_path(&stats, "hashrate_ghs").is_none());
        assert!(resolve_path(&stats, "chains[0].temp_c").is_some());
        assert!(resolve_path(&stats, "chains[1].temp_c").is_none());
    }
}
