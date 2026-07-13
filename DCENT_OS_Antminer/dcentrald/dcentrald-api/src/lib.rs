#![recursion_limit = "512"]
#![allow(
    dead_code,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

//! API server for dcentrald.
//!
//! Provides three API interfaces for external access:
//!
//! - **CGMiner API** (port 4028): TCP socket API compatible with CGMiner/pyasic.
//!   Implements `summary`, `stats`, `pools`, `devs`, `version`, `switchpool`,
//!   `enablepool`, `disablepool`, `addpool`, `restart`, `quit` commands.
//!   Required for pyasic/hass-miner ecosystem compatibility.
//!
//! - **REST API** (port 8080): JSON endpoints for the web dashboard and external
//!   automation. Includes status, stats, config, pools, actions, system info,
//!   home control, hacker debug, and diagnostic endpoints.
//!
//! - **WebSocket** (port 8080, /ws): Real-time streaming for the dashboard.
//!   Pushes stats updates every 1 second, diagnostic progress, and home
//!   status in Home mode.
//!
//! The REST API and WebSocket are served by the same axum server on port 8080.
//! The CGMiner API runs as a separate TCP listener on port 4028.
//!
//! API access is filtered by the active `OperatingMode` via middleware:
//! - Home: basic status + home endpoints + diagnostics
//! - Standard: all Home endpoints + stats/profiles/history
//! - Hacker: all standard endpoints + /api/debug/* raw access

pub mod atomic_io;
pub mod auth;
/// W13.D1 — live cold-boot phase tracker (host-safe API surface).
/// Cold-boot orchestrators publish into this watch channel; the
/// `/api/boot/phase` + `/api/boot/timeline` handlers read it.
pub mod boot_phase_tracker;
pub mod cgminer;
/// LuxOS session model + mutating-command contract layered on the
/// CGMiner :4028 surface. Makes DCENT_OS a drop-in for LuxOS/CGMiner-
/// speaking fleet tools (Foreman, Awesome Miner, pyasic, luxos-tooling).
/// Every mutating verb delegates to the same gated `rest::` handler the
/// dashboard calls — no new control/voltage/NAND path.
pub mod cgminer_luxos;
/// P3-2: in-memory mirror of the persisted config table (`dcentrald.toml`) so
/// read-only status handlers stop re-parsing the file from disk on every
/// request. Post-write-fresh via the `atomic_io` config-write generation.
pub mod config_cache;
pub mod dashboard;
pub mod mining_pipeline_snapshot;
pub mod mode_middleware;
pub mod mqtt;
pub mod ota_signature;
pub mod rest;
///  W8-D: route modules split out of `rest.rs`. Currently
/// hosts the silicon-profile-import endpoints under
/// `/api/profiles/silicon/*` (see `routes/profiles.rs`).
pub mod routes;
/// GROUP C (W8 parity): outbound webhook event dispatch — the event-bus →
/// webhook bridge. Wires the daemon's existing mining-sync event bus and
/// direct event call sites (mining start/stop, pool failover, thermal safety,
/// share milestones, OTA) to fire-and-forget redacted webhook POSTs.
/// Default-OFF (no URL ⇒ no dispatch). See `src/webhook.rs`.
pub mod webhook;
pub mod websocket;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, watch};

use dcentrald_autotuner::{
    AutotunerRuntimeStatus, EfficiencySnapshot, LivePowerEstimate, PowerAuthorityKind,
    PowerCalibration, TelemetryExportState,
};
use dcentrald_diagnostics::progress::DiagnosticProgress;
use dcentrald_diagnostics::DiagnosticService;
use dcentrald_hal::led::LedCommand;
use dcentrald_thermal::curtailment::CurtailmentController;

pub use config_cache::{ConfigFingerprint, ConfigTableCache};

pub use dcentrald_api_types::{
    MiningPipelineFreshnessClassifierStatus, MiningPipelineSnapshot, MiningPipelineSnapshotStatus,
    OperatingMode, MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA,
    MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
    MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS, MINING_PIPELINE_SNAPSHOT_SCHEMA,
};

static DAEMON_STARTED_AT_MS: AtomicU64 = AtomicU64::new(0);
static RUNTIME_HEALTH_RX: OnceLock<watch::Receiver<RuntimeHealthSnapshot>> = OnceLock::new();
/// Wave-G G1 (E3b): live `ThermalSupervisor` snapshot, installed by the
/// daemon thermal loop when `[thermal.supervisor].enabled = true`. Outer
/// `None` (static unset) = the supervisor was never enabled on this daemon
/// path; inner `None` = enabled but no tick produced a snapshot yet. The
/// `/api/thermal/supervisor` handler reads this — honest telemetry, never
/// fabricated. Same OnceLock+watch pattern as `RUNTIME_HEALTH_RX`.
static THERMAL_SUPERVISOR_RX: OnceLock<
    watch::Receiver<Option<dcentrald_thermal::supervisor::SupervisorSnapshot>>,
> = OnceLock::new();
static THERMAL_SUPERVISOR_CONFIGURED_ENABLED: AtomicBool = AtomicBool::new(false);
pub const AUDIT_LOG_PATH_ENV: &str = "DCENTOS_AUDIT_LOG_PATH";
pub const AUDIT_LOG_MAX_BYTES_ENV: &str = "DCENTOS_AUDIT_LOG_MAX_BYTES";
pub const DEFAULT_AUDIT_LOG_PATH: &str = "/data/audit.log";
pub const DEFAULT_AUDIT_LOG_MAX_BYTES: u64 = 1_048_576;
static AUDIT_LOG_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// API subsystem error type.
#[derive(Debug, Error)]
pub enum ApiError {
    /// HTTP server error.
    #[error("HTTP server error: {0}")]
    Http(String),

    /// CGMiner API error.
    #[error("CGMiner API error: {0}")]
    CgMiner(String),

    /// WebSocket error.
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Mode access denied.
    #[error("endpoint {endpoint} requires {required_mode} mode (current: {current_mode})")]
    ModeAccessDenied {
        endpoint: String,
        required_mode: String,
        current_mode: String,
    },

    /// Authentication required.
    #[error("authentication required")]
    AuthRequired,

    /// Hacker mode confirmation required.
    #[error("hacker mode write operation requires {{ \"confirm\": true }}")]
    ConfirmRequired,
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Shared application state accessible by all API handlers.
///
/// Contains watch channels for reading the current miner state,
///  W5 — interior-mutable wrapper around
/// `dcentrald_api_types::firmware_boot_timeline::BootProgressTracker`.
/// Held by `AppState` as `Arc<BootProgressSnapshot>` so handlers and the
/// daemon can both record and snapshot phase transitions without
/// taking a write-lock on the entire AppState.
#[derive(Debug, Default)]
pub struct BootProgressSnapshot {
    inner: Mutex<dcentrald_api_types::firmware_boot_timeline::BootProgressTracker>,
}

impl BootProgressSnapshot {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(
                dcentrald_api_types::firmware_boot_timeline::BootProgressTracker::new(),
            ),
        }
    }

    /// Record a phase transition with the current wall-clock timestamp.
    pub fn record_now(&self, phase: dcentrald_api_types::firmware_boot_timeline::BootPhase) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Ok(mut t) = self.inner.lock() {
            t.record(phase, now_ms);
        }
    }

    /// Snapshot the observed phase entries.
    pub fn snapshot(&self) -> Vec<dcentrald_api_types::firmware_boot_timeline::ObservedBootPhase> {
        self.inner.lock().map(|t| t.snapshot()).unwrap_or_default()
    }
}

///  W1 — best-effort audit ring push.
///
/// Wraps the lock + timestamp + `AuditRing::push` boilerplate so REST
/// handlers can record an operator action with one call. The
/// lock-poisoned path is silently swallowed: audit observability is
/// best-effort and must never panic the caller or leak the lock state
/// up the call stack.
pub fn push_audit_event(
    state: &AppState,
    actor: impl Into<String>,
    event: dcentrald_api_types::audit_log::AuditEvent,
) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let record = dcentrald_api_types::audit_log::AuditRecord::new(now_ms, actor, event);
    if let Ok(mut ring) = state.audit_ring.lock() {
        ring.push(record.clone());
    }
    let _ = append_audit_record_to_path(&audit_log_path(), &record);
}

pub fn audit_log_path() -> PathBuf {
    std::env::var_os(AUDIT_LOG_PATH_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AUDIT_LOG_PATH))
}

pub fn audit_log_max_bytes() -> u64 {
    std::env::var(AUDIT_LOG_MAX_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_AUDIT_LOG_MAX_BYTES)
}

fn http_bind_addr(http_bind: &str, http_port: u16) -> String {
    format!("{}:{}", http_bind, http_port)
}

fn audit_log_file_lock() -> &'static Mutex<()> {
    AUDIT_LOG_FILE_LOCK.get_or_init(|| Mutex::new(()))
}

pub fn trim_audit_log_to_max_bytes(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _};

    if max_bytes == 0 {
        return Ok(());
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= max_bytes {
        return Ok(());
    }

    let read_start = meta.len().saturating_sub(max_bytes);
    let seek_start = read_start.saturating_sub(1);
    let read_len = meta.len().saturating_sub(seek_start);
    let max_read = usize::try_from(read_len).unwrap_or(usize::MAX);
    let max_bytes_usize = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(max_read.min(1024 * 1024));
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(seek_start))?;
    file.take(read_len).read_to_end(&mut bytes)?;

    let mut start = 0;
    if read_start > 0 {
        start = if !bytes.is_empty() && bytes[0] == b'\n' {
            1
        } else if read_start == seek_start || bytes.first().copied() == Some(b'\n') {
            0
        } else {
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|idx| idx + 1)
                .unwrap_or_else(|| bytes.len().saturating_sub(max_bytes_usize))
        };
        while start < bytes.len() && bytes.len().saturating_sub(start) > max_bytes_usize {
            start += 1;
        }
        if start > 0 && start < bytes.len() && bytes[start - 1] != b'\n' {
            while start < bytes.len() && bytes[start] != b'\n' {
                start += 1;
            }
            if start < bytes.len() {
                start += 1;
            }
        }
    }

    // CFG-3: write the trimmed contents atomically (tempfile + fsync + rename)
    // so a crash/power-loss mid-trim cannot leave the forensics log empty or
    // half-written. The caller (`append_audit_record_to_path`) holds the
    // `AUDIT_LOG_FILE_LOCK` across this call, so the append→trim sequence stays
    // serialized. The non-atomic `std::fs::write` previously used here would
    // truncate the file before writing, opening a crash window that empties the
    // audit log.
    crate::atomic_io::atomic_write_bytes(path, &bytes[start..])
}

pub fn append_audit_record_to_path(
    path: &Path,
    record: &dcentrald_api_types::audit_log::AuditRecord,
) -> std::io::Result<()> {
    use std::io::Write as _;

    let _guard = audit_log_file_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = record
        .to_ndjson_line()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    drop(file);
    trim_audit_log_to_max_bytes(path, audit_log_max_bytes())
}

