//! MQTT + webhook + alert plumbing shared by every mining mode.
//!
//! W2.1 extraction (2026-05-07): the notification stack used to live inline
//! at the top of `daemon.rs`. Hoisting it here keeps `daemon.rs` focused on
//! S9 / am2-passthrough hardware orchestration and lets the `--s19j-hybrid`
//! / `--stratum-proxy` modes wire alerts through the same code path
//! whenever they need it.
//!
//! Nothing here touches hardware. The MQTT publisher is a pure tokio task
//! consuming a `broadcast::Receiver<String>` of stats JSON and forwarding
//! to a remote broker; webhook firing is a single `reqwest` POST per
//! event. Both are best-effort and never block the mining hot path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::DcentraldConfig;

/// The MQTT/HA command-subscriber sink handle (P2-7): the validated-setter
/// trait object the daemon supplies so Home-Assistant setpoints route through
/// the SAME clamped paths the REST API uses. `Option<_>` is `None` for
/// transient/proxy bring-ups (no command surface) — the subscriber is
/// default-OFF.
type MqttCommandSinkHandle = Arc<dyn dcentrald_api::mqtt::MqttCommandSink>;

pub const NOTIFICATION_RELOAD_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWebhookConfig {
    pub enabled: bool,
    pub url: String,
    pub events: Vec<String>,
    /// Delivery channel format (Generic / Discord / Slack / Telegram).
    pub format: dcentrald_api::webhook::WebhookFormat,
    /// Telegram bot token (only used when `format == Telegram`). SECRET.
    pub telegram_bot_token: String,
    /// Telegram chat id (only used when `format == Telegram`).
    pub telegram_chat_id: String,
}

