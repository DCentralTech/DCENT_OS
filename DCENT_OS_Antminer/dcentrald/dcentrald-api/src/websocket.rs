//! WebSocket handler for real-time dashboard updates.
//!
//! Provides a WebSocket endpoint at /ws on the same port 80 as the REST API.
//! Pushes three types of messages to connected clients:
//!
//! 1. **Stats** (every 1 second): hashrate, temperatures, fan speed, pool
//!    status, per-chain data. Type: "stats"
//!
//! 2. **Diagnostic Progress** (during tests): phase, progress percentage,
//!    elapsed time, ETA, detail message. Type: "diagnostic_progress"
//!
//! 3. **Heater Status** (in Space Heater mode): power watts, BTU/h, noise
//!    estimate, room temp, cost, sats earned. Type: "heater_status"
//!
//! 4. **Autotuner** (when enabled): live runtime status, efficiency, and
//!    chip-health updates. Types: "autotuner_status", "autotuner_efficiency",
//!    "autotuner_chip_health"
//!
//! 5. **Mining Sync** (event driven): low-latency job / dispatch / nonce / share
//!    events for Hacker Mode instrumentation and sonification. Type: "mining_sync"
//!
//! WebSocket connections are upgraded from HTTP via axum's upgrade mechanism.
//! Multiple clients can connect simultaneously; updates are broadcast to all.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::AppState;
use dcentrald_autotuner::power_budget::RuntimeWattCapState;

/// Hard cap on concurrent WebSocket subscribers. The S9 runs on 512 MB RAM
/// and the dashboard only ever opens one or two; 10 is generous headroom and
/// bounds the impact of a runaway client / mis-configured integration.
const MAX_WS_CONNECTIONS: usize = 10;

/// Idle-close window. A connection that sends no client frame AND receives no
/// broadcast update in this duration is closed. Under normal dashboard load
/// the 1 Hz stats stream keeps it alive continuously.
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Process-global connection counter. Incremented on upgrade, decremented on
/// scope exit via `WsConnectionGuard`. Static is fine here — connections are
/// per-process and this state doesn't outlive the daemon.
static WS_CONNECTION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// RAII guard for WS connection accounting. Drop decrements the counter even
/// on panic / early return. Constructed by `try_acquire_slot()`.
struct WsConnectionGuard;

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        WS_CONNECTION_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Try to acquire a connection slot. Returns None if the cap is hit.
fn try_acquire_slot() -> Option<WsConnectionGuard> {
    // Compare-and-swap loop to keep the cap strict under contention.
    loop {
        let current = WS_CONNECTION_COUNT.load(Ordering::Relaxed);
        if current >= MAX_WS_CONNECTIONS {
            return None;
        }
        if WS_CONNECTION_COUNT
            .compare_exchange(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return Some(WsConnectionGuard);
        }
    }
}

/// WebSocket stats message (pushed every 1 second).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsStatsMessage {
    /// Message type identifier.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Unix timestamp.
    pub timestamp: u64,
    /// Total hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// 5-second rolling average hashrate in GH/s.
    pub hashrate_5s_ghs: f64,
    /// Total accepted shares.
    pub accepted: u64,
    /// Total rejected shares.
    pub rejected: u64,
    /// Per-chain status.
    pub chains: Vec<WsChainStatus>,
    /// Fan status.
    pub fans: WsFanStatus,
    /// Pool connection status.
    pub pool: WsPoolStatus,
    /// Board-level estimated power consumption in watts.
    #[serde(default)]
    pub power_watts: f64,
    /// Wall power in watts (board / PSU efficiency).
    #[serde(default)]
    pub wall_watts: f64,
    /// Energy efficiency in joules per terahash.
    #[serde(default)]
    pub efficiency_jth: f64,
    /// Heat output in BTU/h (critical for space heater mode).
    #[serde(default)]
    pub btu_h: f64,
    /// Provenance for the power fields.
    #[serde(default)]
    pub power_source: String,
    /// Normalized source detail for UI labels and API parity.
    #[serde(default)]
    pub power_source_detail: String,
    /// True only when this frame contains a positive live power reading.
    #[serde(default)]
    pub live_power_available: bool,
    /// True when power is derived from runtime model data instead of direct measurement.
    #[serde(default)]
    pub power_modeled: bool,
    /// Human-readable power provenance note.
    #[serde(default)]
    pub power_note: String,
    /// Whether a persisted wall-meter calibration was applied.
    #[serde(default)]
    pub power_calibrated: bool,
    /// Active wall-meter calibration multiplier, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_calibration_multiplier: Option<f64>,
    /// Current circuit-cap / watt-cap state when runtime enforcement is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watt_cap: Option<RuntimeWattCapState>,
    /// Daemon uptime in seconds — surfaced on the live frame so the dashboard
    /// live-stream + the MQTT/Home-Assistant consumer get it without a
    /// separate poll (cgminer `Elapsed` / every comparator exposes uptime).
    /// `#[serde(default)]` keeps older deserializers byte-compatible.
    #[serde(default)]
    pub uptime_s: u64,
    /// Session energy total in kWh, fed by the daemon-side
    /// [`EnergyAccumulator`] — the value the HA
    /// `device_class=energy` / `state_class=total_increasing` sensor reads.
    /// Monotonic within a daemon run and integrated from the SAME wall-watts
    /// figure this frame displays (zero while live power is unavailable, so
    /// the meter can never creep on a wattage the operator can't see). Resets
    /// to 0 on restart; HA's `total_increasing` contract treats the decrease
    /// as a meter reset and preserves history. `#[serde(default)]` keeps
    /// older deserializers byte-compatible.
    #[serde(default)]
    pub energy_kwh: f64,
}