/// broadcast channels for WebSocket push, and references to the
/// configuration and subsystem handles.
pub struct AppState {
    /// Current miner state (read by API handlers via borrow()).
    pub state_rx: watch::Receiver<MinerState>,
    /// Current operating mode.
    pub mode_rx: watch::Receiver<OperatingMode>,
    /// Broadcast channel for WebSocket stats updates.
    pub stats_tx: broadcast::Sender<String>,
    /// Broadcast channel for low-latency mining sync events (jobs, dispatch,
    /// nonce bursts, share outcomes) consumed by Hacker Mode instruments.
    pub mining_sync_tx: broadcast::Sender<String>,
    /// Optional bounded mining pipeline snapshot receiver.
    ///
    /// REST must not reconstruct this from mining_sync broadcasts.
    /// REST only clones the latest published value. It must not subscribe to
    /// mining events or reconstruct this snapshot from dispatcher internals.
    pub mining_pipeline_snapshot_rx: Option<watch::Receiver<MiningPipelineSnapshot>>,
    /// Freshness window used when REST normalizes the latest snapshot clone.
    pub mining_pipeline_snapshot_stale_after_ms: u64,
    /// Broadcast channel for diagnostic progress updates.
    pub diagnostic_progress_tx: broadcast::Sender<DiagnosticProgress>,
    /// Timed diagnostics job store and launcher.
    pub diagnostic_service: Arc<tokio::sync::Mutex<DiagnosticService>>,
    /// Broadcast channel for live autotuner WebSocket messages.
    pub autotuner_tx: broadcast::Sender<String>,
    /// API configuration.
    pub config: ApiConfig,
    /// Optional Bitcoin network/block source configuration for the dashboard.
    /// Kept separate from Job Declaration so status responses can stay
    /// read-only and credential-redacted.
    pub network_block: NetworkBlockConfig,
    /// Live SV2 Job Declaration / Template Distribution supervisor status.
    pub jd_status_rx: watch::Receiver<dcentrald_stratum::v2::jd::JdStatus>,
    /// Directory path for autotuner profile persistence.
    pub profile_path: String,
    /// LED command sender (None if GPIO unavailable).
    pub led_tx: Option<mpsc::Sender<LedCommand>>,
    /// Live LED engine status (None if GPIO unavailable).
    pub led_status_rx: Option<watch::Receiver<dcentrald_hal::led::LedStatus>>,
    /// Curtailment controller for sleep/wake demand response.
    /// Shared between API handlers and the thermal loop.
    pub curtailment: Arc<tokio::sync::Mutex<CurtailmentController>>,
    /// Live power estimate from the work dispatcher (updated every 5s).
    /// Uses watch channel so REST API and WebSocket can read simultaneously.
    pub power_rx: watch::Receiver<LivePowerEstimate>,
    /// Persistent wall-meter correction shared with the estimator.
    pub power_calibration: Arc<std::sync::RwLock<PowerCalibration>>,
    /// Serialize runtime access to the smart PSU control bus.
    pub psu_lock: Arc<std::sync::Mutex<()>>,
    /// Admission barrier shared with the mining engine. Direct API-owned fan
    /// HAL writes (`POST /api/fan` plus the gRPC/MQTT bridge) and debug smart
    /// PSU control must hold a lease through their final hardware readback.
    /// Teardown can therefore close the control plane and drain those
    /// in-flight calls before observing safe-off. Engine-owned mutations use
    /// the engine's separate, ordered safety lifecycle.
    pub hardware_mutation_gate: dcentrald_hal::platform::HardwareMutationGate,
    /// Live runtime autotuner status.
    pub autotuner_status_rx: watch::Receiver<AutotunerRuntimeStatus>,
    /// Live autotuner efficiency snapshot.
    pub autotuner_efficiency_rx: watch::Receiver<Option<EfficiencySnapshot>>,
    /// Live autotuner chip health snapshot.
    pub autotuner_chip_health_rx: watch::Receiver<Option<dcentrald_autotuner::LiveChipHealthState>>,
    /// Live autotuner telemetry export state.
    pub autotuner_telemetry_rx: watch::Receiver<TelemetryExportState>,
    /// Live autotuner runtime command channel.
    pub autotuner_command_tx: Option<mpsc::Sender<dcentrald_autotuner::AutoTunerCommand>>,
    /// Historical data samples for /api/history (populated by daemon's history task).
    /// Vec<serde_json::Value> to avoid cross-crate dependency on HistoryBuffer.
    /// The daemon pushes serialized HistorySample values into this shared vec.
    pub history_data: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Recent correlated share accept/reject events for live debugging.
    pub recent_share_history: Arc<Mutex<Vec<RecentShareEvent>>>,
    ///  W1 — shared ring buffer that captures the last N local
    /// share-validation rejects (per-chain, per-chip, with hash + target
    /// + generation age). Populated by the work dispatcher via
    /// `WorkDispatcher::set_local_reject_ring`. Read by
    /// `GET /api/diagnostics/shares/local_rejects`. Default capacity 64.
    pub local_reject_ring: Arc<Mutex<dcentrald_api_types::share_validation::LocalRejectRing>>,
    ///  W5 — runtime-observed boot phase timestamps. Populated
    /// by the daemon from the same call sites that emit
    /// `tracing::info!(target: "boot", phase = ?, ...)`. Read by
    /// `GET /api/system/boot_timeline`.
    pub boot_progress: Arc<BootProgressSnapshot>,
    ///  W2 — fixed-capacity ring of recent audit events
    /// (mode/pool/voltage/sysupgrade etc.). Daemon-side push integration
    /// is queued for ;  ships the ring infra + read
    /// endpoint so the surface is in place for the next wave to
    /// populate. Read by `GET /api/history/audit?limit=N`. Default
    /// capacity 256.
    pub audit_ring: Arc<Mutex<dcentrald_api_types::audit_log::AuditRing>>,
    /// Room temperature (Celsius * 10, stored as u32) set via /api/home/room-temp.
    /// Read by the thermal loop for PID targeting. 0 = not set.
    pub room_temp_c10: std::sync::atomic::AtomicU32,
    /// Static hardware information (populated at startup).
    pub hardware_info: Arc<Mutex<HardwareInfo>>,
    /// W13.D1 — live cold-boot phase tracker. Cold-boot orchestrators
    /// publish into this; `/api/boot/phase` + `/api/boot/timeline` read
    /// it. See `crate::boot_phase_tracker` for the publish contract.
    pub boot_phase_tracker: Arc<crate::boot_phase_tracker::BootPhaseTracker>,
    /// Off-grid telemetry (None when off-grid mode disabled).
    pub offgrid_rx: Option<watch::Receiver<dcentrald_thermal::offgrid::OffGridTelemetry>>,
    /// Live thermal PID controller state. Outer `None` = this daemon path
    /// has no thermal loop; inner `None` = no thermal tick produced yet.
    /// Honest telemetry source for `/api/debug/pid-state` — never fabricated.
    pub pid_state_rx: Option<watch::Receiver<Option<dcentrald_thermal::controller::PidState>>>,
    /// Runtime thermal-PID tuning command channel (kp, ki, kd). None on
    /// daemon paths with no thermal loop. Safety-clamped at the handler;
    /// thermal state-machine / fan-caps / thresholds stay independent of
    /// PID gains. Mirrors the led_tx / autotuner_command_tx pattern.
    pub pid_command_tx: Option<mpsc::Sender<(f32, f32, f32)>>,
    /// Solar / hybrid runtime telemetry (None when solar integration disabled).
    pub solar_rx: Option<watch::Receiver<SolarPolicyState>>,
    /// Rolling provider verification samples for commissioning and export.
    pub solar_history: Arc<Mutex<Vec<SolarVerificationSample>>>,
    /// P3-2: in-memory mirror of the persisted config table, shared by the
    /// read-only status handlers (`/api/status`, `/api/stats`,
    /// `/api/home/status`, `/metrics`, `/api/config/{webhook,mqtt}`,
    /// `/api/mqtt/status`) so they no longer re-parse `dcentrald.toml` from disk
    /// on every request. Post-write-fresh via the `atomic_io` config-write
    /// generation — a POST that persists config invalidates this on the next
    /// read. Read-modify-write handlers still go to disk directly.
    pub config_cache: Arc<ConfigTableCache>,
}

pub const SOLAR_VERIFICATION_HISTORY_LIMIT: usize = 720;
pub const RECENT_SHARE_HISTORY_LIMIT: usize = 64;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkBlockConfig {
    /// Enable a local Bitcoin Core JSON-RPC source for /api/network/block.
    /// Disabled by default so dashboard rendering never depends on node access.
    pub enabled: bool,
    /// Local Bitcoin Core RPC URL. Credentials in URLs are rejected by config
    /// validation and redacted by display helpers.
    pub local_node_rpc_url: String,
    /// Optional RPC username. Never exposed through REST responses.
    pub local_node_rpc_user: String,
    /// Optional RPC password. Never exposed through REST responses or Debug.
    pub local_node_rpc_password: String,
    /// Optional cookie file path. The file contents are never exposed.
    pub local_node_rpc_cookie: String,
    /// Future live-RPC request timeout. Kept short for embedded dashboards.
    pub request_timeout_ms: u64,
    /// Cache TTL for future live-RPC success/error results.
    pub cache_ttl_ms: u64,
    /// Public blockchain fallback is intentionally disabled until an
    /// allowlisted, opt-in provider design lands.
    pub public_fallback_enabled: bool,
}

impl Default for NetworkBlockConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_node_rpc_url: "http://127.0.0.1:8332".to_string(),
            local_node_rpc_user: String::new(),
            local_node_rpc_password: String::new(),
            local_node_rpc_cookie: String::new(),
            request_timeout_ms: 1200,
            cache_ttl_ms: 30000,
            public_fallback_enabled: false,
        }
    }
}

impl fmt::Debug for NetworkBlockConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkBlockConfig")
            .field("enabled", &self.enabled)
            .field("local_node_rpc_url", &self.redacted_rpc_url())
            .field(
                "local_node_rpc_user_configured",
                &!self.local_node_rpc_user.trim().is_empty(),
            )
            .field(
                "local_node_rpc_password_configured",
                &!self.local_node_rpc_password.trim().is_empty(),
            )
            .field(
                "local_node_rpc_cookie_configured",
                &!self.local_node_rpc_cookie.trim().is_empty(),
            )
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("cache_ttl_ms", &self.cache_ttl_ms)
            .field("public_fallback_enabled", &self.public_fallback_enabled)
            .finish()
    }
}

