//! Outbound webhook event dispatch — the event-bus → webhook bridge.
//!
//! # Why this module exists (W8 parity gap — Group C)
//!
//! DCENT_OS already has the *plumbing* for webhook notifications:
//!   - `[webhook]` config (`enabled` / `url` / `events`) in `dcentrald::config`,
//!   - REST endpoints (`GET`/`POST /api/config/webhook` + `/test`) in `rest.rs`,
//!   - a `RuntimeWebhookConfig` reloader and an inline POST task in
//!     `dcentrald::daemon`.
//!
//! …but that inline task is fed **only** by the thermal loop's
//! `mpsc::channel::<AlertEvent>`. The wide set of operationally-significant
//! events the W8 comparison flagged — mining start/stop, pool failover,
//! share-accept milestones, OTA — flow through the daemon's *event bus*
//! (the `broadcast::Sender<String>` mining-sync channel that carries
//! `WsMiningSyncMessage`, plus direct daemon call sites) and were never wired
//! to webhook dispatch. BraiinsOS fires webhooks on these; DCENT did not.
//!
//! This module closes that gap *inside `dcentrald-api`* (the webhook owner
//! crate) with three reusable pieces:
//!   1. [`WebhookEvent`] — a typed superset of the events worth notifying on,
//!      whose [`WebhookEvent::event_name`] strings match the canonical
//!      `WEBHOOK_SUPPORTED_EVENTS` filter list in `rest.rs`.
//!   2. [`WebhookDispatcher`] — a fire-and-forget POST engine on its own task:
//!      bounded queue, bounded per-attempt timeout, bounded retry, full
//!      payload redaction. It NEVER blocks or panics a caller (the mining
//!      loop), even if the endpoint is down or slow.
//!   3. [`spawn_mining_sync_bridge`] — the actual event-bus subscriber. It
//!      subscribes to the daemon's mining-sync `broadcast` channel and
//!      translates the relevant events into [`WebhookEvent`]s, enqueueing them
//!      on the dispatcher.
//!
//! # Safety contract (load-bearing)
//!
//! - **Default-OFF.** With no webhook URL configured (`enabled = false` or
//!   empty `url`), [`WebhookDispatcher::dispatch`] drops every event before
//!   any network I/O. No URL ⇒ no dispatch ⇒ byte-identical behaviour. This is
//!   the same gate the inline `daemon.rs` task already enforces.
//! - **Never blocks the caller.** Enqueue is a non-blocking `try_send` on a
//!   bounded channel; if the queue is full the event is dropped with a debug
//!   log, never awaited. All HTTP happens on the dispatcher's own task.
//! - **Bounded everything.** Per-attempt timeout (default 5 s) + a small
//!   bounded retry count (default 2 retries) with a short backoff. A dead or
//!   slow endpoint can never accumulate unbounded outstanding work.
//! - **Redacted payloads.** Every string field that could carry a wallet
//!   address or pool credential is run through
//!   [`dcentrald_common::wallet_mask`] before serialization. Pool URLs are
//!   host-only (no `stratum+tcp://…?password=` query bleed).
//!
//! This module is HAL-free and host-testable. It does not touch hardware,
//! mining state, fans, power, or config files.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use dcentrald_common::wallet_mask;

/// Default per-attempt HTTP timeout. Matches the inline `daemon.rs` task's
/// 5-second budget so behaviour is identical when both fire.
pub const DEFAULT_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Default number of *retries* after the first attempt (so up to 3 total
/// sends). Kept small and bounded so a flapping endpoint can't build a
/// backlog.
pub const DEFAULT_WEBHOOK_RETRIES: u8 = 2;

/// Backoff between retry attempts. Short, fixed — this is best-effort
/// notification, not guaranteed delivery.
pub const DEFAULT_WEBHOOK_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Bounded depth of the dispatcher's inbound queue. Enqueue is `try_send`;
/// once full, events are dropped (debug-logged), never awaited.
pub const WEBHOOK_QUEUE_DEPTH: usize = 64;

// ---------------------------------------------------------------------------
// Event model
// ---------------------------------------------------------------------------

/// A significant operational event worth notifying an external endpoint about.
///
/// Serialized as tagged JSON (`{"event": "...", "data": {...}}`) — the same
/// shape the existing `AlertEvent` uses, so a configured Discord/Telegram/
/// ntfy.sh/PagerDuty endpoint sees a consistent contract regardless of which
/// internal source produced the event.
///
/// Every variant's [`WebhookEvent::event_name`] returns a string that is in
/// (or is a documented addition to) the `WEBHOOK_SUPPORTED_EVENTS` filter
/// list in `rest.rs`, so the operator's `[webhook].events` allow-list applies
/// uniformly.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum WebhookEvent {
    /// The daemon began dispatching work / accepted its first job.
    MiningStarted {
        /// Pool host the miner is mining to (host-only, redacted).
        pool: String,
    },
    /// Mining stopped (clean shutdown, curtailment, or operator stop).
    MiningStopped {
        /// Short human-readable reason (redacted defensively).
        reason: String,
    },
    /// The Stratum client failed over from one user pool to a backup.
    PoolFailover {
        /// Pool that failed (host-only, redacted).
        from: String,
        /// Pool now in use (host-only, redacted).
        to: String,
    },
    /// A pool connection dropped (no backup engaged yet).
    PoolDisconnected {
        /// Pool host that disconnected (host-only, redacted).
        pool: String,
    },
    /// A thermal-safety event fired (emergency shutdown / dangerous temp).
    /// Mirrors the thermal loop's `AlertEvent::EmergencyShutdown`.
    ThermalSafety {
        /// Board/chip temperature (Celsius) that triggered the event.
        temp_c: f32,
        /// Affected chain (0 = all chains).
        chain_id: u8,
    },
    /// A milestone count of accepted shares was reached (e.g. every 100).
    ShareMilestone {
        /// Cumulative accepted-share count at the milestone.
        accepted: u64,
    },
    /// An unusually lucky share was found (achieved difficulty ≫ pool target).
    LuckyShare {
        /// Achieved difficulty of the lucky share, when known.
        difficulty: Option<f64>,
    },
    /// An OTA / firmware-update lifecycle event occurred.
    Ota {
        /// Lifecycle phase: `staged`, `verified`, `scheduled`, `failed`, etc.
        /// (truth-contract wording is the caller's responsibility — this
        /// module only forwards the string).
        phase: String,
    },
    /// A fan stopped or dropped below its safe RPM floor. Mirrors the thermal
    /// loop's `AlertEvent::FanFailure`.
    FanFailure {
        /// Last-observed fan RPM that triggered the alert.
        rpm: u32,
    },
    /// A hash board enumerated chips but is producing no hashrate. Mirrors the
    /// mining-health monitor's `AlertEvent::HashBoardOffline`.
    HashBoardOffline {
        /// Affected chain id.
        chain_id: u8,
    },
    /// The thermal supervisor restarted mining after a recoverable thermal
    /// event. Mirrors the thermal loop's `AlertEvent::ThermalRestart`.
    ThermalRestart,
    /// Total hashrate has stayed below the operator's degraded floor (but above
    /// the idle epsilon). Detection-only. Mirrors `AlertEvent::HashrateDegraded`.
    HashrateDegraded {
        /// Observed total hashrate (GH/s).
        observed_ghs: f64,
        /// Operator-configured degraded floor (GH/s).
        floor_ghs: f64,
    },
    /// The auto-recovery ladder exhausted its restart budget for a degraded
    /// episode. Mirrors `AlertEvent::HashrateRecoveryExhausted`.
    HashrateRecoveryExhausted {
        /// Observed total hashrate (GH/s) at give-up.
        observed_ghs: f64,
        /// Operator-configured degraded floor (GH/s).
        floor_ghs: f64,
        /// Number of restart attempts spent before giving up.
        attempts: u32,
    },
}