impl From<Option<crate::config::WebhookConfig>> for RuntimeWebhookConfig {
    fn from(config: Option<crate::config::WebhookConfig>) -> Self {
        let config = config.unwrap_or_default();
        Self {
            enabled: config.enabled,
            url: config.url,
            events: config.events,
            format: config.format,
            telegram_bot_token: config.telegram_bot_token.unwrap_or_default(),
            telegram_chat_id: config.telegram_chat_id.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMqttConfig {
    pub enabled: bool,
    pub broker: String,
    pub topic_prefix: String,
    pub discovery: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub publish_interval_s: u16,
}

impl From<crate::config::MqttConfig> for RuntimeMqttConfig {
    fn from(config: crate::config::MqttConfig) -> Self {
        Self {
            enabled: config.enabled,
            broker: config.broker,
            topic_prefix: config.topic_prefix,
            discovery: config.discovery,
            username: config.username,
            password: config.password,
            publish_interval_s: config.publish_interval_s,
        }
    }
}

impl RuntimeMqttConfig {
    pub fn publisher_config(&self) -> dcentrald_api::mqtt::MqttPublisherConfig {
        dcentrald_api::mqtt::MqttPublisherConfig {
            broker: self.broker.clone(),
            topic_prefix: self.topic_prefix.clone(),
            discovery: self.discovery,
            username: self.username.clone(),
            password: self.password.clone(),
            publish_interval_s: self.publish_interval_s,
            // Best-effort on-device identity (platform stamp + hostname) so
            // the HA device block distinguishes multi-unit fleets. Absent
            // files (dev hosts, tests) leave the generic fallbacks.
            device: dcentrald_api::mqtt::MqttDeviceIdentity::detect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeNotificationConfig {
    pub webhook: RuntimeWebhookConfig,
    pub mqtt: RuntimeMqttConfig,
}

impl RuntimeNotificationConfig {
    pub fn from_config(config: &DcentraldConfig) -> Self {
        Self {
            webhook: RuntimeWebhookConfig::from(config.webhook.clone()),
            mqtt: RuntimeMqttConfig::from(config.mqtt.clone()),
        }
    }

    pub fn load(path: &str) -> Result<Self> {
        let config = DcentraldConfig::load(path)?;
        Ok(Self::from_config(&config))
    }
}

pub struct MqttPublisherTask {
    shutdown: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl MqttPublisherTask {
    pub fn cancel(self) {
        self.shutdown.cancel();
        std::mem::drop(self.handle);
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

pub fn spawn_mqtt_publisher(
    config: &RuntimeMqttConfig,
    stats_broadcast_tx: &broadcast::Sender<String>,
    mac: &str,
    shutdown: &CancellationToken,
    command_sink: Option<MqttCommandSinkHandle>,
) -> MqttPublisherTask {
    let mqtt_pub_config = config.publisher_config();
    let mqtt_stats_rx = stats_broadcast_tx.subscribe();
    let mqtt_mac = mac.to_string();
    let mqtt_shutdown = shutdown.child_token();
    let task_shutdown = mqtt_shutdown.clone();

    let handle = tokio::spawn(async move {
        if let Err(error) = dcentrald_api::mqtt::run_publisher(
            mqtt_pub_config,
            mqtt_stats_rx,
            mqtt_mac,
            mqtt_shutdown,
            command_sink,
        )
        .await
        {
            tracing::error!(error = %error, "MQTT publisher stopped");
        }
    });

    MqttPublisherTask {
        shutdown: task_shutdown,
        handle,
    }
}

// ---------------------------------------------------------------------------
// Shared notification stack — MQTT + event-bus webhook dispatcher (P1-4 Omega)
// ---------------------------------------------------------------------------

/// Accepted shares between [`dcentrald_api::webhook::WebhookEvent::ShareMilestone`]
/// webhook emissions. The event-bus → webhook bridge counts `ShareAccepted`
/// mining-sync events and fires a milestone every N; lucky shares always fire
/// immediately regardless. 0 disables milestones; 100 is the documented
/// default in [`dcentrald_api::webhook::spawn_mining_sync_bridge`].
pub const WEBHOOK_SHARE_MILESTONE_EVERY: u64 = 100;

/// Build the HAL-free [`dcentrald_api::webhook::WebhookDispatchConfig`] the
/// event-bus dispatcher consumes, from the daemon-side [`RuntimeWebhookConfig`]
/// plus the operator's miner name (payload envelope only).
fn webhook_dispatch_config(
    webhook: &RuntimeWebhookConfig,
    miner_name: &str,
) -> dcentrald_api::webhook::WebhookDispatchConfig {
    dcentrald_api::webhook::WebhookDispatchConfig {
        enabled: webhook.enabled,
        url: webhook.url.clone(),
        events: webhook.events.clone(),
        miner_name: miner_name.to_string(),
        format: webhook.format,
        telegram_bot_token: webhook.telegram_bot_token.clone(),
        telegram_chat_id: webhook.telegram_chat_id.clone(),
    }
}

/// Spawn the full notification stack — MQTT publisher + the event-bus →
/// webhook dispatcher (+ its mining-sync bridge) — shared by EVERY mining mode.
///
/// Before P1-4 (Omega) this plumbing was wired ONLY on the S9 `Daemon::run`
/// path: the three `spawn_mqtt_publisher` call sites lived inline in
/// `daemon.rs`, and the rich [`dcentrald_api::webhook::WebhookDispatcher`] +
/// [`dcentrald_api::webhook::spawn_mining_sync_bridge`] (built and unit-tested
/// in `dcentrald-api`) had ZERO call sites. The am2/am3 `--s19j-hybrid` and
/// `--stratum-proxy` modes got NEITHER. This single entrypoint unifies both so
/// every platform brings the whole stack up identically — called from
/// `Daemon::run` (S9) and from `runtime::api::spawn_proxy_mode_api` (every
/// non-`Daemon` mode: hybrid / proxy / am3-bb / serial-idle / stock-idle).
///
/// Safety / behaviour contract:
/// - **Default-OFF, the same gate the S9 path always used.** MQTT only spawns a
///   publisher when `[mqtt].enabled`; the webhook dispatcher drops every event
///   unless `[webhook].enabled` AND a non-empty URL are set (its `is_live()`
///   gate). With both disabled (the shipped default) this spawns two idle tasks
///   that perform no network I/O.
/// - **Never blocks the caller or the mining hot path.** Everything runs on
///   detached tokio tasks; enqueue onto the dispatcher is a non-blocking
///   `try_send`.
/// - `reload_path = Some(path)` drives the same 5 s live-reload the inline S9
///   MQTT block always had (toggle MQTT + webhook from `dcentrald.toml` with no
///   restart). `None` skips live-reload (used by the transient `/tmp`
///   proxy/hybrid bring-up); the stack still honors the initial config.
///
/// The MQTT half is behaviour-equivalent to the pre-P1-4 inline `daemon.rs`
/// block; the webhook dispatcher is purely additive.
pub fn spawn_notification_stack(
    initial: RuntimeNotificationConfig,
    reload_path: Option<String>,
    mac: String,
    miner_name: String,
    stats_broadcast_tx: broadcast::Sender<String>,
    mining_sync_tx: broadcast::Sender<String>,
    shutdown: CancellationToken,
    command_sink: Option<MqttCommandSinkHandle>,
) {
    // ---- Event-bus → webhook dispatcher (+ mining-sync bridge) ----
    // The dispatcher reads its config from a `watch` channel so a live
    // `[webhook]` toggle applies without a restart (same UX as the MQTT
    // reload). The bridge subscribes to the mining-sync broadcast and forwards
    // share-milestone / lucky-share events; it holds a `WebhookHandle` clone,
    // which keeps the dispatcher task alive for the lifetime of the stack.
    let (webhook_cfg_tx, webhook_cfg_rx) =
        tokio::sync::watch::channel(webhook_dispatch_config(&initial.webhook, &miner_name));
    let webhook_dispatcher = dcentrald_api::webhook::WebhookDispatcher::spawn(
        webhook_cfg_rx,
        dcentrald_api::webhook::WebhookDispatchTuning::default(),
        shutdown.child_token(),
    );
    let _webhook_bridge = dcentrald_api::webhook::spawn_mining_sync_bridge(
        webhook_dispatcher.handle(),
        mining_sync_tx.subscribe(),
        WEBHOOK_SHARE_MILESTONE_EVERY,
        shutdown.child_token(),
    );

    if initial.webhook.enabled && !initial.webhook.url.trim().is_empty() {
        tracing::info!(
            // Redact the embedded token (Discord/Telegram put a secret in the path).
            url = %crate::daemon::sanitize_webhook_url(&initial.webhook.url),
            events = ?initial.webhook.events,
            "Webhook event dispatcher enabled — share milestones / lucky shares will POST to configured URL"
        );
    } else {
        tracing::debug!(
            "Webhook event dispatcher idle at startup (default-OFF) — daemon will poll for config changes"
        );
    }

    // ---- MQTT publisher (+ unified 5 s live-reload) ----
    let mut mqtt_runtime = initial.mqtt;
    if mqtt_runtime.enabled {
        tracing::info!(
            broker = %dcentrald_stratum::pool_api::sanitize_pool_url(&mqtt_runtime.broker),
            prefix = %mqtt_runtime.topic_prefix,
            discovery = mqtt_runtime.discovery,
            "MQTT publisher started — your miner will appear in Home Assistant automatically"
        );
    } else {
        tracing::debug!("MQTT disabled at startup — daemon will poll for config changes");
    }

    tokio::spawn(async move {
        // Hold the dispatcher + its config sender for the task's lifetime so the
        // webhook stack lives as long as the notification task.
        let _webhook_dispatcher = webhook_dispatcher;

        let mut reload_timer = tokio::time::interval(NOTIFICATION_RELOAD_INTERVAL);
        reload_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut publisher = mqtt_runtime.enabled.then(|| {
            spawn_mqtt_publisher(
                &mqtt_runtime,
                &stats_broadcast_tx,
                &mac,
                &shutdown,
                command_sink.clone(),
            )
        });

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    if let Some(task) = publisher.take() {
                        task.cancel();
                    }
                    break;
                }
                _ = reload_timer.tick() => {
                    // No reload path (transient /tmp bring-up): keep the stack
                    // running on the initial config.
                    let Some(ref path) = reload_path else { continue };
                    match RuntimeNotificationConfig::load(path) {
                        Ok(runtime) => {
                            // Webhook dispatcher hot-reload (watch push). A torn-down
                            // dispatcher (all handles dropped) makes this a no-op.
                            let _ = webhook_cfg_tx
                                .send(webhook_dispatch_config(&runtime.webhook, &miner_name));

                            // MQTT publisher hot-reload — behaviour-equivalent to the
                            // pre-P1-4 inline S9 logic.
                            if runtime.mqtt != mqtt_runtime {
                                let old_runtime = mqtt_runtime.clone();
                                mqtt_runtime = runtime.mqtt;

                                if let Some(task) = publisher.take() {
                                    task.cancel();
                                }

                                if mqtt_runtime.enabled {
                                    publisher = Some(spawn_mqtt_publisher(
                                        &mqtt_runtime,
                                        &stats_broadcast_tx,
                                        &mac,
                                        &shutdown,
                                        command_sink.clone(),
                                    ));
                                }

                                tracing::info!(
                                    old_enabled = old_runtime.enabled,
                                    new_enabled = mqtt_runtime.enabled,
                                    broker = %dcentrald_stratum::pool_api::sanitize_pool_url(&mqtt_runtime.broker),
                                    prefix = %mqtt_runtime.topic_prefix,
                                    discovery = mqtt_runtime.discovery,
                                    interval_s = mqtt_runtime.publish_interval_s,
                                    "Reloaded MQTT config from dcentrald.toml"
                                );
                            } else if mqtt_runtime.enabled
                                && publisher.as_ref().is_some_and(MqttPublisherTask::is_finished)
                            {
                                tracing::warn!(
                                    "MQTT publisher exited unexpectedly — restarting with current config"
                                );
                                publisher = Some(spawn_mqtt_publisher(
                                    &mqtt_runtime,
                                    &stats_broadcast_tx,
                                    &mac,
                                    &shutdown,
                                    command_sink.clone(),
                                ));
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                path = %path,
                                "Failed to reload MQTT config — keeping previous runtime settings"
                            );
                        }
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Webhook alert events
// ---------------------------------------------------------------------------

/// Events that trigger webhook notifications.
///
/// Serialized as tagged JSON and POSTed to the configured webhook URL
/// (and surfaced as a browser notification by the dashboard).
/// The thermal loop fires `EmergencyShutdown`, `FanFailure`, and
/// `ThermalRestart`. The three mining-health events a home operator most
/// needs — `PoolDisconnected`, `MiningStopped`, and `HashBoardOffline` —
/// are constructed and fired by [`MiningAlertMonitor`] from the daemon's
/// 1 Hz state-publisher loop, debounced so a flapping condition can't spam
/// the alert surface.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum AlertEvent {
    EmergencyShutdown {
        temp_c: f32,
        chain_id: u8,
    },
    FanFailure {
        rpm: u32,
    },
    PoolDisconnected {
        url: String,
    },
    MiningStopped {
        reason: String,
    },
    HashBoardOffline {
        chain_id: u8,
    },
    ThermalRestart,
    /// HLA-10 (detection-only): total hashrate has stayed below the operator's
    /// `degraded_hashrate_alert_floor_ghs` floor (but above the idle epsilon —
    /// a degradation, not a full stop) for the sustained confirm window. Alert
    /// only — never throttles/reboots/touches hardware.
    HashrateDegraded {
        observed_ghs: f64,
        floor_ghs: f64,
    },
    /// PH-3: the operator-gated auto-recovery ladder exhausted its per-episode
    /// restart budget for a sustained degraded episode and gave up — emitted
    /// once. Signals the degradation is likely a hardware fault, not recoverable
    /// by a daemon restart.
    HashrateRecoveryExhausted {
        observed_ghs: f64,
        floor_ghs: f64,
        attempts: u32,
    },
}

impl AlertEvent {
    /// Return the event name string for filtering against `WebhookConfig.events`.
    pub fn event_name(&self) -> &'static str {
        match self {
            AlertEvent::EmergencyShutdown { .. } => "emergency_shutdown",
            AlertEvent::FanFailure { .. } => "fan_failure",
            AlertEvent::PoolDisconnected { .. } => "pool_disconnected",
            AlertEvent::MiningStopped { .. } => "mining_stopped",
            AlertEvent::HashBoardOffline { .. } => "hashboard_offline",
            AlertEvent::ThermalRestart => "thermal_restart",
            AlertEvent::HashrateDegraded { .. } => "hashrate_degraded",
            AlertEvent::HashrateRecoveryExhausted { .. } => "hashrate_recovery_exhausted",
        }
    }
}

/// Redact every secret-bearing field of an `AlertEvent` IN PLACE.
///
/// Only two variants carry a potential wallet/credential:
/// - `PoolDisconnected { url }` — the pool URL can embed the worker (= wallet)
///   and password (`stratum+tcp://bc1q…:x@pool:3333`). Reduced to a
///   credential-free host[:port] via the SAME [`dcentrald_api::webhook::redact_pool`]
///   the dispatcher uses. **This closes the historical un-redacted-`url` leak.**
/// - `MiningStopped { reason }` — free text scanned for embedded wallet shapes.
///
/// Run this before the AlertEvent is serialized (Generic envelope) OR mapped to
/// a [`dcentrald_api::webhook::WebhookEvent`] for a Discord/Slack/Telegram body,
/// so no formatting path can ever see an un-redacted field. Idempotent.
pub fn redact_alert_event(event: &mut AlertEvent) {
    match event {
        AlertEvent::PoolDisconnected { url } => {
            *url = dcentrald_api::webhook::redact_pool(url);
        }
        AlertEvent::MiningStopped { reason } => {
            *reason = dcentrald_common::wallet_mask::mask_in_string(reason).into_owned();
        }
        AlertEvent::EmergencyShutdown { .. }
        | AlertEvent::FanFailure { .. }
        | AlertEvent::HashBoardOffline { .. }
        | AlertEvent::ThermalRestart
        | AlertEvent::HashrateDegraded { .. }
        | AlertEvent::HashrateRecoveryExhausted { .. } => {}
    }
}

/// Map a daemon-side [`AlertEvent`] onto the `dcentrald-api` webhook event model
/// so thermal/pool/mining-health alerts can reuse the SAME
/// [`dcentrald_api::webhook::render_text`] / `payload_for` channel formatting as
/// the event-bus dispatcher (Discord / Slack / Telegram).
///
/// Total + 1:1 (`WebhookEvent` is a documented superset of `AlertEvent`), so no
/// alert is ever dropped or mis-mapped onto a semantically wrong event (honesty
/// invariant). The caller is expected to have run [`redact_alert_event`] first;
/// for belt-and-braces the caller also re-runs `WebhookEvent::redact()` (it is
/// idempotent), keeping redaction strictly ahead of any formatting.
pub fn alert_event_to_webhook_event(event: &AlertEvent) -> dcentrald_api::webhook::WebhookEvent {
    use dcentrald_api::webhook::WebhookEvent;
    match event {
        AlertEvent::EmergencyShutdown { temp_c, chain_id } => WebhookEvent::ThermalSafety {
            temp_c: *temp_c,
            chain_id: *chain_id,
        },
        AlertEvent::FanFailure { rpm } => WebhookEvent::FanFailure { rpm: *rpm },
        AlertEvent::PoolDisconnected { url } => {
            WebhookEvent::PoolDisconnected { pool: url.clone() }
        }
        AlertEvent::MiningStopped { reason } => WebhookEvent::MiningStopped {
            reason: reason.clone(),
        },
        AlertEvent::HashBoardOffline { chain_id } => WebhookEvent::HashBoardOffline {
            chain_id: *chain_id,
        },
        AlertEvent::ThermalRestart => WebhookEvent::ThermalRestart,
        AlertEvent::HashrateDegraded {
            observed_ghs,
            floor_ghs,
        } => WebhookEvent::HashrateDegraded {
            observed_ghs: *observed_ghs,
            floor_ghs: *floor_ghs,
        },
        AlertEvent::HashrateRecoveryExhausted {
            observed_ghs,
            floor_ghs,
            attempts,
        } => WebhookEvent::HashrateRecoveryExhausted {
            observed_ghs: *observed_ghs,
            floor_ghs: *floor_ghs,
            attempts: *attempts,
        },
    }
}

// ---------------------------------------------------------------------------
// Mining-health alert monitor (PoolDisconnected / MiningStopped /
// HashBoardOffline)  — C-4 (Omega P0-5)
// ---------------------------------------------------------------------------

/// A hashrate at or below this (GH/s) is treated as "not hashing". Tiny but
/// non-zero so floating-point dust from a decaying rolling average doesn't
/// keep a board looking alive after it has stopped producing nonces.
pub const HASHRATE_IDLE_EPSILON_GHS: f64 = 0.001;

/// Consecutive idle confirmations required before `MiningStopped` /
/// `HashBoardOffline` fire. The daemon calls [`MiningAlertMonitor::evaluate`]
/// at 1 Hz, so this is ~30 s of *sustained* idle — long enough to ride
/// through a brief work-queue gap or a slow board ramp without alerting.
pub const ALERT_IDLE_CONFIRM_TICKS: u32 = 30;

/// Minimum spacing between repeat emissions of the *same* event key across
/// distinct episodes. Within a single sustained episode the event fires
/// exactly once (edge-triggered); this guards against a fast flap
/// (idle→busy→idle→busy) re-firing on every idle edge.
pub const ALERT_REPEAT_SUPPRESS: Duration = Duration::from_secs(900);

/// Per-event edge-debounce state.
///
/// Fires once when a condition has been continuously active for
/// `confirm_ticks`, then stays latched (`fired = true`) until the condition
/// clears, so a persistent fault does not re-alert. A cleared condition
/// re-arms the edge; the next episode is additionally throttled by
/// `repeat_suppress` relative to the previous emission.
#[derive(Debug, Clone, Default)]
struct EdgeDebounce {
    idle_ticks: u32,
    fired: bool,
    last_fired: Option<Instant>,
}

impl EdgeDebounce {
    /// Advance the debounce by one evaluation. Returns `true` exactly on the
    /// tick the event should be emitted.
    fn update(
        &mut self,
        active: bool,
        confirm_ticks: u32,
        repeat_suppress: Duration,
        now: Instant,
    ) -> bool {
        if !active {
            // Condition cleared → re-arm for the next episode.
            self.idle_ticks = 0;
            self.fired = false;
            return false;
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        if self.fired || self.idle_ticks < confirm_ticks {
            return false;
        }
        // Condition confirmed and not yet alerted this episode. Latch it so
        // the persistent case never re-fires.
        let suppressed = self
            .last_fired
            .map(|t| now.duration_since(t) < repeat_suppress)
            .unwrap_or(false);
        self.fired = true;
        if suppressed {
            return false;
        }
        self.last_fired = Some(now);
        true
    }

    /// PH-3: true while the condition is currently CONFIRMED (latched) — it has
    /// been active for `confirm_ticks` and has not since cleared. Distinct from
    /// `update()`'s edge return (which fires once and is repeat-suppressed); this
    /// is the steady per-tick "is the fault present right now" signal.
    fn is_confirmed(&self) -> bool {
        self.fired
    }
}

/// HLA-10 %-form resolver: the effective degraded-hashrate alert floor (GH/s).
///
/// A configured percent (`pct > 0`) of a known nominal (`nominal_ghs > 0`) takes
/// PRECEDENCE and yields `pct/100 * nominal_ghs`; otherwise the absolute floor
/// (`abs_floor_ghs`, which may itself be `0.0` = disabled) applies. Pure +
/// host-testable; the daemon calls this at the publisher to feed the existing
/// `MiningHealthSnapshot.degraded_floor_ghs` (so the monitor logic is unchanged).
pub fn effective_degraded_floor_ghs(pct: f64, abs_floor_ghs: f64, nominal_ghs: f64) -> f64 {
    if pct > 0.0 && pct.is_finite() && nominal_ghs > 0.0 && nominal_ghs.is_finite() {
        (pct / 100.0) * nominal_ghs
    } else {
        abs_floor_ghs
    }
}

/// Per-chain health input to [`MiningAlertMonitor::evaluate`].
#[derive(Debug, Clone)]
pub struct ChainHealth {
    pub chain_id: u8,
    /// Number of enumerated/responding chips on the chain.
    pub chips: u8,
    /// Chain hashrate in GH/s.
    pub hashrate_ghs: f64,
}

/// A single 1 Hz snapshot of mining health fed to the monitor.
#[derive(Debug, Clone)]
pub struct MiningHealthSnapshot {
    /// Whether `[mining].enabled` is set (false on management-only boots).
    pub mining_enabled: bool,
    /// The pool connection status string from `MinerState.pool.status`
    /// ("Disconnected" / "Connecting" / "Authorized" / "Alive" / "Donating").
    pub pool_status: String,
    /// Pool URL for the `PoolDisconnected { url }` payload.
    pub pool_url: String,
    /// Total miner hashrate in GH/s.
    pub total_hashrate_ghs: f64,
    /// HLA-10: operator's degraded-hashrate alert floor in GH/s
    /// (`[mining].degraded_hashrate_alert_floor_ghs`). `0.0` = disabled.
    pub degraded_floor_ghs: f64,
    /// Per-chain health.
    pub chains: Vec<ChainHealth>,
}

/// Classify a `MinerState.pool.status` string as an established connection.
fn pool_status_is_connected(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "authorized" | "alive" | "mining" | "donating" | "connected"
    )
}

/// Classify a `MinerState.pool.status` string as a lost connection.
fn pool_status_is_disconnected(status: &str) -> bool {
    let s = status.trim();
    s.eq_ignore_ascii_case("disconnected") || s.eq_ignore_ascii_case("dead")
}

/// Constructs and debounces the three home-operator mining-health alerts.
///
/// Stateful and single-threaded: the daemon owns one instance inside the
/// state-publisher task and calls [`evaluate`](Self::evaluate) once per
/// second. Each returned [`AlertEvent`] is routed through the same
/// `mpsc::Sender<AlertEvent>` the thermal loop uses (webhook + browser
/// notification).
///
/// Anti-noise guarantees:
/// - `MiningStopped` only fires after real mining was observed at least once
///   this run (`ever_hashed`), so a cold-start ramp (enabled + 0 H/s before
///   the first nonce) never alerts — it is a true hashrate→0 transition.
/// - `PoolDisconnected` only fires after a real pool connection was
///   established (`pool_was_connected`), so initial connect attempts don't
///   alert.
/// - `HashBoardOffline` deliberately does NOT require ever-hashed: a board
///   that enumerates chips but never produces hashrate (dead-from-start) is
///   exactly the case to surface. It is gated on `mining_enabled` so
///   management-only units stay silent.
#[derive(Debug)]
pub struct MiningAlertMonitor {
    confirm_ticks: u32,
    repeat_suppress: Duration,
    ever_hashed: bool,
    /// HLA-10: set once the total hashrate has reached/exceeded the degraded
    /// floor at least once. Gates `HashrateDegraded` so a cold RAMP-UP that
    /// climbs through the sub-floor zone (e.g. 100 → 5000 → 26000 GH/s) never
    /// false-alerts — degradation means "fell below a level it once achieved".
    ever_at_or_above_floor: bool,
    pool_was_connected: bool,
    mining_stopped: EdgeDebounce,
    pool_disconnected: EdgeDebounce,
    hashboard_offline: HashMap<u8, EdgeDebounce>,
    hashrate_degraded: EdgeDebounce,
}

impl Default for MiningAlertMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MiningAlertMonitor {
    /// Construct with production debounce tuning (~30 s confirm at 1 Hz,
    /// 15 min repeat-suppress).
    pub fn new() -> Self {
        Self::with_tuning(ALERT_IDLE_CONFIRM_TICKS, ALERT_REPEAT_SUPPRESS)
    }

    /// Construct with explicit tuning (used by tests for fast confirmation).
    pub fn with_tuning(confirm_ticks: u32, repeat_suppress: Duration) -> Self {
        Self {
            confirm_ticks: confirm_ticks.max(1),
            repeat_suppress,
            ever_hashed: false,
            ever_at_or_above_floor: false,
            pool_was_connected: false,
            mining_stopped: EdgeDebounce::default(),
            pool_disconnected: EdgeDebounce::default(),
            hashboard_offline: HashMap::new(),
            hashrate_degraded: EdgeDebounce::default(),
        }
    }

    /// PH-3: whether the HashrateDegraded condition is currently confirmed
    /// (debounced + still active). The recovery ladder consumes this as its
    /// per-tick `degraded_confirmed` input so it inherits the 30-tick confirm and
    /// the `ever_at_or_above_floor` cold-ramp gate for free.
    pub fn hashrate_degraded_confirmed(&self) -> bool {
        self.hashrate_degraded.is_confirmed()
    }

    /// Evaluate one snapshot and return the alert events to dispatch.
    pub fn evaluate(&mut self, snap: &MiningHealthSnapshot, now: Instant) -> Vec<AlertEvent> {
        let mut events = Vec::new();

        if snap.total_hashrate_ghs > HASHRATE_IDLE_EPSILON_GHS {
            self.ever_hashed = true;
        }
        let connected = pool_status_is_connected(&snap.pool_status);
        let disconnected = pool_status_is_disconnected(&snap.pool_status);
        if connected {
            self.pool_was_connected = true;
        }

        // ---- PoolDisconnected ----
        let pool_cond = snap.mining_enabled && self.pool_was_connected && disconnected;
        if self
            .pool_disconnected
            .update(pool_cond, self.confirm_ticks, self.repeat_suppress, now)
        {
            events.push(AlertEvent::PoolDisconnected {
                url: snap.pool_url.clone(),
            });
        }

        // ---- MiningStopped ----
        let mining_cond = snap.mining_enabled
            && self.ever_hashed
            && snap.total_hashrate_ghs <= HASHRATE_IDLE_EPSILON_GHS;
        if self
            .mining_stopped
            .update(mining_cond, self.confirm_ticks, self.repeat_suppress, now)
        {
            let reason = if disconnected {
                "hashrate fell to zero while mining is enabled (pool disconnected)".to_string()
            } else {
                "hashrate fell to zero while mining is enabled".to_string()
            };
            events.push(AlertEvent::MiningStopped { reason });
        }

        // ---- HashBoardOffline (per chain: powered/enumerated but not hashing) ----
        for chain in &snap.chains {
            let cond = snap.mining_enabled
                && chain.chips > 0
                && chain.hashrate_ghs <= HASHRATE_IDLE_EPSILON_GHS;
            let entry = self.hashboard_offline.entry(chain.chain_id).or_default();
            if entry.update(cond, self.confirm_ticks, self.repeat_suppress, now) {
                events.push(AlertEvent::HashBoardOffline {
                    chain_id: chain.chain_id,
                });
            }
        }

        // ---- HashrateDegraded (HLA-10, detection-only) ----
        // Fires when the operator set a floor (>0), the miner has ONCE reached
        // that floor (so a cold ramp-up climbing through the sub-floor zone never
        // false-alerts), and the total hashrate has since stayed BELOW the floor
        // but ABOVE the idle epsilon — a true degradation, NOT a full stop
        // (MiningStopped owns the hashrate->0 case; the `> EPSILON` guard keeps
        // the two mutually exclusive so a stopped miner never double-alerts).
        // Alert only; no hardware action. Disabled (floor == 0) costs nothing.
        if snap.degraded_floor_ghs > 0.0 && snap.total_hashrate_ghs >= snap.degraded_floor_ghs {
            self.ever_at_or_above_floor = true;
        }
        let degraded_cond = snap.mining_enabled
            && self.ever_at_or_above_floor
            && snap.degraded_floor_ghs > 0.0
            && snap.total_hashrate_ghs > HASHRATE_IDLE_EPSILON_GHS
            && snap.total_hashrate_ghs < snap.degraded_floor_ghs;
        if self.hashrate_degraded.update(
            degraded_cond,
            self.confirm_ticks,
            self.repeat_suppress,
            now,
        ) {
            events.push(AlertEvent::HashrateDegraded {
                observed_ghs: snap.total_hashrate_ghs,
                floor_ghs: snap.degraded_floor_ghs,
            });
        }

        events
    }
}

#[cfg(test)]
mod mining_alert_tests {
    use super::*;

    fn snap(
        mining_enabled: bool,
        pool_status: &str,
        total_ghs: f64,
        chains: Vec<(u8, u8, f64)>,
    ) -> MiningHealthSnapshot {
        MiningHealthSnapshot {
            mining_enabled,
            pool_status: pool_status.to_string(),
            pool_url: "stratum+tcp://public-pool.io:21496".to_string(),
            total_hashrate_ghs: total_ghs,
            // Degraded-hashrate alert disabled by default in the shared helper,
            // so existing tests are unaffected. The HLA-10 tests set it explicitly.
            degraded_floor_ghs: 0.0,
            chains: chains
                .into_iter()
                .map(|(chain_id, chips, hashrate_ghs)| ChainHealth {
                    chain_id,
                    chips,
                    hashrate_ghs,
                })
                .collect(),
        }
    }

    /// Snapshot with the HLA-10 degraded-hashrate floor set (mining enabled,
    /// pool authorized). Used by the HashrateDegraded tests.
    fn snap_floor(total_ghs: f64, floor_ghs: f64) -> MiningHealthSnapshot {
        let mut s = snap(true, "Authorized", total_ghs, vec![]);
        s.degraded_floor_ghs = floor_ghs;
        s
    }

    fn names(events: &[AlertEvent]) -> Vec<&'static str> {
        events.iter().map(|e| e.event_name()).collect()
    }

    #[test]
    fn cold_start_idle_does_not_alert_mining_stopped() {
        // Mining enabled but never produced hashrate yet (ramp). Must stay silent.
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..10u64 {
            let ev = m.evaluate(
                &snap(true, "Connecting", 0.0, vec![]),
                t0 + Duration::from_secs(i),
            );
            assert!(ev.is_empty(), "no alert expected during cold-start ramp");
        }
    }

    #[test]
    fn mining_stopped_fires_once_after_hashing_then_zero() {
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(3600));
        let t0 = Instant::now();
        // Was mining (arms ever_hashed).
        assert!(m
            .evaluate(&snap(true, "Alive", 13_000.0, vec![]), t0)
            .is_empty());
        // Hashrate collapses to zero. Confirm window = 2 ticks.
        let e1 = m.evaluate(
            &snap(true, "Alive", 0.0, vec![]),
            t0 + Duration::from_secs(1),
        );
        assert!(e1.is_empty(), "first idle tick is within confirm window");
        let e2 = m.evaluate(
            &snap(true, "Alive", 0.0, vec![]),
            t0 + Duration::from_secs(2),
        );
        assert_eq!(names(&e2), vec!["mining_stopped"]);
        match &e2[0] {
            AlertEvent::MiningStopped { reason } => assert!(reason.contains("zero")),
            other => panic!("expected MiningStopped, got {other:?}"),
        }
        // Persisting condition must not re-fire (dedup).
        let e3 = m.evaluate(
            &snap(true, "Alive", 0.0, vec![]),
            t0 + Duration::from_secs(3),
        );
        assert!(e3.is_empty(), "persistent idle must not re-alert");
    }

    #[test]
    fn hashboard_offline_fires_for_powered_but_idle_chain() {
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(3600));
        let t0 = Instant::now();
        // Chain 7 has chips but produces no hashrate; chain 6 is healthy.
        let s = |i: u64| {
            (
                snap(true, "Alive", 6_000.0, vec![(6, 63, 6_000.0), (7, 63, 0.0)]),
                t0 + Duration::from_secs(i),
            )
        };
        let (s0, n0) = s(0);
        assert!(m.evaluate(&s0, n0).is_empty());
        let (s1, n1) = s(1);
        let e = m.evaluate(&s1, n1);
        assert_eq!(names(&e), vec!["hashboard_offline"]);
        match &e[0] {
            AlertEvent::HashBoardOffline { chain_id } => assert_eq!(*chain_id, 7),
            other => panic!("expected HashBoardOffline, got {other:?}"),
        }
        // Recovery re-arms; no event while healthy.
        let healthy = snap(
            true,
            "Alive",
            12_000.0,
            vec![(6, 63, 6_000.0), (7, 63, 6_000.0)],
        );
        assert!(m.evaluate(&healthy, t0 + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn pool_disconnected_fires_only_after_real_connection() {
        let mut m = MiningAlertMonitor::with_tuning(1, Duration::from_secs(3600));
        let t0 = Instant::now();
        // Disconnected before ever connecting → silent (initial connect attempts).
        assert!(m
            .evaluate(&snap(true, "Disconnected", 0.0, vec![]), t0)
            .is_empty());
        // Establish a real connection.
        assert!(m
            .evaluate(
                &snap(true, "Authorized", 0.0, vec![]),
                t0 + Duration::from_secs(1)
            )
            .is_empty());
        // Now a disconnect fires (confirm = 1 tick).
        let e = m.evaluate(
            &snap(true, "Disconnected", 0.0, vec![]),
            t0 + Duration::from_secs(2),
        );
        assert_eq!(names(&e), vec!["pool_disconnected"]);
        match &e[0] {
            AlertEvent::PoolDisconnected { url } => assert!(url.contains("public-pool.io")),
            other => panic!("expected PoolDisconnected, got {other:?}"),
        }
    }

    #[test]
    fn management_only_unit_stays_silent() {
        // mining disabled: no PoolDisconnected/MiningStopped/HashBoardOffline.
        let mut m = MiningAlertMonitor::with_tuning(1, Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..5u64 {
            let ev = m.evaluate(
                &snap(false, "Disconnected", 0.0, vec![(6, 63, 0.0)]),
                t0 + Duration::from_secs(i),
            );
            assert!(ev.is_empty(), "management-only unit must not alert");
        }
    }

    // ---- HLA-10 HashrateDegraded (detection-only) ----------------------------

    #[test]
    fn hashrate_degraded_fires_after_sustained_below_floor() {
        let mut m = MiningAlertMonitor::with_tuning(3, Duration::from_secs(1));
        let t0 = Instant::now();
        // Establish ever_hashed with a healthy reading above the floor.
        let _ = m.evaluate(&snap_floor(20_000.0, 10_000.0), t0);
        // Now degrade: 5 TH/s < 10 TH/s floor, but well above the idle epsilon.
        let mut fired = None;
        for i in 1..=4u64 {
            let ev = m.evaluate(&snap_floor(5_000.0, 10_000.0), t0 + Duration::from_secs(i));
            if names(&ev).contains(&"hashrate_degraded") {
                fired = Some(i);
                if let AlertEvent::HashrateDegraded {
                    observed_ghs,
                    floor_ghs,
                } = &ev[0]
                {
                    assert_eq!(*observed_ghs, 5_000.0);
                    assert_eq!(*floor_ghs, 10_000.0);
                }
                break;
            }
        }
        assert!(
            fired.is_some(),
            "degraded must fire after the sustained confirm window"
        );
    }

    #[test]
    fn hashrate_degraded_disabled_when_floor_is_zero() {
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(1));
        let t0 = Instant::now();
        let _ = m.evaluate(&snap_floor(20_000.0, 0.0), t0);
        for i in 1..=6u64 {
            let ev = m.evaluate(&snap_floor(1.0, 0.0), t0 + Duration::from_secs(i));
            assert!(
                !names(&ev).contains(&"hashrate_degraded"),
                "floor==0 (disabled) must never emit hashrate_degraded"
            );
        }
    }

    #[test]
    fn hashrate_degraded_does_not_fire_on_full_stop() {
        // At/below the idle epsilon, MiningStopped owns it -- degraded must NOT
        // also fire (the two are mutually exclusive by the > EPSILON guard).
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(1));
        let t0 = Instant::now();
        let _ = m.evaluate(&snap_floor(20_000.0, 10_000.0), t0);
        for i in 1..=6u64 {
            let ev = m.evaluate(
                &snap_floor(HASHRATE_IDLE_EPSILON_GHS / 2.0, 10_000.0),
                t0 + Duration::from_secs(i),
            );
            assert!(
                !names(&ev).contains(&"hashrate_degraded"),
                "a full stop (<= idle epsilon) must be MiningStopped, not HashrateDegraded"
            );
        }
    }

    #[test]
    fn effective_floor_prefers_pct_of_nominal_over_absolute() {
        // %-form active: 80% of 26 TH/s nominal = 20.8 TH/s, regardless of abs.
        assert_eq!(
            effective_degraded_floor_ghs(80.0, 5_000.0, 26_000.0),
            20_800.0
        );
        // pct == 0 → fall back to the absolute floor.
        assert_eq!(
            effective_degraded_floor_ghs(0.0, 5_000.0, 26_000.0),
            5_000.0
        );
        // pct set but nominal unknown (0) → fall back to the absolute floor.
        assert_eq!(effective_degraded_floor_ghs(80.0, 5_000.0, 0.0), 5_000.0);
        // both disabled → 0 (disabled).
        assert_eq!(effective_degraded_floor_ghs(0.0, 0.0, 26_000.0), 0.0);
        // non-finite pct/nominal are ignored (fall back to abs floor).
        assert_eq!(
            effective_degraded_floor_ghs(f64::NAN, 5_000.0, 26_000.0),
            5_000.0
        );
    }

    #[test]
    fn hashrate_degraded_silent_during_rampup_that_never_reached_floor() {
        // A cold ramp-up that hashes (100 GH/s) but has NOT yet climbed to the
        // floor must stay silent — degradation means "fell below a level it once
        // achieved", so the ever-at-or-above-floor gate suppresses ramp-up noise.
        let mut m = MiningAlertMonitor::with_tuning(2, Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..6u64 {
            let ev = m.evaluate(&snap_floor(100.0, 10_000.0), t0 + Duration::from_secs(i));
            assert!(
                !names(&ev).contains(&"hashrate_degraded"),
                "must not alert degraded while ramping up before ever reaching the floor"
            );
        }
    }
}

#[cfg(test)]
mod alert_mapping_tests {
    use super::*;
    use dcentrald_api::webhook::WebhookEvent;

    const LEAKY_POOL: &str =
        "stratum+tcp://bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6:x@public-pool.io:21496";
    const WALLET: &str = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";

    #[test]
    fn redact_alert_event_closes_pool_disconnected_url_leak() {
        // The historical inline loop serialized this RAW — the wallet rode the
        // webhook body. Redaction reduces it to host[:port].
        let mut ev = AlertEvent::PoolDisconnected {
            url: LEAKY_POOL.to_string(),
        };
        redact_alert_event(&mut ev);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains(WALLET), "wallet must not survive: {json}");
        assert!(json.contains("public-pool.io:21496"), "{json}");
    }

    #[test]
    fn redact_alert_event_masks_wallet_in_mining_stopped_reason() {
        let mut ev = AlertEvent::MiningStopped {
            reason: format!("stopped for worker {WALLET}"),
        };
        redact_alert_event(&mut ev);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains(WALLET), "{json}");
    }

    #[test]
    fn redact_alert_event_is_noop_for_non_secret_variants() {
        let mut ev = AlertEvent::EmergencyShutdown {
            temp_c: 75.0,
            chain_id: 1,
        };
        let before = serde_json::to_string(&ev).unwrap();
        redact_alert_event(&mut ev);
        assert_eq!(before, serde_json::to_string(&ev).unwrap());
    }

    #[test]
    fn mapping_is_total_and_preserves_event_name_for_filtering() {
        // Every AlertEvent maps to a WebhookEvent. The inline loop filters on the
        // AlertEvent name (operator allow-list contract), and the mapped
        // WebhookEvent name must match for these so a future dispatcher-side
        // allow-list stays consistent. (EmergencyShutdown is the one intentional
        // rename → thermal_safety; its Generic envelope still uses the AlertEvent.)
        let samples = [
            (
                AlertEvent::EmergencyShutdown {
                    temp_c: 72.0,
                    chain_id: 2,
                },
                "thermal_safety",
            ),
            (AlertEvent::FanFailure { rpm: 0 }, "fan_failure"),
            (
                AlertEvent::PoolDisconnected {
                    url: "pool:3333".to_string(),
                },
                "pool_disconnected",
            ),
            (
                AlertEvent::MiningStopped {
                    reason: "x".to_string(),
                },
                "mining_stopped",
            ),
            (
                AlertEvent::HashBoardOffline { chain_id: 1 },
                "hashboard_offline",
            ),
            (AlertEvent::ThermalRestart, "thermal_restart"),
            (
                AlertEvent::HashrateDegraded {
                    observed_ghs: 1.0,
                    floor_ghs: 2.0,
                },
                "hashrate_degraded",
            ),
            (
                AlertEvent::HashrateRecoveryExhausted {
                    observed_ghs: 1.0,
                    floor_ghs: 2.0,
                    attempts: 3,
                },
                "hashrate_recovery_exhausted",
            ),
        ];
        for (alert, expect_webhook_name) in samples {
            let mapped = alert_event_to_webhook_event(&alert);
            assert_eq!(mapped.event_name(), expect_webhook_name);
        }
    }

    #[test]
    fn mapping_then_redact_yields_clean_webhook_event() {
        // The path the inline loop takes for Discord/Slack/Telegram: redact the
        // AlertEvent, map it, then WebhookEvent::redact() (idempotent).
        let mut alert = AlertEvent::PoolDisconnected {
            url: LEAKY_POOL.to_string(),
        };
        redact_alert_event(&mut alert);
        let mut webhook = alert_event_to_webhook_event(&alert);
        webhook.redact();
        match &webhook {
            WebhookEvent::PoolDisconnected { pool } => {
                assert!(!pool.contains(WALLET));
                assert_eq!(pool, "public-pool.io:21496");
            }
            other => panic!("expected PoolDisconnected, got {other:?}"),
        }
    }

    #[test]
    fn mapping_fields_are_preserved() {
        let mapped = alert_event_to_webhook_event(&AlertEvent::HashrateRecoveryExhausted {
            observed_ghs: 5_000.0,
            floor_ghs: 10_000.0,
            attempts: 4,
        });
        assert_eq!(
            mapped,
            WebhookEvent::HashrateRecoveryExhausted {
                observed_ghs: 5_000.0,
                floor_ghs: 10_000.0,
                attempts: 4,
            }
        );
    }
}

#[cfg(test)]
mod notification_stack_tests {
    use super::*;

    fn default_off_config() -> RuntimeNotificationConfig {
        // The shipped default: MQTT + webhook both disabled.
        RuntimeNotificationConfig {
            webhook: RuntimeWebhookConfig {
                enabled: false,
                url: String::new(),
                events: Vec::new(),
                format: dcentrald_api::webhook::WebhookFormat::Generic,
                telegram_bot_token: String::new(),
                telegram_chat_id: String::new(),
            },
            mqtt: RuntimeMqttConfig {
                enabled: false,
                broker: "mqtt://localhost:1883".to_string(),
                topic_prefix: "dcentrald".to_string(),
                discovery: true,
                username: None,
                password: None,
                publish_interval_s: 5,
            },
        }
    }

    /// P1-4 (Omega): the shared notification entrypoint MUST be reachable from a
    /// NON-S9 mode. A non-`Daemon` mode (proxy / hybrid / am3-bb / serial-idle /
    /// stock-idle) reaches it through `runtime::api::spawn_proxy_mode_api` with
    /// only the two broadcast senders a minimal `AppState` exposes (`stats_tx`,
    /// `mining_sync_tx`) plus a cancellation token — exactly the inputs this
    /// test supplies. It wires the mining-sync bridge onto the event bus even
    /// when MQTT + webhook are default-OFF (no Daemon, no hardware, no network).
    #[tokio::test]
    async fn shared_entrypoint_reachable_from_non_daemon_channels() {
        // Mirror the exact channels a minimal (proxy/hybrid) AppState builds.
        let (stats_tx, stats_rx) = broadcast::channel::<String>(64);
        let (mining_sync_tx, mining_sync_rx) = broadcast::channel::<String>(256);
        // Drop the seed receivers so receiver_count reflects only what the
        // shared entrypoint subscribes.
        drop(stats_rx);
        drop(mining_sync_rx);
        let shutdown = CancellationToken::new();

        // Reachable with no Daemon — just channels + token + reload_path=None
        // (the transient /tmp bring-up parity the proxy/hybrid path uses).
        spawn_notification_stack(
            default_off_config(),
            None,
            "02:00:00:00:00:01".to_string(),
            "test-miner".to_string(),
            stats_tx.clone(),
            mining_sync_tx.clone(),
            shutdown.clone(),
            None,
        );

        // The mining-sync bridge subscribes synchronously inside the entrypoint
        // (the `.subscribe()` is evaluated before the bridge task is spawned),
        // so the event bus has the bridge as a live consumer immediately. This
        // is the wiring the pre-P1-4 non-S9 modes lacked entirely.
        assert!(
            mining_sync_tx.receiver_count() >= 1,
            "mining-sync bridge must be subscribed to the non-Daemon event bus"
        );

        // Publishing to the bus must not block or panic the caller even when
        // the dispatcher is default-OFF (it drops the event before any I/O).
        // A non-JSON payload is tolerated (translate returns None).
        let _ = mining_sync_tx.send("ping".to_string());

        // The stack winds down promptly on shutdown (no hang, no panic).
        shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