/// Per-chain status in WebSocket stats message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsChainStatus {
    /// Chain ID.
    pub id: u8,
    /// Responding chip count.
    pub chips: u8,
    /// Current frequency in MHz.
    pub frequency_mhz: u16,
    /// Current voltage in millivolts.
    pub voltage_mv: u16,
    /// Temperature in celsius. May be the XADC SoC die-temp fallback on S9
    /// when board sensors are silent — see `temp_source`.
    pub temp_c: f32,
    /// Provenance of `temp_c` (`"board_sensor"` / `"soc_die_fallback"` /
    /// absent). Mirrors `ChainState::temp_source`.
    #[serde(default)]
    pub temp_source: Option<String>,
    /// Chain hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// CRC error count.
    pub errors: u32,
    /// Chain status string.
    pub status: String,
}

/// Fan status in WebSocket stats message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsFanStatus {
    /// Current PWM duty cycle.
    pub pwm: u8,
    /// Current RPM.
    pub rpm: u32,
    /// Per-fan RPM readings when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_fan: Vec<crate::PerFanReading>,
}

/// Pool status in WebSocket stats message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsPoolStatus {
    /// Pool URL.
    pub url: String,
    /// Connection status.
    pub status: String,
    /// Current pool target difficulty for share validation.
    pub difficulty: f64,
    /// Seconds since last accepted share.
    pub last_share_s: u64,
    /// Stratum protocol version (e.g. "V1", "V2").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Whether the pool connection is encrypted (TLS/Noise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    /// Whether currently mining on the donation pool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donating: Option<bool>,
    /// W5.5: URL of the active donation pool when `donating == true`. Empty
    /// otherwise. Lets the live WS feed update the dashboard chip route
    /// without an extra /api/pools poll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donation_active_url: Option<String>,
    /// W5.5: Worker name authenticated with the active donation pool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donation_active_worker: Option<String>,
    /// W5.5: 0 = primary D-Central donation, 1 = visible Braiins fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donation_pool_index: Option<usize>,
    /// Rolling accepted-share efficiency window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_efficiency: Option<crate::ShareEfficiencyWindow>,
    /// Temporary auto-fallback state for SV2 auto mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fallback_active: Option<bool>,
    /// Seconds until auto mode retries the preferred SV2 endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_retry_sv2_after_s: Option<u64>,
    /// Human-readable reason for the temporary fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fallback_reason: Option<String>,
    /// Read-only V1 user-pool failover state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failover: Option<dcentrald_stratum::types::PoolFailoverStatus>,
    /// Live SV2 session metadata when connected over Stratum V2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sv2_session: Option<crate::Sv2SessionInfo>,
}

/// WebSocket heater status message (Space Heater mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsHeaterStatus {
    /// Message type identifier.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Current power consumption in watts.
    pub power_watts: u32,
    /// Heat output in BTU/h.
    pub btu_h: u32,
    /// Power telemetry source.
    #[serde(default)]
    pub power_source: String,
    /// Normalized source detail for UI labels and API parity.
    #[serde(default)]
    pub power_source_detail: String,
    /// True only when the frame is backed by positive live power.
    #[serde(default)]
    pub live_power_available: bool,
    /// True when power is runtime/model-derived instead of direct measurement.
    #[serde(default)]
    pub power_modeled: bool,
    /// Human-readable power provenance note.
    #[serde(default)]
    pub power_note: String,
    /// Whether persisted wall-meter calibration was applied.
    #[serde(default)]
    pub power_calibrated: bool,
    /// Active wall-meter calibration multiplier, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_calibration_multiplier: Option<f64>,
    /// Estimated noise in dB, derived from live fan tachometer RPM.
    ///
    /// `None` (serialized as JSON `null`) when no fan RPM is available — a
    /// commanded PWM alone is NOT acoustic proof, so a dB figure is never
    /// fabricated. The dashboard renders an honest "unavailable" note instead.
    /// Mirrors the `GET /api/home/status` truth-contract.
    pub noise_db: Option<u8>,
    /// Provenance of `noise_db`: `"tach_estimate"` when backed by a live fan
    /// RPM, `"unavailable_no_rpm_feedback"` otherwise. The dashboard keys its
    /// "render the dB value vs. show the literal `RPM` placeholder" decision
    /// off this tag (D-17), so it MUST travel with every heater_status message.
    pub noise_source: String,
    /// Human-readable note explaining the noise estimate or its absence.
    pub noise_note: String,
    /// Estimated airflow in CFM.
    pub airflow_cfm: u32,
    /// Active preset name.
    pub preset: String,
    /// Room temperature (if available).
    pub room_temp_c: Option<f32>,
    /// Cost today in configured currency.
    pub cost_today_usd: f64,
    /// Satoshis earned today.
    pub sats_today: u64,
    /// Whether night mode is currently active.
    pub night_mode_active: bool,
    /// Seconds until night mode starts (None if disabled).
    pub night_mode_starts_in_s: Option<u64>,
}