impl NetworkBlockConfig {
    pub fn validate(&self) -> std::result::Result<(), String> {
        let url = self.local_node_rpc_url.trim();
        if url.is_empty() {
            return Err("network_block.local_node_rpc_url must not be empty".to_string());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(
                "network_block.local_node_rpc_url must start with http:// or https://".to_string(),
            );
        }
        if rpc_url_authority(url).contains('@') {
            return Err(
                "network_block.local_node_rpc_url must not embed credentials; use user/password fields instead"
                    .to_string(),
            );
        }
        if !(250..=3000).contains(&self.request_timeout_ms) {
            return Err(
                "network_block.request_timeout_ms must be between 250 and 3000".to_string(),
            );
        }
        if !(5000..=300000).contains(&self.cache_ttl_ms) {
            return Err("network_block.cache_ttl_ms must be between 5000 and 300000".to_string());
        }
        if self.public_fallback_enabled {
            return Err(
                "network_block.public_fallback_enabled is not supported until an allowlisted provider design exists"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn redacted_rpc_url(&self) -> String {
        redact_rpc_url(&self.local_node_rpc_url)
    }

    pub fn credential_source(&self) -> &'static str {
        if !self.local_node_rpc_cookie.trim().is_empty() {
            "cookie_file"
        } else if !self.local_node_rpc_user.trim().is_empty()
            || !self.local_node_rpc_password.trim().is_empty()
        {
            "user_password"
        } else {
            "none"
        }
    }
}

fn rpc_url_authority(url: &str) -> &str {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    without_scheme.split('/').next().unwrap_or(without_scheme)
}

fn redact_rpc_url(url: &str) -> String {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => return trimmed.to_string(),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{}", path)),
        None => (rest, String::new()),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let path_without_query = path
        .find(['?', '#'])
        .map(|idx| &path[..idx])
        .unwrap_or(path.as_str());
    format!("{}://{}{}", scheme, host, path_without_query)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RecentShareEvent {
    pub timestamp_ms: u64,
    pub result: String,
    pub job_id: String,
    /// Locally computed achieved share difficulty, when proven from the exact
    /// accepted header/hash. Null means unknown, not "same as pool target".
    pub difficulty: Option<f64>,
    /// Pool-assigned target difficulty active when the share was submitted.
    pub target_difficulty: Option<f64>,
    pub error_code: Option<i64>,
    pub error_msg: Option<String>,
    pub worker_name: Option<String>,
    pub nonce: Option<String>,
    pub ntime: Option<String>,
    pub extranonce2: Option<String>,
    pub version_bits: Option<String>,
    pub version: Option<u32>,
    pub protocol_meta_present: bool,
}

pub fn push_recent_share_event(
    history: &Arc<Mutex<Vec<RecentShareEvent>>>,
    event: RecentShareEvent,
) {
    if let Ok(mut events) = history.lock() {
        if events.len() >= RECENT_SHARE_HISTORY_LIMIT {
            let overflow = events.len() + 1 - RECENT_SHARE_HISTORY_LIMIT;
            events.drain(..overflow);
        }
        events.push(event);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SolarPolicyState {
    pub enabled: bool,
    pub provider: String,
    pub provider_live_backend: bool,
    pub provider_telemetry_backed: bool,
    pub provider_configured: bool,
    pub provider_stage: String,
    pub provider_stage_reason: Option<String>,
    pub runtime_adopted: bool,
    pub commissioning_state: String,
    pub source_profile: String,
    pub connected: bool,
    pub transport: String,
    pub matched_fields: Vec<String>,
    pub production_watts: u32,
    pub consumption_watts: u32,
    pub mining_watts: u32,
    pub mining_watts_source: String,
    pub mining_watts_live: bool,
    pub mining_watts_modeled: bool,
    pub mining_watts_note: String,
    pub net_grid_watts: i64,
    pub solar_surplus_watts: u32,
    pub battery_soc_pct: Option<f32>,
    pub solar_only_mode: bool,
    pub control_active: bool,
    pub sleeping: bool,
    pub battery_floor_active: bool,
    pub target_freq_mhz: Option<u16>,
    pub action: String,
    pub sample_age_ms: Option<u64>,
    pub stale: bool,
    pub consecutive_failures: u32,
    pub last_success_ms: Option<u64>,
    pub message: String,
    pub last_update_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolarMiningPowerStatus {
    pub watts: u32,
    pub source: String,
    pub live: bool,
    pub modeled: bool,
    pub note: &'static str,
}

pub fn solar_mining_power_status(power: &LivePowerEstimate) -> SolarMiningPowerStatus {
    if power.wall_watts.is_finite() && power.wall_watts > 0.0 {
        let source = if power.source.trim().is_empty() {
            "live_power_watch".to_string()
        } else {
            power.source.clone()
        };
        let authority = PowerAuthorityKind::from_source(&source, power.calibrated);
        let measured = authority.is_measured();
        return SolarMiningPowerStatus {
            watts: power.wall_watts.round().clamp(0.0, u32::MAX as f64) as u32,
            source,
            live: true,
            modeled: !measured,
            note: if measured {
                "Miner load is sourced from live measured power telemetry."
            } else if authority == PowerAuthorityKind::WallCalibratedEstimate {
                "Miner load is modeled from live runtime state with an operator wall-meter calibration."
            } else {
                "Miner load is modeled from the live dispatcher estimate."
            },
        };
    }

    SolarMiningPowerStatus {
        watts: 0,
        source: "unavailable".to_string(),
        live: false,
        modeled: false,
        note: "Live miner power has not published a positive wall-power reading.",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SolarVerificationSample {
    pub timestamp_ms: u64,
    pub provider: String,
    pub transport: String,
    pub connected: bool,
    pub sample_age_ms: Option<u64>,
    pub stale: bool,
    pub consecutive_failures: u32,
    pub last_success_ms: Option<u64>,
    pub matched_fields: Vec<String>,
    pub production_watts: u32,
    pub consumption_watts: u32,
    pub net_grid_watts: i64,
    pub battery_soc_pct: Option<f32>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolarProviderSupport {
    pub provider: String,
    pub live_backend: bool,
    pub stage: String,
    pub stage_reason: Option<String>,
    pub recommended_provider: Option<String>,
    pub backend_scope: Option<String>,
    pub accepted_payload_shapes: Vec<String>,
}

pub fn supported_solar_providers() -> &'static [&'static str] {
    &[
        "manual",
        "victron",
        "bridge",
        "ecoflow",
        "enphase",
        "solaredge",
        "tesla",
    ]
}

pub fn solar_provider_support(provider: &str) -> SolarProviderSupport {
    match provider.trim() {
        "manual" | "victron" | "bridge" | "enphase" | "solaredge" | "tesla" => {
            SolarProviderSupport {
                provider: provider.trim().to_string(),
                live_backend: true,
                stage: "live".to_string(),
                stage_reason: None,
                recommended_provider: None,
                backend_scope: None,
                accepted_payload_shapes: Vec::new(),
            }
        }
        "ecoflow" => SolarProviderSupport {
            provider: "ecoflow".to_string(),
            live_backend: true,
            stage: "limited".to_string(),
            stage_reason: Some("EcoFlow support is intentionally narrow: DCENT_OS only accepts validated, normalized EcoFlow HTTP payloads that can be mapped safely to production, consumption or net-grid, optional battery SoC, and optional sample age/timestamp metadata. It does not claim direct EcoFlow cloud/local authentication coverage across device families.".to_string()),
            recommended_provider: None,
            backend_scope: Some("Normalized EcoFlow HTTP payload ingestion only. Direct EcoFlow cloud/local auth, device discovery, and family-specific protocol handling remain out of scope for this backend stage.".to_string()),
            accepted_payload_shapes: vec![
                "bridge-contract: productionWatts + (consumptionWatts | netGridWatts) + optional batterySocPct + optional sampleAgeMs/timestampMs".to_string(),
                "site-summary: pvWatts | solarWatts + (homeLoadWatts | loadPowerWatts | consumptionWatts) + optional gridExchangeWatts + optional batteryPercent + optional updatedAtMs/lastUpdatedMs".to_string(),
                "power-summary: solarInputWatts + (outputWatts | homeLoadWatts | consumptionWatts) + optional netGridWatts + optional batteryLevelPct + optional telemetryTimestampMs".to_string(),
            ],
        },
        other => SolarProviderSupport {
            provider: other.to_string(),
            live_backend: false,
            stage: "unsupported".to_string(),
            stage_reason: Some("Unknown solar provider. Use manual, victron, bridge, ecoflow, enphase, solaredge, or tesla.".to_string()),
            recommended_provider: None,
            backend_scope: None,
            accepted_payload_shapes: Vec::new(),
        },
    }
}

pub fn solar_provider_telemetry_backed(provider: &str) -> bool {
    provider.trim() != "manual"
}

pub fn solar_commissioning_state(
    runtime_adopted: bool,
    provider: &str,
    connected: bool,
    stale: bool,
    consecutive_failures: u32,
) -> &'static str {
    if !runtime_adopted {
        return "pending_restart";
    }

    if !solar_provider_telemetry_backed(provider) {
        return "manual_runtime";
    }

    if connected && !stale && consecutive_failures == 0 {
        "telemetry_live"
    } else {
        "telemetry_degraded"
    }
}

pub fn solar_transport(provider: &str, endpoint: &str) -> String {
    let support = solar_provider_support(provider);
    if !support.live_backend {
        "staged".to_string()
    } else if provider == "ecoflow" {
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            "ecoflow-http-bridge".to_string()
        } else if endpoint.starts_with("mqtt://")
            || endpoint.starts_with("mqtts://")
            || endpoint.starts_with("ws://")
            || endpoint.starts_with("wss://")
        {
            "ecoflow-mqtt-bridge".to_string()
        } else {
            "ecoflow-unsupported".to_string()
        }
    } else if provider == "manual" {
        "manual".to_string()
    } else if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        "http-json".to_string()
    } else {
        "mqtt".to_string()
    }
}

/// API server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// CGMiner API port (default: 4028).
    pub cgminer_port: u16,
    /// HTTP/REST/WebSocket port (default: 8080).
    pub http_port: u16,
    /// HTTP/REST/WebSocket bind address (default: 0.0.0.0).
    pub http_bind: String,
    /// Enable WebSocket on /ws.
    pub websocket_enabled: bool,
    /// Enable one-time WebSocket auth tickets as a browser-compatible bearer alternative.
    pub websocket_tickets: bool,
    /// Bind CGMiner API to LAN (0.0.0.0) instead of localhost (127.0.0.1).
    /// SECURITY: CGMiner protocol has no auth — LAN exposure allows hashrate redirection.
    pub cgminer_bind_lan: bool,
    /// Allow MUTATING CGMiner/LuxOS verbs (voltageset/fanset/curtail/addpool/
    /// switchpool/profileset/...) from NON-loopback peers.
    ///
    /// SECURITY (API-1): `cgminer_bind_lan=true` is a documented MONITORING
    /// opt-in (pyasic/hass-miner), but the same TCP listener also serves
    /// mutating LuxOS verbs gated only by a credential-less `logon` session —
    /// so enabling LAN monitoring would silently enable unauthenticated LAN
    /// CONTROL (hashrate theft / denial-of-mining). This flag keeps mutations
    /// loopback-only by default even when the listener is LAN-bound; reads
    /// stay open. Default false (fail-closed). Set true ONLY on a trusted LAN.
    #[serde(default)]
    pub cgminer_lan_writes: bool,
    /// Require authentication for /metrics endpoint.
    /// Default true so production images fail closed.
    pub metrics_require_auth: bool,
    /// W13.D1 — expose `/api/boot/timeline` (dev-mode diagnostics).
    /// Off by default; the dashboard's diagnostics tab flips this on
    /// after the operator opens Hacker mode. Returning the timeline by
    /// default would leak per-boot timing fingerprints to LAN scanners.
    #[serde(default)]
    pub expose_boot_timeline: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            cgminer_port: 4028,
            http_port: 8080,
            http_bind: "0.0.0.0".to_string(),
            websocket_enabled: true,
            websocket_tickets: false,
            cgminer_bind_lan: false,
            cgminer_lan_writes: false,
            metrics_require_auth: true,
            expose_boot_timeline: false,
        }
    }
}

/// Runtime ownership mode surfaced by `/api/system/health`.
///
/// This is intentionally separate from [`OperatingMode`], which is the UI mode
/// (Home / Standard / Hacker). Runtime mode answers who owns mining hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeHealthMode {
    Native,
    Proxy,
    Hybrid,
}

impl RuntimeHealthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Proxy => "proxy",
            Self::Hybrid => "hybrid",
        }
    }
}