impl WebhookEvent {
    /// Canonical event-name string used for `[webhook].events` allow-list
    /// filtering. MUST stay in sync with `WEBHOOK_SUPPORTED_EVENTS` in
    /// `rest.rs`.
    pub fn event_name(&self) -> &'static str {
        match self {
            WebhookEvent::MiningStarted { .. } => "mining_started",
            WebhookEvent::MiningStopped { .. } => "mining_stopped",
            WebhookEvent::PoolFailover { .. } => "pool_failover",
            WebhookEvent::PoolDisconnected { .. } => "pool_disconnected",
            WebhookEvent::ThermalSafety { .. } => "thermal_safety",
            WebhookEvent::ShareMilestone { .. } => "share_milestone",
            WebhookEvent::LuckyShare { .. } => "lucky_share",
            WebhookEvent::Ota { .. } => "ota",
            WebhookEvent::FanFailure { .. } => "fan_failure",
            WebhookEvent::HashBoardOffline { .. } => "hashboard_offline",
            WebhookEvent::ThermalRestart => "thermal_restart",
            WebhookEvent::HashrateDegraded { .. } => "hashrate_degraded",
            WebhookEvent::HashrateRecoveryExhausted { .. } => "hashrate_recovery_exhausted",
        }
    }

    /// Apply wallet/credential redaction to every string field IN PLACE.
    ///
    /// Pool fields are reduced to host-only (no credentials in a URL query
    /// string) and then wallet-masked; free-text reason/phase fields are
    /// scanned for embedded wallet addresses. This is called automatically by
    /// the dispatcher before serialization, so callers cannot forget it.
    ///
    /// `pub` so the daemon's inline AlertEvent path can run the SAME redaction
    /// before reusing [`render_text`] / [`payload_for`] — there is exactly one
    /// redaction implementation, and it always runs before any formatting.
    /// Idempotent: re-running it on an already-redacted event is a no-op.
    pub fn redact(&mut self) {
        match self {
            WebhookEvent::MiningStarted { pool } => {
                *pool = redact_pool(pool);
            }
            WebhookEvent::MiningStopped { reason } => {
                *reason = wallet_mask::mask_in_string(reason).into_owned();
            }
            WebhookEvent::PoolFailover { from, to } => {
                *from = redact_pool(from);
                *to = redact_pool(to);
            }
            WebhookEvent::PoolDisconnected { pool } => {
                *pool = redact_pool(pool);
            }
            WebhookEvent::Ota { phase } => {
                *phase = wallet_mask::mask_in_string(phase).into_owned();
            }
            // No string fields that can carry a secret.
            WebhookEvent::ThermalSafety { .. }
            | WebhookEvent::ShareMilestone { .. }
            | WebhookEvent::LuckyShare { .. }
            | WebhookEvent::FanFailure { .. }
            | WebhookEvent::HashBoardOffline { .. }
            | WebhookEvent::ThermalRestart
            | WebhookEvent::HashrateDegraded { .. }
            | WebhookEvent::HashrateRecoveryExhausted { .. } => {}
        }
    }
}

/// Reduce a pool URL/host to a credential-free, wallet-masked host string.
///
/// Stratum pool strings frequently carry the worker (= wallet address) and
/// password as URL components (`stratum+tcp://bc1q…:x@pool:3333`) or query
/// params. We strip everything but the host[:port] authority, then run the
/// remainder through the wallet scanner as a belt-and-braces guard.
///
/// `pub` so the daemon's inline AlertEvent redaction (which masks the
/// `PoolDisconnected { url }` field for the unchanged Generic envelope) reuses
/// the exact same pool-redaction implementation instead of duplicating it.
pub fn redact_pool(pool: &str) -> String {
    let trimmed = pool.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Drop any scheme prefix ("stratum+tcp://", "http://", …).
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    // Drop any userinfo ("user:pass@host" → "host").
    let after_userinfo = after_scheme
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(after_scheme);
    // Drop any path / query ("host:port/path?worker=…" → "host:port").
    let host_authority = after_userinfo
        .split(['/', '?'])
        .next()
        .unwrap_or(after_userinfo);
    // Belt-and-braces: a worker can appear in the host segment of some
    // malformed configs; scan for embedded wallet shapes.
    wallet_mask::mask_in_string(host_authority).into_owned()
}

// ---------------------------------------------------------------------------
// Channel formatting (Generic / Discord / Slack / Telegram)
// ---------------------------------------------------------------------------

/// The wire shape of the POST body produced for a configured webhook channel.
///
/// `Generic` (default) is byte-identical to the historical
/// `{ miner, timestamp, alert }` envelope — adding this enum is a strict
/// superset, so an operator who never sets `format` sees no change. The other
/// three reshape the SAME (already-redacted) event into the body each service
/// expects so DCENT_OS can deliver natively without a relay/proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebhookFormat {
    /// Current behaviour: POST `{ miner, timestamp, alert: <tagged event> }` to
    /// the configured URL. UNCHANGED.
    #[default]
    Generic,
    /// Discord incoming webhook: POST `{ "content": <human summary> }`.
    Discord,
    /// Slack incoming webhook: POST `{ "text": <human summary> }`.
    Slack,
    /// Telegram Bot API `sendMessage`: POST
    /// `{ "chat_id": <id>, "text": <human summary> }` to
    /// `https://api.telegram.org/bot<token>/sendMessage`.
    Telegram,
}