/// Event kind for low-latency mining instrumentation messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WsMiningSyncEventKind {
    AuthorizeState,
    JobReceived,
    CleanJob,
    DispatchBurst,
    NonceBurst,
    ShareAccepted,
    ShareRejected,
    LuckyShare,
}

/// Event-driven mining sync message for Hacker Mode instruments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsMiningSyncMessage {
    /// Message type identifier.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Event kind.
    pub event: WsMiningSyncEventKind,
    /// Dominant or source chain when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u8>,
    /// Event count for burst-style messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Pool job identifier when the event maps to a specific job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Achieved share difficulty when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<f64>,
    /// Target pool difficulty at the time of the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_difficulty: Option<f64>,
    /// Pre-normalized event intensity hint for UI/audio consumers (0.0-1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// Pool error code for rejected shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    /// Human-readable pool error for rejected shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
}

/// Handle WebSocket upgrade request.
///
/// This is the axum handler for GET /ws. It upgrades the HTTP connection
/// to a WebSocket and spawns a task to handle the connection.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

/// Handle an established WebSocket connection.
///
/// Subscribes to the broadcast channels for stats and diagnostic progress
/// updates, and forwards them to the connected client as JSON messages.
///
/// Bounded by:
/// - `MAX_WS_CONNECTIONS` concurrent sockets (rejected clients get close 1013).
/// - `WS_IDLE_TIMEOUT` window per connection (kept alive by the 1 Hz stats
///   broadcast under normal dashboard use; only idle clients hit this).
/// - Explicit `broadcast::error::RecvError::Lagged` handling so a slow client
///   skips stale frames instead of disconnecting every time the daemon
///   fills the 64-slot broadcast buffer.
///
/// Also handles incoming messages from the client (currently unused,
/// but reserved for future interactive features like subscribing to
/// specific channels).
async fn handle_ws_connection(mut socket: WebSocket, state: Arc<AppState>) {
    // Connection cap — reject with 1013 "Try Again Later" if the daemon is busy.
    let _slot = match try_acquire_slot() {
        Some(guard) => guard,
        None => {
            tracing::warn!(
                max = MAX_WS_CONNECTIONS,
                "WebSocket connection rejected: concurrent cap reached"
            );
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1013, // Try Again Later
                    reason: "dcentrald: WebSocket connection cap reached".into(),
                })))
                .await;
            return;
        }
    };

    // Subscribe to broadcast channels
    let mut stats_rx = state.stats_tx.subscribe();
    let mut mining_sync_rx = state.mining_sync_tx.subscribe();
    let mut diag_rx = state.diagnostic_progress_tx.subscribe();
    let mut autotuner_rx = state.autotuner_tx.subscribe();

    loop {
        // Idle-close timer resets on any selected arm. `interval` would
        // fire periodically; we want "timeout between events", which is
        // exactly `tokio::time::sleep` in the select branch.
        let idle_timer = tokio::time::sleep(WS_IDLE_TIMEOUT);
        tokio::pin!(idle_timer);

        tokio::select! {
            // Forward stats updates to client.
            // Using biased recv() so we can match on Lagged explicitly.
            result = stats_rx.recv() => match result {
                Ok(msg) => {
                    if socket.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "WS client lagged on stats stream — dropping stale frames");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            result = mining_sync_rx.recv() => match result {
                Ok(msg) => {
                    if socket.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "WS client lagged on mining sync stream — dropping stale frames");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // Forward diagnostic progress to client
            result = diag_rx.recv() => match result {
                Ok(msg) => {
                    let Ok(payload) = serde_json::to_string(&msg) else {
                        continue;
                    };
                    if socket.send(Message::Text(payload)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            result = autotuner_rx.recv() => match result {
                Ok(msg) => {
                    if socket.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // Handle incoming messages from client
            Some(msg) = recv_ws_message(&mut socket) => {
                match msg {
                    Ok(Message::Text(text)) => {
                        // The dashboard client sends a TEXT "ping" every ~25s as
                        // an application-level keepalive and force-closes a socket
                        // silent for >60s (its half-open watchdog, DASH-STATE-4).
                        // Reply "pong" so that watchdog measures true link
                        // liveness, not data cadence — otherwise a healthy but
                        // quiet socket (e.g. management-only mode, which
                        // broadcasts no telemetry) is wrongly closed and
                        // reconnect-thrashes. (Receiving the ping already resets
                        // this side's idle_timer, so the server stays alive.)
                        if let Some(response) = websocket_text_response(&text) {
                            let _ = socket.send(Message::Text(response.to_string())).await;
                        } else {
                            tracing::debug!(msg = %text, "WebSocket client message");
                        }
                    }
                    Ok(Message::Close(_)) => {
                        tracing::debug!("WebSocket client closed connection");
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Ok(_) => {} // Ignore binary and other message types
                    Err(_) => break, // Connection error
                }
            }
            // Idle-close safety net — only fires if no other arm resolved in
            // WS_IDLE_TIMEOUT. Under normal dashboard load the stats broadcast
            // keeps the timer reset.
            _ = &mut idle_timer => {
                tracing::debug!(
                    timeout_s = WS_IDLE_TIMEOUT.as_secs(),
                    "WebSocket idle timeout — closing"
                );
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1001, // Going Away
                        reason: "idle timeout".into(),
                    })))
                    .await;
                break;
            }
            else => break,
        }
    }

    tracing::debug!("WebSocket connection closed");
}

/// Helper to receive a WebSocket message (wraps the recv future).
async fn recv_ws_message(
    socket: &mut WebSocket,
) -> Option<std::result::Result<Message, axum::Error>> {
    socket.recv().await
}

#[derive(Debug, Clone)]
struct WsPowerTelemetryProjection {
    source: String,
    source_detail: &'static str,
    live_power_available: bool,
    modeled: bool,
    calibrated: bool,
    calibration_multiplier: Option<f64>,
    note: &'static str,
}

fn (
    power: &dcentrald_autotuner::LivePowerEstimate,
) -> WsPowerTelemetryProjection {
    let live_power_available = power.board_watts.is_finite()
        && power.board_watts > 0.0
        && power.wall_watts.is_finite()
        && power.wall_watts > 0.0;
    if !live_power_available {
        return WsPowerTelemetryProjection {
            source: "unavailable".to_string(),
            source_detail: "live_power_unavailable",
            live_power_available: false,
            modeled: false,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published positive board and wall watts.",
        };
    }

    let source = if power.source.trim().is_empty() {
        "live_power_watch".to_string()
    } else {
        power.source.clone()
    };
    let authority = dcentrald_autotuner::PowerAuthorityKind::from_source(&source, power.calibrated);
    let source_detail = match authority {
        dcentrald_autotuner::PowerAuthorityKind::Pmbus => "pmbus_measured",
        dcentrald_autotuner::PowerAuthorityKind::Adc => "adc_measured",
        dcentrald_autotuner::PowerAuthorityKind::WallCalibratedEstimate => {
            "wall_calibrated_estimate"
        }
        dcentrald_autotuner::PowerAuthorityKind::Estimated
        | dcentrald_autotuner::PowerAuthorityKind::Unknown => "live_runtime_model",
    };
    let measured = authority.is_measured();

    WsPowerTelemetryProjection {
        source,
        source_detail,
        live_power_available: true,
        modeled: !measured,
        calibrated: power.calibrated,
        calibration_multiplier: power.calibration_multiplier,
        note: if measured {
            "Power is sourced from live measured telemetry."
        } else if authority == dcentrald_autotuner::PowerAuthorityKind::WallCalibratedEstimate {
            "Power is modeled from live runtime state with an operator wall-meter calibration."
        } else {
            "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement."
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HA Energy sensor — pure watt-seconds accumulator (daemon analog of the ESP
// `dcentaxe::mqtt_ha::EnergyAccumulator`, same fail-benign contract).
//
// The HA Energy dashboard REQUIRES a `device_class=energy` +
// `state_class=total_increasing` sensor in a kWh-family unit. The daemon never
// emitted one, so the space-heater ROI story (kWh/day, $/day) could not be told
// in HA for Antminer units. The 1 Hz state publisher owns one of these, feeds
// it a power sample per tick, and puts the running kWh total on the shared
// stats frame the MQTT/HA publisher re-broadcasts.
// ─────────────────────────────────────────────────────────────────────────────

/// Watt-seconds integrator producing a monotonic session energy total (kWh).
///
/// Pure: no clock — the caller supplies the elapsed seconds between samples,
/// so it host-tests deterministically. The total only ever increases (each
/// sample adds `power_w * elapsed_s`; non-finite/negative inputs are ignored),
/// so it satisfies HA's `total_increasing` contract. Deliberately NOT
/// persisted to flash: a periodic NAND write for a cosmetic meter is not worth
/// the wear, and HA keeps long-term history across the reset itself.
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
    /// elapsed, is IGNORED — a garbage sample can never corrupt the monotonic
    /// total or make it decrease (which would break `total_increasing` in
    /// HA). Returns the running kWh total for convenience.
    pub fn add_sample(&mut self, power_w: f64, elapsed_s: f64) -> f64 {
        if power_w.is_finite() && power_w >= 0.0 && elapsed_s.is_finite() && elapsed_s > 0.0 {
            self.watt_seconds += power_w * elapsed_s;
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

/// The watts the energy accumulator integrates for one frame: the SAME
/// wall-watts figure the frame displays, gated by the SAME live-power
/// projection. While live power is unavailable
/// the frame shows 0 W, so the meter holds steady instead of creeping on a
/// wattage the operator can't see. Wall watts (not board watts) because the
/// HA Energy dashboard models what the grid delivers — what the operator pays
/// for.
pub fn energy_integration_watts(power: &dcentrald_autotuner::LivePowerEstimate) -> f64 {
    if (power).live_power_available {
        power.wall_watts
    } else {
        0.0
    }
}

/// Build a stats message JSON string from current miner state and live power.
///
/// `power` is the latest `LivePowerEstimate` from the work dispatcher's watch
/// channel. If the power estimate hasn't been computed yet (e.g., first second),
/// the power fields will be zero (default) and explicitly marked unavailable.
///
/// `energy_kwh` is the running session total from the publisher-owned
/// [`EnergyAccumulator`]; non-finite/negative values are floored to 0 so a
/// garbage total can never reach the `total_increasing` HA sensor.
pub fn build_stats_message(
    state: &crate::MinerState,
    power: &dcentrald_autotuner::LivePowerEstimate,
    energy_kwh: f64,
) -> String {
    let power_projection = (power);
    let live_power_available = power_projection.live_power_available;
    let (power_watts, wall_watts, efficiency_jth, btu_h) = if live_power_available {
        (
            power.board_watts,
            power.wall_watts,
            power.efficiency_jth,
            power.btu_h,
        )
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    let msg = WsStatsMessage {
        msg_type: "stats".to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        hashrate_ghs: state.hashrate_ghs,
        hashrate_5s_ghs: state.hashrate_5s_ghs,
        accepted: state.accepted,
        rejected: state.rejected,
        chains: state
            .chains
            .iter()
            .map(|c| WsChainStatus {
                id: c.id,
                chips: c.chips,
                frequency_mhz: c.frequency_mhz,
                voltage_mv: c.voltage_mv,
                temp_c: c.temp_c,
                temp_source: c.temp_source.clone(),
                hashrate_ghs: c.hashrate_ghs,
                errors: c.errors,
                status: c.status.clone(),
            })
            .collect(),
        fans: WsFanStatus {
            pwm: state.fans.pwm,
            rpm: state.fans.rpm,
            per_fan: state.fans.per_fan.clone(),
        },
        pool: WsPoolStatus {
            // SEC (W20 / parity #66): strip inline stratum credentials from the
            // live WS frame (pushed to every dashboard client + the MQTT/HA
            // publisher). The daemon reconnects via the real config URL.
            url: dcentrald_stratum::pool_api::sanitize_pool_url(&state.pool.url),
            status: state.pool.status.clone(),
            difficulty: state.pool.difficulty,
            last_share_s: {
                // Compute elapsed seconds from epoch timestamp
                if state.pool.last_share_at == 0 {
                    0
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(state.pool.last_share_at)
                }
            },
            protocol: Some(state.pool.protocol.clone()),
            encrypted: Some(state.pool.encrypted),
            donating: Some(state.pool.donating),
            // W5.5: surface the active donation route on the live WS frame
            // so the dashboard chip can update without polling /api/pools.
            // Empty strings collapse to None so non-donation frames don't
            // pad the wire shape.
            //
            // TEL-2 ( masking): the live WS frame is pushed to every
            // dashboard client AND the MQTT/HA publisher, so it MUST mask the
            // same way REST does (rest.rs `donation_active_url`/`active_worker`).
            // The donation URL can carry inline `user:pass@` credentials →
            // `sanitize_pool_url`; the donation worker can be a full BTC payout
            // address → `mask_wallet`. Previously both were emitted raw here
            // while REST masked them, a wire-surface credential leak.
            donation_active_url: if state.pool.donation_active_url.is_empty() {
                None
            } else {
                Some(dcentrald_stratum::pool_api::sanitize_pool_url(
                    &state.pool.donation_active_url,
                ))
            },
            donation_active_worker: if state.pool.donation_active_worker.is_empty() {
                None
            } else {
                Some(dcentrald_common::wallet_mask::mask_wallet(
                    &state.pool.donation_active_worker,
                ))
            },
            donation_pool_index: if state.pool.donating {
                Some(state.pool.donation_pool_index)
            } else {
                None
            },
            share_efficiency: state.pool.share_efficiency.clone(),
            auto_fallback_active: Some(state.pool.auto_fallback_active),
            auto_retry_sv2_after_s: state.pool.auto_retry_sv2_after_s,
            auto_fallback_reason: state.pool.auto_fallback_reason.clone(),
            failover: Some(state.pool.failover.clone()),
            sv2_session: state.pool.sv2_session.clone(),
        },
        power_watts,
        wall_watts,
        efficiency_jth,
        btu_h,
        power_source: power_projection.source,
        power_source_detail: power_projection.source_detail.to_string(),
        live_power_available: power_projection.live_power_available,
        power_modeled: power_projection.modeled,
        power_note: power_projection.note.to_string(),
        power_calibrated: power_projection.calibrated,
        power_calibration_multiplier: power_projection.calibration_multiplier,
        watt_cap: power.watt_cap.clone(),
        uptime_s: state.uptime_s,
        energy_kwh: if energy_kwh.is_finite() {
            energy_kwh.max(0.0)
        } else {
            0.0
        },
    };

    serde_json::to_string(&msg).unwrap_or_default()
}

pub fn build_mining_sync_message(msg: &WsMiningSyncMessage) -> String {
    serde_json::to_string(msg).unwrap_or_default()
}

pub fn build_mining_sync_message_with_fields(
    msg: &WsMiningSyncMessage,
    fields: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> String {
    let mut value = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        for (key, field_value) in fields {
            object.insert(key.to_string(), field_value);
        }
    }
    serde_json::to_string(&value).unwrap_or_default()
}

fn websocket_text_response(text: &str) -> Option<&'static str> {
    if text == "ping" {
        Some("pong")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn websocket_text_response_never_panics_on_arbitrary_text(text in ".{0,2048}") {
            let response = websocket_text_response(&text);
            prop_assert_eq!(response, if text == "ping" { Some("pong") } else { None });
        }
    }

    // D-17: a heater_status message backed by a live fan tachometer RPM must
    // carry BOTH the noise_db value AND the `tach_estimate` provenance tag, so
    // the dashboard's `noiseBackedByRpm` check passes and it renders the real
    // dB estimate instead of the literal "RPM" placeholder. This mirrors the
    // REST `GET /api/home/status` truth-contract (see rest.rs::home_noise_from_fans).
    #[test]
    fn heater_status_present_rpm_carries_db_and_tach_source() {
        let msg = WsHeaterStatus {
            msg_type: "heater_status".to_string(),
            power_watts: 320,
            btu_h: 1091,
            power_source: "pmbus".to_string(),
            power_source_detail: "pmbus_measured".to_string(),
            live_power_available: true,
            power_modeled: false,
            power_note: "Power is sourced from live measured telemetry.".to_string(),
            power_calibrated: false,
            power_calibration_multiplier: None,
            noise_db: Some(50),
            noise_source: "tach_estimate".to_string(),
            noise_note: "Estimated from live fan RPM".to_string(),
            airflow_cfm: 80,
            preset: "medium".to_string(),
            room_temp_c: None,
            cost_today_usd: 0.0,
            sats_today: 0,
            night_mode_active: false,
            night_mode_starts_in_s: None,
        };

        let v = serde_json::to_value(&msg).expect("serialize heater_status");
        assert_eq!(v["noise_db"], serde_json::json!(50));
        assert_eq!(v["noise_source"], serde_json::json!("tach_estimate"));
        assert_eq!(v["type"], serde_json::json!("heater_status"));
    }

    // D-17 truth-contract: with no fan RPM, the message must NOT fabricate a dB
    // figure — `noise_db` serializes as explicit JSON `null` and the source is
    // tagged unavailable, so the UI shows an honest "unavailable" note rather
    // than a made-up number.
    #[test]
    fn heater_status_absent_rpm_emits_null_db_and_unavailable_source() {
        let msg = WsHeaterStatus {
            msg_type: "heater_status".to_string(),
            power_watts: 0,
            btu_h: 0,
            power_source: "unavailable".to_string(),
            power_source_detail: "live_power_unavailable".to_string(),
            live_power_available: false,
            power_modeled: false,
            power_note: "Live power has not published positive board and wall watts.".to_string(),
            power_calibrated: false,
            power_calibration_multiplier: None,
            noise_db: None,
            noise_source: "unavailable_no_rpm_feedback".to_string(),
            noise_note: "No noise estimate: PWM command alone is not acoustic proof".to_string(),
            airflow_cfm: 0,
            preset: "medium".to_string(),
            room_temp_c: None,
            cost_today_usd: 0.0,
            sats_today: 0,
            night_mode_active: false,
            night_mode_starts_in_s: None,
        };

        let v = serde_json::to_value(&msg).expect("serialize heater_status");
        assert!(
            v["noise_db"].is_null(),
            "noise_db must serialize as JSON null when no tach RPM is present"
        );
        assert_eq!(
            v["noise_source"],
            serde_json::json!("unavailable_no_rpm_feedback")
        );
    }

    #[test]
    fn heater_status_serializes_power_provenance() {
        let msg = WsHeaterStatus {
            msg_type: "heater_status".to_string(),
            power_watts: 980,
            btu_h: 3412,
            power_source: "live_power_watch".to_string(),
            power_source_detail: "wall_calibrated_estimate".to_string(),
            live_power_available: true,
            power_modeled: true,
            power_note:
                "Power is modeled from live runtime state with an operator wall-meter calibration."
                    .to_string(),
            power_calibrated: true,
            power_calibration_multiplier: Some(1.075),
            noise_db: Some(48),
            noise_source: "tach_estimate".to_string(),
            noise_note: "Estimated from live fan RPM".to_string(),
            airflow_cfm: 72,
            preset: "quiet".to_string(),
            room_temp_c: Some(21.5),
            cost_today_usd: 0.12,
            sats_today: 42,
            night_mode_active: true,
            night_mode_starts_in_s: Some(120),
        };

        let v = serde_json::to_value(&msg).expect("serialize heater_status");
        assert_eq!(v["power_source"], serde_json::json!("live_power_watch"));
        assert_eq!(
            v["power_source_detail"],
            serde_json::json!("wall_calibrated_estimate")
        );
        assert_eq!(v["live_power_available"], serde_json::json!(true));
        assert_eq!(v["power_modeled"], serde_json::json!(true));
        assert_eq!(v["power_calibrated"], serde_json::json!(true));
        assert_eq!(v["power_calibration_multiplier"], serde_json::json!(1.075));
        assert_eq!(
            v["power_note"],
            serde_json::json!(
                "Power is modeled from live runtime state with an operator wall-meter calibration."
            )
        );
    }

    /// Build a minimal `MinerState` fixture in an active donation window with the
    /// given raw donation URL + worker, to exercise the live WS stats frame.
    fn miner_state_donating(donation_url: &str, donation_worker: &str) -> crate::MinerState {
        serde_json::from_value(serde_json::json!({
            "hashrate_ghs": 0.0,
            "hashrate_5s_ghs": 0.0,
            "accepted": 0u64,
            "rejected": 0u64,
            "chains": [],
            "fans": { "pwm": 0u8, "rpm": 0u32 },
            "pool": {
                "url": "stratum+tcp://pool.example:3333",
                "worker": "rig.1",
                "status": "Alive",
                "difficulty": 512.0,
                "last_share_at": 0u64,
                "donating": true,
                "donation_active_url": donation_url,
                "donation_active_worker": donation_worker
            },
            "uptime_s": 100u64,
            "firmware_version": "0.5.0",
            "mode": "standard"
        }))
        .expect("MinerState donation fixture must deserialize")
    }

    // TEL-2 ( masking) NEGATIVE regression: the live WS stats frame is
    // pushed to every dashboard client AND the MQTT/HA publisher, so it MUST mask
    // the donation route the same way REST does. The donation URL may carry inline
    // `user:pass@` credentials, and the donation worker may be a full BTC payout
    // address — neither may survive raw on the wire.
    #[test]
    fn ws_stats_frame_masks_donation_url_and_worker() {
        let donation_url = "stratum+tcp://donateuser:secretpass@pool.d-central.tech:3333";
        let donation_worker = "";
        let state = miner_state_donating(donation_url, donation_worker);
        let power = dcentrald_autotuner::LivePowerEstimate::default();

        let frame = build_stats_message(&state, &power, 0.0);

        // NEGATIVE: no inline pool credentials survive the wire.
        assert!(
            !frame.contains("secretpass") && !frame.contains(":secretpass@"),
            "WS stats frame leaked inline donation pool credentials: {frame}"
        );
        // NEGATIVE: no raw bech32 wallet body survives the wire.
        assert!(
            !frame.contains("dzgmtjex6jlsv2fwhe4se4jxje6"),
            "WS stats frame leaked raw donation worker bech32 body: {frame}"
        );
        assert!(
            !frame.contains(donation_worker),
            "WS stats frame leaked full donation worker wallet: {frame}"
        );

        // POSITIVE: the masked forms are exactly what REST emits.
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");
        assert_eq!(
            v["pool"]["donation_active_worker"],
            serde_json::json!(dcentrald_common::wallet_mask::mask_wallet(donation_worker))
        );
        assert_eq!(
            v["pool"]["donation_active_url"],
            serde_json::json!(dcentrald_stratum::pool_api::sanitize_pool_url(donation_url))
        );
    }

    #[test]
    fn ws_stats_frame_marks_missing_live_power_unavailable() {
        let state = miner_state_donating("", "");
        let power = dcentrald_autotuner::LivePowerEstimate::default();

        let frame = build_stats_message(&state, &power, 0.0);
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");

        assert_eq!(v["power_watts"], serde_json::json!(0.0));
        assert_eq!(v["wall_watts"], serde_json::json!(0.0));
        assert_eq!(v["power_source"], serde_json::json!("unavailable"));
        assert_eq!(
            v["power_source_detail"],
            serde_json::json!("live_power_unavailable")
        );
        assert_eq!(v["live_power_available"], serde_json::json!(false));
        assert_eq!(v["power_modeled"], serde_json::json!(false));
        assert_eq!(
            v["power_note"],
            serde_json::json!("Live power has not published positive board and wall watts.")
        );
    }

    #[test]
    fn ws_stats_frame_suppresses_partial_unavailable_power_scalars() {
        let state = miner_state_donating("", "");
        let power = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 900.0,
            wall_watts: 0.0,
            efficiency_jth: 42.0,
            btu_h: 3_070.0,
            source: "estimated".to_string(),
            ..Default::default()
        };

        let frame = build_stats_message(&state, &power, 0.0);
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");

        assert_eq!(v["power_watts"], serde_json::json!(0.0));
        assert_eq!(v["wall_watts"], serde_json::json!(0.0));
        assert_eq!(v["efficiency_jth"], serde_json::json!(0.0));
        assert_eq!(v["btu_h"], serde_json::json!(0.0));
        assert_eq!(v["power_source"], serde_json::json!("unavailable"));
        assert_eq!(v["live_power_available"], serde_json::json!(false));
    }

    #[test]
    fn ws_stats_frame_labels_live_power_provenance() {
        let state = miner_state_donating("", "");
        let power = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1000.0,
            wall_watts: 1100.0,
            efficiency_jth: 40.0,
            btu_h: dcentrald_autotuner::btu_from_watts(1100.0),
            source: "pmbus".to_string(),
            ..Default::default()
        };

        let frame = build_stats_message(&state, &power, 0.0);
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");

        assert_eq!(v["power_source"], serde_json::json!("pmbus"));
        assert_eq!(
            v["power_source_detail"],
            serde_json::json!("pmbus_measured")
        );
        assert_eq!(v["live_power_available"], serde_json::json!(true));
        assert_eq!(v["power_modeled"], serde_json::json!(false));
        assert_eq!(v["power_calibrated"], serde_json::json!(false));
    }

    #[test]
    fn ws_stats_frame_labels_wall_calibrated_runtime_estimates() {
        let state = miner_state_donating("", "");
        let power = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 900.0,
            wall_watts: 1000.0,
            efficiency_jth: 42.0,
            btu_h: dcentrald_autotuner::btu_from_watts(1000.0),
            calibrated: true,
            calibration_multiplier: Some(1.075),
            source: "estimated".to_string(),
            ..Default::default()
        };

        let frame = build_stats_message(&state, &power, 0.0);
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");

        assert_eq!(v["power_source"], serde_json::json!("estimated"));
        assert_eq!(
            v["power_source_detail"],
            serde_json::json!("wall_calibrated_estimate")
        );
        assert_eq!(v["live_power_available"], serde_json::json!(true));
        assert_eq!(v["power_modeled"], serde_json::json!(true));
        assert_eq!(v["power_calibrated"], serde_json::json!(true));
        assert_eq!(v["power_calibration_multiplier"], serde_json::json!(1.075));
    }

    // ── HA Energy sensor pins (daemon analog of the ESP EnergyAccumulator) ──

    #[test]
    fn energy_accumulator_integrates_watt_seconds_to_kwh() {
        let mut acc = EnergyAccumulator::new();
        assert_eq!(acc.energy_kwh(), 0.0);
        // 1000 W for 3600 s = 1 kWh.
        acc.add_sample(1000.0, 3600.0);
        assert!((acc.energy_kwh() - 1.0).abs() < 1e-9, "1 kWh expected");
        // Another 500 W for 7200 s = +1 kWh.
        let total = acc.add_sample(500.0, 7200.0);
        assert!((total - 2.0).abs() < 1e-9);
    }

    #[test]
    fn energy_accumulator_ignores_garbage_and_never_decreases() {
        let mut acc = EnergyAccumulator::new();
        acc.add_sample(1200.0, 60.0);
        let after_good = acc.energy_kwh();
        assert!(after_good > 0.0);

        // Garbage power and garbage elapsed are each ignored, never subtracted.
        acc.add_sample(f64::NAN, 60.0);
        acc.add_sample(f64::INFINITY, 60.0);
        acc.add_sample(-500.0, 60.0);
        acc.add_sample(1200.0, 0.0);
        acc.add_sample(1200.0, -5.0);
        acc.add_sample(1200.0, f64::NAN);
        assert_eq!(
            acc.energy_kwh(),
            after_good,
            "garbage samples must not move the monotonic total"
        );

        // A subsequent good sample still integrates.
        acc.add_sample(1200.0, 60.0);
        assert!(acc.energy_kwh() > after_good);
    }

    #[test]
    fn energy_integration_watts_gates_on_the_displayed_power_projection() {
        // Live power unavailable (default estimate) → integrate 0 W, matching
        // the 0 W the frame displays.
        let unavailable = dcentrald_autotuner::LivePowerEstimate::default();
        assert_eq!(energy_integration_watts(&unavailable), 0.0);

        // Live power available → integrate the SAME wall watts the frame shows.
        let available = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1000.0,
            wall_watts: 1100.0,
            efficiency_jth: 40.0,
            btu_h: 3753.0,
            source: "pmbus".to_string(),
            ..Default::default()
        };
        assert_eq!(energy_integration_watts(&available), 1100.0);
    }

    #[test]
    fn ws_stats_frame_carries_finite_guarded_energy_kwh() {
        let state = miner_state_donating("", "");
        let power = dcentrald_autotuner::LivePowerEstimate::default();

        let frame = build_stats_message(&state, &power, 12.3456);
        let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");
        assert_eq!(v["energy_kwh"], serde_json::json!(12.3456));

        // Non-finite / negative totals are floored to 0 — a decreasing or NaN
        // value would break HA's total_increasing contract.
        for bad in [f64::NAN, f64::NEG_INFINITY, -3.0] {
            let frame = build_stats_message(&state, &power, bad);
            let v: serde_json::Value = serde_json::from_str(&frame).expect("parse WS stats frame");
            assert_eq!(
                v["energy_kwh"],
                serde_json::json!(0.0),
                "energy_kwh must be finite-guarded, input {bad}"
            );
        }
    }
}