impl Default for RuntimeHealthMode {
    fn default() -> Self {
        Self::Native
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeBosminerSummary {
    pub accepted: u64,
    pub rejected: u64,
    pub mhs_5s: f64,
}

impl Default for RuntimeBosminerSummary {
    fn default() -> Self {
        Self {
            accepted: 0,
            rejected: 0,
            mhs_5s: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeBosminerHealth {
    pub alive: bool,
    pub pid: Option<u32>,
    pub pid_history: Vec<u32>,
    pub last_seen_ms: u64,
    pub blockers: Vec<String>,
    pub last_summary: RuntimeBosminerSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeScrapeHealth {
    pub cgminer_url: Option<String>,
    pub cgminer_reachable: Option<bool>,
    pub last_poll_ms: Option<u64>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeHealthSnapshot {
    pub mode: RuntimeHealthMode,
    pub bosminer: RuntimeBosminerHealth,
    pub scrape: RuntimeScrapeHealth,
}

impl RuntimeHealthSnapshot {
    pub fn for_mode(mode: RuntimeHealthMode) -> Self {
        let mut snapshot = Self {
            mode,
            ..Self::default()
        };
        if matches!(mode, RuntimeHealthMode::Proxy) {
            snapshot.scrape.cgminer_url = Some("http://127.0.0.1:4028".to_string());
            snapshot.scrape.cgminer_reachable = Some(false);
        }
        snapshot
    }
}

impl Default for RuntimeHealthSnapshot {
    fn default() -> Self {
        Self {
            mode: RuntimeHealthMode::Native,
            bosminer: RuntimeBosminerHealth::default(),
            scrape: RuntimeScrapeHealth::default(),
        }
    }
}

pub fn install_runtime_health_rx(rx: watch::Receiver<RuntimeHealthSnapshot>) -> bool {
    RUNTIME_HEALTH_RX.set(rx).is_ok()
}

/// Install the live thermal-supervisor snapshot channel (Wave-G G1). Called
/// once by the daemon thermal loop when `[thermal.supervisor].enabled`.
pub fn install_thermal_supervisor_rx(
    rx: watch::Receiver<Option<dcentrald_thermal::supervisor::SupervisorSnapshot>>,
    configured_enabled: bool,
) -> bool {
    THERMAL_SUPERVISOR_CONFIGURED_ENABLED.store(configured_enabled, Ordering::Relaxed);
    THERMAL_SUPERVISOR_RX.set(rx).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ThermalSupervisorRuntimeStatus {
    pub configured_enabled: bool,
    pub runtime_present: bool,
    pub snapshot_available: bool,
    pub commissioning_state: &'static str,
}

pub fn thermal_supervisor_commissioning_state(
    runtime_present: bool,
    configured_enabled: bool,
    snapshot_available: bool,
) -> &'static str {
    if !runtime_present {
        "unsupported"
    } else if !configured_enabled {
        "disabled"
    } else if snapshot_available {
        "running"
    } else {
        "pending_tick"
    }
}

/// Read the latest thermal-supervisor snapshot. `None` when the supervisor
/// is disabled on this daemon path OR enabled-but-no-tick-yet — the
/// `/api/thermal/supervisor` handler renders that truthfully (never
/// fabricated board state).
pub fn thermal_supervisor_snapshot() -> Option<dcentrald_thermal::supervisor::SupervisorSnapshot> {
    THERMAL_SUPERVISOR_RX
        .get()
        .and_then(|rx| rx.borrow().clone())
}

pub fn thermal_supervisor_runtime_status() -> ThermalSupervisorRuntimeStatus {
    let runtime_present = THERMAL_SUPERVISOR_RX.get().is_some();
    let snapshot_available = THERMAL_SUPERVISOR_RX
        .get()
        .map(|rx| rx.borrow().is_some())
        .unwrap_or(false);
    let configured_enabled =
        runtime_present && THERMAL_SUPERVISOR_CONFIGURED_ENABLED.load(Ordering::Relaxed);
    ThermalSupervisorRuntimeStatus {
        configured_enabled,
        runtime_present,
        snapshot_available,
        commissioning_state: thermal_supervisor_commissioning_state(
            runtime_present,
            configured_enabled,
            snapshot_available,
        ),
    }
}

pub fn runtime_health_snapshot() -> RuntimeHealthSnapshot {
    RUNTIME_HEALTH_RX
        .get()
        .map(|rx| rx.borrow().clone())
        .unwrap_or_default()
}

pub fn mark_daemon_started() {
    let now = unix_epoch_ms();
    let _ = DAEMON_STARTED_AT_MS.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
}

pub fn daemon_uptime_s() -> Option<u64> {
    let started = DAEMON_STARTED_AT_MS.load(Ordering::Relaxed);
    if started == 0 {
        return None;
    }
    Some(unix_epoch_ms().saturating_sub(started) / 1000)
}

pub fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Snapshot of the current miner state.
///
/// Published via watch channel by the state publisher task.
/// Read by API handlers to serve status/stats responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinerState {
    /// Total hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// 5-second rolling average hashrate in GH/s.
    pub hashrate_5s_ghs: f64,
    /// Total accepted shares.
    pub accepted: u64,
    /// Total rejected shares.
    pub rejected: u64,
    /// Per-chain status.
    pub chains: Vec<ChainState>,
    /// Fan status.
    pub fans: FanState,
    /// Pool connection status.
    pub pool: PoolState,
    /// System uptime in seconds.
    pub uptime_s: u64,
    /// Firmware version.
    pub firmware_version: String,
    /// Current operating mode.
    pub mode: OperatingMode,
}

impl MinerState {
    /// AT-DASH (2026-06-14): construct an empty `MinerState` to seed a live
    /// `watch::channel` before any telemetry has been published. Mirrors the
    /// `build_minimal_app_state` initial-state shape (zero hashrate / no chains /
    /// "connecting" pool). Used by mining modes (e.g. `--s19j-hybrid`) that own
    /// their own `MinerState` publisher and wire its receiver into the API.
    /// `MinerState` deliberately does not derive `Default` because `PoolState`
    /// pulls in non-`Default` stratum types; this is the single seed helper.
    pub fn empty(mode: OperatingMode) -> Self {
        MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: Vec::new(),
            fans: FanState {
                pwm: 0,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: PoolState {
                url: String::new(),
                worker: String::new(),
                status: "connecting".to_string(),
                difficulty: 0.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 0,
            firmware_version: env!("CARGO_PKG_VERSION").to_string(),
            mode,
        }
    }
}

/// Per-chain state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainState {
    /// Chain ID (6, 7, 8 for S9).
    pub id: u8,
    /// Number of responding chips.
    pub chips: u8,
    /// Current frequency in MHz.
    pub frequency_mhz: u16,
    /// Current voltage in millivolts.
    pub voltage_mv: u16,
    /// Temperature in celsius.
    ///
    /// On S9/Zynq the on-board TMP451/ADT7461/NCT218 sensors are read via
    /// BM1387 I²C passthrough and need 12V hashboard power; when they return
    /// no data (a NORMAL S9 condition — the PIC answers on the 3.3V rail but
    /// the temp sensors need the board powered) this carries the honest XADC
    /// SoC die-temp fallback instead of 0.0. `temp_source` distinguishes the
    /// two so the dashboard never mistakes a die-temp fallback for a missing
    /// board sensor or an unpowered board. 0.0 means "no temperature at all"
    /// (board genuinely unpowered / no sensor + no die proxy).
    pub temp_c: f32,
    /// Provenance of `temp_c`. `None`/absent = legacy/unknown source (treated
    /// as a board sensor by older clients). See [`ChainTempSource`] for the
    /// canonical string values. Serialized as a plain string for dashboard
    /// labeling; `#[serde(default)]` keeps older deserializers compatible.
    #[serde(default)]
    pub temp_source: Option<String>,
    /// Chain hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// CRC error count.
    pub errors: u32,
    /// Chain status string.
    pub status: String,
}

/// Canonical `ChainState::temp_source` values. Kept as `&'static str`
/// constants (not a serde enum) so the field stays a forgiving plain string
/// on the wire and the dashboard can switch on known values while tolerating
/// unknown future ones.
pub struct ChainTempSource;

impl ChainTempSource {
    /// `temp_c` is a real on-board hashboard sensor reading (TMP451 /
    /// ADT7461 / NCT218 via BM1387 passthrough on Zynq, or the platform's
    /// direct board sensor elsewhere).
    pub const BOARD_SENSOR: &'static str = "board_sensor";
    /// `temp_c` is the XADC SoC die-temperature fallback — used when the
    /// hashboard board sensors return no data (the normal S9 case). This is
    /// an honest proxy for the enclosure/board temperature, NOT a per-board
    /// sensor reading, and is typically ~20-30 °C cooler than a true board
    /// sensor at the same hashrate.
    pub const SOC_DIE_FALLBACK: &'static str = "soc_die_fallback";
}

/// Fan state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanState {
    /// Current PWM duty cycle as a percent (0-100).
    pub pwm: u8,
    /// Current RPM reading (legacy, from primary tach).
    pub rpm: u32,
    /// Per-fan readings (Fan 0, Fan 1, ...). Length = fan_count.
    #[serde(default)]
    pub per_fan: Vec<PerFanReading>,
}

/// Per-fan reading for individual fan monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerFanReading {
    /// Fan index (0-based).
    pub id: u8,
    /// RPM reading (0 = not connected or not spinning).
    pub rpm: u32,
    /// PWM duty cycle as percentage (0-100).
    pub pwm_percent: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HardwareCapabilities {
    #[serde(default)]
    pub voltage_control: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_rpm_feedback: Option<bool>,
    #[serde(default)]
    pub sleep_wake_supported: bool,
}

pub use dcent_schema::capability::IdentityConfidence as HardwareIdentityConfidence;

fn unknown_hardware_identity_confidence() -> HardwareIdentityConfidence {
    HardwareIdentityConfidence::Unknown
}

mod hardware_identity_confidence_wire {
    use super::HardwareIdentityConfidence;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(
        value: &HardwareIdentityConfidence,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match value {
            HardwareIdentityConfidence::Exact => "exact",
            HardwareIdentityConfidence::High => "high",
            HardwareIdentityConfidence::Low => "low",
            HardwareIdentityConfidence::Unknown => "unknown",
        })
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HardwareIdentityConfidence, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim().to_ascii_lowercase().as_str() {
            "exact" => Ok(HardwareIdentityConfidence::Exact),
            "high" => Ok(HardwareIdentityConfidence::High),
            // Historical `medium` remains readable but maps to the canonical
            // capability enum's conservative non-authorizing level.
            "medium" | "low" => Ok(HardwareIdentityConfidence::Low),
            "unknown" => Ok(HardwareIdentityConfidence::Unknown),
            _ => Err(serde::de::Error::custom(
                "identity confidence must be exact, high, medium, low, or unknown",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareIdentityClaim {
    AsicFamily,
    ControlBoard,
    HashboardModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredIdentitySource {
    ConfigModel,
    BoardTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedIdentitySource {
    PlatformProbe,
    HashboardEeprom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasuredIdentitySource {
    AsicEnumeration,
}

/// Evidence source whose variant makes the proof level impossible to confuse.
///
/// `config_model` and `board_target` can only deserialize as `declared`;
/// `asic_enumeration` can only deserialize as `measured`. This prevents a raw
/// source string from being relabeled as stronger evidence by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", content = "source", rename_all = "lowercase")]
pub enum HardwareIdentityEvidenceSource {
    Declared(DeclaredIdentitySource),
    Observed(ObservedIdentitySource),
    Measured(MeasuredIdentitySource),
}

impl HardwareIdentityEvidenceSource {
    pub const fn level(self) -> HardwareIdentityEvidenceLevel {
        match self {
            Self::Declared(_) => HardwareIdentityEvidenceLevel::Declared,
            Self::Observed(_) => HardwareIdentityEvidenceLevel::Observed,
            Self::Measured(_) => HardwareIdentityEvidenceLevel::Measured,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareIdentityEvidenceLevel {
    Declared,
    Observed,
    Measured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareCompositionToken {
    pub generation: u64,
    pub fingerprint: String,
}

impl HardwareCompositionToken {
    pub fn new(generation: u64, fingerprint: impl Into<String>) -> Self {
        Self {
            generation,
            fingerprint: fingerprint.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareIdentityEvidence {
    pub claim: HardwareIdentityClaim,
    #[serde(flatten)]
    pub source: HardwareIdentityEvidenceSource,
    pub source_value: String,
    pub resolved_value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<HardwareCompositionToken>,
}

impl HardwareIdentityEvidence {
    pub fn declared_asic_config(model: impl Into<String>, chip: impl Into<String>) -> Self {
        Self {
            claim: HardwareIdentityClaim::AsicFamily,
            source: HardwareIdentityEvidenceSource::Declared(DeclaredIdentitySource::ConfigModel),
            source_value: model.into(),
            resolved_value: chip.into(),
            composition: None,
        }
    }

    pub fn declared_asic_board_target(
        board_target: impl Into<String>,
        chip: impl Into<String>,
    ) -> Self {
        Self {
            claim: HardwareIdentityClaim::AsicFamily,
            source: HardwareIdentityEvidenceSource::Declared(DeclaredIdentitySource::BoardTarget),
            source_value: board_target.into(),
            resolved_value: chip.into(),
            composition: None,
        }
    }

    pub fn observed_control_board(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            claim: HardwareIdentityClaim::ControlBoard,
            source: HardwareIdentityEvidenceSource::Observed(ObservedIdentitySource::PlatformProbe),
            source_value: value.clone(),
            resolved_value: value,
            composition: None,
        }
    }

    pub fn observed_hashboard_model(
        source_value: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            claim: HardwareIdentityClaim::HashboardModel,
            source: HardwareIdentityEvidenceSource::Observed(
                ObservedIdentitySource::HashboardEeprom,
            ),
            source_value: source_value.into(),
            resolved_value: model.into(),
            composition: None,
        }
    }

    pub fn measured_asic_enumeration(
        chip_id: u16,
        chip: impl Into<String>,
        composition: HardwareCompositionToken,
    ) -> Self {
        Self {
            claim: HardwareIdentityClaim::AsicFamily,
            source: HardwareIdentityEvidenceSource::Measured(
                MeasuredIdentitySource::AsicEnumeration,
            ),
            source_value: format!("0x{chip_id:04X}"),
            resolved_value: chip.into(),
            composition: Some(composition),
        }
    }

    pub const fn level(&self) -> HardwareIdentityEvidenceLevel {
        self.source.level()
    }

    fn legacy_source_tag(&self) -> String {
        let prefix = match self.source {
            HardwareIdentityEvidenceSource::Declared(DeclaredIdentitySource::ConfigModel) => {
                "config_model"
            }
            HardwareIdentityEvidenceSource::Declared(DeclaredIdentitySource::BoardTarget) => {
                "board_target"
            }
            HardwareIdentityEvidenceSource::Observed(ObservedIdentitySource::PlatformProbe) => {
                "platform_probe"
            }
            HardwareIdentityEvidenceSource::Observed(ObservedIdentitySource::HashboardEeprom) => {
                "hashboard_eeprom"
            }
            HardwareIdentityEvidenceSource::Measured(MeasuredIdentitySource::AsicEnumeration) => {
                "asic_enumeration"
            }
        };
        format!("{prefix}:{}->{}", self.source_value, self.resolved_value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareIdentification {
    /// Compatibility confidence field, now a closed enum rather than a string.
    #[serde(
        default = "unknown_hardware_identity_confidence",
        with = "hardware_identity_confidence_wire"
    )]
    pub confidence: HardwareIdentityConfidence,
    /// Legacy tags retained for existing REST/MCP clients. New code must build
    /// `evidence`; constructors derive this field deterministically.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Canonical typed evidence. Empty when deserializing a legacy snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<HardwareIdentityEvidence>,
    /// Human-readable explanation for operators and support bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for HardwareIdentification {
    fn default() -> Self {
        Self {
            confidence: HardwareIdentityConfidence::Unknown,
            sources: Vec::new(),
            evidence: Vec::new(),
            note: Some("hardware identity has not been resolved".to_string()),
        }
    }
}

impl HardwareIdentification {
    pub fn from_evidence(evidence: Vec<HardwareIdentityEvidence>, note: Option<String>) -> Self {
        let confidence = Self::classify_confidence(&evidence);
        let sources = evidence
            .iter()
            .map(HardwareIdentityEvidence::legacy_source_tag)
            .collect();
        Self {
            confidence,
            sources,
            evidence,
            note,
        }
    }

    pub fn push_evidence(&mut self, evidence: HardwareIdentityEvidence) {
        self.evidence.push(evidence);
        self.confidence = Self::classify_confidence(&self.evidence);
        self.sources = self
            .evidence
            .iter()
            .map(HardwareIdentityEvidence::legacy_source_tag)
            .collect();
    }

    pub fn clear_measured_asic_evidence(&mut self) {
        self.evidence.retain(|evidence| {
            evidence.claim != HardwareIdentityClaim::AsicFamily
                || evidence.level() != HardwareIdentityEvidenceLevel::Measured
        });
        self.confidence = Self::classify_confidence(&self.evidence);
        self.sources = self
            .evidence
            .iter()
            .map(HardwareIdentityEvidence::legacy_source_tag)
            .collect();
    }

    pub fn strongest_asic_evidence_level(&self) -> Option<HardwareIdentityEvidenceLevel> {
        self.evidence
            .iter()
            .filter(|evidence| evidence.claim == HardwareIdentityClaim::AsicFamily)
            .map(HardwareIdentityEvidence::level)
            .max()
    }

    /// Highest-quality non-measured ASIC label, preserving evidence order as
    /// the deterministic tie-breaker. Used when a measured composition is
    /// revoked so presentation falls back to declarations/observations rather
    /// than retaining a stale enumeration label.
    pub fn best_non_measured_asic_resolved_value(&self) -> Option<&str> {
        let strongest = self
            .evidence
            .iter()
            .filter(|evidence| {
                evidence.claim == HardwareIdentityClaim::AsicFamily
                    && evidence.level() != HardwareIdentityEvidenceLevel::Measured
            })
            .map(HardwareIdentityEvidence::level)
            .max()?;
        self.evidence
            .iter()
            .find(|evidence| {
                evidence.claim == HardwareIdentityClaim::AsicFamily && evidence.level() == strongest
            })
            .map(|evidence| evidence.resolved_value.as_str())
    }

    fn classify_confidence(evidence: &[HardwareIdentityEvidence]) -> HardwareIdentityConfidence {
        let asic = evidence
            .iter()
            .filter(|evidence| evidence.claim == HardwareIdentityClaim::AsicFamily)
            .collect::<Vec<_>>();
        if asic
            .iter()
            .any(|evidence| evidence.level() == HardwareIdentityEvidenceLevel::Measured)
        {
            return HardwareIdentityConfidence::High;
        }
        if asic
            .iter()
            .any(|evidence| evidence.level() == HardwareIdentityEvidenceLevel::Observed)
        {
            return HardwareIdentityConfidence::Low;
        }
        if !asic.is_empty() {
            let first = &asic[0].resolved_value;
            if asic.len() >= 2
                && asic
                    .iter()
                    .all(|evidence| evidence.resolved_value == *first)
            {
                return HardwareIdentityConfidence::Low;
            }
            return HardwareIdentityConfidence::Low;
        }
        if evidence
            .iter()
            .any(|evidence| evidence.level() == HardwareIdentityEvidenceLevel::Observed)
        {
            HardwareIdentityConfidence::Low
        } else if evidence.is_empty() {
            HardwareIdentityConfidence::Unknown
        } else {
            HardwareIdentityConfidence::Low
        }
    }
}

#[cfg(test)]
mod hardware_identity_evidence_tests {
    use super::*;

    #[test]
    fn correlated_declarations_remain_low_and_keep_legacy_wire_tags() {
        let identity = HardwareIdentification::from_evidence(
            vec![
                HardwareIdentityEvidence::declared_asic_config("s19jpro", "BM1362"),
                HardwareIdentityEvidence::declared_asic_board_target("am2-s19j", "BM1362"),
            ],
            None,
        );

        assert_eq!(identity.confidence, HardwareIdentityConfidence::Low);
        assert_eq!(
            identity.strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Declared)
        );
        assert_eq!(
            identity.sources,
            [
                "config_model:s19jpro->BM1362",
                "board_target:am2-s19j->BM1362",
            ]
        );
        let wire = serde_json::to_value(identity).unwrap();
        assert_eq!(wire["confidence"], "low");
        assert_eq!(wire["evidence"][0]["level"], "declared");
        assert_eq!(wire["evidence"][0]["source"], "config_model");
    }

    #[test]
    fn measured_enumeration_is_the_only_high_asic_constructor() {
        let identity = HardwareIdentification::from_evidence(
            vec![HardwareIdentityEvidence::measured_asic_enumeration(
                0x1387,
                "BM1387",
                HardwareCompositionToken::new(1, "test:am1-s9"),
            )],
            None,
        );

        assert_eq!(identity.confidence, HardwareIdentityConfidence::High);
        assert_eq!(
            identity.strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );
        assert_eq!(identity.sources, ["asic_enumeration:0x1387->BM1387"]);
    }

    #[test]
    fn legacy_exact_parses_but_has_no_typed_authority() {
        let identity: HardwareIdentification = serde_json::from_value(serde_json::json!({
            "confidence": "exact",
            "sources": ["board_target:am1-s9->BM1387"]
        }))
        .unwrap();

        assert_eq!(identity.confidence, HardwareIdentityConfidence::Exact);
        assert!(identity.evidence.is_empty());
        assert_eq!(identity.strongest_asic_evidence_level(), None);
        assert_eq!(
            serde_json::to_value(identity).unwrap()["confidence"],
            "exact"
        );
    }

    #[test]
    fn measured_level_rejects_a_declared_source_kind() {
        let result = serde_json::from_value::<HardwareIdentityEvidence>(serde_json::json!({
            "claim": "asic_family",
            "level": "measured",
            "source": "config_model",
            "source_value": "s9",
            "resolved_value": "BM1387"
        }));
        assert!(result.is_err());
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareEfficiencyWindow {
    #[serde(default)]
    pub window_s: u64,
    #[serde(default)]
    pub accepted_share_count: u64,
    /// Compatibility alias for `accepted_pool_target_difficulty_sum`.
    ///
    /// This is pool target/credit work, not achieved lucky-share difficulty.
    #[serde(default)]
    pub accepted_difficulty_sum: f64,
    #[serde(default)]
    pub accepted_pool_target_difficulty_sum: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_difficulty_sum: Option<f64>,
    #[serde(default)]
    pub estimated_wall_energy_kwh: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_shares_per_kwh: Option<f64>,
    /// Compatibility alias for `accepted_pool_target_difficulty_per_kwh`.
    ///
    /// This is pool target/credit work per kWh, not achieved share difficulty
    /// per kWh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_difficulty_per_kwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_pool_target_difficulty_per_kwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_difficulty_per_kwh: Option<f64>,
    #[serde(default)]
    pub difficulty_source: String,
    #[serde(default)]
    pub power_source: String,
    #[serde(default)]
    pub calibrated: bool,
}

/// Hardware information snapshot (populated once at startup, refreshed on demand).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HardwareInfo {
    /// Miner serial number (from control board EEPROM or MAC-derived).
    #[serde(default)]
    pub miner_serial: Option<String>,
    /// Control board identifier (e.g., "AML am3-aml", "Zynq am1-s9").
    #[serde(default)]
    pub control_board: String,
    /// Hash board type/model (e.g., "BHB42831", "BHB68606").
    #[serde(default)]
    pub hb_type: Option<String>,
    /// ASIC chip type detected (e.g., "BM1387", "BM1368").
    #[serde(default)]
    pub chip_type: String,
    /// PSU model name (e.g., "APW121215f").
    #[serde(default)]
    pub psu_model: Option<String>,
    /// PSU firmware version string.
    #[serde(default)]
    pub psu_fw_version: Option<String>,
    /// PSU serial number.
    #[serde(default)]
    pub psu_serial: Option<String>,
    /// PSU voltage range string (e.g., "11.96 V - 15.2 V").
    #[serde(default)]
    pub psu_voltage_range: Option<String>,
    /// PSU override active (user bypassing PSU auto-detect).
    #[serde(default)]
    pub psu_override_active: bool,
    /// Cross-family capability surface for product/UI truthfulness.
    #[serde(default)]
    pub capabilities: HardwareCapabilities,
    /// Structured confidence/evidence for the chip/control-board identity.
    #[serde(default)]
    pub identification: HardwareIdentification,
    /// Family-aware autotuner preset truth snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autotuner: Option<dcentrald_autotuner::AutotunerPolicyStatus>,
}

/// Canonical reject-reason buckets (HLA: superior diagnostic observability).
/// Most firmware shows only a TOTAL reject count; DCENT classifies each pool
/// reject into an actionable cause so an operator can diagnose WHY shares fail
/// (low-difficulty → vardiff lag / clock skew; stale → work-propagation latency;
/// duplicate → a nonce-dedup bug; above-target → a target-calc bug). The fixed
/// bucket set also BOUNDS Prometheus label cardinality (pool-controlled reject
/// strings would otherwise be unbounded — a known metrics anti-pattern).
/// Index order is the contract for `PoolState.reject_reason_counts`.
pub const REJECT_REASON_LABELS: [&str; 6] = [
    "low_difficulty",
    "stale",
    "duplicate",
    "above_target",
    "unauthorized",
    "other",
];

/// Canonical `PoolState::*_source` values from the no-HAL Stratum reducer.
pub use dcentrald_stratum::pool_quality::PoolQualitySource;

/// Classify a pool share-reject `(error_code, error_msg)` into a
/// [`REJECT_REASON_LABELS`] bucket index. Pure + host-testable. Prefers the
/// de-facto Stratum error codes (21=stale/job-not-found, 22=duplicate,
/// 23=low-difficulty, 24/25=unauthorized/not-subscribed) and falls back to
/// case-insensitive message keywords; anything unrecognized is `other` (5).
pub fn classify_reject_reason(error_code: i64, error_msg: &str) -> usize {
    dcentrald_stratum::pool_quality::classify_reject_reason(error_code, error_msg)
}

/// Build a forgiving string source tag for additive `PoolState` provenance
/// fields. `None`/absent remains the legacy unknown-source contract.
pub fn pool_quality_source_tag(source: &'static str) -> Option<String> {
    Some(source.to_string())
}

pub fn pool_quality_honest_default_source() -> Option<String> {
    pool_quality_source_tag(dcentrald_stratum::pool_quality::PoolQualitySource::HONEST_DEFAULT)
}

pub fn pool_quality_config_source() -> Option<String> {
    pool_quality_source_tag(dcentrald_stratum::pool_quality::PoolQualitySource::CONFIG)
}

/// Pool connection state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolState {
    /// Pool URL.
    pub url: String,
    /// Worker name authenticated with the pool (mining.authorize username).
    /// May be empty for proxy/test fixtures or before authorize completes.
    #[serde(default)]
    pub worker: String,
    /// Connection status.
    pub status: String,
    /// Current pool target difficulty for share validation.
    pub difficulty: f64,
    /// Unix epoch timestamp of last accepted share (seconds). 0 = no shares yet.
    /// BUG FIX (2026-04-11): Was "seconds since last share" but never incremented.
    /// Now stores epoch timestamp; API computes elapsed at read time.
    pub last_share_at: u64,
    /// Stratum protocol version: "sv1" or "sv2".
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// Whether the pool connection is encrypted (SV2 Noise_NX).
    #[serde(default)]
    pub encrypted: bool,
    /// Provenance of `encrypted`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub encrypted_source: Option<String>,
    /// SV2 session metadata (populated when connected via Stratum V2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv2_session: Option<Sv2SessionInfo>,
    /// Provenance of `sv2_session`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub sv2_session_source: Option<String>,
    /// SV2 Job Declaration custom-job injection state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv2_custom_job: Option<Sv2CustomJobInfo>,
    /// Whether currently mining on the donation pool (transparent time-switching).
    #[serde(default)]
    pub donating: bool,
    /// Provenance of `donating`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub donating_source: Option<String>,
    /// W5.5: URL of the active donation pool when `donating == true`. Empty
    /// when not in a donation window. Pool URLs never include passwords —
    /// safe for unauthenticated dashboard surfaces.
    #[serde(default)]
    pub donation_active_url: String,
    /// W5.5: Worker name authenticated with the active donation pool. Empty
    /// when not in a donation window. Useful to distinguish primary D-Central
    /// donation worker from the visible Braiins Pool fallback (also `DungeonMaster`).
    #[serde(default)]
    pub donation_active_worker: String,
    /// W5.5: Zero-based index of the active donation route. 0 = primary
    /// D-Central donation pool, 1 = visible Braiins fallback worker. Stays
    /// at 0 outside the donation window — pair with `donating` to interpret.
    #[serde(default)]
    pub donation_pool_index: usize,
    /// Rolling accepted-share efficiency window grounded in accepted difficulty work and wall energy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_efficiency: Option<ShareEfficiencyWindow>,
    /// Whether Auto mode is temporarily running on V1 fallback.
    #[serde(default)]
    pub auto_fallback_active: bool,
    /// Provenance of auto-fallback fields.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub auto_fallback_source: Option<String>,
    /// Seconds until Auto mode retries the preferred SV2 endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_retry_sv2_after_s: Option<u64>,
    /// Human-readable reason for the current temporary fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_fallback_reason: Option<String>,
    /// Read-only user-pool failover state emitted by the Stratum runtime.
    #[serde(default)]
    pub failover: dcentrald_stratum::types::PoolFailoverStatus,
    /// Provenance of `failover`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub failover_source: Option<String>,
    /// Read-only weighted user-hashrate split state emitted by the Stratum runtime.
    #[serde(default)]
    pub hashrate_split: dcentrald_stratum::types::HashrateSplitStatus,
    /// Provenance of `hashrate_split`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub hashrate_split_source: Option<String>,

    /// Last measured pool round-trip latency in milliseconds (share submit ->
    /// pool response). Mirrored from `StratumStats::latency_ms` via the daemon's
    /// `StratumStatus::Latency` handler so `/api/pools` + Prometheus can surface
    /// it (VNish `pools[].ping` parity). 0 = not yet measured (no submit/response
    /// round-trip observed) -- a fresh boot or a pool that has not ACKed a share.
    #[serde(default)]
    pub latency_ms: u64,
    /// Provenance of `latency_ms`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub latency_ms_source: Option<String>,

    /// Session reject counts bucketed by cause, indexed per
    /// [`REJECT_REASON_LABELS`]. The daemon's `StratumStatus::ShareRejected`
    /// handler classifies each reject (`classify_reject_reason`) and increments
    /// the bucket. Superior diagnostic observability vs the bare total count.
    #[serde(default)]
    pub reject_reason_counts: [u64; 6],
    /// Provenance of `reject_reason_counts`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub reject_reason_counts_source: Option<String>,

    /// W6.3: rolling 30-min pool acceptance percentage.
    ///
    /// Mirrored from `StratumStats::rolling_acceptance_pct` so the
    /// dashboard can render "Acceptance (last 30 min): X%" without
    /// reading the stratum stats Mutex on the hot poll path. The
    /// daemon's state publisher copies the value on every refresh
    /// tick.
    #[serde(default = "default_rolling_acceptance_pct_state")]
    pub rolling_acceptance_pct_30min: f64,

    /// W6.3: rolling 30-min `(accepted, total)` share counts.
    /// Lets the dashboard render "X / Y accepted (last 30 min)".
    #[serde(default)]
    pub rolling_acceptance_count_30min: (u32, u32),
    /// Provenance of `rolling_acceptance_pct_30min` and
    /// `rolling_acceptance_count_30min`.
    /// `None`/absent = legacy or unknown source. See [`PoolQualitySource`].
    #[serde(default)]
    pub rolling_acceptance_source: Option<String>,

    /// W6.4: worst per-chip HW error rate (0.0..=1.0).
    ///
    /// TEL-1 (2026-06-20): RESERVED / NOT WIRED — this field is currently
    /// always `None`. No state publisher populates it (every `PoolState`
    /// construction sets `None`, and `apply_quality_snapshot` does not touch
    /// it). The live worst-chip HW-error data DOES exist, but flows on a
    /// separate channel: the `autotuner_chip_health` WebSocket message and
    /// `GET /api/autotuner/chip-health`. Treat this field as a reserved
    /// always-null placeholder until a publisher wires it (do not read it as
    /// live telemetry). Earlier doc claimed it was "mirrored from
    /// `HwErrTracker::worst_chip()` by the daemon's state publisher" — that
    /// was an overclaim; no such mirroring is implemented.
    #[serde(default)]
    pub worst_chip_hw_err_rate: Option<f64>,
}

fn default_rolling_acceptance_pct_state() -> f64 {
    100.0
}

impl PoolState {
    /// Project a stratum-native pool-quality snapshot onto the API wire DTO.
    pub fn apply_quality_snapshot(
        &mut self,
        quality: &dcentrald_stratum::pool_quality::PoolQualitySnapshot,
    ) {
        self.encrypted = quality.encrypted;
        self.encrypted_source = pool_quality_source_tag(quality.sources.encrypted);
        if quality.encrypted {
            self.protocol = "sv2".to_string();
        } else if quality.sources.encrypted
            == dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS
        {
            self.protocol = "sv1".to_string();
        }
        self.sv2_session = quality.sv2_session.as_ref().map(Sv2SessionInfo::from);
        self.sv2_session_source = pool_quality_source_tag(quality.sources.sv2_session);

        self.donating = quality.donating;
        self.donation_active_url = quality.donation_active_url.clone();
        self.donation_active_worker = quality.donation_active_worker.clone();
        self.donation_pool_index = quality.donation_pool_index;
        self.donating_source = pool_quality_source_tag(quality.sources.donating);

        self.auto_fallback_active = quality.auto_fallback_active;
        self.auto_retry_sv2_after_s = quality.auto_retry_sv2_after_s;
        self.auto_fallback_reason = quality.auto_fallback_reason.clone();
        self.auto_fallback_source = pool_quality_source_tag(quality.sources.auto_fallback);

        if !quality.failover.active_pool_url.is_empty() && !self.donating {
            self.url = quality.failover.active_pool_url.clone();
        }
        self.failover = quality.failover.clone();
        self.failover_source = pool_quality_source_tag(quality.sources.failover);

        self.hashrate_split = quality.hashrate_split.clone();
        self.hashrate_split_source = pool_quality_source_tag(quality.sources.hashrate_split);

        self.latency_ms = quality.latency_ms;
        self.latency_ms_source = pool_quality_source_tag(quality.sources.latency_ms);
        self.reject_reason_counts = quality.reject_reason_counts;
        self.reject_reason_counts_source =
            pool_quality_source_tag(quality.sources.reject_reason_counts);
        self.rolling_acceptance_pct_30min = quality.rolling_acceptance_pct_30min;
        self.rolling_acceptance_count_30min = quality.rolling_acceptance_count_30min;
        self.rolling_acceptance_source =
            pool_quality_source_tag(quality.sources.rolling_acceptance);

        // FWT-2: project the REAL Stratum connection state onto `status` when a
        // `StateChanged` event has been observed. Publishers set a local-heuristic
        // status before calling this (e.g. the hybrid path's `accepted()>0`
        // proxy); the real state, once known, is more truthful and lets the
        // dashboard distinguish connecting / authorized / mining / disconnected /
        // auth_failed. `None` leaves the publisher's fallback intact.
        if let Some(state) = quality.connection_state.as_ref() {
            self.status =
                dcentrald_stratum::pool_quality::stratum_state_status_str(state).to_string();
        }
    }
}

/// SV2 session metadata — populated when connected via Stratum V2.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sv2SessionInfo {
    /// Noise protocol cipher suite identifier.
    #[serde(default)]
    pub cipher_suite: String,
    /// Time taken for Noise_NX handshake in milliseconds.
    #[serde(default)]
    pub handshake_latency_ms: u64,
    /// First 16 hex chars of pool static public key.
    #[serde(default)]
    pub pool_pubkey_fingerprint: String,
    /// Certificate validity start (unix timestamp).
    #[serde(default)]
    pub certificate_valid_from: u64,
    /// Certificate expiry (unix timestamp).
    #[serde(default)]
    pub certificate_not_after: u64,
    /// SV2 channel ID (assigned after OpenStandardMiningChannel).
    #[serde(default)]
    pub channel_id: Option<u32>,
    /// Noise protocol TX nonce counter.
    #[serde(default)]
    pub noise_nonce_tx: u64,
    /// Noise protocol RX nonce counter.
    #[serde(default)]
    pub noise_nonce_rx: u64,
    /// Total bytes encrypted (sent).
    #[serde(default)]
    pub bytes_encrypted: u64,
    /// Total bytes decrypted (received).
    #[serde(default)]
    pub bytes_decrypted: u64,
    /// Total SV2 messages sent.
    #[serde(default)]
    pub messages_sent: u64,
    /// Total SV2 messages received.
    #[serde(default)]
    pub messages_received: u64,
}

impl From<&dcentrald_stratum::pool_quality::PoolSv2SessionSnapshot> for Sv2SessionInfo {
    fn from(value: &dcentrald_stratum::pool_quality::PoolSv2SessionSnapshot) -> Self {
        Self {
            cipher_suite: value.cipher_suite.clone(),
            handshake_latency_ms: value.handshake_latency_ms,
            pool_pubkey_fingerprint: value.pool_pubkey_fingerprint.clone(),
            certificate_valid_from: value.certificate_valid_from,
            certificate_not_after: value.certificate_not_after,
            channel_id: value.channel_id,
            noise_nonce_tx: value.noise_nonce_tx,
            noise_nonce_rx: value.noise_nonce_rx,
            bytes_encrypted: value.bytes_encrypted,
            bytes_decrypted: value.bytes_decrypted,
            messages_sent: value.messages_sent,
            messages_received: value.messages_received,
        }
    }
}

/// SV2 Job Declaration custom-job bridge state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sv2CustomJobInfo {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub channel_id: Option<u32>,
    #[serde(default)]
    pub request_id: Option<u32>,
    #[serde(default)]
    pub template_id: Option<u64>,
    #[serde(default)]
    pub job_id: Option<u32>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub updated_at_s: u64,
}

/// A single SV2 protocol message record for the protocol inspector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sv2MessageRecord {
    /// Message direction: "sent" or "recv".
    #[serde(default)]
    pub direction: String,
    /// SV2 message type byte.
    #[serde(default)]
    pub msg_type: u8,
    /// Human-readable message name.
    #[serde(default)]
    pub msg_name: String,
    /// Timestamp in milliseconds (unix epoch).
    #[serde(default)]
    pub timestamp_ms: u64,
    /// Payload size in bytes.
    #[serde(default)]
    pub payload_size: usize,
}

fn default_protocol() -> String {
    "sv1".to_string()
}

/// Inputs needed to build a minimal `AppState` that boots the dashboard +
/// CGMiner API without requiring HAL / chains / autotuner / LED engine.
///
/// Used by proxy mode (`--stratum-proxy`) and `--s19j-hybrid` so :8080 + :4028
/// come up regardless of whether dcentrald is the chain driver. All
/// HAL-dependent fields default to "no data" / "disabled".
///
/// Pass primitives only — keeps `dcentrald-api` from depending on the
/// daemon's `Config` type and avoids a circular crate dependency.
#[derive(Debug, Clone)]
pub struct MinimalAppStateInputs {
    /// API ports + WebSocket toggle.
    pub api_config: ApiConfig,
    /// Pool URL displayed in the dashboard "Pool" tile.
    pub pool_url: String,
    /// Stratum protocol label ("sv1" / "sv2"). Free text — dashboard treats it
    /// as a hint, not a contract.
    pub pool_protocol: String,
    /// Initial OperatingMode (Home / Standard / Hacker).
    pub mode: OperatingMode,
    /// Firmware version string surfaced in /api/system/info.
    pub firmware_version: String,
    /// Initial fan PWM displayed in the dashboard. Use 0 when no live fan
    /// command has been read; PWM alone is not acoustic proof.
    pub fan_pwm: u8,
    /// Optional local-node block source configuration.
    pub network_block: NetworkBlockConfig,
    /// Autotuner profile directory (used by /api/autotuner/* endpoints).
    pub profile_path: String,
    /// Optional control board label (e.g. "Zynq am2-s19j (proxy mode)").
    pub control_board_label: String,
    /// Optional chip type label (e.g. "BM1362 (proxied)").
    pub chip_type_label: String,
    /// Optional live `MinerState` receiver supplied by a mining mode that
    /// publishes its own telemetry (e.g. the `--s19j-hybrid` standalone path).
    ///
    /// When `Some`, the API serves THIS receiver instead of a static
    /// default-empty `MinerState`, so `/api/status` shows live hashrate,
    /// per-chain state, and accepted/rejected shares. When `None` (the
    /// default for `--stratum-proxy` / idle paths that own no telemetry) the
    /// builder falls back to a local default-empty channel as before.
    ///
    /// Purely additive: callers that don't set this field (`..Default` style
    /// construction is not used here, but every existing literal omitting it
    /// is a compile error that the migration fills with `None`) keep the
    /// prior behaviour.
    pub external_state_rx: Option<watch::Receiver<MinerState>>,
}

/// Build a minimal [`AppState`] suitable for dashboard + CGMiner API only.
///
/// All HAL-dependent senders (LED, curtailment data, off-grid, solar) are
/// defaulted/disabled. Watch channels are constructed locally with default
/// state — proxy / hybrid callers can later replace them with live updates if
/// they spawn their own publishers, but the API server itself does not require
/// those publishers to exist.
///
/// IMPORTANT: this function does NOT spawn any tasks or open any sockets. Use
/// [`start_api_servers`] on the returned `Arc<AppState>` to bring the API up.
pub fn build_minimal_app_state(inputs: MinimalAppStateInputs) -> Arc<AppState> {
    build_minimal_app_state_with_hardware_mutation_gate(
        inputs,
        dcentrald_hal::platform::HardwareMutationGate::new_open(),
    )
}

/// Build a minimal [`AppState`] with mutation admission owned by the caller.
///
/// Mining modes that mutate hardware outside the API runtime must pass the
/// same gate to both owners. This prevents the API from minting an unrelated
/// open admission domain that teardown cannot close and drain. Callers that
/// do not own hardware should continue using [`build_minimal_app_state`],
/// whose compatibility behavior remains open-by-default.
pub fn build_minimal_app_state_with_hardware_mutation_gate(
    inputs: MinimalAppStateInputs,
    hardware_mutation_gate: dcentrald_hal::platform::HardwareMutationGate,
) -> Arc<AppState> {
    let initial_state = MinerState {
        hashrate_ghs: 0.0,
        hashrate_5s_ghs: 0.0,
        accepted: 0,
        rejected: 0,
        chains: Vec::new(),
        fans: FanState {
            pwm: inputs.fan_pwm,
            rpm: 0,
            per_fan: Vec::new(),
        },
        pool: PoolState {
            url: inputs.pool_url,
            worker: String::new(),
            status: "Proxied".to_string(),
            difficulty: 0.0,
            last_share_at: 0,
            protocol: inputs.pool_protocol,
            encrypted: false,
            encrypted_source: pool_quality_honest_default_source(),
            sv2_session: None,
            sv2_session_source: pool_quality_honest_default_source(),
            sv2_custom_job: None,
            donating: false,
            donating_source: pool_quality_honest_default_source(),
            donation_active_url: String::new(),
            donation_active_worker: String::new(),
            donation_pool_index: 0,
            share_efficiency: None,
            auto_fallback_active: false,
            auto_fallback_source: pool_quality_honest_default_source(),
            auto_retry_sv2_after_s: None,
            auto_fallback_reason: None,
            failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
            failover_source: pool_quality_honest_default_source(),
            hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
            hashrate_split_source: pool_quality_honest_default_source(),
            latency_ms: 0,
            latency_ms_source: pool_quality_honest_default_source(),
            reject_reason_counts: [0; 6],
            reject_reason_counts_source: pool_quality_honest_default_source(),
            // W6.3 / W6.4 — empty rolling window starts at the
            // honest "no rolling evidence of rejection" baseline; the
            // worst-chip HW err is `None` until at least one nonce
            // arrives.
            rolling_acceptance_pct_30min: 100.0,
            rolling_acceptance_count_30min: (0, 0),
            rolling_acceptance_source: pool_quality_honest_default_source(),
            worst_chip_hw_err_rate: None,
        },
        uptime_s: 0,
        firmware_version: inputs.firmware_version,
        mode: inputs.mode,
    };

    // If a mining mode supplied a live `MinerState` publisher, serve that
    // receiver so `/api/status` reflects real hashrate / chains / shares.
    // Otherwise keep the prior behaviour: a local default-empty channel whose
    // sender is dropped (static state).
    let state_rx = match inputs.external_state_rx {
        Some(rx) => rx,
        None => {
            let (_state_tx, state_rx) = watch::channel(initial_state);
            state_rx
        }
    };
    let (_mode_tx, mode_rx) = watch::channel(inputs.mode);
    let (stats_tx, _) = broadcast::channel::<String>(64);
    let (mining_sync_tx, _) = broadcast::channel::<String>(256);
    let (diag_tx, _) = broadcast::channel::<DiagnosticProgress>(32);
    let (autotuner_tx, _) = broadcast::channel::<String>(64);
    let (_power_tx, power_rx) = watch::channel(LivePowerEstimate::default());
    let (_autotuner_status_tx, autotuner_status_rx) =
        watch::channel(AutotunerRuntimeStatus::default());
    let (_autotuner_efficiency_tx, autotuner_efficiency_rx) =
        watch::channel(None::<EfficiencySnapshot>);
    let (_autotuner_chip_health_tx, autotuner_chip_health_rx) =
        watch::channel(None::<dcentrald_autotuner::LiveChipHealthState>);
    let (_autotuner_telemetry_tx, autotuner_telemetry_rx) =
        watch::channel(TelemetryExportState::default());
    let (_jd_status_tx, jd_status_rx) =
        watch::channel(dcentrald_stratum::v2::jd::JdStatus::default());

    let hardware_info = Arc::new(Mutex::new(HardwareInfo {
        control_board: inputs.control_board_label,
        chip_type: inputs.chip_type_label,
        ..HardwareInfo::default()
    }));

    let curtailment = Arc::new(tokio::sync::Mutex::new(CurtailmentController::new()));
    let power_calibration = Arc::new(std::sync::RwLock::new(PowerCalibration::default()));
    let psu_lock = Arc::new(std::sync::Mutex::new(()));
    let history_data = Arc::new(Mutex::new(Vec::new()));
    let recent_share_history = Arc::new(Mutex::new(Vec::new()));
    let solar_history = Arc::new(Mutex::new(Vec::new()));

    Arc::new(AppState {
        state_rx,
        mode_rx,
        stats_tx,
        mining_sync_tx,
        mining_pipeline_snapshot_rx: None,
        mining_pipeline_snapshot_stale_after_ms: MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        diagnostic_progress_tx: diag_tx.clone(),
        diagnostic_service: Arc::new(tokio::sync::Mutex::new(DiagnosticService::new(diag_tx))),
        autotuner_tx,
        config: inputs.api_config,
        network_block: inputs.network_block,
        jd_status_rx,
        profile_path: inputs.profile_path,
        led_tx: None,
        led_status_rx: None,
        curtailment,
        power_rx,
        power_calibration,
        psu_lock,
        hardware_mutation_gate,
        autotuner_status_rx,
        autotuner_efficiency_rx,
        autotuner_chip_health_rx,
        autotuner_telemetry_rx,
        autotuner_command_tx: None,
        history_data,
        recent_share_history,
        local_reject_ring: Arc::new(Mutex::new(
            dcentrald_api_types::share_validation::LocalRejectRing::with_default_capacity(),
        )),
        boot_progress: Arc::new(BootProgressSnapshot::new()),
        audit_ring: Arc::new(Mutex::new(
            dcentrald_api_types::audit_log::AuditRing::with_default_capacity(),
        )),
        room_temp_c10: std::sync::atomic::AtomicU32::new(0),
        hardware_info,
        // W13.D1: ships with default-Generic(Booting) phase. Cold-boot
        // orchestrators publish into this once the platform-dispatch
        // refactor lands (W14+).
        boot_phase_tracker: Arc::new(crate::boot_phase_tracker::BootPhaseTracker::new()),
        offgrid_rx: None,
        pid_state_rx: None,
        pid_command_tx: None,
        solar_rx: None,
        solar_history,
        config_cache: Arc::new(ConfigTableCache::new()),
    })
}

/// W13-A: variant of `build_minimal_app_state` that accepts a live
/// `autotuner_command_tx` sender. Used by integration tests to drive
/// the `PUT /api/profiles/silicon/active` runtime hop end-to-end.
///
/// Production callers should keep using `build_minimal_app_state` —
/// the daemon constructs the channel itself in `daemon.rs` and
/// assigns it directly into `AppState`.
pub fn build_minimal_app_state_with_autotuner_tx(
    inputs: MinimalAppStateInputs,
    autotuner_command_tx: mpsc::Sender<dcentrald_autotuner::AutoTunerCommand>,
) -> Arc<AppState> {
    let base = build_minimal_app_state(inputs);
    // `AppState` doesn't impl Clone (it owns broadcast/watch senders),
    // so we rebuild the wrapper Arc by extracting + reconstructing.
    // The cheap path: construct a fresh AppState by mirroring
    // `build_minimal_app_state` then injecting the sender. Easiest
    // mechanically — re-run the constructor body via a private
    // helper. To avoid duplicating that body we use the trick of
    // dropping the original Arc and rebuilding via mutable access
    // through `Arc::try_unwrap` (succeeds because `base` is not
    // shared yet).
    let mut state = match Arc::try_unwrap(base) {
        Ok(s) => s,
        Err(_) => {
            // Should be impossible — `build_minimal_app_state` never
            // shares the returned Arc. Fall back to a panic with a
            // clear message rather than silently returning a state
            // with the wrong tx.
            panic!(
                "build_minimal_app_state returned an Arc that was already shared; \
                 cannot inject autotuner_command_tx"
            );
        }
    };
    state.autotuner_command_tx = Some(autotuner_command_tx);
    Arc::new(state)
}

#[cfg(test)]
mod reject_reason_tests {
    use super::*;

    #[test]
    fn classify_prefers_stratum_error_codes() {
        assert_eq!(classify_reject_reason(23, ""), 0); // low difficulty
        assert_eq!(classify_reject_reason(21, ""), 1); // job not found / stale
        assert_eq!(classify_reject_reason(22, ""), 2); // duplicate
        assert_eq!(classify_reject_reason(24, ""), 4); // unauthorized
        assert_eq!(classify_reject_reason(25, ""), 4); // not subscribed
    }

    #[test]
    fn classify_falls_back_to_message_keywords() {
        assert_eq!(classify_reject_reason(0, "Low difficulty share"), 0);
        assert_eq!(classify_reject_reason(0, "JOB NOT FOUND"), 1);
        assert_eq!(classify_reject_reason(0, "stale share"), 1);
        assert_eq!(classify_reject_reason(0, "Duplicate share"), 2);
        assert_eq!(classify_reject_reason(0, "Above target"), 3);
        assert_eq!(classify_reject_reason(0, "Unauthorized worker"), 4);
    }

    #[test]
    fn classify_unknown_is_other_and_index_is_in_range() {
        let idx = classify_reject_reason(999, "some novel pool error");
        assert_eq!(idx, 5);
        // Every classification must index a real label (no OOB on the [u64;6]).
        assert!(idx < REJECT_REASON_LABELS.len());
        assert_eq!(REJECT_REASON_LABELS.len(), 6);
    }
}

#[cfg(test)]
mod minimal_app_state_tests {
    use super::*;

    fn minimal_inputs() -> MinimalAppStateInputs {
        MinimalAppStateInputs {
            api_config: ApiConfig::default(),
            pool_url: String::new(),
            pool_protocol: default_protocol(),
            mode: OperatingMode::Standard,
            firmware_version: "test".to_string(),
            fan_pwm: 10,
            network_block: NetworkBlockConfig::default(),
            profile_path: "/tmp/profiles".to_string(),
            control_board_label: "test-control".to_string(),
            chip_type_label: "test-chip".to_string(),
            external_state_rx: None,
        }
    }

    #[test]
    fn http_bind_addr_preserves_default_and_accepts_loopback_override() {
        let default = ApiConfig::default();
        assert_eq!(default.http_bind, "0.0.0.0");
        assert_eq!(
            http_bind_addr(&default.http_bind, default.http_port),
            "0.0.0.0:8080"
        );
        assert_eq!(http_bind_addr("127.0.0.1", 18080), "127.0.0.1:18080");
    }

    #[test]
    fn minimal_app_state_keeps_mining_pipeline_snapshot_receiver_absent() {
        let state = build_minimal_app_state(minimal_inputs());

        assert!(state.mining_pipeline_snapshot_rx.is_none());
    }

    #[test]
    fn minimal_app_state_keeps_default_mutation_admission_open() {
        let state = build_minimal_app_state(minimal_inputs());

        let lease = state.hardware_mutation_gate.try_acquire().unwrap();
        drop(lease);
    }

    #[test]
    fn minimal_app_state_uses_the_owner_supplied_mutation_gate() {
        let owner_gate = dcentrald_hal::platform::HardwareMutationGate::new_open();
        let state = build_minimal_app_state_with_hardware_mutation_gate(
            minimal_inputs(),
            owner_gate.clone(),
        );

        let lease = state.hardware_mutation_gate.try_acquire().unwrap();
        assert!(owner_gate
            .close_and_drain(std::time::Duration::ZERO)
            .is_err());
        drop(lease);
        owner_gate
            .close_and_drain(std::time::Duration::ZERO)
            .unwrap();
        assert!(state.hardware_mutation_gate.try_acquire().is_err());
    }

    #[test]
    fn minimal_app_state_preserves_closed_owner_admission() {
        let owner_gate = dcentrald_hal::platform::HardwareMutationGate::new_closed();
        let state = build_minimal_app_state_with_hardware_mutation_gate(
            minimal_inputs(),
            owner_gate.clone(),
        );

        assert!(owner_gate.try_acquire().is_err());
        assert!(state.hardware_mutation_gate.try_acquire().is_err());
        assert!(owner_gate
            .close_and_drain(std::time::Duration::ZERO)
            .is_ok());
    }

    #[test]
    fn thermal_supervisor_commissioning_state_splits_disabled_pending_running_unsupported() {
        assert_eq!(
            thermal_supervisor_commissioning_state(false, false, false),
            "unsupported"
        );
        assert_eq!(
            thermal_supervisor_commissioning_state(true, false, false),
            "disabled"
        );
        assert_eq!(
            thermal_supervisor_commissioning_state(true, true, false),
            "pending_tick"
        );
        assert_eq!(
            thermal_supervisor_commissioning_state(true, true, true),
            "running"
        );
    }

    ///  W1 — `push_audit_event` must never panic the caller, even
    /// if a prior holder of the audit-ring mutex paniced and poisoned the
    /// lock. Audit observability is best-effort.
    #[test]
    fn push_audit_event_handles_poisoned_lock() {
        let state = build_minimal_app_state(minimal_inputs());

        // Poison the audit_ring mutex by panicking inside a `lock()`
        // guard on a separate thread. The mutex is left in the poisoned
        // state.
        let ring = state.audit_ring.clone();
        let _ = std::thread::spawn(move || {
            let _guard = ring.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(state.audit_ring.is_poisoned());

        // The push helper must return without panicking. The event is
        // silently dropped — observability is best-effort.
        super::push_audit_event(
            &state,
            "rest_attempt",
            dcentrald_api_types::audit_log::AuditEvent::ModeChange {
                from: "standard".to_string(),
                to: "home".to_string(),
            },
        );
    }

    #[test]
    fn append_audit_record_to_path_writes_ndjson_lines() {
        let path = std::env::temp_dir().join(format!(
            "dcent_api_audit_{}_{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);

        let first = dcentrald_api_types::audit_log::AuditRecord::new(
            1,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "first".to_string(),
            },
        );
        let second = dcentrald_api_types::audit_log::AuditRecord::new(
            2,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "second".to_string(),
            },
        );

        append_audit_record_to_path(&path, &first).expect("write first audit row");
        append_audit_record_to_path(&path, &second).expect("write second audit row");

        let blob = std::fs::read_to_string(&path).expect("read audit log");
        let rows = dcentrald_api_types::audit_log::parse_ndjson_batch_lossy(&blob);
        assert_eq!(rows, vec![first, second]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trim_audit_log_to_max_bytes_retains_recent_complete_rows() {
        let path = std::env::temp_dir().join(format!(
            "dcent_api_audit_trim_{}_{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);

        let old = dcentrald_api_types::audit_log::AuditRecord::new(
            1,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "old".to_string(),
            },
        );
        let newest = dcentrald_api_types::audit_log::AuditRecord::new(
            2,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "newest".to_string(),
            },
        );

        let mut blob = old.to_ndjson_line().expect("old line");
        blob.push('\n');
        let newest_line = newest.to_ndjson_line().expect("newest line");
        blob.push_str(&newest_line);
        blob.push('\n');
        std::fs::write(&path, blob).expect("write audit fixture");

        trim_audit_log_to_max_bytes(&path, (newest_line.len() + 1) as u64).expect("trim audit log");

        let trimmed = std::fs::read_to_string(&path).expect("read trimmed audit log");
        let rows = dcentrald_api_types::audit_log::parse_ndjson_batch_lossy(&trimmed);
        assert_eq!(rows, vec![newest]);

        let _ = std::fs::remove_file(&path);
    }

    /// CFG-3 — the audit-log trim must rewrite the file atomically (tempfile +
    /// rename) and leave no temp artifact behind in the log directory. This
    /// pins that `trim_audit_log_to_max_bytes` routes through
    /// `atomic_io::atomic_write_bytes` (a non-atomic `std::fs::write` would
    /// open a crash window that empties the forensics log) and that the
    /// resulting content is still the most recent complete row.
    #[test]
    fn trim_audit_log_is_atomic_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!(
            "dcent_api_audit_trim_atomic_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create audit dir");
        let path = dir.join("audit.log");

        let old = dcentrald_api_types::audit_log::AuditRecord::new(
            1,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "old".to_string(),
            },
        );
        let newest = dcentrald_api_types::audit_log::AuditRecord::new(
            2,
            "test",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "unit".to_string(),
                message: "newest".to_string(),
            },
        );

        let mut blob = old.to_ndjson_line().expect("old line");
        blob.push('\n');
        let newest_line = newest.to_ndjson_line().expect("newest line");
        blob.push_str(&newest_line);
        blob.push('\n');
        std::fs::write(&path, blob).expect("write audit fixture");

        trim_audit_log_to_max_bytes(&path, (newest_line.len() + 1) as u64).expect("trim audit log");

        // Content is the newest complete row.
        let trimmed = std::fs::read_to_string(&path).expect("read trimmed audit log");
        let rows = dcentrald_api_types::audit_log::parse_ndjson_batch_lossy(&trimmed);
        assert_eq!(rows, vec![newest]);

        // No `.tmp.*` sibling left behind by the atomic rename — only the
        // audit log itself remains in the directory.
        let entries: Vec<String> = std::fs::read_dir(&dir)
            .expect("read audit dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["audit.log".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Start all API servers.
///
/// Spawns:
/// - CGMiner TCP listener on port 4028
/// - axum HTTP server on port 8080 (REST + WebSocket + dashboard)
///
/// Returns JoinHandles for both servers.
pub async fn start_api_servers(
    state: Arc<AppState>,
) -> Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)> {
    let cgminer_port = state.config.cgminer_port;
    let http_port = state.config.http_port;
    let http_bind = state.config.http_bind.clone();
    mark_daemon_started();

    // Initialize auth module with config-driven flags
    auth::init_auth_config(
        state.config.metrics_require_auth,
        state.config.websocket_tickets,
    );

    // SECURITY (W1.5, 2026-05-07): verify and auto-correct on-disk auth file
    // perms BEFORE binding any sockets. /data/dcent/auth.json must be 0o600
    // and /data/dcent/ must be 0o700. Wider perms are auto-tightened in
    // place; ownership drift (uid != 0) logs ERROR but does not fail closed
    // — fail-soft because losing access to /data/dcent on first boot would
    // brick the password-setup wizard, which is a worse user-facing failure
    // than a transient wide-perms window during the few hundred ms between
    // boot and the first verify_auth_file_perms() call. See auth.rs.
    if let Err(err) = auth::verify_auth_file_perms() {
        tracing::warn!(error = %err, "verify_auth_file_perms failed (non-fatal)");
    }

    // --- CGMiner TCP server on port 4028 ---
    let cgminer_state = state.clone();
    let cgminer_handle = tokio::spawn(async move {
        let server = cgminer::CgMinerServer::new(cgminer_state, cgminer_port);
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "CGMiner API server exited with error");
        }
    });

    // --- HTTP server on port 8080 (REST + WebSocket + dashboard) ---
    let http_state = state.clone();
    let http_handle = tokio::spawn(async move {
        // CORS (2026-04-11): Restrict origins to the miner's own addresses.
        // Previously used mirror_request() which reflects ANY origin — allows
        // malicious sites to make authenticated cross-origin requests if the
        // user has an active session. Now we only allow:
        //   - Same-origin (dashboard served from miner)
        //   - localhost (development)
        //   - The miner's .local hostname
        //
        // Since the miner's IP is dynamic, we use a predicate that checks
        // if the Origin matches known-safe patterns.
        let cors = tower_http::cors::CorsLayer::new()
            .allow_origin(tower_http::cors::AllowOrigin::predicate(
                |origin: &axum::http::HeaderValue, request_parts: &axum::http::request::Parts| {
                    let origin_str = origin.to_str().unwrap_or("");

                    // Allow localhost (any port) for development
                    if origin_str.starts_with("http://localhost")
                        || origin_str.starts_with("https://localhost")
                        || origin_str.starts_with("http://127.0.0.1")
                        || origin_str.starts_with("https://127.0.0.1")
                    {
                        return true;
                    }

                    // Allow .local mDNS hostnames (e.g., http://dcentos.local).
                    //  W10-D (A1-LOW-1): the previous predicate
                    // used `.contains(".local")`, which matched any
                    // origin that contained the substring `.local` —
                    // including hostile origins like
                    // `local.example.com` or `evil.local.attacker.io`.
                    // Now we strip the scheme (and an optional port),
                    // split the host on `.`, and require the LAST
                    // label to be exactly `local`. This is a strict
                    // TLD-style suffix match.
                    let host_only = origin_str
                        .trim_start_matches("http://")
                        .trim_start_matches("https://");
                    let host_no_port = host_only.split(':').next().unwrap_or("");
                    if host_no_port
                        .rsplit('.')
                        .next()
                        .map(|tld| tld == "local")
                        .unwrap_or(false)
                    {
                        return true;
                    }

                    // Allow if Origin matches the request's Host header
                    // (same-origin: dashboard served from the miner itself)
                    if let Some(host) = request_parts.headers.get("host") {
                        if let Ok(host_str) = host.to_str() {
                            // Origin is scheme://host[:port], Host is host[:port]
                            let origin_host = origin_str
                                .trim_start_matches("http://")
                                .trim_start_matches("https://");
                            if origin_host == host_str {
                                return true;
                            }
                        }
                    }

                    false
                },
            ))
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::ACCEPT,
                axum::http::header::AUTHORIZATION,
            ]);

        // Build the combined router: REST + WebSocket + dashboard.
        // Layer order (axum wraps bottom-up — last .layer() is outermost):
        //   .layer(auth)  — inner: runs second, checks Bearer/Basic
        //   .layer(cors)  — outer: runs first, handles OPTIONS preflight
        // This ensures CORS preflight passes through without hitting auth.
        let app = rest::build_router()
            .route("/ws", axum::routing::get(websocket::ws_handler))
            .route("/", axum::routing::get(dashboard::index_handler))
            .with_state(http_state)
            .layer(axum::middleware::from_fn(auth::auth_middleware))
            .layer(cors);

        let bind_addr = http_bind_addr(&http_bind, http_port);
        tracing::info!(bind = %http_bind, port = http_port, "HTTP API server listening");

        let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, addr = %bind_addr, "Failed to bind HTTP server");
                return;
            }
        };

        // Use into_make_service_with_connect_info to enable ConnectInfo<SocketAddr>
        // extraction in handlers (used by auth/setup rate limiter).
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            tracing::error!(error = %e, "HTTP server exited with error");
        }
    });

    tracing::info!(cgminer_port, http_port, "API servers started");

    Ok((cgminer_handle, http_handle))
}

// ─── LED Config Helpers ───────────────────────────────────────────────

/// Read the [led] section from a dcentrald.toml config file.
pub fn read_led_config(path: &str) -> anyhow::Result<serde_json::Value> {
    let contents = std::fs::read_to_string(path)?;
    let table: toml::Table = toml::from_str(&contents)?;
    if let Some(led) = table.get("led") {
        let json_str = serde_json::to_string(led)?;
        let val: serde_json::Value = serde_json::from_str(&json_str)?;
        Ok(val)
    } else {
        // Return defaults
        Ok(serde_json::json!({
            "enabled": true,
            "heartbeat_on_ms": 100,
            "heartbeat_off_ms": 900,
            "locate_pattern": "imperial_march",
            "locate_duration_s": 30,
            "flash_on_accepted_share": true,
            "flash_on_rejected_share": true,
            "night_mode_disable": false,
            "celebration_on_lucky_share": true,
            "chain_status_blink_codes": true,
        }))
    }
}

/// Update specific fields in the [led] section and save.
pub fn update_led_config(
    path: &str,
    update: &crate::rest::LedConfigUpdateRequest,
) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let mut table: toml::Table = toml::from_str(&contents)?;

    let led = table
        .entry("led".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));

    if let toml::Value::Table(ref mut led_table) = led {
        if let Some(v) = &update.locate_pattern {
            led_table.insert("locate_pattern".into(), toml::Value::String(v.clone()));
        }
        if let Some(v) = update.heartbeat_on_ms {
            led_table.insert("heartbeat_on_ms".into(), toml::Value::Integer(v as i64));
        }
        if let Some(v) = update.heartbeat_off_ms {
            led_table.insert("heartbeat_off_ms".into(), toml::Value::Integer(v as i64));
        }
        if let Some(v) = update.locate_duration_s {
            led_table.insert("locate_duration_s".into(), toml::Value::Integer(v as i64));
        }
        if let Some(v) = update.flash_on_accepted_share {
            led_table.insert("flash_on_accepted_share".into(), toml::Value::Boolean(v));
        }
        if let Some(v) = update.flash_on_rejected_share {
            led_table.insert("flash_on_rejected_share".into(), toml::Value::Boolean(v));
        }
        if let Some(v) = update.night_mode_disable {
            led_table.insert("night_mode_disable".into(), toml::Value::Boolean(v));
        }
        if let Some(v) = update.celebration_on_lucky_share {
            led_table.insert("celebration_on_lucky_share".into(), toml::Value::Boolean(v));
        }
        if let Some(v) = update.chain_status_blink_codes {
            led_table.insert("chain_status_blink_codes".into(), toml::Value::Boolean(v));
        }
        if let Some(v) = update.enabled {
            led_table.insert("enabled".into(), toml::Value::Boolean(v));
        }
    }

    let output = toml::to_string_pretty(&table)?;
    std::fs::write(path, output)?;
    Ok(())
}