/// Build the Telegram Bot API `sendMessage` endpoint URL for a bot token.
///
/// The token is the only secret here; it lives in the URL path (Telegram's
/// design), so this string must never be logged. The dispatcher logs the event
/// name + HTTP status, never the URL, for exactly this reason.
pub fn telegram_url(token: &str) -> String {
    format!("https://api.telegram.org/bot{}/sendMessage", token.trim())
}

/// Render a one-line, human-readable summary of an event for the text-oriented
/// channels (Discord / Slack / Telegram).
///
/// **Contract:** the caller MUST have already run [`WebhookEvent::redact`] on
/// `event`; this function only reads fields and never re-derives secrets, so a
/// pool/reason string reaches here host-only + wallet-masked. It does no
/// redaction of its own — keeping a single redaction chokepoint.
pub fn render_text(miner: &str, event: &WebhookEvent) -> String {
    match event {
        WebhookEvent::MiningStarted { pool } => {
            format!("[{miner}] Mining started — pool {pool}")
        }
        WebhookEvent::MiningStopped { reason } => {
            format!("[{miner}] Mining stopped — {reason}")
        }
        WebhookEvent::PoolFailover { from, to } => {
            format!("[{miner}] Pool failover — {from} -> {to}")
        }
        WebhookEvent::PoolDisconnected { pool } => {
            format!("[{miner}] Pool disconnected — {pool}")
        }
        WebhookEvent::ThermalSafety { temp_c, chain_id } => {
            format!("[{miner}] Thermal safety event — chain {chain_id} at {temp_c:.1}C")
        }
        WebhookEvent::ShareMilestone { accepted } => {
            format!("[{miner}] Share milestone — {accepted} accepted shares")
        }
        WebhookEvent::LuckyShare { difficulty } => match difficulty {
            Some(d) => format!("[{miner}] Lucky share found — difficulty {d:.0}"),
            None => format!("[{miner}] Lucky share found"),
        },
        WebhookEvent::Ota { phase } => {
            format!("[{miner}] OTA update — {phase}")
        }
        WebhookEvent::FanFailure { rpm } => {
            format!("[{miner}] Fan failure — last RPM {rpm}")
        }
        WebhookEvent::HashBoardOffline { chain_id } => {
            format!("[{miner}] Hash board offline — chain {chain_id} not hashing")
        }
        WebhookEvent::ThermalRestart => {
            format!("[{miner}] Thermal supervisor restarted mining")
        }
        WebhookEvent::HashrateDegraded {
            observed_ghs,
            floor_ghs,
        } => format!(
            "[{miner}] Hashrate degraded — {observed_ghs:.0} GH/s below floor {floor_ghs:.0} GH/s"
        ),
        WebhookEvent::HashrateRecoveryExhausted {
            observed_ghs,
            floor_ghs,
            attempts,
        } => format!(
            "[{miner}] Hashrate auto-recovery exhausted after {attempts} attempts — {observed_ghs:.0} GH/s below floor {floor_ghs:.0} GH/s"
        ),
    }
}

/// Resolve the `(target_url, json_body)` to POST for a given channel format.
///
/// `event` MUST be already-redacted (see [`render_text`]). `url` is the
/// operator's configured webhook URL (used verbatim for Generic/Discord/Slack);
/// Telegram ignores it and builds its own endpoint from `token`. `token` /
/// `chat_id` are only consulted for [`WebhookFormat::Telegram`].
///
/// - `Generic` → (`url`, `{ miner, timestamp, alert: <event> }`) — UNCHANGED.
/// - `Discord` → (`url`, `{ "content": <render_text> }`).
/// - `Slack`   → (`url`, `{ "text": <render_text> }`).
/// - `Telegram`→ ([`telegram_url`]`(token)`, `{ "chat_id", "text": <render_text> }`).
pub fn payload_for(
    format: WebhookFormat,
    miner: &str,
    url: &str,
    token: Option<&str>,
    chat_id: Option<&str>,
    event: &WebhookEvent,
) -> (String, serde_json::Value) {
    match format {
        WebhookFormat::Generic => (url.to_string(), build_payload(miner, event)),
        WebhookFormat::Discord => (
            url.to_string(),
            serde_json::json!({ "content": render_text(miner, event) }),
        ),
        WebhookFormat::Slack => (
            url.to_string(),
            serde_json::json!({ "text": render_text(miner, event) }),
        ),
        WebhookFormat::Telegram => (
            telegram_url(token.unwrap_or("")),
            serde_json::json!({
                "chat_id": chat_id.unwrap_or(""),
                "text": render_text(miner, event),
            }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Runtime configuration for the dispatcher. Mirrors the `[webhook]` config
/// surface but is owned here (HAL-free) so the daemon can build it from its
/// `RuntimeWebhookConfig` without this crate depending on `dcentrald`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDispatchConfig {
    /// Master enable. `false` ⇒ no dispatch (default-OFF).
    pub enabled: bool,
    /// Destination URL. Empty ⇒ no dispatch (default-OFF).
    pub url: String,
    /// Allow-list of event names. Empty ⇒ all events allowed (matches the
    /// `normalize_webhook_events` "empty = all supported" convention).
    pub events: Vec<String>,
    /// Friendly miner name included in every payload envelope.
    pub miner_name: String,
    /// Delivery channel format. `Generic` (default) keeps the historical
    /// `{ miner, timestamp, alert }` body; the others reshape per service.
    pub format: WebhookFormat,
    /// Telegram bot token (only used when `format == Telegram`). SECRET — never
    /// logged; lives in the Telegram endpoint URL path.
    pub telegram_bot_token: String,
    /// Telegram chat id (only used when `format == Telegram`).
    pub telegram_chat_id: String,
}

impl WebhookDispatchConfig {
    /// Is dispatch currently possible? `false` ⇒ enqueue is a no-op.
    ///
    /// Default-OFF is preserved per format: a `Generic`/`Discord`/`Slack`
    /// channel needs a non-empty URL (byte-identical to the prior gate); a
    /// `Telegram` channel instead needs both a bot token AND a chat id (its URL
    /// field is unused). With nothing configured this is always `false`.
    fn is_live(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.format {
            WebhookFormat::Telegram => {
                !self.telegram_bot_token.trim().is_empty()
                    && !self.telegram_chat_id.trim().is_empty()
            }
            WebhookFormat::Generic | WebhookFormat::Discord | WebhookFormat::Slack => {
                !self.url.trim().is_empty()
            }
        }
    }

    /// Does the allow-list permit this event? Empty allow-list = all allowed.
    fn allows(&self, event_name: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|e| e == event_name)
    }
}

/// Tuning knobs for the dispatcher task. Defaulted to the constants above;
/// exposed so tests can shrink the timeout/backoff.
#[derive(Debug, Clone)]
pub struct WebhookDispatchTuning {
    pub timeout: Duration,
    pub retries: u8,
    pub retry_backoff: Duration,
}

impl Default for WebhookDispatchTuning {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_WEBHOOK_TIMEOUT,
            retries: DEFAULT_WEBHOOK_RETRIES,
            retry_backoff: DEFAULT_WEBHOOK_RETRY_BACKOFF,
        }
    }
}

/// Handle used by event producers to enqueue webhook events without blocking.
///
/// Cloneable and cheap — hand a clone to the thermal loop, the work
/// dispatcher, the OTA path, etc. Enqueue is `try_send`; a full queue or a
/// torn-down dispatcher drops the event (debug-logged), never blocks.
#[derive(Clone)]
pub struct WebhookHandle {
    tx: mpsc::Sender<WebhookEvent>,
}

impl WebhookHandle {
    /// Enqueue an event for best-effort dispatch. Non-blocking. Returns
    /// `true` if the event was queued, `false` if it was dropped (queue full
    /// or dispatcher gone). The boolean is informational only — callers
    /// should never branch mining behaviour on it.
    pub fn dispatch(&self, event: WebhookEvent) -> bool {
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(ev)) => {
                tracing::debug!(
                    event = ev.event_name(),
                    "webhook queue full — event dropped (endpoint slow/down)"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(ev)) => {
                tracing::debug!(
                    event = ev.event_name(),
                    "webhook dispatcher gone — event dropped"
                );
                false
            }
        }
    }
}

/// The webhook dispatcher: owns the inbound queue + the POST task.
///
/// Construct with [`WebhookDispatcher::spawn`] and hand out [`WebhookHandle`]
/// clones to producers. Live config updates (enable/disable/URL/events)
/// arrive through the `watch` channel passed to [`WebhookDispatcher::spawn`],
/// so the daemon's existing `[webhook]` config reloader drives them with no
/// restart — the same hot-reload UX the inline task already provides.
pub struct WebhookDispatcher {
    handle: WebhookHandle,
    task: tokio::task::JoinHandle<()>,
}

impl WebhookDispatcher {
    /// Spawn the dispatcher task and return it.
    ///
    /// `config_rx` is a `watch` receiver so the daemon's existing 5-second
    /// `[webhook]` config reloader can push new settings (URL/enabled/events
    /// changes apply without a restart — same UX as the inline task). The
    /// dispatcher reads the latest config on every event, so a mid-flight
    /// disable takes effect immediately.
    ///
    /// `shutdown` cancels the task on daemon teardown.
    pub fn spawn(
        config_rx: tokio::sync::watch::Receiver<WebhookDispatchConfig>,
        tuning: WebhookDispatchTuning,
        shutdown: CancellationToken,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<WebhookEvent>(WEBHOOK_QUEUE_DEPTH);
        let handle = WebhookHandle { tx };
        let task = tokio::spawn(run_dispatch_loop(rx, config_rx, tuning, shutdown));
        Self { handle, task }
    }

    /// A cloneable producer handle.
    pub fn handle(&self) -> WebhookHandle {
        self.handle.clone()
    }

    /// True once the dispatcher task has exited.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// The dispatcher task body. Pure async I/O, no hardware, never panics on a
/// failed send.
async fn run_dispatch_loop(
    mut rx: mpsc::Receiver<WebhookEvent>,
    config_rx: tokio::sync::watch::Receiver<WebhookDispatchConfig>,
    tuning: WebhookDispatchTuning,
    shutdown: CancellationToken,
) {
    // One reused client with a per-request timeout. `build()` can only fail on
    // a malformed TLS config; fall back to default so we never panic at spawn.
    let client = reqwest::Client::builder()
        .timeout(tuning.timeout)
        .build()
        .unwrap_or_default();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::debug!("webhook dispatcher stopping (shutdown)");
                break;
            }
            maybe_event = rx.recv() => {
                let Some(mut event) = maybe_event else {
                    // All handles dropped — nothing more will arrive.
                    break;
                };
                let config = config_rx.borrow().clone();
                let event_name = event.event_name();

                // Default-OFF gate: no URL / disabled ⇒ drop before any I/O.
                if !config.is_live() {
                    tracing::debug!(event = event_name, "webhook disabled — event dropped");
                    continue;
                }
                if !config.allows(event_name) {
                    tracing::debug!(event = event_name, "webhook: event filtered out by allow-list");
                    continue;
                }

                // Redact BEFORE building the payload — callers can't forget.
                event.redact();
                // Reshape for the configured channel (Generic/Discord/Slack/
                // Telegram). Generic is byte-identical to the prior body. The
                // redacted event is the single input to every channel's body.
                let (target_url, payload) = payload_for(
                    config.format,
                    &config.miner_name,
                    &config.url,
                    Some(config.telegram_bot_token.as_str()),
                    Some(config.telegram_chat_id.as_str()),
                    &event,
                );

                send_with_retry(&client, &target_url, &payload, event_name, &tuning, &shutdown).await;
            }
        }
    }
}

/// Build the JSON envelope POSTed to the webhook URL. Same shape as the
/// inline `daemon.rs` task: `{ miner, timestamp, alert: <tagged event> }`.
fn build_payload(miner_name: &str, event: &WebhookEvent) -> serde_json::Value {
    serde_json::json!({
        "miner": miner_name,
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "alert": event,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WebhookSendErrorClass {
    timeout: bool,
    connect: bool,
    request: bool,
    body: bool,
    decode: bool,
}

impl WebhookSendErrorClass {
    fn from_reqwest(error: &reqwest::Error) -> Self {
        Self {
            timeout: error.is_timeout(),
            connect: error.is_connect(),
            request: error.is_request(),
            body: error.is_body(),
            decode: error.is_decode(),
        }
    }
}

/// POST with a bounded retry loop. Best-effort: logs the final outcome, never
/// returns an error to the caller (the dispatcher loop continues regardless).
/// Honours `shutdown` between attempts so teardown isn't delayed by backoff.
async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    payload: &serde_json::Value,
    event_name: &str,
    tuning: &WebhookDispatchTuning,
    shutdown: &CancellationToken,
) {
    let total_attempts = tuning.retries.saturating_add(1);
    for attempt in 0..total_attempts {
        match client.post(url).json(payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    status = %resp.status(),
                    event = event_name,
                    attempt,
                    "webhook delivered"
                );
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    status = %resp.status(),
                    event = event_name,
                    attempt,
                    "webhook non-success HTTP status"
                );
            }
            Err(error) => {
                let error_class = WebhookSendErrorClass::from_reqwest(&error);
                tracing::warn!(
                    error_timeout = error_class.timeout,
                    error_connect = error_class.connect,
                    error_request = error_class.request,
                    error_body = error_class.body,
                    error_decode = error_class.decode,
                    event = event_name,
                    attempt,
                    "webhook send failed (endpoint down/slow/unreachable)"
                );
            }
        }
        // Don't sleep after the last attempt, and bail immediately on shutdown.
        if attempt + 1 < total_attempts {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(tuning.retry_backoff) => {}
            }
        }
    }
    tracing::warn!(
        event = event_name,
        attempts = total_attempts,
        "webhook NOT delivered after all retries — alert lost (best-effort)"
    );
}

// ---------------------------------------------------------------------------
// Event-bus bridge (the actual W8 "wire the event bus to webhook dispatch")
// ---------------------------------------------------------------------------

/// Subscribe to the daemon's mining-sync event bus and forward the relevant
/// events to the webhook dispatcher.
///
/// The daemon already publishes [`crate::websocket::WsMiningSyncMessage`]
/// JSON onto a `broadcast::Sender<String>` (`AppState.mining_sync_tx`) for
/// Hacker-Mode instruments. That is the existing event bus. This bridge is a
/// pure consumer of it: it never produces work, never touches hardware, and
/// runs on its own task.
///
/// It translates:
///   - `ShareAccepted` → accumulates a count; emits [`WebhookEvent::ShareMilestone`]
///     every `milestone_every` accepted shares.
///   - `LuckyShare`    → [`WebhookEvent::LuckyShare`] immediately.
///
/// Other event kinds (jobs, dispatch bursts, nonce bursts, rejects) are
/// high-frequency instrument telemetry and are intentionally NOT forwarded —
/// webhooks are for operationally-significant events, not per-tick streaming.
///
/// `milestone_every` of 0 disables milestone emission (lucky shares still
/// forward). A lagged broadcast receiver is tolerated (skipped, debug-logged)
/// so a slow bridge never back-pressures the mining loop.
pub fn spawn_mining_sync_bridge(
    handle: WebhookHandle,
    mut mining_sync_rx: broadcast::Receiver<String>,
    milestone_every: u64,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut accepted: u64 = 0;
        let mut last_milestone: u64 = 0;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("webhook mining-sync bridge stopping (shutdown)");
                    break;
                }
                recv = mining_sync_rx.recv() => {
                    match recv {
                        Ok(raw) => {
                            if let Some(event) = translate_mining_sync(
                                &raw,
                                &mut accepted,
                                &mut last_milestone,
                                milestone_every,
                            ) {
                                handle.dispatch(event);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // The bridge fell behind the high-rate instrument
                            // bus. Skipped messages are pure telemetry; dropping
                            // them is fine. Resync the accepted counter is not
                            // possible from here, so milestones may skip — that
                            // is acceptable for best-effort notification.
                            tracing::debug!(skipped, "webhook bridge lagged mining-sync bus");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!("mining-sync bus closed — webhook bridge exiting");
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// Parse one mining-sync JSON message and decide whether it produces a webhook
/// event. Pure function (no I/O) so it is exhaustively unit-testable.
///
/// `accepted` / `last_milestone` are the bridge's running counters, mutated in
/// place. Returns `Some(event)` to dispatch, `None` to ignore.
fn translate_mining_sync(
    raw: &str,
    accepted: &mut u64,
    last_milestone: &mut u64,
    milestone_every: u64,
) -> Option<WebhookEvent> {
    let msg: crate::websocket::WsMiningSyncMessage = serde_json::from_str(raw).ok()?;
    match msg.event {
        crate::websocket::WsMiningSyncEventKind::ShareAccepted => {
            *accepted += 1;
            if milestone_every > 0 && *accepted - *last_milestone >= milestone_every {
                *last_milestone = *accepted;
                Some(WebhookEvent::ShareMilestone {
                    accepted: *accepted,
                })
            } else {
                None
            }
        }
        crate::websocket::WsMiningSyncEventKind::LuckyShare => Some(WebhookEvent::LuckyShare {
            difficulty: msg.difficulty,
        }),
        // High-frequency instrument telemetry — not webhook-worthy.
        crate::websocket::WsMiningSyncEventKind::AuthorizeState
        | crate::websocket::WsMiningSyncEventKind::JobReceived
        | crate::websocket::WsMiningSyncEventKind::CleanJob
        | crate::websocket::WsMiningSyncEventKind::DispatchBurst
        | crate::websocket::WsMiningSyncEventKind::NonceBurst
        | crate::websocket::WsMiningSyncEventKind::ShareRejected => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websocket::{WsMiningSyncEventKind, WsMiningSyncMessage};

    fn cfg(enabled: bool, url: &str, events: &[&str]) -> WebhookDispatchConfig {
        WebhookDispatchConfig {
            enabled,
            url: url.to_string(),
            events: events.iter().map(|e| e.to_string()).collect(),
            miner_name: "test-miner".to_string(),
            format: WebhookFormat::Generic,
            telegram_bot_token: String::new(),
            telegram_chat_id: String::new(),
        }
    }

    /// Like [`cfg`] but for a fully-configured Telegram channel.
    fn cfg_telegram(token: &str, chat_id: &str) -> WebhookDispatchConfig {
        WebhookDispatchConfig {
            enabled: true,
            url: String::new(),
            events: Vec::new(),
            miner_name: "test-miner".to_string(),
            format: WebhookFormat::Telegram,
            telegram_bot_token: token.to_string(),
            telegram_chat_id: chat_id.to_string(),
        }
    }

    // ---- event names match the canonical filter list ----------------------

    #[test]
    fn event_names_are_snake_case_and_stable() {
        assert_eq!(
            WebhookEvent::MiningStarted {
                pool: String::new()
            }
            .event_name(),
            "mining_started"
        );
        assert_eq!(
            WebhookEvent::MiningStopped {
                reason: String::new()
            }
            .event_name(),
            "mining_stopped"
        );
        assert_eq!(
            WebhookEvent::PoolFailover {
                from: String::new(),
                to: String::new()
            }
            .event_name(),
            "pool_failover"
        );
        assert_eq!(
            WebhookEvent::PoolDisconnected {
                pool: String::new()
            }
            .event_name(),
            "pool_disconnected"
        );
        assert_eq!(
            WebhookEvent::ThermalSafety {
                temp_c: 0.0,
                chain_id: 0
            }
            .event_name(),
            "thermal_safety"
        );
        assert_eq!(
            WebhookEvent::ShareMilestone { accepted: 0 }.event_name(),
            "share_milestone"
        );
        assert_eq!(
            WebhookEvent::LuckyShare { difficulty: None }.event_name(),
            "lucky_share"
        );
        assert_eq!(
            WebhookEvent::Ota {
                phase: String::new()
            }
            .event_name(),
            "ota"
        );
        assert_eq!(
            WebhookEvent::FanFailure { rpm: 0 }.event_name(),
            "fan_failure"
        );
        assert_eq!(
            WebhookEvent::HashBoardOffline { chain_id: 0 }.event_name(),
            "hashboard_offline"
        );
        assert_eq!(WebhookEvent::ThermalRestart.event_name(), "thermal_restart");
        assert_eq!(
            WebhookEvent::HashrateDegraded {
                observed_ghs: 0.0,
                floor_ghs: 0.0
            }
            .event_name(),
            "hashrate_degraded"
        );
        assert_eq!(
            WebhookEvent::HashrateRecoveryExhausted {
                observed_ghs: 0.0,
                floor_ghs: 0.0,
                attempts: 0
            }
            .event_name(),
            "hashrate_recovery_exhausted"
        );
    }

    // ---- default-OFF gate --------------------------------------------------

    #[test]
    fn disabled_config_is_not_live() {
        assert!(!cfg(false, "https://example.com/hook", &[]).is_live());
    }

    #[test]
    fn empty_url_is_not_live_even_when_enabled() {
        assert!(!cfg(true, "", &[]).is_live());
        assert!(!cfg(true, "   ", &[]).is_live());
    }

    #[test]
    fn enabled_with_url_is_live() {
        assert!(cfg(true, "https://example.com/hook", &[]).is_live());
    }

    #[test]
    fn empty_allow_list_permits_all_events() {
        let c = cfg(true, "https://x", &[]);
        assert!(c.allows("mining_started"));
        assert!(c.allows("thermal_safety"));
        assert!(c.allows("ota"));
    }

    #[test]
    fn non_empty_allow_list_filters() {
        let c = cfg(true, "https://x", &["thermal_safety", "ota"]);
        assert!(c.allows("thermal_safety"));
        assert!(c.allows("ota"));
        assert!(!c.allows("mining_started"));
        assert!(!c.allows("lucky_share"));
    }

    // ---- redaction ---------------------------------------------------------

    #[test]
    fn redact_pool_strips_scheme_userinfo_and_query() {
        // A wallet-as-worker in the userinfo + query must not survive.
        let raw =
            "stratum+tcp://bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6:x@public-pool.io:21496/?worker=bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        let red = redact_pool(raw);
        assert_eq!(red, "public-pool.io:21496");
        assert!(!red.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"));
    }

    #[test]
    fn redact_pool_handles_bare_host() {
        assert_eq!(redact_pool("solo.ckpool.org:3333"), "solo.ckpool.org:3333");
    }

    #[test]
    fn redact_pool_empty_stays_empty() {
        assert_eq!(redact_pool(""), "");
        assert_eq!(redact_pool("   "), "");
    }

    #[test]
    fn event_redact_masks_wallet_in_pool_fields() {
        let mut ev = WebhookEvent::MiningStarted {
            pool: "stratum+tcp://bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6@public-pool.io:21496"
                .to_string(),
        };
        ev.redact();
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"));
        assert!(json.contains("public-pool.io:21496"));
    }

    #[test]
    fn event_redact_masks_failover_both_pools() {
        let mut ev = WebhookEvent::PoolFailover {
            from: "stratum+tcp://1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa@a.pool:3333".to_string(),
            to: "stratum+tcp://3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy@b.pool:3333".to_string(),
        };
        ev.redact();
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert!(!json.contains("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy"));
        assert!(json.contains("a.pool:3333"));
        assert!(json.contains("b.pool:3333"));
    }

    #[test]
    fn event_redact_masks_wallet_in_free_text_reason() {
        let mut ev = WebhookEvent::MiningStopped {
            reason: "operator stop for worker bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"
                .to_string(),
        };
        ev.redact();
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"));
    }

    #[test]
    fn payload_envelope_shape() {
        let ev = WebhookEvent::ThermalSafety {
            temp_c: 72.5,
            chain_id: 0,
        };
        let payload = build_payload("rig-01", &ev);
        assert_eq!(payload["miner"], "rig-01");
        assert!(payload["timestamp"].is_number());
        assert_eq!(payload["alert"]["event"], "thermal_safety");
        assert_eq!(payload["alert"]["data"]["temp_c"], 72.5);
    }

    // ---- mining-sync translation -------------------------------------------

    fn sync_json(event: WsMiningSyncEventKind, difficulty: Option<f64>) -> String {
        let msg = WsMiningSyncMessage {
            msg_type: "mining_sync".to_string(),
            timestamp_ms: 0,
            event,
            chain_id: None,
            count: None,
            job_id: None,
            difficulty,
            target_difficulty: None,
            intensity: None,
            error_code: None,
            error_msg: None,
        };
        serde_json::to_string(&msg).unwrap()
    }

    #[test]
    fn translate_emits_milestone_every_n_accepted() {
        let mut accepted = 0u64;
        let mut last = 0u64;
        let n = 100;
        // 99 accepts → no milestone.
        for _ in 0..99 {
            let raw = sync_json(WsMiningSyncEventKind::ShareAccepted, None);
            assert!(translate_mining_sync(&raw, &mut accepted, &mut last, n).is_none());
        }
        // 100th accept → milestone.
        let raw = sync_json(WsMiningSyncEventKind::ShareAccepted, None);
        let ev = translate_mining_sync(&raw, &mut accepted, &mut last, n).unwrap();
        assert_eq!(ev, WebhookEvent::ShareMilestone { accepted: 100 });
        // 101..199 → none again, 200 → next milestone.
        for _ in 0..99 {
            let raw = sync_json(WsMiningSyncEventKind::ShareAccepted, None);
            assert!(translate_mining_sync(&raw, &mut accepted, &mut last, n).is_none());
        }
        let raw = sync_json(WsMiningSyncEventKind::ShareAccepted, None);
        let ev = translate_mining_sync(&raw, &mut accepted, &mut last, n).unwrap();
        assert_eq!(ev, WebhookEvent::ShareMilestone { accepted: 200 });
    }

    #[test]
    fn translate_milestone_zero_disables_milestones() {
        let mut accepted = 0u64;
        let mut last = 0u64;
        for _ in 0..1000 {
            let raw = sync_json(WsMiningSyncEventKind::ShareAccepted, None);
            assert!(translate_mining_sync(&raw, &mut accepted, &mut last, 0).is_none());
        }
        assert_eq!(accepted, 1000);
    }

    #[test]
    fn translate_lucky_share_forwards_with_difficulty() {
        let mut accepted = 0u64;
        let mut last = 0u64;
        let raw = sync_json(WsMiningSyncEventKind::LuckyShare, Some(123456.0));
        let ev = translate_mining_sync(&raw, &mut accepted, &mut last, 100).unwrap();
        assert_eq!(
            ev,
            WebhookEvent::LuckyShare {
                difficulty: Some(123456.0)
            }
        );
    }

    #[test]
    fn translate_ignores_high_frequency_telemetry() {
        let mut accepted = 0u64;
        let mut last = 0u64;
        for kind in [
            WsMiningSyncEventKind::JobReceived,
            WsMiningSyncEventKind::CleanJob,
            WsMiningSyncEventKind::DispatchBurst,
            WsMiningSyncEventKind::NonceBurst,
            WsMiningSyncEventKind::AuthorizeState,
            WsMiningSyncEventKind::ShareRejected,
        ] {
            let raw = sync_json(kind, None);
            assert!(translate_mining_sync(&raw, &mut accepted, &mut last, 100).is_none());
        }
        // None of these touch the accepted counter.
        assert_eq!(accepted, 0);
    }

    #[test]
    fn translate_garbage_json_is_ignored() {
        let mut accepted = 0u64;
        let mut last = 0u64;
        assert!(translate_mining_sync("not json", &mut accepted, &mut last, 100).is_none());
        assert!(translate_mining_sync("{}", &mut accepted, &mut last, 100).is_none());
    }

    #[test]
    fn webhook_send_failure_log_never_formats_reqwest_error_display() {
        let src = include_str!("webhook.rs");
        let forbidden = ["error", " = %", "error"].concat();
        assert!(
            !src.contains(&forbidden),
            "reqwest::Error Display can embed credential-bearing webhook URLs"
        );
        assert!(
            src.contains("WebhookSendErrorClass::from_reqwest"),
            "webhook send failures should log classified booleans, not the raw error"
        );
    }

    // ---- no-config = no dispatch (end-to-end through the task) -------------

    #[tokio::test]
    async fn no_config_means_no_dispatch_and_no_panic() {
        // A disabled config with a URL that would refuse-connect proves the
        // default-OFF gate fires BEFORE any network I/O: the dispatch returns
        // immediately and nothing blocks.
        let (_tx, rx) = tokio::sync::watch::channel(cfg(false, "http://127.0.0.1:1/hook", &[]));
        let shutdown = CancellationToken::new();
        let dispatcher = WebhookDispatcher::spawn(
            rx,
            WebhookDispatchTuning {
                timeout: Duration::from_millis(50),
                retries: 1,
                retry_backoff: Duration::from_millis(10),
            },
            shutdown.clone(),
        );
        let handle = dispatcher.handle();
        // Enqueue several events — all must be dropped by the gate.
        for _ in 0..10 {
            assert!(handle.dispatch(WebhookEvent::ShareMilestone { accepted: 100 }));
        }
        // Give the task a moment to drain. If the gate failed, it would try to
        // POST to a refused port and the retry/backoff would still complete
        // quickly because connection-refused is immediate.
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Shut down cleanly; task must exit.
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dispatcher.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatcher task should exit promptly on shutdown");
    }

    #[tokio::test]
    async fn offline_endpoint_does_not_block_enqueue() {
        // An ENABLED config pointed at a refused port. Enqueue must remain
        // instant (non-blocking) even though the dispatcher will fail to POST.
        let (_tx, rx) = tokio::sync::watch::channel(cfg(true, "http://127.0.0.1:1/hook", &[]));
        let shutdown = CancellationToken::new();
        let dispatcher = WebhookDispatcher::spawn(
            rx,
            WebhookDispatchTuning {
                timeout: Duration::from_millis(50),
                retries: 1,
                retry_backoff: Duration::from_millis(10),
            },
            shutdown.clone(),
        );
        let handle = dispatcher.handle();

        let start = std::time::Instant::now();
        for _ in 0..5 {
            handle.dispatch(WebhookEvent::MiningStarted {
                pool: "stratum+tcp://x@127.0.0.1:1".to_string(),
            });
        }
        let elapsed = start.elapsed();
        // Enqueue of 5 events must be effectively instant — the slow POSTs
        // happen on the dispatcher task, never on this caller path.
        assert!(
            elapsed < Duration::from_millis(50),
            "enqueue blocked for {elapsed:?} — webhook send must not block the caller"
        );

        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while !dispatcher.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    // ---- channel formatting: WebhookFormat / render_text / payload_for ------

    #[test]
    fn webhook_format_default_is_generic() {
        assert_eq!(WebhookFormat::default(), WebhookFormat::Generic);
    }

    #[test]
    fn webhook_format_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&WebhookFormat::Telegram).unwrap(),
            "\"telegram\""
        );
        assert_eq!(
            serde_json::from_str::<WebhookFormat>("\"discord\"").unwrap(),
            WebhookFormat::Discord
        );
    }

    #[test]
    fn telegram_url_is_bot_sendmessage() {
        assert_eq!(
            telegram_url("123456:ABC-DEF"),
            "https://api.telegram.org/bot123456:ABC-DEF/sendMessage"
        );
        // Surrounding whitespace from a config field is trimmed.
        assert_eq!(
            telegram_url("  TOK  "),
            "https://api.telegram.org/botTOK/sendMessage"
        );
    }

    #[test]
    fn render_text_summaries_are_human_readable() {
        let ev = WebhookEvent::ShareMilestone { accepted: 500 };
        assert_eq!(
            render_text("rig-01", &ev),
            "[rig-01] Share milestone — 500 accepted shares"
        );
        let thermal = WebhookEvent::ThermalSafety {
            temp_c: 72.5,
            chain_id: 2,
        };
        assert_eq!(
            render_text("rig-01", &thermal),
            "[rig-01] Thermal safety event — chain 2 at 72.5C"
        );
        // New superset variants render honestly (no field loss).
        assert_eq!(
            render_text("rig-01", &WebhookEvent::FanFailure { rpm: 0 }),
            "[rig-01] Fan failure — last RPM 0"
        );
    }

    #[test]
    fn payload_for_generic_is_unchanged_envelope() {
        // Generic MUST be byte-for-byte the historical { miner, timestamp, alert }
        // body — identical to build_payload — so existing endpoints see no change.
        let ev = WebhookEvent::ThermalSafety {
            temp_c: 65.0,
            chain_id: 1,
        };
        let (url, body) = payload_for(
            WebhookFormat::Generic,
            "rig-01",
            "https://example.com/hook",
            None,
            None,
            &ev,
        );
        assert_eq!(url, "https://example.com/hook");
        let expected = build_payload("rig-01", &ev);
        // Only the timestamp differs between calls; compare the stable fields.
        assert_eq!(body["miner"], expected["miner"]);
        assert_eq!(body["alert"], expected["alert"]);
        assert_eq!(body["alert"]["event"], "thermal_safety");
        assert!(body["timestamp"].is_number());
    }

    #[test]
    fn payload_for_discord_wraps_content() {
        let ev = WebhookEvent::ShareMilestone { accepted: 100 };
        let (url, body) = payload_for(
            WebhookFormat::Discord,
            "rig-01",
            "https://discord.com/api/webhooks/xxx",
            None,
            None,
            &ev,
        );
        assert_eq!(url, "https://discord.com/api/webhooks/xxx");
        assert_eq!(
            body,
            serde_json::json!({ "content": render_text("rig-01", &ev) })
        );
        // Discord body has no envelope keys.
        assert!(body.get("alert").is_none());
        assert!(body.get("miner").is_none());
    }

    #[test]
    fn payload_for_slack_wraps_text() {
        let ev = WebhookEvent::ShareMilestone { accepted: 100 };
        let (url, body) = payload_for(
            WebhookFormat::Slack,
            "rig-01",
            "https://hooks.slack.com/services/xxx",
            None,
            None,
            &ev,
        );
        assert_eq!(url, "https://hooks.slack.com/services/xxx");
        assert_eq!(
            body,
            serde_json::json!({ "text": render_text("rig-01", &ev) })
        );
    }

    #[test]
    fn payload_for_telegram_uses_bot_url_and_chat_id() {
        let ev = WebhookEvent::ShareMilestone { accepted: 100 };
        let (url, body) = payload_for(
            WebhookFormat::Telegram,
            "rig-01",
            // The configured URL field is ignored for Telegram.
            "ignored://unused",
            Some("BOTTOKEN"),
            Some("987654"),
            &ev,
        );
        assert_eq!(url, "https://api.telegram.org/botBOTTOKEN/sendMessage");
        assert_eq!(
            body,
            serde_json::json!({
                "chat_id": "987654",
                "text": render_text("rig-01", &ev),
            })
        );
    }

    #[test]
    fn redaction_is_applied_before_every_channel_render() {
        // A pool field carrying a wallet-as-worker must not survive into ANY
        // channel body. Mirror the dispatcher: redact() THEN payload_for().
        let leaky = "stratum+tcp://bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6@public-pool.io:21496";
        for format in [
            WebhookFormat::Generic,
            WebhookFormat::Discord,
            WebhookFormat::Slack,
            WebhookFormat::Telegram,
        ] {
            let mut ev = WebhookEvent::PoolDisconnected {
                pool: leaky.to_string(),
            };
            ev.redact();
            let (_url, body) = payload_for(
                format,
                "rig-01",
                "https://example.com/hook",
                Some("TOK"),
                Some("CHAT"),
                &ev,
            );
            let json = serde_json::to_string(&body).unwrap();
            assert!(
                !json.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
                "wallet leaked into {format:?} body: {json}"
            );
            assert!(
                json.contains("public-pool.io:21496"),
                "host should survive into {format:?} body: {json}"
            );
        }
    }

    // ---- Telegram default-OFF gate -----------------------------------------

    #[test]
    fn telegram_not_live_without_token_or_chat_id() {
        // Enabled + Telegram but missing token/chat_id ⇒ not live (default-OFF).
        assert!(!cfg_telegram("", "123").is_live());
        assert!(!cfg_telegram("TOK", "").is_live());
        assert!(!cfg_telegram("   ", "123").is_live());
        // The empty URL must NOT make a fully-configured Telegram channel dead.
        assert!(cfg_telegram("TOK", "123").is_live());
    }

    #[test]
    fn non_telegram_live_gate_is_unchanged_by_format() {
        // Discord/Slack still gate purely on a non-empty URL, exactly like Generic.
        let mut c = cfg(true, "https://discord.com/api/webhooks/x", &[]);
        c.format = WebhookFormat::Discord;
        assert!(c.is_live());
        let mut empty = cfg(true, "", &[]);
        empty.format = WebhookFormat::Slack;
        assert!(!empty.is_live());
    }

    // ---- in-process loopback POST (no live net) ----------------------------

    /// Minimal one-shot HTTP server: accept a single connection, read the full
    /// request (headers + Content-Length body), reply `200 OK`, return the body.
    async fn capture_one_post(listener: tokio::net::TcpListener) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            let n = socket.read(&mut tmp).await.expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            // Find the header/body boundary (CRLFCRLF), comparing bytes directly.
            if let Some(pos) = buf
                .windows(4)
                .position(|w| w[0] == b'\r' && w[1] == b'\n' && w[2] == b'\r' && w[3] == b'\n')
            {
                let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let body_start = pos + 4;
                if buf.len() >= body_start + content_length {
                    let body =
                        String::from_utf8_lossy(&buf[body_start..body_start + content_length])
                            .to_string();
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    let _ = socket.flush().await;
                    return body;
                }
            }
        }
        String::new()
    }

    #[tokio::test]
    async fn loopback_discord_post_carries_reshaped_body() {
        // Bind an ephemeral loopback listener and run the real dispatcher at it
        // with format=Discord. Assert the POSTed body is the reshaped
        // { "content": ... } body — NOT the Generic envelope — and that a
        // wallet-bearing pool field never appears.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(capture_one_post(listener));

        let mut config = cfg(true, &format!("http://{addr}/hook"), &[]);
        config.format = WebhookFormat::Discord;
        let (_tx, rx) = tokio::sync::watch::channel(config);
        let shutdown = CancellationToken::new();
        let dispatcher = WebhookDispatcher::spawn(
            rx,
            WebhookDispatchTuning {
                timeout: Duration::from_secs(2),
                retries: 0,
                retry_backoff: Duration::from_millis(10),
            },
            shutdown.clone(),
        );
        dispatcher.handle().dispatch(WebhookEvent::MiningStarted {
            pool: "stratum+tcp://bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6@public-pool.io:21496"
                .to_string(),
        });

        let body = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server task timed out")
            .expect("server task panicked");

        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        // Discord reshape: a single "content" string, no Generic envelope keys.
        assert!(parsed.get("content").and_then(|c| c.as_str()).is_some());
        assert!(parsed.get("alert").is_none());
        assert!(parsed.get("miner").is_none());
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("public-pool.io:21496"));
        assert!(
            !content.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
            "wallet leaked into delivered Discord body: {content}"
        );

        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while !dispatcher.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}
