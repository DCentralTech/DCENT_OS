//! REST API endpoints for dcentrald.
//!
//! JSON endpoints served on port 8080 by the axum HTTP server.
//! Provides status, configuration, control, diagnostics, home,
//! and hacker debug endpoints. The dashboard and WebSocket are served
//! from the same axum server.
//!
//! Endpoint categories:
//!   /api/status          - Overall miner status (polled by dashboard every 5s)
//!   /api/network/block   - Honest Bitcoin block/source status for dashboard cards
//!   /api/mining/work/posture - Read-only pool/job/share provenance posture
//!   /api/mining/pipeline/snapshot/schema - Default-off pipeline snapshot contract
//!   /api/mining/pipeline/snapshot - Read-only unavailable-or-watch snapshot
//!   /api/stats           - Detailed per-chain, per-chip statistics
//!   /api/pools           - Pool configuration and status
//!   /api/config          - Configuration read/write
//!   /api/action/*        - Control actions (restart, reboot, sleep, wake)
//!   /api/system/info     - System identification (pyasic compatible)
//!   /api/system/stats    - Live Linux system telemetry
//!   /api/system/asic     - Per-ASIC data (AxeOS compatible for pyasic)
//!   /api/compatibility/* - Read-only API compatibility manifests
//!   /api/history         - Historical data (24h hashrate, temp, power)
//!   /api/profiles        - Tuning profile management
//!   /api/profiles/silicon-table - Read-only 21-step BM1362 silicon ladder
//!   /api/home/*        - Space Home mode endpoints
//!   /api/debug/*         - Hacker mode raw access (mode-gated)
//!   /api/led/*            - LED control (status, patterns, locate, config)
//!   /api/pool/sv2/*      - SV2 protocol status, handshake, messages
//!   /api/diagnostics/*   - Diagnostic test management

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::atomic_io::{atomic_write, storage_write_failure_kind, StorageWriteFailureKind};
use crate::{solar_provider_support, solar_transport as shared_solar_transport};
use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Json, Multipart, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use dcent_schema::capability::{
    AsicCapability, AsicFamily, BoardCapability, CapabilityError, CapabilityReferences,
    ControlBoardCapability, ControllerCapability, ControllerKind, DeviceCapabilityDescriptor,
    DeviceFamily, FailSafePolicy, FanControlMode, FanDescriptor, FanEnvelope, FanTopology,
    FrequencyEnvelope, HardwareIdentity, HashboardDescriptor, IdentityConfidence,
    InstallCapability, InstallCapabilityPlan, OperatingEnvelopes, PlannerOutcome, PowerCapability,
    ProofScope, PsuMode, RuntimeCapability, SafeDefaults, SupportTier as CapabilitySupportTier,
    TempSensorClass, TempSensorDescriptor, ThermalCapability, TopologyCapability,
    CAPABILITY_SCHEMA_VERSION, READ_ONLY_RUNTIME_CAPABILITIES,
};
use dcent_schema::config::{
    SharedAuthConfig, SharedConfigPatch, SharedConfigSnapshot, SharedMiningConfig,
    SharedNetworkConfig, SharedPoolConfig, SharedThermalConfig, CONFIG_SCHEMA_VERSION,
};
use dcent_schema::mcp::{minimal_profile, MCP_PROTOCOL_VERSION, MINIMAL_PROFILE_ID};
use dcent_schema::swarm::{
    DcentSwarmInfo, HomeControlMode, SwarmCoordinationStatus, SwarmDiscoveryInfo, SwarmNode,
    SwarmRole, SwarmRoomTempRequest, SwarmSource, SwarmStatus,
};
use dcent_schema::update::{
    InstallIntent, ToolboxPackageInfo, UpdateMetadata, UPDATE_SCHEMA_VERSION,
};
use dcentrald_asic::drivers::{MinerProfile, PicType};
use dcentrald_diagnostics::builders::{
    build_board_health_snapshot, build_chip_health_snapshot, build_hashreport_snapshot,
};
use dcentrald_diagnostics::report::ReportGenerator;
use dcentrald_diagnostics::snapshot::{
    SnapshotChain, SnapshotChipHealth, SnapshotContext, SnapshotHistorySample, SnapshotProfile,
    SnapshotProfileChip,
};
use dcentrald_diagnostics::{
    DiagnosticJobConfig, HashReportJobConfig, TestResult, TestStatus, TestType,
};
use reqwest::header::{HeaderValue, COOKIE};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::AppState;

const MCP_HTTP_PATH: &str = "/mcp";
const MCP_TRANSPORT: &str = "streamable-http";
const ERROR_ENVELOPE_BODY_LIMIT: usize = 64 * 1024;

fn api_error(
    status: StatusCode,
    code: impl Into<String>,
    error: impl Into<String>,
    suggestion: Option<&str>,
) -> Response {
    let mut body = dcentrald_api_types::ApiErrorBody::new(error).with_code(code);
    if let Some(suggestion) = suggestion {
        body = body.with_suggestion(suggestion);
    }
    (status, Json(body)).into_response()
}

fn pool_validation_error(message: impl Into<String>) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        dcentrald_api_types::api_error_codes::POOL_VALIDATION,
        message,
        Some("Check the pool URL format, worker name, and failover split settings."),
    )
}

#[derive(Debug)]
enum ConfigPersistenceError {
    BadRequest(String),
    Storage {
        kind: StorageWriteFailureKind,
        detail: String,
    },
}

impl ConfigPersistenceError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    fn from_io(action: &'static str, error: io::Error) -> Self {
        if let Some(kind) = storage_write_failure_kind(&error) {
            return Self::Storage {
                kind,
                detail: format!("{action}: {error}"),
            };
        }
        Self::BadRequest(format!("{action}: {error}"))
    }

    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(
                    dcentrald_api_types::ApiErrorBody::new(message)
                        .with_code(dcentrald_api_types::api_error_codes::CONFIG_VALIDATION)
                        .with_suggestion(
                            "Check the submitted config fields and retry with supported values.",
                        ),
                ),
            )
                .into_response(),
            Self::Storage { kind, detail } => {
                let (message, suggestion) = match kind {
                    StorageWriteFailureKind::StorageFull => (
                        "Persistent storage is full; configuration was not saved.",
                        "Free space under /data, rotate logs, or move snapshots off-device, then retry.",
                    ),
                    StorageWriteFailureKind::ReadOnly => (
                        "Persistent storage is read-only; configuration was not saved.",
                        "Check the /data mount state and filesystem health, then retry after it is writable.",
                    ),
                };
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        dcentrald_api_types::ApiErrorBody::new(message)
                            .with_code(kind.code())
                            .with_detail(detail)
                            .with_suggestion(suggestion),
                    ),
                )
                    .into_response()
            }
        }
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

fn is_text_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("text/plain")
}

fn error_body_bytes(body: dcentrald_api_types::ApiErrorBody) -> Vec<u8> {
    serde_json::to_vec(&body).unwrap_or_else(|_| br#"{"error":"Request failed"}"#.to_vec())
}

fn envelope_bytes(message: String) -> Vec<u8> {
    error_body_bytes(
        dcentrald_api_types::ApiErrorBody::new(message)
            .with_code(dcentrald_api_types::api_error_codes::UNCLASSIFIED_ERROR),
    )
}

fn legacy_error_body_from_json(
    value: &serde_json::Value,
) -> Option<dcentrald_api_types::ApiErrorBody> {
    let object = value.as_object()?;
    let status = object.get("status")?.as_str()?;
    if !status.eq_ignore_ascii_case("error") {
        return None;
    }
    let message = object.get("message")?.as_str()?.trim();
    if message.is_empty() {
        return None;
    }

    let mut body = dcentrald_api_types::ApiErrorBody::new(message.to_string())
        .with_code(dcentrald_api_types::api_error_codes::LEGACY_ERROR);
    if let Some(detail) = object
        .get("detail")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_detail(detail.to_string());
    }
    if let Some(code) = object
        .get("code")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_code(code.to_string());
    }
    if let Some(suggestion) = object
        .get("suggestion")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_suggestion(suggestion.to_string());
    }
    Some(body)
}

fn legacy_error_envelope_bytes(value: &serde_json::Value) -> Option<Vec<u8>> {
    legacy_error_body_from_json(value).map(error_body_bytes)
}

async fn normalize_api_error_response(response: Response) -> Response {
    if response.status().is_success() {
        return response;
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let is_text = is_text_content_type(&content_type);
    let is_json = is_json_content_type(&content_type);
    if !is_text && !is_json {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, ERROR_ENVELOPE_BODY_LIMIT).await else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            dcentrald_api_types::api_error_codes::ERROR_BODY_UNAVAILABLE,
            "The daemon could not normalize the error response body.",
            None,
        );
    };

    let envelope = if is_text {
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        (!text.is_empty()).then(|| envelope_bytes(text))
    } else {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::String(message)) if !message.trim().is_empty() => {
                Some(envelope_bytes(message.trim().to_string()))
            }
            Ok(value) => legacy_error_envelope_bytes(&value),
            _ => None,
        }
    };

    if let Some(envelope) = envelope {
        parts.headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return Response::from_parts(parts, Body::from(envelope));
    }

    Response::from_parts(parts, Body::from(bytes))
}

// Pool-state predicates for API/UI/metrics surfaces. Keep these evidence levels
// separate: connecting is not connected, and connected is not necessarily mining.
fn pool_status_matches(status: &str, states: &[&str]) -> bool {
    let status = status.trim();
    states
        .iter()
        .any(|candidate| status.eq_ignore_ascii_case(candidate))
}

fn is_pool_connecting(status: &str) -> bool {
    status.trim().eq_ignore_ascii_case("connecting")
}

/// The canonical set of pool-status strings that mean "connected". Single source
/// of truth shared by `is_pool_connected` AND the MQTT/HA `binary_sensor.pool`
/// value_template (`mqtt.rs`), so the two can never drift (R-O1: the template had
/// hardcoded a 3-element subset that omitted `Donating`/`Authorized`, so Home
/// Assistant showed "Pool Connected = OFF" during every donation window while the
/// miner was connected + hashing). A drift guard in the mqtt tests pins the
/// template against this list.
pub(crate) const POOL_CONNECTED_STATUSES: &[&str] = &[
    "Alive",
    "Donating",
    "Connected",
    "Authorized",
    "Active",
    "Mining",
];

fn is_pool_connected(status: &str) -> bool {
    pool_status_matches(status, POOL_CONNECTED_STATUSES)
}

/// Check if a pool status string represents a mining-capable Stratum session.
/// "Donating" is active because donation pool mining is fully functional.
/// "Connecting" is deliberately excluded: a socket attempt is not connected,
/// authorized, job-fresh, or mining-capable telemetry.
fn is_pool_mining_capable(status: &str) -> bool {
    pool_status_matches(status, &["Alive", "Donating", "Active", "Mining"])
}

// ---------------------------------------------------------------------------
// RE-011 — network-difficulty fetcher + space-heater heat-reuse credit.
//
// A small, FAILURE-TOLERANT background task polls mempool.space for the
// current Bitcoin network difficulty so the profitability / space-heater-ROI
// surfaces can show solo-mining odds without baking a value into firmware. It
// is OPT-OUT-by-being-offline: every network error is swallowed (the cache
// just stays empty / stale), so an offline / LAN-only home unit never panics
// or blocks. The cache is read by `/api/stats`'s profitability summary.
//
// The heat-reuse credit is the space-heater economic insight: a miner in a
// room you would otherwise heat with resistive electric heat earns back the
// value of that displaced heating. It's a pure function of wall watts +
// electricity rate + how much of the heat actually offsets heating
// (`heating_offset_fraction`, 0..1). No network call — host-testable.
// ---------------------------------------------------------------------------

/// Cached network-difficulty sample from mempool.space. `None` until the first
/// successful fetch; carries the fetch timestamp so readers can show staleness.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct NetworkDifficultyCache {
    /// Current network difficulty (unitless).
    pub difficulty: f64,
    /// Estimated next-adjustment difficulty change percent (signed).
    pub difficulty_change_percent: f64,
    /// Estimated network hashrate in EH/s, if available.
    pub network_hashrate_ehs: Option<f64>,
    /// Unix ms when this sample was fetched.
    pub fetched_at_ms: u64,
    /// Source URL for transparency.
    pub source: &'static str,
}

static NETWORK_DIFFICULTY_CACHE: std::sync::OnceLock<
    std::sync::RwLock<Option<NetworkDifficultyCache>>,
> = std::sync::OnceLock::new();

fn network_difficulty_cell() -> &'static std::sync::RwLock<Option<NetworkDifficultyCache>> {
    NETWORK_DIFFICULTY_CACHE.get_or_init(|| std::sync::RwLock::new(None))
}

/// Read the most recent cached network difficulty (if any).
pub(crate) fn cached_network_difficulty() -> Option<NetworkDifficultyCache> {
    network_difficulty_cell()
        .read()
        .ok()
        .and_then(|g| g.clone())
}

/// P0-4 (C-5/C-6): canonical, network-difficulty-anchored daily-sats estimator.
///
/// A miner at `hashrate_ths` TH/s performs `hashrate_ths * 1e12` hashes per
/// second. The network expects `network_difficulty * 2^32` hashes per block,
/// so the miner finds blocks at rate `hashes_per_sec / hashes_per_block`. Over
/// a day that is `hashes_per_sec * 86_400 / hashes_per_block` blocks, each
/// worth `block_subsidy_sats` satoshis:
///
///   sats/day = hashrate_ths * 1e12 * 86_400 / (network_difficulty * 2^32)
///              * block_subsidy_sats
///
/// Returns `None` when any input is non-positive — most importantly when the
/// live network difficulty is absent — so the caller can label the surface an
/// "uncalibrated estimate" instead of emitting a fabricated number. This is the
/// ONE canonical estimator: the old inflated path multiplied accepted-share
/// count by share difficulty with no network term (~22,000x too high), and the
/// dashboard's `satsPerThPerDay=5` stub ignored its inputs entirely.
pub(crate) fn estimate_daily_sats_network_anchored(
    hashrate_ths: f64,
    network_difficulty: f64,
    block_subsidy_sats: f64,
) -> Option<u64> {
    fn is_strictly_positive(value: f64) -> bool {
        matches!(value.partial_cmp(&0.0), Some(std::cmp::Ordering::Greater))
    }

    if !is_strictly_positive(hashrate_ths)
        || !is_strictly_positive(network_difficulty)
        || !is_strictly_positive(block_subsidy_sats)
    {
        return None;
    }
    let hashes_per_sec = hashrate_ths * 1e12;
    let hashes_per_block = network_difficulty * 4_294_967_296.0_f64; // 2^32
    let blocks_per_day = hashes_per_sec * 86_400.0 / hashes_per_block;
    let sats = blocks_per_day * block_subsidy_sats;
    if sats.is_finite() && sats >= 0.0 {
        Some(sats.round() as u64)
    } else {
        None
    }
}

/// RE-011: parse a mempool.space `/api/v1/difficulty-adjustment` JSON body into
/// a cache sample. Pure (no I/O) so the parse is host-testable. mempool.space
/// returns e.g. `{"difficultyChange": 1.23, "previousRetarget": ...,
/// "estimatedRetargetDate": ...}` — note this endpoint does NOT carry the raw
/// difficulty, so we accept an optional companion `difficulty` (from the
/// `/api/v1/mining/hashrate/1d` body's `currentDifficulty`) and hashrate.
fn parse_difficulty_adjustment(
    adjustment_body: &serde_json::Value,
    difficulty: Option<f64>,
    network_hashrate_ehs: Option<f64>,
    now_ms: u64,
) -> Option<NetworkDifficultyCache> {
    let change = adjustment_body
        .get("difficultyChange")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    // Difficulty itself comes from the companion body; if neither endpoint gave
    // us a raw difficulty, we still record the change (difficulty 0.0 signals
    // "unknown" to readers, which they treat as null).
    Some(NetworkDifficultyCache {
        difficulty: difficulty.unwrap_or(0.0),
        difficulty_change_percent: change,
        network_hashrate_ehs,
        fetched_at_ms: now_ms,
        source: "mempool.space",
    })
}

/// RE-011: background task that refreshes the network-difficulty cache from
/// mempool.space. **Failure-tolerant by construction** — every error path is a
/// `continue` (the cache keeps its last good value, or stays empty), so an
/// offline / firewalled home unit never panics. Polls every 10 minutes
/// (difficulty only changes every ~2016 blocks). Spawn this from the daemon
/// once at startup; it is a no-op on units with no internet.
///
/// NOTE: this performs OUTBOUND HTTP to a third party. It is gated by the
/// daemon (only spawned when the operator hasn't disabled external calls);
/// this function itself just does the polling loop.
pub async fn run_difficulty_fetcher() {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("dcentrald")
        .build()
    {
        Ok(c) => c,
        // Can't build a client → never poll (failure-tolerant: no panic).
        Err(error) => {
            tracing::warn!(%error, "RE-011 difficulty fetcher: client build failed; disabled");
            return;
        }
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
    loop {
        interval.tick().await;
        // Fetch the difficulty-adjustment estimate (change %).
        let adjustment = match client
            .get("https://mempool.space/api/v1/difficulty-adjustment")
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
            Err(error) => {
                tracing::debug!(%error, "RE-011 difficulty fetcher: adjustment fetch failed (offline?)");
                None
            }
        };
        // Fetch raw difficulty + network hashrate (best-effort companion).
        let (difficulty, hashrate_ehs) = match client
            .get("https://mempool.space/api/v1/mining/hashrate/1d")
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(body) => {
                    let diff = body.get("currentDifficulty").and_then(|v| v.as_f64());
                    // currentHashrate is in H/s; convert to EH/s.
                    let hr = body
                        .get("currentHashrate")
                        .and_then(|v| v.as_f64())
                        .map(|h| h / 1e18);
                    (diff, hr)
                }
                Err(_) => (None, None),
            },
            Err(error) => {
                tracing::debug!(%error, "RE-011 difficulty fetcher: hashrate fetch failed (offline?)");
                (None, None)
            }
        };

        if let Some(adj) = adjustment {
            if let Some(sample) =
                parse_difficulty_adjustment(&adj, difficulty, hashrate_ehs, unix_time_ms())
            {
                if let Ok(mut guard) = network_difficulty_cell().write() {
                    *guard = Some(sample);
                }
            }
        }
    }
}

/// RE-011: space-heater heat-reuse credit (USD/day).
///
/// When a miner runs in a space you would otherwise heat with resistive
/// electric heat, the waste heat it produces displaces that heating. The
/// credit = the value of the electricity you would have spent on a resistive
/// heater to produce the same usable heat. A resistive heater is ~100%
/// efficient (1 W electric = 1 W heat), and the miner's wall watts are ~100%
/// converted to heat too, so the displaced electricity ≈ `wall_watts ×
/// heating_offset_fraction`, valued at the electricity rate.
///
/// `heating_offset_fraction` (0..1) is the share of the miner's heat that
/// actually offsets heating you would otherwise pay for (1.0 in winter in the
/// heated room; 0.0 in summer / a space you don't heat). Pure + host-testable.
fn compute_heat_reuse_credit_usd_per_day(
    wall_watts: f64,
    electricity_rate_usd_per_kwh: f64,
    heating_offset_fraction: f64,
) -> f64 {
    let frac = heating_offset_fraction.clamp(0.0, 1.0);
    let w = wall_watts.max(0.0);
    let rate = electricity_rate_usd_per_kwh.max(0.0);
    // displaced kWh/day × rate
    (w * frac * 24.0 / 1000.0) * rate
}

#[derive(Debug, Clone, Serialize)]
struct PowerTargetingState {
    active: bool,
    source: Option<String>,
    mode: Option<String>,
    preset: Option<String>,
    schedule_label: Option<String>,
    target_watts: Option<u32>,
    current_wall_watts: u32,
    current_wall_watts_measured: bool,
    current_wall_watts_source_detail: Option<&'static str>,
    delta_watts: Option<i32>,
    comparison: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FleetMinersResponse {
    generated_at_ms: u64,
    miners: Vec<FleetMinerSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct FleetMinerSummary {
    id: String,
    hostname: String,
    ip: String,
    model: String,
    hashrate_ghs: f64,
    temp_c: f64,
    fan_pwm: u8,
    status: FleetMinerStatus,
    last_seen_ms: u64,
    /// PR-048b: pool-assigned target difficulty for this miner's last share
    /// (pool credit / minimum-work evidence). `None` when no mining pipeline
    /// snapshot publisher is wired or no share has been observed yet. This is
    /// NOT achieved difficulty — never infer lucky-share proof from it.
    /// Mirrors the  `ShareAccepted.pool_target_difficulty` /
    ///  `MiningPipelineSnapshot.last_share_target_difficulty` contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pool_target_difficulty: Option<f64>,
    /// PR-048b: locally proven achieved difficulty of this miner's last share.
    /// `None` means "not locally proven" — consumers MUST NOT fall back to
    /// `pool_target_difficulty`. Mirrors the
    /// `ShareAccepted.achieved_difficulty` /
    /// `MiningPipelineSnapshot.last_share_achieved_difficulty` contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    achieved_difficulty: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FleetMinerStatus {
    Alive,
    Dead,
    Starting,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FleetDiscoverRequest {
    #[serde(default)]
    include_configured: bool,
    #[serde(default)]
    manual_ips: Vec<String>,
    #[serde(default)]
    hint_ips: Vec<String>,
}

#[derive(Debug, Clone)]
struct ConfiguredPowerTarget {
    source: String,
    mode: Option<String>,
    preset: Option<String>,
    schedule_label: Option<String>,
    target_watts: Option<u32>,
}

fn dcentrald_config_path() -> &'static str {
    if Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn current_hour_with_offset(offset_hours: i8) -> u8 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let total_hours = (secs / 3600) as i64;
    (((total_hours + offset_hours as i64) % 24 + 24) % 24) as u8
}

fn hour_in_schedule_slot(hour: u8, start_hour: u8, end_hour: u8) -> bool {
    if start_hour == end_hour {
        return true;
    }
    if start_hour < end_hour {
        hour >= start_hour && hour < end_hour
    } else {
        hour >= start_hour || hour < end_hour
    }
}

fn read_configured_power_target(mode: crate::OperatingMode) -> Option<ConfiguredPowerTarget> {
    let contents = std::fs::read_to_string(dcentrald_config_path()).ok()?;
    let table: toml::Table = toml::from_str(&contents).ok()?;

    if mode == crate::OperatingMode::Home {
        if let Some(home) = table.get("home").and_then(|value| value.as_table()) {
            let target_watts = home
                .get("target_watts")
                .and_then(|value| value.as_integer())
                .filter(|value| *value > 0)
                .map(|value| value as u32);
            let preset = home
                .get("preset")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if target_watts.is_some() || preset.is_some() {
                return Some(ConfiguredPowerTarget {
                    source: "home".to_string(),
                    mode: Some("power".to_string()),
                    preset,
                    schedule_label: None,
                    target_watts,
                });
            }
        }
    }

    if let Some(autotuner) = table.get("autotuner").and_then(|value| value.as_table()) {
        if let Some(schedule) = autotuner.get("schedule").and_then(|value| value.as_table()) {
            let enabled = schedule
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if enabled {
                let now_hour = current_hour_with_offset(
                    schedule
                        .get("timezone_offset_hours")
                        .and_then(|value| value.as_integer())
                        .unwrap_or(0) as i8,
                );
                if let Some(slots) = schedule.get("slots").and_then(|value| value.as_array()) {
                    for slot in slots {
                        let Some(slot_table) = slot.as_table() else {
                            continue;
                        };
                        let start_hour = slot_table
                            .get("start_hour")
                            .and_then(|value| value.as_integer())
                            .unwrap_or(0) as u8;
                        let end_hour = slot_table
                            .get("end_hour")
                            .and_then(|value| value.as_integer())
                            .unwrap_or(0) as u8;
                        if !hour_in_schedule_slot(now_hour, start_hour, end_hour) {
                            continue;
                        }
                        let target_watts = slot_table
                            .get("target_watts")
                            .and_then(|value| value.as_integer())
                            .filter(|value| *value > 0)
                            .map(|value| value as u32);
                        if target_watts.is_some() {
                            return Some(ConfiguredPowerTarget {
                                source: "schedule".to_string(),
                                mode: Some("power".to_string()),
                                preset: None,
                                schedule_label: slot_table
                                    .get("label")
                                    .and_then(|value| value.as_str())
                                    .map(|value| value.to_string()),
                                target_watts,
                            });
                        }
                    }
                }
            }
        }

        let preset = autotuner
            .get("preset")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
        let mode = autotuner
            .get("target_mode")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .or_else(|| preset.as_ref().map(|_| "power".to_string()));
        let target_watts = autotuner
            .get("target_watts")
            .and_then(|value| value.as_integer())
            .filter(|value| *value > 0)
            .map(|value| value as u32);

        if mode.is_some() || target_watts.is_some() || preset.is_some() {
            return Some(ConfiguredPowerTarget {
                source: "autotuner".to_string(),
                mode,
                preset,
                schedule_label: None,
                target_watts,
            });
        }
    }

    None
}

fn build_power_targeting_state_from_configured(
    configured: Option<ConfiguredPowerTarget>,
    projection: &PowerTelemetryProjection,
) -> PowerTargetingState {
    let measured_wall_watts = measured_wall_watts_for_unprovenanced_surface(projection);
    let current_wall_watts = measured_wall_watts.max(0.0).round() as u32;
    let current_wall_watts_measured = current_wall_watts > 0;
    let delta_watts = configured.as_ref().and_then(|target| {
        if !current_wall_watts_measured {
            return None;
        }
        target
            .target_watts
            .map(|target_watts| current_wall_watts as i32 - target_watts as i32)
    });
    let comparison = delta_watts.and_then(|delta| {
        let target_watts = configured.as_ref()?.target_watts? as i32;
        let tolerance = ((target_watts as f64) * 0.03).round() as i32;
        let tolerance = tolerance.max(25);
        Some(if delta.abs() <= tolerance {
            "near".to_string()
        } else if delta > 0 {
            "over".to_string()
        } else {
            "under".to_string()
        })
    });

    PowerTargetingState {
        active: configured.is_some(),
        source: configured.as_ref().map(|target| target.source.clone()),
        mode: configured.as_ref().and_then(|target| target.mode.clone()),
        preset: configured.as_ref().and_then(|target| target.preset.clone()),
        schedule_label: configured
            .as_ref()
            .and_then(|target| target.schedule_label.clone()),
        target_watts: configured.as_ref().and_then(|target| target.target_watts),
        current_wall_watts,
        current_wall_watts_measured,
        current_wall_watts_source_detail: if current_wall_watts_measured {
            Some(projection.source_detail)
        } else {
            None
        },
        delta_watts,
        comparison,
    }
}

fn build_power_targeting_state(
    mode: crate::OperatingMode,
    projection: &PowerTelemetryProjection,
) -> PowerTargetingState {
    build_power_targeting_state_from_configured(read_configured_power_target(mode), projection)
}

fn chip_type_to_chip_id(chip_type: &str) -> Option<u16> {
    match chip_type.trim().to_ascii_uppercase().as_str() {
        "BM1387" => Some(0x1387),
        "BM1397" => Some(0x1397),
        "BM1398" => Some(0x1398),
        "BM1362" => Some(0x1362),
        "BM1366" => Some(0x1366),
        "BM1368" => Some(0x1368),
        "BM1370" => Some(0x1370),
        _ => None,
    }
}

fn push_runtime_cap(caps: &mut Vec<RuntimeCapability>, cap: RuntimeCapability) {
    if !caps.contains(&cap) {
        caps.push(cap);
    }
}

fn source_contains_any(sources: &[String], needles: &[&str]) -> bool {
    sources.iter().any(|source| {
        let source = source.to_ascii_lowercase();
        needles.iter().any(|needle| source.contains(needle))
    })
}

fn capability_identity_confidence(
    hw: &crate::HardwareInfo,
    profile: Option<&MinerProfile>,
) -> IdentityConfidence {
    match hw
        .identification
        .confidence
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "exact" => IdentityConfidence::Exact,
        "high" => IdentityConfidence::High,
        "medium" | "low" => IdentityConfidence::Low,
        _ if profile.is_some() && !hw.chip_type.trim().is_empty() => IdentityConfidence::Low,
        _ => IdentityConfidence::Unknown,
    }
}

fn capability_identity_sources(hw: &crate::HardwareInfo) -> Vec<String> {
    let mut sources = hw.identification.sources.clone();
    if !hw.chip_type.trim().is_empty() {
        let source = format!("hardware_info.chip_type:{}", hw.chip_type.trim());
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    if !hw.control_board.trim().is_empty() {
        let source = format!("hardware_info.control_board:{}", hw.control_board.trim());
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources
}

fn capability_identity_note(
    hw: &crate::HardwareInfo,
    confidence: IdentityConfidence,
) -> Option<String> {
    if confidence == IdentityConfidence::Unknown {
        return hw.identification.note.clone().or_else(|| {
            Some("hardware identity has not been resolved; runtime is read-only".to_string())
        });
    }

    if hw.identification.sources.is_empty() {
        return Some(
            "ASIC family is derived from HardwareInfo.chip_type only; exact SKU tier is not inferred"
                .to_string(),
        );
    }

    hw.identification.note.clone()
}

fn antminer_beta_anchor(chip_id: Option<u16>, sources: &[String]) -> bool {
    match chip_id {
        // BM1387 is the S9 beta anchor in the current public support matrix.
        Some(0x1387) => true,
        // BM1362 is not enough by itself: plain S19j/S19j Pro routes and
        // non-Zynq control boards exist. Require the exact AM2 S19j Pro
        // evidence tags emitted by the daemon identity resolver.
        Some(0x1362) => source_contains_any(sources, &["am2-s19jpro-zynq", "s19jproam2"]),
        _ => false,
    }
}

fn antminer_support_tier(
    chip_id: Option<u16>,
    profile: Option<&MinerProfile>,
    sources: &[String],
) -> CapabilitySupportTier {
    if antminer_beta_anchor(chip_id, sources) {
        return CapabilitySupportTier::Beta;
    }

    match chip_id {
        Some(0x1489) => CapabilitySupportTier::Unsupported,
        Some(0x1372 | 0x1373) => CapabilitySupportTier::Unknown, // NF-05: S23 dual-key
        Some(0x1397 | 0x1398 | 0x1362 | 0x1366 | 0x1368 | 0x1370) if profile.is_some() => {
            CapabilitySupportTier::Experimental
        }
        _ => CapabilitySupportTier::Unknown,
    }
}

fn capability_baud_for_chip(chip_id: Option<u16>) -> Option<u32> {
    match chip_id {
        // Pin the Antminer BM1366/BM1368/BM1370 runtime convention. Do not
        // inherit ESP/Bitaxe's 1 Mbaud default through this shared contract.
        Some(0x1366 | 0x1368 | 0x1370) => Some(3_125_000),
        _ => None,
    }
}

fn voltage_control_label(profile: Option<&MinerProfile>) -> Option<String> {
    profile.map(|profile| match profile.pic_type {
        PicType::Pic16F1704 => "pic16f1704".to_string(),
        PicType::DsPic33EP => "dspic33ep".to_string(),
        PicType::NoPic => "nopic".to_string(),
    })
}

fn topology_temp_sensors(miner: &crate::MinerState) -> Vec<String> {
    let mut sensors: Vec<String> = miner
        .chains
        .iter()
        .filter_map(|chain| chain.temp_source.clone())
        .filter(|source| !source.trim().is_empty())
        .collect();
    sensors.sort();
    sensors.dedup();
    sensors
}

fn antminer_hashboards(
    miner: &crate::MinerState,
    profile: Option<&MinerProfile>,
    chip_id: Option<u16>,
) -> Vec<HashboardDescriptor> {
    if !miner.chains.is_empty() {
        return miner
            .chains
            .iter()
            .enumerate()
            .map(|(index, chain)| HashboardDescriptor {
                index: u8::try_from(index).ok(),
                chain_index: Some(chain.id),
                chip_model: profile.map(|profile| profile.name.to_string()),
                asic_family: AsicFamily::BitmainBm13xx,
                chip_id,
                chips_per_chain: if chain.chips > 0 {
                    Some(chain.chips as u16)
                } else {
                    profile.map(|profile| profile.chips_per_chain as u16)
                },
                present: Some(chain.chips > 0),
                serial: None,
            })
            .collect();
    }

    profile
        .map(|profile| {
            (0..profile.chain_count)
                .map(|index| HashboardDescriptor {
                    index: Some(index),
                    chain_index: profile.chain_ids.get(index as usize).copied(),
                    chip_model: Some(profile.name.to_string()),
                    asic_family: AsicFamily::BitmainBm13xx,
                    chip_id,
                    chips_per_chain: Some(profile.chips_per_chain as u16),
                    present: None,
                    serial: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn antminer_fan_topology(miner: &crate::MinerState) -> FanTopology {
    let fan_count = if !miner.fans.per_fan.is_empty() {
        u8::try_from(miner.fans.per_fan.len()).ok()
    } else if miner.fans.rpm > 0 {
        Some(1)
    } else {
        None
    };
    let per_fan: Vec<FanDescriptor> = miner
        .fans
        .per_fan
        .iter()
        .map(|fan| FanDescriptor {
            index: Some(fan.id),
            tach_channel: Some(fan.id),
            pwm_channel: Some(fan.id),
            label: Some(format!("fan{}", fan.id)),
        })
        .collect();

    FanTopology {
        control_mode: if fan_count.is_some() {
            FanControlMode::PwmAndTach
        } else {
            FanControlMode::Unknown
        },
        fan_count,
        tach_channels: per_fan.iter().filter_map(|fan| fan.tach_channel).collect(),
        pwm_channels: per_fan.iter().filter_map(|fan| fan.pwm_channel).collect(),
        per_fan,
    }
}

fn antminer_temp_sensor_descriptors(miner: &crate::MinerState) -> Vec<TempSensorDescriptor> {
    topology_temp_sensors(miner)
        .into_iter()
        .enumerate()
        .map(|(index, source)| {
            let class = match source.as_str() {
                "soc_die_fallback" => TempSensorClass::Xadc,
                "board_sensor" => TempSensorClass::BoardI2c,
                _ => TempSensorClass::Unknown,
            };
            TempSensorDescriptor {
                class,
                name: Some(source),
                bus: None,
                address: None,
                index: u8::try_from(index).ok(),
                fallback_order: u8::try_from(index).ok(),
            }
        })
        .collect()
}

fn antminer_controller_kind(profile: &MinerProfile) -> ControllerKind {
    match profile.pic_type {
        PicType::Pic16F1704 => ControllerKind::Pic16f1704,
        PicType::DsPic33EP => ControllerKind::Dspic33ep,
        PicType::NoPic => ControllerKind::TasNoPic,
    }
}

fn antminer_controller_capabilities(
    profile: Option<&MinerProfile>,
    board_target: &str,
) -> Vec<ControllerCapability> {
    let Some(profile) = profile else {
        return Vec::new();
    };
    let write_denied_addrs = if board_target == "am1-s9" {
        Vec::new()
    } else {
        (0x50..=0x57).collect()
    };

    vec![ControllerCapability {
        kind: antminer_controller_kind(profile),
        fw_version: None,
        write_denied_addrs,
        degraded_fw_refuse: matches!(profile.pic_type, PicType::DsPic33EP),
    }]
}

fn antminer_operating_envelopes(profile: Option<&MinerProfile>) -> OperatingEnvelopes {
    OperatingEnvelopes {
        frequency: profile.map(|profile| FrequencyEnvelope {
            min_mhz: None,
            max_mhz: Some(profile.max_freq_mhz),
            step_mhz: None,
        }),
        voltage: None,
        fan: Some(FanEnvelope {
            min_pwm: Some(0),
            max_pwm: Some(30),
        }),
    }
}

fn antminer_grants_mutating_capabilities(
    support: CapabilitySupportTier,
    confidence: IdentityConfidence,
) -> bool {
    support == CapabilitySupportTier::Beta
        && matches!(
            confidence,
            IdentityConfidence::Exact | IdentityConfidence::High
        )
}

fn antminer_runtime_caps(grants_mutations: bool, board_target: &str) -> Vec<RuntimeCapability> {
    let mut caps = READ_ONLY_RUNTIME_CAPABILITIES.to_vec();
    if grants_mutations {
        push_runtime_cap(&mut caps, RuntimeCapability::PoolsRw);
        push_runtime_cap(&mut caps, RuntimeCapability::ConfigRw);
        push_runtime_cap(&mut caps, RuntimeCapability::AsicOptions);
        push_runtime_cap(&mut caps, RuntimeCapability::Identify);
        push_runtime_cap(&mut caps, RuntimeCapability::Reboot);
        push_runtime_cap(&mut caps, RuntimeCapability::Backup);
        if board_target.starts_with("antminer-zynq-") {
            push_runtime_cap(&mut caps, RuntimeCapability::FlashOta);
            push_runtime_cap(&mut caps, RuntimeCapability::Restore);
        }
    }
    caps
}

fn antminer_install_plan(
    support: CapabilitySupportTier,
    grants_mutations: bool,
    board_target: &str,
) -> InstallCapabilityPlan {
    if grants_mutations {
        return InstallCapabilityPlan {
            planner_outcome: PlannerOutcome::RuntimeOnly,
            proof_scope: Some(ProofScope::ExactTargetLabOnly),
            required_capabilities: vec![
                InstallCapability::RuntimeInstall,
                InstallCapability::Backup,
                InstallCapability::ManifestBoardMatch,
            ],
            missing_capabilities: vec![
                InstallCapability::PersistentInstall,
                InstallCapability::RestoreVerified,
            ],
            recovery_route_id: Some(format!("antminer-{board_target}-operator-gated")),
            note: Some(format!(
                "beta means mining-proven on the current support matrix; persistent install and restore remain operator-gated for {board_target}."
            )),
        };
    }

    let note = match support {
        CapabilitySupportTier::Experimental => {
            "Experimental Antminer identity: host/runtime metadata exists, but this descriptor withholds install and mutating runtime claims until live evidence promotes the exact SKU."
        }
        CapabilitySupportTier::Unsupported => {
            "Unsupported Antminer identity for DCENT_OS Bitcoin firmware; no install or mutating runtime capability is granted."
        }
        _ => {
            "Hardware identity or exact SKU evidence is incomplete; install planner outcome is an evidence gap."
        }
    };

    InstallCapabilityPlan {
        planner_outcome: PlannerOutcome::EvidenceGap,
        proof_scope: None,
        required_capabilities: Vec::new(),
        missing_capabilities: vec![
            InstallCapability::PersistentInstall,
            InstallCapability::RestoreVerified,
        ],
        recovery_route_id: None,
        note: Some(note.to_string()),
    }
}

fn antminer_fail_safe_policy(
    support: CapabilitySupportTier,
    grants_mutations: bool,
) -> FailSafePolicy {
    if grants_mutations {
        return FailSafePolicy {
            read_only: false,
            mining_start_allowed: false,
            mutating_routes_allowed: true,
            reason: "Exact beta-anchor identity grants API mutations, but first-boot mining remains opt-in."
                .to_string(),
        };
    }

    let reason = match support {
        CapabilitySupportTier::Experimental => {
            "experimental support tier keeps this shared capability descriptor read-only"
        }
        CapabilitySupportTier::Unsupported => {
            "unsupported hardware keeps this shared capability descriptor read-only"
        }
        _ => "identity evidence gap keeps this shared capability descriptor read-only",
    };
    FailSafePolicy::evidence_gap(reason)
}

fn build_antminer_capability_descriptor(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
) -> DeviceCapabilityDescriptor {
    let chip_id = chip_type_to_chip_id(&hw.chip_type);
    let profile = chip_id.and_then(MinerProfile::for_chip);
    let board_target = antminer_board_target(hw);
    let board_version = antminer_board_version(hw);
    let sources = capability_identity_sources(hw);
    let confidence = capability_identity_confidence(hw, profile);
    let support = antminer_support_tier(chip_id, profile, &sources);
    let grants_mutations = antminer_grants_mutating_capabilities(support, confidence);
    let mut runtime_caps = antminer_runtime_caps(grants_mutations, &board_target);
    let power_writes_enabled = grants_mutations
        && profile
            .map(|profile| profile.pic_type != dcentrald_asic::drivers::PicType::NoPic)
            .unwrap_or(false);
    let mut power_runtime_caps = vec![RuntimeCapability::Monitoring];
    if power_writes_enabled {
        push_runtime_cap(&mut runtime_caps, RuntimeCapability::PowerControl);
        push_runtime_cap(&mut power_runtime_caps, RuntimeCapability::PowerControl);
    }

    DeviceCapabilityDescriptor {
        schema_version: CAPABILITY_SCHEMA_VERSION,
        family: DeviceFamily::Antminer,
        identity: HardwareIdentity {
            confidence,
            sources,
            note: capability_identity_note(hw, confidence),
            device_model: profile.map(|profile| profile.name.to_string()),
            board_target: Some(board_target.clone()),
            board_version: Some(board_version.clone()),
            platform: Some("dcentos-antminer".to_string()),
        },
        support,
        board: BoardCapability {
            board_target: Some(board_target.clone()),
            family: Some("antminer".to_string()),
            control_board: if hw.control_board.trim().is_empty() {
                None
            } else {
                Some(hw.control_board.clone())
            },
            fixture_refs: Vec::new(),
        },
        control_board: ControlBoardCapability {
            soc: Some(board_version.clone()),
            control_board_id: if hw.control_board.trim().is_empty() {
                None
            } else {
                Some(hw.control_board.clone())
            },
            uio_model: None,
        },
        asic: AsicCapability {
            chip_model: profile
                .map(|_| hw.chip_type.trim().to_ascii_uppercase())
                .filter(|chip| !chip.is_empty()),
            asic_family: AsicFamily::BitmainBm13xx,
            chip_id,
            baud: capability_baud_for_chip(chip_id),
            cores_per_chip: profile.map(|profile| profile.cores_per_chip),
            nonce_attribution_cores: profile.map(|profile| profile.nonce_attribution_cores),
        },
        topology: TopologyCapability {
            chain_count: if miner.chains.is_empty() {
                profile.map(|profile| profile.chain_count)
            } else {
                u8::try_from(miner.chains.len()).ok()
            },
            chips_per_chain: miner
                .chains
                .iter()
                .map(|chain| chain.chips as u16)
                .max()
                .filter(|chips| *chips > 0)
                .or_else(|| profile.map(|profile| profile.chips_per_chain as u16)),
            fan_count: if miner.fans.per_fan.is_empty() {
                None
            } else {
                u8::try_from(miner.fans.per_fan.len()).ok()
            },
            temp_sensors: topology_temp_sensors(miner),
            hashboards: antminer_hashboards(miner, profile, chip_id),
        },
        fan_topology: antminer_fan_topology(miner),
        temp_sensors: antminer_temp_sensor_descriptors(miner),
        thermal: ThermalCapability {
            runtime_caps: vec![RuntimeCapability::Monitoring],
            fail_closed_on_sensor_loss: true,
        },
        power: PowerCapability {
            runtime_caps: power_runtime_caps,
            voltage_control: voltage_control_label(profile),
            psu_protocol: hw.psu_model.clone(),
            psu_mode: if hw.psu_model.is_some() {
                PsuMode::PmbusMonitor
            } else {
                PsuMode::Unknown
            },
            psu_model: hw.psu_model.clone(),
            writes_enabled: power_writes_enabled,
        },
        controllers: antminer_controller_capabilities(profile, &board_target),
        operating_envelopes: antminer_operating_envelopes(profile),
        references: CapabilityReferences {
            fixture_refs: Vec::new(),
            sim_profile_ref: None,
            bench_checklist_ref: Some(format!("bench/{board_target}")),
        },
        runtime_caps,
        install: antminer_install_plan(support, grants_mutations, &board_target),
        safe_defaults: SafeDefaults {
            mining_enabled: false,
            fan_pwm_cap: 30,
            frequency_mhz: profile.map(|profile| profile.default_freq_mhz),
            voltage_mv: profile.map(|profile| profile.default_voltage_mv),
        },
        fail_safe: antminer_fail_safe_policy(support, grants_mutations),
    }
}

fn current_antminer_capability_descriptor(state: &AppState) -> DeviceCapabilityDescriptor {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    build_antminer_capability_descriptor(&miner, &hw)
}

fn capability_error_response(error: CapabilityError) -> Response {
    let status = StatusCode::from_u16(error.http_status).unwrap_or(StatusCode::CONFLICT);
    (status, Json(error)).into_response()
}

fn capability_error_tuple(error: CapabilityError) -> (StatusCode, Json<serde_json::Value>) {
    let status = StatusCode::from_u16(error.http_status).unwrap_or(StatusCode::CONFLICT);
    (
        status,
        Json(serde_json::to_value(error).unwrap_or_else(|err| {
            serde_json::json!({
                "schemaVersion": CAPABILITY_SCHEMA_VERSION,
                "kind": "conflict",
                "httpStatus": status.as_u16(),
                "message": format!("failed to serialize capability error: {err}"),
                "capability": null,
            })
        })),
    )
}

pub(crate) fn require_antminer_runtime_capability(
    state: &AppState,
    required: RuntimeCapability,
    route: &str,
) -> std::result::Result<(), Response> {
    let descriptor = current_antminer_capability_descriptor(state);
    match runtime_capability_guard_error(&descriptor, required, route) {
        Some(error) => Err(capability_error_response(error)),
        None => Ok(()),
    }
}

fn static_power_fallback_from_miner(
    miner: &crate::MinerState,
    hardware: &crate::HardwareInfo,
) -> (u32, u32, f64, f64) {
    let chip_id = chip_type_to_chip_id(&hardware.chip_type).unwrap_or(0x1387);
    let power_model = dcentrald_autotuner::power_budget::PowerModel::new_for_chip(chip_id);
    let mut board_watts = power_model.control_board_w();

    for c in &miner.chains {
        if c.chips == 0 || c.frequency_mhz == 0 {
            continue;
        }
        let voltage_v = if c.voltage_mv > 0 {
            c.voltage_mv as f64 / 1000.0
        } else {
            dcentrald_asic::drivers::MinerProfile::for_chip(chip_id)
                .map(|profile| profile.default_voltage_mv as f64 / 1000.0)
                .unwrap_or(9.1)
        };
        let chain_dynamic: f64 = (0..c.chips)
            .map(|_| power_model.chip_power_w(voltage_v, c.frequency_mhz))
            .sum();
        board_watts += chain_dynamic + power_model.static_per_chain_w();
    }

    let wall_watts = board_watts / 0.88;
    let efficiency_jth = if miner.hashrate_ghs > 0.0 {
        wall_watts / (miner.hashrate_ghs / 1000.0)
    } else {
        0.0
    };

    (
        board_watts as u32,
        wall_watts as u32,
        efficiency_jth,
        dcentrald_autotuner::btu_from_watts(wall_watts),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct PowerTelemetryProjection {
    pub(crate) board_watts: u32,
    pub(crate) wall_watts: u32,
    pub(crate) efficiency_jth: f64,
    pub(crate) btu_h: f64,
    pub(crate) source: String,
    pub(crate) source_detail: &'static str,
    pub(crate) live_power_available: bool,
    pub(crate) modeled: bool,
    pub(crate) calibrated: bool,
    pub(crate) calibration_multiplier: Option<f64>,
    pub(crate) note: &'static str,
}

impl PowerTelemetryProjection {
    /// The canonical [`dcentrald_autotuner::PowerAuthorityKind`] for this
    /// projection, resolved from the (already-classified) source string +
    /// calibration flag via the SHARED authority model. Consumers classify
    /// measured-ness through this instead of re-matching `source_detail`
    /// strings, so an ADC-measured source is not silently downgraded to
    /// "modeled" the way a `== "pmbus_measured"` compare would.
    pub(crate) fn authority(&self) -> dcentrald_autotuner::PowerAuthorityKind {
        dcentrald_autotuner::PowerAuthorityKind::from_source(&self.source, self.calibrated)
    }

    /// True when this projection is backed by a live MEASURED power source
    /// (PMBus or ADC), not a model/estimate. Gated on `live_power_available`
    /// so a stale/absent reading never counts as measured.
    pub(crate) fn is_measured(&self) -> bool {
        self.live_power_available && self.authority().is_measured()
    }
}

#[derive(Debug, Clone, Copy)]
struct PowerCostProjection {
    wall_watts: u32,
    daily_cost_usd: f64,
    heat_reuse_credit_usd_per_day: f64,
    net_daily_cost_after_heat_credit: f64,
    live_power_available: bool,
    modeled: bool,
    source_detail: &'static str,
    note: &'static str,
}

#[derive(Debug, Clone)]
struct PowerCalibrationPowerContract {
    current_reported_wall_watts: f64,
    current_reported_unit_watts: f64,
    projected_wall_watts: Option<f64>,
    projected_unit_watts: Option<f64>,
    power_source: String,
    power_source_detail: &'static str,
    live_power_available: bool,
    power_modeled: bool,
    power_note: &'static str,
    calibrated: bool,
    calibration_multiplier: Option<f64>,
}

pub(crate) fn (
    live: &dcentrald_autotuner::LivePowerEstimate,
    miner: &crate::MinerState,
    hardware: &crate::HardwareInfo,
) -> PowerTelemetryProjection {
    let live_power_available = live.board_watts.is_finite()
        && live.board_watts > 0.0
        && live.wall_watts.is_finite()
        && live.wall_watts > 0.0;
    if live_power_available {
        let source = if live.source.trim().is_empty() {
            "live_power_watch".to_string()
        } else {
            live.source.clone()
        };
        let authority =
            dcentrald_autotuner::PowerAuthorityKind::from_source(&source, live.calibrated);
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
        return PowerTelemetryProjection {
            board_watts: live.board_watts as u32,
            wall_watts: live.wall_watts as u32,
            efficiency_jth: if live.efficiency_jth.is_finite() {
                live.efficiency_jth
            } else {
                0.0
            },
            btu_h: if live.btu_h.is_finite() && live.btu_h >= 0.0 {
                live.btu_h
            } else {
                dcentrald_autotuner::btu_from_watts(live.wall_watts)
            },
            source,
            source_detail,
            live_power_available: true,
            modeled: !measured,
            calibrated: live.calibrated,
            calibration_multiplier: live.calibration_multiplier,
            note: if measured {
                "Power is sourced from live measured telemetry."
            } else if authority == dcentrald_autotuner::PowerAuthorityKind::WallCalibratedEstimate {
                "Power is modeled from live runtime state with an operator wall-meter calibration."
            } else {
                "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement."
            },
        };
    }

    let (board_watts, wall_watts, efficiency_jth, btu_h) =
        static_power_fallback_from_miner(miner, hardware);
    PowerTelemetryProjection {
        board_watts,
        wall_watts,
        efficiency_jth,
        btu_h,
        source: "static_model_fallback".to_string(),
        source_detail: "static_power_fallback_from_miner_state",
        live_power_available: false,
        modeled: true,
        calibrated: false,
        calibration_multiplier: None,
        note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
    }
}

fn (
    projection: &PowerTelemetryProjection,
    electricity_rate_usd_per_kwh: f64,
    heating_offset_fraction: f64,
) -> PowerCostProjection {
    let wall_watts = if projection.live_power_available {
        projection.wall_watts
    } else {
        0
    };
    let daily_cost_usd = wall_watts as f64 * 24.0 * electricity_rate_usd_per_kwh.max(0.0) / 1000.0;
    let heat_reuse_credit_usd_per_day = compute_heat_reuse_credit_usd_per_day(
        wall_watts as f64,
        electricity_rate_usd_per_kwh,
        heating_offset_fraction,
    );
    let net_daily_cost_after_heat_credit =
        (daily_cost_usd - heat_reuse_credit_usd_per_day).max(0.0);

    PowerCostProjection {
        wall_watts,
        daily_cost_usd,
        heat_reuse_credit_usd_per_day,
        net_daily_cost_after_heat_credit,
        live_power_available: projection.live_power_available,
        modeled: projection.live_power_available && projection.modeled,
        source_detail: projection.source_detail,
        note: if projection.live_power_available {
            if projection.modeled {
                "Cost and circuit estimates are computed from live modeled wall power, not a direct wall-meter measurement."
            } else {
                "Cost and circuit estimates are computed from live measured wall power."
            }
        } else {
            "Live power has not published a positive wall reading; cost and circuit estimates are suppressed instead of using static fallback power."
        },
    }
}

fn (
    projection: &PowerTelemetryProjection,
    projected_multiplier: Option<f64>,
) -> PowerCalibrationPowerContract {
    let current_reported_wall_watts = if projection.live_power_available {
        projection.wall_watts as f64
    } else {
        0.0
    };
    let current_reported_unit_watts = if projection.live_power_available {
        projection.board_watts as f64
    } else {
        0.0
    };
    let projected_wall_watts =
        projected_multiplier.map(|multiplier| current_reported_wall_watts * multiplier);
    let projected_unit_watts =
        projected_multiplier.map(|multiplier| current_reported_unit_watts * multiplier);
    let projected_from_live_power =
        projected_multiplier.is_some() && projection.live_power_available;

    PowerCalibrationPowerContract {
        current_reported_wall_watts,
        current_reported_unit_watts,
        projected_wall_watts,
        projected_unit_watts,
        power_source: projection.source.clone(),
        power_source_detail: projection.source_detail,
        live_power_available: projection.live_power_available,
        power_modeled: projection.modeled,
        power_note: projection.note,
        calibrated: projection.calibrated || projected_from_live_power,
        calibration_multiplier: projected_multiplier
            .filter(|_| projection.live_power_available)
            .or(projection.calibration_multiplier),
    }
}

/// Build the `/api/status` `power` object.
///
/// O2 (intentional per-endpoint divergence — do NOT "unify"): unlike the *metric*
/// surfaces (`/api/system/info` power, CGMiner devs, thermal posture, and every
/// Prometheus/CSV/rolling family) which ZERO or OMIT power when live power is
/// unavailable, `/api/status` deliberately surfaces the static-model FALLBACK
/// watts here — but ALWAYS paired, in this same object, with
/// `live_power_available:false` + `source` + `modeled` + `note` so a consumer can
/// tell it apart. The dashboard's status view wants a best-effort estimate with an
/// explicit provenance flag; a metrics scrape must never fabricate a live value.
/// A consumer reading `.watts` MUST also read `live_power_available`/`source`.
/// Neither zeroing here nor surfacing modeled watts on the metric surfaces is
/// correct — both directions break a documented truth contract.
fn build_status_power_section(
    projection: &PowerTelemetryProjection,
    power: &dcentrald_autotuner::LivePowerEstimate,
    targeting: PowerTargetingState,
) -> serde_json::Value {
    serde_json::json!({
        "watts": projection.board_watts,
        "wall_watts": projection.wall_watts,
        "efficiency_jth": projection.efficiency_jth,
        "btu_h": projection.btu_h,
        "source": projection.source.as_str(),
        "source_detail": projection.source_detail,
        "live_power_available": projection.live_power_available,
        "modeled": projection.modeled,
        "note": projection.note,
        "calibrated": projection.calibrated,
        "calibration_multiplier": projection.calibration_multiplier,
        "per_chain_watts": power.per_chain_watts.clone(),
        "runtime_limits": power.dispatcher_limits.clone(),
        "watt_cap": power.watt_cap.clone(),
        "targeting": targeting,
    })
}

/// Provenance-tagged per-chain telemetry projection for the public `/api/status`
/// surface (P0-2 / C-2 / D-1 / D-2).
///
/// Two S9/am1 hardware truths drive this projection and keep the status surface
/// honest instead of emitting bare/false zeros:
///
/// 1. **No per-chain voltage ADC.** The S9 control board commands the hash-board
///    DC-DC rail open-loop through the PIC DAC — there is no per-chain voltage
///    sense. A per-chain `voltage_mv` is therefore the *commanded* value, never a
///    measured rail reading. Emitting a bare `0` (for a chain that has not been
///    individually commanded yet) reads downstream as "0 V measured", which is a
///    lie; we instead fall back to the chip-profile commanded default and tag the
///    provenance so the dashboard never presents a commanded value as measured.
///
/// 2. **Per-chain hashrate can lag the live topline.** `ChainState::hashrate_ghs`
///    is only populated once the nonce tracker has a full per-chain window;
///    before that — exactly the live S9 `.100` audit that motivated this fix —
///    every chain reads `0.0` while the aggregate topline is a live ~1.1 TH/s.
///    Rather than show a bare per-chain `0` under a live topline, we split the
///    topline across the responding chains (proportional to chip count) and tag
///    it `derived_topline_split` so an operator never mistakes the estimate for a
///    measured per-chain figure, and never sees a false zero. A genuine measured
///    zero on a dead chain (while siblings publish real per-chain numbers) is
///    preserved as-is — we only split when NO chain has per-chain data.
struct ChainTelemetryProjection {
    frequency_mhz: u16,
    /// `"chain_state"` (runtime per-chain value), `"unreported_active_chain"`
    /// (responding chips but no frequency published), or `"unavailable"` (no
    /// responding chips / no runtime frequency).
    frequency_source: &'static str,
    hashrate_ghs: f64,
    /// `"per_chain"` (real measured per-chain value, incl. a genuine 0),
    /// `"derived_topline_split"` (topline split across responding chains), or
    /// `"idle"` (no topline and no per-chain data).
    hashrate_source: &'static str,
    voltage_mv: u16,
    /// `"commanded_not_measured"` (DAC value commanded to this chain),
    /// `"commanded_default"` (chip-profile default — chain not yet commanded), or
    /// `"unknown"` (no commanded value and no profile default available). S9 has
    /// no per-chain ADC, so this is NEVER `"measured"`.
    voltage_source: &'static str,
}

fn chain_frequency_source(chain: &crate::ChainState) -> &'static str {
    if chain.frequency_mhz > 0 {
        "chain_state"
    } else if chain.chips > 0 {
        "unreported_active_chain"
    } else {
        "unavailable"
    }
}

fn primary_frequency_source(chain: Option<&crate::ChainState>) -> &'static str {
    chain.map(chain_frequency_source).unwrap_or("unavailable")
}

/// Build the per-chain hashrate/voltage provenance projection (see
/// [`ChainTelemetryProjection`]). Pure over its inputs so it is host-testable
/// without an `AppState`/HAL.
fn (
    chains: &[crate::ChainState],
    topline_ghs: f64,
    default_voltage_mv: u16,
) -> Vec<ChainTelemetryProjection> {
    // AT-1: the open-loop path supplies no measured rail readings, so the result
    // is byte-identical to the historical commanded-only projection. Kept pure
    // (empty measured map) so it stays host-testable without touching the AT-3
    // process-global slot; production callers use
    // [].
    (
        chains,
        topline_ghs,
        default_voltage_mv,
        &std::collections::HashMap::new(),
    )
}

/// AT-3 production wrapper: the per-chain projection fed by the *live*
/// AT-3 measured-rail slot.
///
/// Reads the currently-fresh per-chain measured rails published by the am2
/// hybrid loop's gated, default-OFF AT-3 read
/// ([`dcentrald_common::at3_rail::snapshot_fresh_default`]) and delegates to
/// []. When AT-3 is disabled (the
/// default) or no fresh reading exists, the snapshot is empty and the result is
/// byte-identical to the commanded-only []. A fresh,
/// plausible reading flips that chain's `voltage_source` to `measured`.
fn (
    chains: &[crate::ChainState],
    topline_ghs: f64,
    default_voltage_mv: u16,
) -> Vec<ChainTelemetryProjection> {
    let measured = dcentrald_common::at3_rail::snapshot_fresh_default();
    (chains, topline_ghs, default_voltage_mv, &measured)
}

/// AT-1 (chip-rail voltage read-back): the per-chain projection, with an optional
/// per-chain *measured* rail voltage (decoded from the dsPIC 0x3A
/// `MEASURE_VOLTAGE` ADC, keyed by chain id) taking priority over the commanded
/// DAC value where a plausible reading exists — tagged `voltage_source =
/// "measured"`.
///
/// When `measured_voltage_mv` has no entry (or an implausible one) for a chain,
/// the projection falls back to the commanded value exactly as the legacy
/// open-loop path did. The voltage-provenance decision is delegated to the
/// shared [`dcentrald_autotuner::resolve_chain_rail_voltage`] resolver so the API
/// and the autotuner agree on the tag vocabulary AND the plausibility ceiling
/// (`DSPIC_MAX_VOLTAGE_MV`). Pure over its inputs (host-testable).
///
/// NOTE: AT-1 only wires the *consumption* point — no caller feeds a live
/// `measured_voltage_mv` yet, because a safe quiet-window 0x3A read cadence is
/// AT-3..13 work (the hot-path 0x3A read can corrupt the dsPIC parser). Today
/// every caller passes an empty map via [].
fn (
    chains: &[crate::ChainState],
    topline_ghs: f64,
    default_voltage_mv: u16,
    measured_voltage_mv: &std::collections::HashMap<u8, u16>,
) -> Vec<ChainTelemetryProjection> {
    // A real per-chain split exists the moment ANY chain publishes a non-zero
    // per-chain hashrate. Only when NO chain has per-chain data do we fall back
    // to splitting the live topline (so we never overwrite a genuine measured 0
    // on a dead chain whose siblings are live).
    let has_per_chain = chains.iter().any(|c| c.hashrate_ghs > 0.0);
    let responding_chips: u32 = chains
        .iter()
        .filter(|c| c.chips > 0)
        .map(|c| c.chips as u32)
        .sum();

    chains
        .iter()
        .map(|c| {
            let (hashrate_ghs, hashrate_source) = if c.hashrate_ghs > 0.0 {
                (c.hashrate_ghs, "per_chain")
            } else if !has_per_chain && topline_ghs > 0.0 && c.chips > 0 && responding_chips > 0 {
                let share = c.chips as f64 / responding_chips as f64;
                (topline_ghs * share, "derived_topline_split")
            } else if has_per_chain {
                // Genuine measured zero on this chain while siblings are live.
                (0.0, "per_chain")
            } else {
                (0.0, "idle")
            };

            // AT-1: prefer a plausible measured 0x3A reading; else fall back to
            // the commanded DAC value, else the chip-profile default (only for a
            // responding chain), else honest unknown — delegated to the shared
            // autotuner resolver so the tag vocabulary + the DSPIC_MAX_VOLTAGE_MV
            // plausibility ceiling stay in lockstep across crates.
            let commanded_mv = (c.voltage_mv > 0).then_some(c.voltage_mv);
            let default_mv = (default_voltage_mv > 0 && c.chips > 0).then_some(default_voltage_mv);
            let rail = dcentrald_autotuner::resolve_chain_rail_voltage(
                c.id,
                measured_voltage_mv.get(&c.id).copied(),
                commanded_mv,
                default_mv,
            );

            ChainTelemetryProjection {
                frequency_mhz: c.frequency_mhz,
                frequency_source: chain_frequency_source(c),
                hashrate_ghs,
                hashrate_source,
                voltage_mv: rail.mv,
                voltage_source: rail.source.as_str(),
            }
        })
        .collect()
}

fn snapshot_context(state: &AppState) -> SnapshotContext {
    let miner = state.state_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let profiles = load_profiles(&state.profile_path);
    let history = read_history_samples(state);
    let chip_type = hardware.chip_type.clone();
    let live_chip_health = state
        .autotuner_chip_health_rx
        .borrow()
        .clone()
        .map(|runtime| {
            runtime
                .chips
                .into_iter()
                .map(|chip| SnapshotChipHealth {
                    chain_id: chip.chain_id,
                    chip_index: chip.chip_index,
                    health_score: chip.health_score,
                    trend: chip.trend,
                    estimated_days_to_warning: chip.estimated_days_to_warning,
                    error_rate_pct: chip.error_rate_pct,
                    freq_mhz: chip.freq_mhz,
                    backoff_count: chip.backoff_count,
                    hashrate_ratio: chip.hashrate_ratio,
                    status: chip.status.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    SnapshotContext {
        report_id: Uuid::new_v4(),
        generated_at: chrono_now_iso(),
        firmware_version: miner.firmware_version,
        serial: hardware.miner_serial,
        mac: std::fs::read_to_string("/sys/class/net/eth0/address")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        model: hardware
            .hb_type
            .clone()
            .or_else(|| (!chip_type.is_empty()).then(|| format!("{} miner", chip_type))),
        chip_type: chip_type.clone(),
        chip_id: chip_type_to_chip_id(&chip_type),
        control_board: hardware.control_board,
        board_type: hardware.hb_type,
        chain_states: miner
            .chains
            .into_iter()
            .map(|chain| SnapshotChain {
                chain_id: chain.id,
                chips: chain.chips,
                frequency_mhz: chain.frequency_mhz,
                voltage_mv: chain.voltage_mv,
                temp_c: chain.temp_c,
                hashrate_ghs: chain.hashrate_ghs,
                errors: chain.errors,
                status: chain.status,
            })
            .collect(),
        fan_pwm: miner.fans.pwm,
        fan_rpm: miner.fans.rpm,
        accepted_shares: miner.accepted,
        rejected_shares: miner.rejected,
        pool_url: dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
        pool_status: miner.pool.status,
        pool_difficulty: miner.pool.difficulty,
        uptime_s: miner.uptime_s,
        history,
        live_chip_health,
        saved_profiles: profiles
            .into_values()
            .map(|profile| SnapshotProfile {
                chain_id: profile.chain_id,
                chip_count: profile.chip_count,
                voltage_mv: profile.voltage_mv,
                estimated_hashrate_ghs: profile.stats.estimated_hashrate_ghs,
                chips: profile
                    .chips
                    .into_iter()
                    .map(|chip| SnapshotProfileChip {
                        chip_index: chip.chip_index,
                        operating_mhz: chip.operating_mhz,
                        grade: chip.grade.to_string().chars().next().unwrap_or('B'),
                        error_rate: chip.error_rate,
                        nonces_counted: chip.nonces_counted,
                        thermal_max_stable_mhz: chip.thermal_max_stable_mhz,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn parse_test_id_or_response(test_id: &str) -> Result<Uuid, axum::response::Response> {
    Uuid::parse_str(test_id).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": format!("invalid test_id: {test_id}"),
            })),
        )
            .into_response()
    })
}

fn report_not_found_response(test_id: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "status": "error",
            "message": format!("diagnostic artifact not found for test_id {test_id}"),
        })),
    )
        .into_response()
}

fn report_storage_error_response(error: &impl std::fmt::Display) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "status": "error",
            "message": error.to_string(),
        })),
    )
        .into_response()
}

fn persist_snapshot_artifact(
    test_id: Uuid,
    artifact: &impl Serialize,
    html: Option<&str>,
) -> Result<serde_json::Value, axum::response::Response> {
    let json_value = serde_json::to_value(artifact).map_err(|error| {
        report_storage_error_response(&format!("failed to serialize diagnostic artifact: {error}"))
    })?;
    ReportGenerator::new()
        .save_report(&test_id, html, &json_value)
        .map(|_| json_value)
        .map_err(|error| report_storage_error_response(&error))
}

fn load_snapshot_artifact(test_id: &Uuid) -> Result<serde_json::Value, axum::response::Response> {
    ReportGenerator::new()
        .load_report_json(test_id)
        .map_err(|error| match error {
            dcentrald_diagnostics::DiagnosticError::Io(io)
                if io.kind() == std::io::ErrorKind::NotFound =>
            {
                report_not_found_response(&test_id.to_string())
            }
            _ => report_storage_error_response(&error),
        })
}

fn load_snapshot_html(test_id: &Uuid) -> Result<String, axum::response::Response> {
    ReportGenerator::new()
        .load_report_html(test_id)
        .map_err(|error| match error {
            dcentrald_diagnostics::DiagnosticError::Io(io)
                if io.kind() == std::io::ErrorKind::NotFound =>
            {
                report_not_found_response(&test_id.to_string())
            }
            _ => report_storage_error_response(&error),
        })
}

fn build_timed_hashreport_result(
    state: &Arc<AppState>,
    test_id: Uuid,
    chain: Option<u8>,
    elapsed_s: u64,
) -> dcentrald_diagnostics::Result<TestResult> {
    let mut context = snapshot_context(state);
    context.report_id = test_id;
    let mut report = build_hashreport_snapshot(&context, chain);
    report.report_id = test_id;
    report.report_version = "timed-v1".to_string();
    report.report_kind = "timed_background_job".to_string();
    report.duration_seconds = elapsed_s.min(u32::MAX as u64) as u32;
    report.source = if context.history.is_empty() {
        "timed_runtime_snapshot_at_completion".to_string()
    } else {
        "timed_runtime_plus_history_at_completion".to_string()
    };
    report.unit_grade_explanation = format!(
        "Timed HashReport background job observed live runtime for {}s and generated the final report from completion-time miner state.",
        report.duration_seconds
    );
    report.warnings.push(
        "This first engine step measures elapsed test time honestly but still builds the final HashReport from completion-time runtime data rather than per-window nonce capture."
            .to_string(),
    );
    report.warnings.sort();
    report.warnings.dedup();

    let html = ReportGenerator::new()
        .render_hashreport(&report)
        .map_err(|error| {
            dcentrald_diagnostics::DiagnosticError::ReportGeneration(error.to_string())
        })?;
    persist_snapshot_artifact(test_id, &report, Some(&html))
        .map_err(response_to_diagnostic_error)?;

    let data = serde_json::to_value(&report).map_err(|error| {
        dcentrald_diagnostics::DiagnosticError::ReportGeneration(error.to_string())
    })?;
    Ok(TestResult {
        test_id,
        test_type: TestType::HashReport,
        duration_s: elapsed_s,
        data,
        grade: Some(report.unit_grade.to_string()),
        warnings: report.warnings.clone(),
        recommendations: report.recommendations.clone(),
    })
}

fn response_to_diagnostic_error(
    response: axum::response::Response,
) -> dcentrald_diagnostics::DiagnosticError {
    dcentrald_diagnostics::DiagnosticError::ReportGeneration(format!(
        "HTTP {} while persisting diagnostic artifact",
        response.status()
    ))
}

fn test_status_as_str(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Running => "running",
        TestStatus::Completed => "completed",
        TestStatus::Failed => "failed",
        TestStatus::Cancelled => "cancelled",
    }
}

fn read_history_samples(state: &AppState) -> Vec<SnapshotHistorySample> {
    read_history_data(state)
        .into_iter()
        .filter_map(|value| serde_json::from_value::<SnapshotHistorySample>(value).ok())
        .collect()
}

const AUTOTUNER_STALE_AFTER_S: u64 = 15;
const SYSTEM_UPGRADE_MAX_UPLOAD_BYTES: usize = 128 * 1024 * 1024;
const SYSTEM_UPGRADE_STAGE_ROOT: &str = "/tmp/dcentos-upgrade";
const SYSTEM_UPGRADE_RELEASE_PUBKEY: &str = "/etc/dcentos/release_ed25519.pub";
const KERNEL_WATCHDOG0_SYSFS: &str = "/sys/class/watchdog/watchdog0";

/// The installed init-script name. Every Buildroot overlay ships
/// `/etc/init.d/S82dcentrald` (verified across zynq / amlogic / beaglebone /
/// cvitek / am2-*); there is **no** `/etc/init.d/dcentrald` symlink anywhere.
///
/// WAVE 0 STABILIZE (2026-06-05) — RESTART-NO-RESPAWN fix: the prior
/// `trigger_daemon_restart()` ran `Command::new("/etc/init.d/dcentrald")`,
/// a path that does NOT exist, so the init.d restart always failed
/// ("No such file or directory") and fell through to `kill -TERM self`.
/// On a unit whose supervisor wrapper (`S82dcentrald`) reads a clean SIGTERM
/// exit as an *intentional stop*, the respawn loop then exits and the daemon
/// stays DEAD until power-cycle — i.e. the operator-facing "Restart" button
/// killed the unit. This constant + `build_daemon_restart_command` target the
/// REAL installed script, identical to the daemon crate's
/// `restart.rs::schedule_daemon_restart` (the documented, unit-tested
/// auto-recovery path) so both control planes restart the same way. The API
/// crate cannot call into the `dcentrald` binary crate directly (the dep edge
/// runs the other way), so this mirrors that proven logic byte-for-byte.
const DAEMON_RESTART_INIT_SCRIPT: &str = "/etc/init.d/S82dcentrald";

/// Build the detached restart shell command. Pure + unit-testable so the
/// init-script path can never silently drift from the installed name again,
/// and so the SIGTERM respawn fallback is regression-pinned.
///
/// Runs `<script> restart`; if the script is somehow absent the `|| kill`
/// fallback sends `kill -TERM <self_pid>` so the `S82dcentrald` crash-wrapper
/// (or procd) respawns. This matches `restart.rs::build_restart_command` in the
/// daemon crate exactly.
fn build_daemon_restart_command(self_pid: u32) -> String {
    format!(
        "{DAEMON_RESTART_INIT_SCRIPT} restart >/tmp/dcentrald_restart_cmd.log 2>&1 \
         || kill -TERM {self_pid}"
    )
}

fn sanitize_upload_filename(name: &str) -> String {
    let candidate = std::path::Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dcentos-sysupgrade.tar");

    let sanitized: String = candidate
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '_',
        })
        .collect();

    if sanitized.is_empty() {
        "dcentos-sysupgrade.tar".to_string()
    } else {
        sanitized
    }
}

fn system_upgrade_error(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({
            "status": "error",
            "message": message.into(),
        })),
    )
}

fn command_output_message(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{}\n{}", stdout, stderr),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => "sysupgrade returned no diagnostic output".to_string(),
    }
}

async fn resolve_staged_upgrade_path(requested_path: &str) -> std::result::Result<String, String> {
    let requested_path = requested_path.trim();
    if requested_path.is_empty() {
        return Err("Staged package path is required".to_string());
    }

    let stage_root = tokio::fs::canonicalize(SYSTEM_UPGRADE_STAGE_ROOT)
        .await
        .map_err(|_| "Upgrade staging directory is unavailable on this target".to_string())?;
    let candidate = tokio::fs::canonicalize(requested_path)
        .await
        .map_err(|_| "Previously staged package is missing or inaccessible".to_string())?;

    if !candidate.starts_with(&stage_root) {
        return Err(
            "Refusing to use a package outside the browser update staging area".to_string(),
        );
    }
    if !candidate.is_file() {
        return Err("Previously staged package is not a regular file".to_string());
    }
    //  W9-A — accept stock Bitmain tarballs (.tar.gz / .tgz / .bmu) for
    // re-use as staged inputs, per R3-CRITICAL-3. The restore-to-stock flash
    // path consumes them directly. DCENT_OS sysupgrade still rejects the
    // gzipped/.bmu variants downstream.
    let candidate_name = candidate
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let extension_ok = candidate_name.ends_with(".tar")
        || candidate_name.ends_with(".tar.gz")
        || candidate_name.ends_with(".tgz")
        || candidate_name.ends_with(".bmu");
    if !extension_ok {
        return Err(
            "Only sysupgrade .tar packages or stock Bitmain firmware archives (.tar.gz, .tgz, .bmu) can be reused".to_string(),
        );
    }

    Ok(candidate.to_string_lossy().into_owned())
}

fn read_history_data(state: &AppState) -> Vec<serde_json::Value> {
    state
        .history_data
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|sample| match sample {
            serde_json::Value::Object(mut obj) => {
                if !obj.contains_key("timestamp") {
                    if let Some(timestamp) = obj.get("timestamp_s").cloned() {
                        obj.insert("timestamp".to_string(), timestamp);
                    }
                }
                serde_json::Value::Object(obj)
            }
            other => other,
        })
        .collect()
}

fn history_response(samples: Vec<serde_json::Value>) -> axum::response::Response {
    if samples.is_empty() {
        Json(serde_json::json!({
            "history": [],
            "interval_s": 300,
            "count": 0,
            "message": "Historical data collection starting — first sample in ~5 minutes",
        }))
        .into_response()
    } else {
        let count = samples.len();
        Json(serde_json::json!({
            "history": samples,
            "interval_s": 300,
            "count": count,
        }))
        .into_response()
    }
}

fn autotuner_now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn autotuner_runtime_freshness(last_update_s: u64, live_runtime: bool) -> (u64, bool, bool) {
    let age_s = if last_update_s == 0 {
        0
    } else {
        autotuner_now_s().saturating_sub(last_update_s)
    };
    let stale = live_runtime && last_update_s > 0 && age_s > AUTOTUNER_STALE_AFTER_S;
    (age_s, stale, live_runtime && !stale)
}

fn insert_autotuner_runtime_meta(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    last_update_s: u64,
    source: &str,
    live_runtime: bool,
    message: &str,
) {
    let (age_s, stale, live_runtime) = autotuner_runtime_freshness(last_update_s, live_runtime);
    obj.insert("source".to_string(), serde_json::json!(source));
    obj.insert("live_runtime".to_string(), serde_json::json!(live_runtime));
    obj.insert("stale".to_string(), serde_json::json!(stale));
    obj.insert("age_s".to_string(), serde_json::json!(age_s));
    obj.insert(
        "last_update_s".to_string(),
        serde_json::json!(last_update_s),
    );
    obj.insert("message".to_string(), serde_json::json!(message));
}

/// Surface the power provenance of a serialized [`dcentrald_autotuner::EfficiencySnapshot`]
/// JSON object. The per-chip/total watts in this snapshot are ALWAYS
/// model-derived, so the `source: "runtime"` freshness label (injected by
/// [`insert_autotuner_runtime_meta`]) must never be mistaken for a measured
/// wattage. This mirrors the snapshot struct's own `power_basis`/`modeled` but
/// sets them explicitly at the consumer surface (from the shared
/// [`dcentrald_autotuner::PowerAuthorityKind`] model) so the guarantee holds
/// regardless of how the snapshot was constructed or deserialized.
fn insert_efficiency_power_provenance(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let basis = dcentrald_autotuner::PowerAuthorityKind::Estimated;
    obj.insert("power_basis".to_string(), serde_json::json!(basis.as_str()));
    obj.insert(
        "modeled".to_string(),
        serde_json::json!(!basis.is_measured()),
    );
}

/// Build the saved-profile efficiency snapshot, scaled by the ACTIVE operator
/// wall-meter calibration. `PowerCalibration::effective_multiplier()` is `1.0`
/// when the calibration is disabled, so uncalibrated units are unaffected; a
/// calibrated unit's saved-profile watts now match its wall meter instead of
/// silently ignoring the multiplier the live power path already honors.
fn saved_profile_efficiency_snapshot(
    profiles: &std::collections::HashMap<u8, dcentrald_autotuner::TuningProfile>,
    calibration: &dcentrald_autotuner::PowerCalibration,
) -> dcentrald_autotuner::EfficiencySnapshot {
    dcentrald_autotuner::build_efficiency_snapshot(profiles, calibration.effective_multiplier())
}

fn eth0_ipv4() -> String {
    std::process::Command::new("ip")
        .args(["-4", "addr", "show", "eth0"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .find(|line| line.contains("inet "))
                .and_then(|line| line.split_whitespace().nth(1))
                .map(|ip| ip.split('/').next().unwrap_or(ip).to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn local_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "dcentos".to_string())
        .trim()
        .to_string()
}

fn fleet_model_label(hw: &crate::HardwareInfo) -> String {
    if let Some(profile) = chip_type_to_chip_id(&hw.chip_type).and_then(MinerProfile::for_chip) {
        return profile.name.to_string();
    }

    if let Some(hb_type) = hw
        .hb_type
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return hb_type.to_string();
    }

    if !hw.chip_type.trim().is_empty() {
        return format!("Antminer ({})", hw.chip_type);
    }

    "Antminer (unknown ASIC)".to_string()
}

fn fleet_status_for_miner(miner: &crate::MinerState) -> FleetMinerStatus {
    if miner.hashrate_ghs > 0.0 || is_pool_mining_capable(&miner.pool.status) {
        return FleetMinerStatus::Alive;
    }

    if miner.uptime_s < 180
        || miner.pool.status.trim().is_empty()
        || is_pool_connecting(&miner.pool.status)
        || miner.pool.status.eq_ignore_ascii_case("proxied")
    {
        return FleetMinerStatus::Starting;
    }

    FleetMinerStatus::Dead
}

fn primary_fleet_temp_c(miner: &crate::MinerState) -> f64 {
    miner
        .chains
        .iter()
        .filter(|chain| chain.temp_c.is_finite() && chain.temp_c > 0.0)
        .map(|chain| chain.temp_c as f64)
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0)
}

fn build_local_fleet_miners_response(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: String,
    ip: String,
    now_ms: u64,
    // PR-048b:  share-difficulty pair sourced from the SAME mining
    // pipeline snapshot the single-miner path reads
    // (`last_share_target_difficulty`, `last_share_achieved_difficulty`).
    // `(None, None)` when no snapshot publisher is wired — never fabricated,
    // never filled from `miner.pool.difficulty` (which Wave-9D9/9F forbid
    // treating as achieved difficulty).
    pool_target_difficulty: Option<f64>,
    achieved_difficulty: Option<f64>,
) -> FleetMinersResponse {
    let id = if hostname.trim().is_empty() {
        "dcentos-local".to_string()
    } else {
        hostname.clone()
    };

    FleetMinersResponse {
        generated_at_ms: now_ms,
        miners: vec![FleetMinerSummary {
            id,
            hostname,
            ip,
            model: fleet_model_label(hw),
            hashrate_ghs: miner.hashrate_ghs.max(0.0),
            temp_c: primary_fleet_temp_c(miner),
            fan_pwm: miner.fans.pwm.min(100),
            status: fleet_status_for_miner(miner),
            last_seen_ms: now_ms,
            pool_target_difficulty,
            achieved_difficulty,
        }],
    }
}

fn fleet_discovery_status_for_miner(miner: &crate::MinerState) -> &'static str {
    match fleet_status_for_miner(miner) {
        FleetMinerStatus::Alive => "online",
        FleetMinerStatus::Starting => "sleeping",
        FleetMinerStatus::Dead => "error",
    }
}

fn fleet_discovery_reported_power_watts(
    power: &dcentrald_autotuner::LivePowerEstimate,
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
) -> Option<f64> {
    let projection = (power, miner, hw);
    let measured_wall_watts = measured_wall_watts_for_unprovenanced_surface(&projection);
    if measured_wall_watts > 0.0 {
        Some(measured_wall_watts)
    } else {
        None
    }
}

fn build_local_fleet_discover_response(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: String,
    ip: String,
    mac: String,
    reported_power_watts: Option<f64>,
    request: &FleetDiscoverRequest,
    now_ms: u64,
) -> serde_json::Value {
    let firmware = if miner.firmware_version.trim().is_empty() {
        "DCENTos".to_string()
    } else {
        format!("DCENTos v{}", miner.firmware_version)
    };
    let reported_power_watts =
        reported_power_watts.filter(|watts| watts.is_finite() && *watts > 0.0);

    serde_json::json!({
        "status": "ok",
        "source": "local_state",
        "generated_at_ms": now_ms,
        "miners": [{
            "ip": ip,
            "hostname": hostname,
            "model": fleet_model_label(hw),
            "firmware": firmware,
            "hashrateThs": miner.hashrate_ghs.max(0.0) / 1000.0,
            "powerWatts": reported_power_watts,
            "status": fleet_discovery_status_for_miner(miner),
            "uptimeS": miner.uptime_s,
            "mac": mac,
        }],
        "request": {
            "includeConfigured": request.include_configured,
            "manualIps": &request.manual_ips,
            "hintIps": &request.hint_ips,
        },
        "limitations": [
            "This endpoint is read-only and does not scan subnets or contact other miners.",
            "Manual IP probing remains browser-side in the dashboard until LAN discovery is linked.",
            "Status is derived from local runtime state; it is not proof of pool share acceptance."
        ],
    })
}

fn pool_status_is_connected(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "alive" | "connected" | "mining" | "proxied"
    )
}

fn build_local_fleet_pool_stats_response(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: String,
    ip: String,
    now_s: u64,
) -> serde_json::Value {
    let shares_answered = miner.accepted.saturating_add(miner.rejected);
    let snapshot = dcentrald_stratum::pool_api::MinerPoolSnapshot {
        miner_id: hostname.clone(),
        host: ip,
        model: Some(fleet_model_label(hw).to_string()),
        active_pool_url: miner.pool.url.clone(),
        connected: pool_status_is_connected(&miner.pool.status),
        donating: miner.pool.donating,
        // W5.5: surface the donation route on the local-fleet snapshot so
        // single-miner dashboards and fleet aggregators show the same
        // primary-vs-fallback breakdown.
        // TEL-003: sanitize the donation URL on the fleet snapshot too (it is
        // serialized into the unauthenticated /api/pools.stats-class surface),
        // matching the masked donation worker just below and every other surface.
        donation_active_url: dcentrald_stratum::pool_api::sanitize_pool_url(
            &miner.pool.donation_active_url,
        ),
        // GROUP-B SW-08 follow-up: mask the wallet-shaped donation worker
        // before it lands in the fleet-stats snapshot. This snapshot is
        // serialized into the unauthenticated `/api/pools.stats`-class
        // surface; redact for consistency with the masked `/api/pools`
        // donation block and the setup-wizard wallet masking.
        donation_active_worker: dcentrald_common::wallet_mask::mask_wallet(
            &miner.pool.donation_active_worker,
        ),
        donation_pool_index: miner.pool.donation_pool_index,
        shares_submitted: shares_answered,
        shares_accepted: miner.accepted,
        shares_rejected: miner.rejected,
        shares_unresolved: miner.pool.failover.shares_unresolved,
        pending_submit_dropped: miner.pool.failover.pending_submit_dropped,
        jobs_received: 0,
        current_difficulty: if miner.pool.difficulty.is_finite() && miner.pool.difficulty >= 0.0 {
            miner.pool.difficulty
        } else {
            0.0
        },
        failover_switch_count: miner.pool.failover.switch_count,
        last_seen_s: now_s,
    };
    let stats = dcentrald_stratum::pool_api::aggregate_pool_stats([snapshot], now_s, 60);

    serde_json::json!({
        "schema": "dcentrald-stratum::pool_api::FleetPoolStats v1",
        "status": "ok",
        "source": "local_state",
        "generated_at_s": now_s,
        "stats": stats,
        "limitations": [
            "This endpoint aggregates the local miner state only; it does not scan LAN peers or contact pools.",
            "shares_submitted is derived from accepted + rejected replies when no pending-submit publisher is installed.",
            "connected reflects the local pool status label and is not proof of accepted shares."
        ],
    })
}

pub(crate) fn push_rest_audit_free(state: &AppState, category: &str, message: impl Into<String>) {
    crate::push_audit_event(
        state,
        "rest_dashboard",
        dcentrald_api_types::audit_log::AuditEvent::Free {
            category: category.to_string(),
            message: message.into(),
        },
    );
}

fn room_temp_json(state: &AppState) -> serde_json::Value {
    let raw = state
        .room_temp_c10
        .load(std::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!(raw as f32 / 10.0)
    }
}

fn room_temp_c(state: &AppState) -> Option<f32> {
    let raw = state
        .room_temp_c10
        .load(std::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        None
    } else {
        Some(raw as f32 / 10.0)
    }
}

fn swarm_control_mode(state: &AppState) -> HomeControlMode {
    if room_temp_c(state).is_some() {
        HomeControlMode::Thermal
    } else {
        HomeControlMode::Manual
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn antminer_board_version(hw: &crate::HardwareInfo) -> String {
    if hw.control_board.starts_with("AML") || hw.control_board.contains("am2") {
        hw.control_board.clone()
    } else {
        "am1-s9".to_string()
    }
}

fn antminer_board_target(hw: &crate::HardwareInfo) -> String {
    if hw.control_board.starts_with("AML") {
        "antminer-amlogic".to_string()
    } else if hw.control_board.contains("am2") {
        "antminer-zynq-am2".to_string()
    } else {
        "antminer-zynq-am1".to_string()
    }
}

fn swarm_api_url(ipv4: &str) -> Option<String> {
    if ipv4.is_empty() || ipv4 == "unknown" {
        None
    } else {
        Some(format!("http://{}/api/swarm", ipv4))
    }
}

fn swarm_mcp_url(ipv4: &str) -> Option<String> {
    if ipv4.is_empty() || ipv4 == "unknown" {
        None
    } else {
        Some(format!("http://{}{}", ipv4, MCP_HTTP_PATH))
    }
}

#[derive(Debug, Deserialize)]
struct McpJsonRpcRequest {
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

fn mcp_jsonrpc_response(
    id: Option<serde_json::Value>,
    result: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn mcp_jsonrpc_error(id: Option<serde_json::Value>, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

fn mcp_read_tool_descriptors() -> Vec<serde_json::Value> {
    minimal_profile(MCP_TRANSPORT)
        .tools
        .into_iter()
        .filter(|tool| !tool.write)
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                },
                "annotations": {
                    "readOnlyHint": true,
                },
            })
        })
        .collect()
}

#[cfg(test)]
fn mcp_read_tool_names() -> Vec<String> {
    minimal_profile(MCP_TRANSPORT)
        .tools
        .into_iter()
        .filter(|tool| !tool.write)
        .map(|tool| tool.name)
        .collect()
}

fn mcp_profile_write_tool_name(name: &str) -> bool {
    minimal_profile(MCP_TRANSPORT)
        .tools
        .into_iter()
        .any(|tool| tool.write && tool.name == name)
}

fn mcp_initialize_payload() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {
                "listChanged": false,
            },
        },
        "serverInfo": {
            "name": "dcentos-dcentrald",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "profile": MINIMAL_PROFILE_ID,
        "transport": MCP_TRANSPORT,
        "readOnly": true,
    })
}

fn mcp_tools_list_payload() -> serde_json::Value {
    serde_json::json!({
        "tools": mcp_read_tool_descriptors(),
    })
}

fn mcp_tool_content(payload: serde_json::Value) -> serde_json::Value {
    // data-model-fields §3: mirror the canonical 3-flag read/control envelope
    // {read_only, control_actions, hardware_writes} onto the tools/call result so
    // the cross-firmware MCP consumer reads the SAME self-describing envelope
    // structure on both DCENT_OS and DCENT_axe. This REST-mounted /mcp bridge is
    // read-only BY CONSTRUCTION — `mcp_tool_payload` rejects every write-profile
    // tool (mcp_tool_payload:1711) before reaching here — so every result on this
    // mount is read_only=true. This is contract-structure alignment ONLY; it is
    // NOT an auth-posture change (the mount stays read-only-by-default exactly as
    // today, and these flags drive no behavior — they are purely descriptive).
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        }],
        "isError": false,
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
    })
}

fn mcp_status_payload(
    miner: &crate::MinerState,
    mode: crate::OperatingMode,
    power: &dcentrald_autotuner::LivePowerEstimate,
    hardware: &crate::HardwareInfo,
) -> serde_json::Value {
    let power_projection = (power, miner, hardware);
    serde_json::json!({
        "status": "ok",
        "source": "dcentrald-api",
        "mode": mode,
        "firmware_version": miner.firmware_version,
        "uptime_s": miner.uptime_s,
        "hashrate_ghs": miner.hashrate_ghs,
        "shares": {
            "accepted": miner.accepted,
            "rejected": miner.rejected,
        },
        "pool": {
            "url": dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
            "status": miner.pool.status,
            "connected": is_pool_connected(&miner.pool.status),
            "connecting": is_pool_connecting(&miner.pool.status),
            "mining_capable": is_pool_mining_capable(&miner.pool.status),
        },
        "fans": {
            "pwm": miner.fans.pwm,
            "rpm": miner.fans.rpm,
        },
        "power": build_mcp_status_power_section(&power_projection),
    })
}

fn build_mcp_status_power_section(projection: &PowerTelemetryProjection) -> serde_json::Value {
    let live_available = projection.live_power_available;
    serde_json::json!({
        "wall_watts": if live_available && projection.wall_watts > 0 {
            serde_json::json!(projection.wall_watts)
        } else {
            serde_json::Value::Null
        },
        "board_watts": if live_available && projection.board_watts > 0 {
            serde_json::json!(projection.board_watts)
        } else {
            serde_json::Value::Null
        },
        "efficiency_jth": if live_available && projection.efficiency_jth > 0.0 {
            serde_json::json!(projection.efficiency_jth)
        } else {
            serde_json::Value::Null
        },
        "btu_h": if live_available && projection.btu_h > 0.0 {
            serde_json::json!(projection.btu_h)
        } else {
            serde_json::Value::Null
        },
        "source": projection.source.as_str(),
        "source_detail": projection.source_detail,
        "live_power_available": live_available,
        "modeled": projection.modeled,
        "note": projection.note,
        "calibrated": projection.calibrated,
        "calibration_multiplier": projection.calibration_multiplier,
    })
}

fn measured_wall_watts_for_unprovenanced_surface(projection: &PowerTelemetryProjection) -> f64 {
    if projection.live_power_available
        && matches!(projection.source_detail, "pmbus_measured" | "adc_measured")
    {
        projection.wall_watts as f64
    } else {
        0.0
    }
}

fn mcp_device_info_payload(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: String,
    ipv4: String,
) -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "source": "dcentrald-api",
        "hostname": hostname,
        "ipv4": ipv4,
        "firmware_version": miner.firmware_version,
        "control_board": hw.control_board,
        "chip_type": hw.chip_type,
        "identification_confidence": &hw.identification.confidence,
        "identification": &hw.identification,
        "hashboard_type": hw.hb_type,
        "board_version": antminer_board_version(hw),
        "board_target": antminer_board_target(hw),
    })
}

fn mcp_tool_payload(state: &AppState, name: &str) -> Result<serde_json::Value, &'static str> {
    if mcp_profile_write_tool_name(name) {
        return Err("MCP tool is not available on this read-only mount");
    }

    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    match name {
        "get_status" => {
            let mode = *state.mode_rx.borrow();
            let power = state.power_rx.borrow().clone();
            Ok(mcp_status_payload(&miner, mode, &power, &hw))
        }
        "get_device_info" => Ok(mcp_device_info_payload(
            &miner,
            &hw,
            local_hostname(),
            eth0_ipv4(),
        )),
        "get_swarm_status" => {
            let mac = std::fs::read_to_string("/sys/class/net/eth0/address")
                .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
                .trim()
                .to_string();
            let ipv4 = eth0_ipv4();
            let power = state.power_rx.borrow().clone();
            let power_projection = (&power, &miner, &hw);
            let measured_wall_watts =
                measured_wall_watts_for_unprovenanced_surface(&power_projection);
            Ok(serde_json::to_value(swarm_status_payload(
                state,
                &miner,
                &hw,
                &local_hostname(),
                &mac,
                &ipv4,
                measured_wall_watts,
            ))
            .unwrap_or_else(|_| serde_json::json!({"status": "error"})))
        }
        _ => Err("Unknown MCP tool"),
    }
}

fn mcp_extract_tool_name(params: &serde_json::Value) -> Option<&str> {
    params.get("name").and_then(|value| value.as_str())
}

async fn post_mcp(
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpJsonRpcRequest>,
) -> impl IntoResponse {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => mcp_jsonrpc_response(id, mcp_initialize_payload()),
        "tools/list" => mcp_jsonrpc_response(id, mcp_tools_list_payload()),
        "tools/call" => {
            let Some(name) = mcp_extract_tool_name(&request.params) else {
                return Json(mcp_jsonrpc_error(
                    request.id,
                    -32602,
                    "Missing MCP tool name",
                ))
                .into_response();
            };
            match mcp_tool_payload(&state, name) {
                Ok(payload) => mcp_jsonrpc_response(id, mcp_tool_content(payload)),
                Err(message) => mcp_jsonrpc_error(id, -32601, message),
            }
        }
        "get_status" | "get_device_info" | "get_swarm_status" => {
            match mcp_tool_payload(&state, &request.method) {
                Ok(payload) => mcp_jsonrpc_response(id, payload),
                Err(message) => mcp_jsonrpc_error(id, -32601, message),
            }
        }
        _ => mcp_jsonrpc_error(id, -32601, "Unknown MCP method"),
    };

    Json(result).into_response()
}

fn local_swarm_node(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: &str,
    mac: &str,
    ipv4: &str,
) -> SwarmNode {
    let profile = chip_type_to_chip_id(&hw.chip_type).and_then(MinerProfile::for_chip);
    SwarmNode {
        id: format!("dcentos-{}", mac.replace(':', "").to_ascii_lowercase()),
        hostname: hostname.to_string(),
        display_name: profile.map(|p| p.name).unwrap_or("Antminer").to_string(),
        ip: ipv4.to_string(),
        board_model: profile.map(|p| p.name).unwrap_or("Antminer").to_string(),
        board_version: antminer_board_version(hw),
        board_target: antminer_board_target(hw),
        asic_model: hw.chip_type.clone(),
        firmware_version: miner.firmware_version.clone(),
        mining_enabled: miner.hashrate_ghs > 0.0 || is_pool_mining_capable(&miner.pool.status),
        pool_connected: is_pool_connected(&miner.pool.status),
        hashrate_ghs: miner.hashrate_ghs,
        last_seen_unix_ms: unix_time_ms(),
        source: SwarmSource::SelfReported,
    }
}

fn swarm_discovery(ipv4: &str) -> SwarmDiscoveryInfo {
    let mcp_url = swarm_mcp_url(ipv4);
    let mcp_mounted = mcp_url.is_some();
    SwarmDiscoveryInfo {
        mdns_enabled: false,
        mdns_hostname: None,
        discovery_hint: "LAN discovery is not linked yet; use the shared /api/swarm status surface"
            .to_string(),
        api_url: swarm_api_url(ipv4),
        mcp_url,
        mcp_transport: mcp_mounted.then(|| MCP_TRANSPORT.to_string()),
        mcp_profile: mcp_mounted.then(|| MINIMAL_PROFILE_ID.to_string()),
    }
}

/// P2-5 truth-contract: the swarm capabilities this node honestly advertises.
///
/// `target_temp_control` is **false**. Although `dcentrald-thermal` defines a
/// `HeaterController` PID (room-temp setpoint → power), it is **not instantiated
/// or run anywhere in the daemon** (its `compute_adjustment`/`set_target_watts`/
/// `effective_target_watts` are only called inside `heater.rs` itself), and there
/// is **no REST endpoint to set a room-temperature setpoint** (only observed-temp
/// inputs via `POST /api/home/room-temp` and `/api/swarm/room-temp`). With no
/// live closed-loop controller and no setpoint, the node cannot drive a room to a
/// target temperature, so advertising the capability would be a truth-contract
/// violation. `room_temp_input` (observed temp) and `target_watts_control` (power
/// target via `POST /api/home/target`, read by the thermal/autotuner loop) ARE
/// wired, so they stay true.
///
/// Do NOT flip `target_temp_control` back to true without first wiring a live
/// closed-loop room-temp controller AND a setpoint endpoint — and that controller
/// MUST keep the existing fan PWM ≤ 30 home cap and voltage caps on every actuated
/// command (cut hash before raising fan noise).
fn swarm_node_capabilities(identify: bool) -> dcent_schema::swarm::SwarmCapabilities {
    dcent_schema::swarm::SwarmCapabilities {
        can_coordinate: true,
        room_temp_input: true,
        target_temp_control: false,
        target_watts_control: true,
        identify,
        mcp: true,
    }
}

fn dcent_swarm_info(
    state: &AppState,
    miner: &crate::MinerState,
    mac: &str,
    heat_watts: f64,
    wall_watts: f64,
) -> DcentSwarmInfo {
    let _ = heat_watts;
    DcentSwarmInfo {
        schema: dcent_schema::swarm::SWARM_SCHEMA_VERSION,
        node_id: format!("dcentos-{}", mac.replace(':', "").to_ascii_lowercase()),
        family: "antminer".to_string(),
        role: SwarmRole::Worker,
        cluster_id: None,
        queen_id: None,
        capabilities: swarm_node_capabilities(state.led_tx.is_some()),
        home: dcent_schema::swarm::SwarmHomeStatus {
            control_mode: swarm_control_mode(state),
            observed_room_temp_c: room_temp_c(state),
            // P2-5: no room-temp setpoint exists (target_temp_control = false),
            // so this is honestly None — never fabricate a setpoint.
            target_room_temp_c: None,
            target_watts: None,
            heat_watts: wall_watts,
            heat_btu_h: wall_watts * 3.412_142,
            heating_active: miner.hashrate_ghs > 0.0,
        },
    }
}

fn swarm_status_payload(
    state: &AppState,
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
    hostname: &str,
    mac: &str,
    ipv4: &str,
    wall_watts: f64,
) -> SwarmStatus {
    SwarmStatus {
        schema: dcent_schema::swarm::SWARM_SCHEMA_VERSION,
        node_id: format!("dcentos-{}", mac.replace(':', "").to_ascii_lowercase()),
        role: SwarmRole::Worker,
        cluster_id: None,
        queen_id: None,
        hashrate_ghs: miner.hashrate_ghs,
        power_watts: wall_watts,
        heat_watts: wall_watts,
        heat_btu_h: wall_watts * 3.412_142,
        control_mode: swarm_control_mode(state),
        observed_room_temp_c: room_temp_c(state),
        // P2-5: no room-temp setpoint exists (target_temp_control = false),
        // so this is honestly None — never fabricate a setpoint.
        target_room_temp_c: None,
        target_watts: None,
        heating_active: miner.hashrate_ghs > 0.0,
        updated_at: autotuner_now_s(),
        local: Some(local_swarm_node(miner, hw, hostname, mac, ipv4)),
        peers: Vec::new(),
        peer_count: 0,
        discovery: Some(swarm_discovery(ipv4)),
        coordination: SwarmCoordinationStatus::default(),
    }
}

/// Get the active config file path (prefers /data/dcentrald.toml, falls back to /etc/).
pub(crate) fn get_config_path() -> &'static str {
    if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    }
}

/// First-boot setup must always materialize a persistent config in /data.
pub(crate) fn get_writable_config_path() -> &'static str {
    "/data/dcentrald.toml"
}

pub(crate) fn load_config_table_for_write() -> std::result::Result<toml::Table, String> {
    let writable_path = get_writable_config_path();
    let contents = if std::path::Path::new(writable_path).exists() {
        std::fs::read_to_string(writable_path)
            .map_err(|e| format!("Failed to read config: {}", e))?
    } else {
        std::fs::read_to_string(get_config_path()).unwrap_or_default()
    };

    let mut table: toml::Table = if contents.trim().is_empty() {
        toml::Table::new()
    } else {
        toml::from_str(&contents).map_err(|e| format!("Failed to parse config: {}", e))?
    };

    // SW-13 (config-loss hardening): every read-modify-write path in this file
    // funnels through this loader. Run the schema-version preservation step here
    // so that on the next save (a) the on-disk file becomes self-describing
    // (`[general].schema_version` is stamped) and (b) a version drift is logged.
    // The read-modify-write pattern itself is what preserves unknown/renamed
    // keys: callers parse into a `toml::Table`, mutate only the section they own,
    // and re-serialize the whole table — so any key from an older or a newer
    // build survives the round-trip untouched. This step deliberately does NOT
    // delete, rename, or coerce any key (it only ADDS the version stamp), so it
    // can never itself cause the field loss it guards against.
    migrate_config_schema(&mut table);
    Ok(table)
}

fn ensure_toml_value_table_section<'a>(
    doc: &'a mut toml::Value,
    section: &str,
) -> std::result::Result<&'a mut toml::value::Table, String> {
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "Config document root is not a TOML table".to_string())?;
    let value = root
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
    value
        .as_table_mut()
        .ok_or_else(|| format!("[{}] is not a TOML table", section))
}

/// Best-effort, read-only load of the active config table (`/data` then `/etc`),
/// defaulting to an empty table when unreadable/unparseable. The read-only
/// sibling of [`load_config_table_for_write`] for status/estimate handlers that
/// never mutate the file.
pub(crate) fn read_config_table_or_default() -> toml::Table {
    std::fs::read_to_string(get_config_path())
        .ok()
        .and_then(|c| toml::from_str::<toml::Table>(&c).ok())
        .unwrap_or_default()
}

/// Default residential electricity rate ($/kWh) used when the operator has not
/// confirmed one. MUST stay in lockstep with the daemon's
/// `config.rs::default_electricity_rate()` (`[home].electricity_rate` default).
/// P2-4 (§4.E): the previous client default of `0.10` disagreed with the daemon
/// default of `0.12`; the daemon config is now the single source of truth and
/// the dashboard surfaces THIS value rather than its own localStorage guess.
pub(crate) const DEFAULT_ELECTRICITY_RATE_USD_PER_KWH: f64 = 0.12;
/// Default display currency, matching `config.rs::default_currency()`.
pub(crate) const DEFAULT_CURRENCY: &str = "USD";

/// Home-economics view read from the daemon `[home]` config section. This is the
/// SINGLE SOURCE OF TRUTH for the electricity rate + currency used by every
/// cost/earnings surface (daemon-side estimates AND the dashboard, which must
/// read these back instead of guessing).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HomeEconomics {
    /// Electricity rate in `currency` per kWh.
    pub rate_usd_per_kwh: f64,
    /// Display currency code (e.g. "USD").
    pub currency: String,
    /// True once the operator has explicitly confirmed a rate
    /// (`[home].electricity_rate_calibrated = true`). While false, `rate` is the
    /// daemon DEFAULT guess and cost/earnings surfaces must be labelled
    /// "uncalibrated" — never presented as an operator-confirmed value.
    pub rate_calibrated: bool,
}

/// Read the home electricity rate + currency + calibration flag from a parsed
/// config table. Pure — host-safe for unit tests. Negative / non-finite rates
/// are rejected back to the default so a corrupt value can never poison every
/// cost estimate.
pub(crate) fn home_economics_from_table(table: &toml::Table) -> HomeEconomics {
    let home = table.get("home").and_then(|v| v.as_table());
    let rate = home
        .and_then(|h| h.get("electricity_rate"))
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .filter(|r| r.is_finite() && *r >= 0.0)
        .unwrap_or(DEFAULT_ELECTRICITY_RATE_USD_PER_KWH);
    let currency = home
        .and_then(|h| h.get("currency"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(DEFAULT_CURRENCY)
        .to_string();
    let rate_calibrated = home
        .and_then(|h| h.get("electricity_rate_calibrated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    HomeEconomics {
        rate_usd_per_kwh: rate,
        currency,
        rate_calibrated,
    }
}

/// CFG-1/CFG-2: apply a `[home]` power-target write to a *full* effective config
/// table in place, mutating ONLY the `[home].target_watts` (and optional
/// `[home].preset`) keys.
///
/// This is the pure core of [`post_home_power_target`]. It MUST be fed the full
/// effective table from [`load_config_table_for_write`] (which merges the baked
/// `/etc` config when `/data` is absent) so that on a fresh beta install the
/// first home-UX touch does not write a `[home]`-only file that shadows the
/// baked `[pool]`/`[power]`/`[thermal]`/`[auth]` sections on the next reboot.
/// Every section other than `[home]` is left byte-for-byte; only the two home
/// keys are inserted/overwritten. Host-safe for unit tests.
pub(crate) fn apply_home_power_target_to_table(
    table: &mut toml::Table,
    target_watts: u32,
    preset: Option<&str>,
) {
    let home = table
        .entry("home".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(ref mut home_table) = home {
        home_table.insert(
            "target_watts".into(),
            toml::Value::Integer(target_watts as i64),
        );
        if let Some(preset) = preset {
            home_table.insert("preset".into(), toml::Value::String(preset.to_string()));
        }
    }
}

/// Host-safe helper that mutates only `[mode.home.night_mode]` in a full config.
pub(crate) fn apply_home_night_mode_to_table(
    table: &mut toml::Table,
    enabled: bool,
    start_hour: u8,
    end_hour: u8,
    max_fan_pwm: u8,
    power_reduction_pct: u8,
) {
    let mode = table
        .entry("mode".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(ref mut mode_table) = mode {
        let home = mode_table
            .entry("home".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut home_table) = home {
            let nm = home_table
            .entry("night_mode".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(ref mut nm_table) = nm {
                nm_table.insert("enabled".into(), toml::Value::Boolean(enabled));
                nm_table.insert("start_hour".into(), toml::Value::Integer(start_hour as i64));
                nm_table.insert("end_hour".into(), toml::Value::Integer(end_hour as i64));
                nm_table.insert(
                    "max_fan_pwm".into(),
                    toml::Value::Integer(max_fan_pwm as i64),
                );
                nm_table.insert(
                    "power_reduction_pct".into(),
                    toml::Value::Integer(power_reduction_pct as i64),
                );
            }
        }
    }
}

/// In-place, additive config schema preservation/migration step.
///
/// Reads the on-disk schema marker `[general].schema_version`, compares it to the
/// current [`CONFIG_SCHEMA_VERSION`], emits a `tracing` line describing the drift,
/// and stamps the current version into `[general].schema_version` so the file is
/// self-describing after the next save.
///
/// **Invariants (load-bearing — see SW-13):**
/// - Never removes a top-level key, a section, or a key inside a section.
/// - Never renames or coerces an existing value.
/// - Only mutation is creating `[general]` (if absent) and writing
///   `schema_version` into it. Every other key the operator (or an older/newer
///   firmware build) wrote is left byte-for-byte and survives the round-trip.
///
/// Returns the number of top-level keys preserved (used by tests; the count is
/// computed *after* the version stamp so an empty config reports `>= 1`).
fn migrate_config_schema(table: &mut toml::Table) -> usize {
    let current = i64::from(CONFIG_SCHEMA_VERSION);

    // The on-disk marker. A config written by any build before SW-13 has no
    // `[general].schema_version` — treat that as "unversioned / pre-1" so we log
    // it once and stamp it, but never drop the operator's existing keys.
    let on_disk: Option<i64> = table
        .get("general")
        .and_then(|value| value.as_table())
        .and_then(|general| general.get("schema_version"))
        .and_then(|value| value.as_integer());

    let preserved_top_level_keys: Vec<String> = table.keys().cloned().collect();

    match on_disk {
        Some(found) if found == current => {
            // Up to date — nothing to log, stamp is a no-op (idempotent).
        }
        Some(found) if found < current => {
            tracing::warn!(
                target: "config_migrate",
                on_disk_schema = found,
                current_schema = current,
                preserved_top_level_keys = ?preserved_top_level_keys,
                "Loaded an OLDER config schema; preserving all existing keys (no field drop) \
                 and stamping the current schema_version on next save"
            );
        }
        Some(found) => {
            // found > current: config written by a NEWER firmware build (e.g. a
            // downgrade). Preserve everything; do NOT overwrite a higher version
            // down to ours, or a re-upgrade would think the file is stale.
            tracing::warn!(
                target: "config_migrate",
                on_disk_schema = found,
                current_schema = current,
                preserved_top_level_keys = ?preserved_top_level_keys,
                "Loaded a NEWER config schema than this firmware build; preserving all keys \
                 and leaving the higher schema_version intact (forward-compatible round-trip)"
            );
            // Return early WITHOUT re-stamping a lower version.
            return table.len();
        }
        None => {
            tracing::warn!(
                target: "config_migrate",
                current_schema = current,
                preserved_top_level_keys = ?preserved_top_level_keys,
                "Loaded an UNVERSIONED config (pre-SW-13); preserving all existing keys \
                 (no field drop) and stamping schema_version on next save"
            );
        }
    }

    // Stamp the current version (only when on-disk is missing/older/equal).
    let general = table
        .entry("general".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(general_table) = general {
        general_table.insert("schema_version".to_string(), toml::Value::Integer(current));
    }

    table.len()
}

#[derive(Debug, Clone, Copy)]
struct ConfigBackupSourceSpec {
    id: &'static str,
    label: &'static str,
    path: &'static str,
}

#[derive(Debug, Serialize)]
struct ConfigBackupSourceEntry {
    id: &'static str,
    label: &'static str,
    path: &'static str,
    active: bool,
    writable_target: bool,
    metadata_status: String,
    exists: bool,
    size_bytes: Option<u64>,
    modified_ms: Option<u128>,
}

#[derive(Debug, Serialize)]
struct ConfigBackupRedactionPolicy {
    content_included: bool,
    secret_key_patterns: &'static [&'static str],
    notes: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct ConfigBackupManifestResponse {
    status: &'static str,
    read_only: bool,
    content_collected: bool,
    restore_supported: bool,
    daemon_config_export_supported: bool,
    dashboard_preferences_export_supported: bool,
    sources: Vec<ConfigBackupSourceEntry>,
    redaction_policy: ConfigBackupRedactionPolicy,
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct ApiCompatibilityRouteEntry {
    method: &'static str,
    path: &'static str,
    support: &'static str,
    mutates: bool,
    compatibility: &'static [&'static str],
    provenance: &'static str,
    unsupported_fields: &'static [&'static str],
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct ApiCompatibilityCommandEntry {
    name: &'static str,
    support: &'static str,
    mutates: bool,
    provenance: &'static str,
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct ApiCompatibilitySurface {
    id: &'static str,
    label: &'static str,
    protocol: &'static str,
    default_port: Option<u16>,
    default_bind: Option<&'static str>,
    compatibility: &'static [&'static str],
    routes: &'static [ApiCompatibilityRouteEntry],
    commands: &'static [ApiCompatibilityCommandEntry],
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct ApiCompatibilityOmission {
    path: Option<&'static str>,
    surface: Option<&'static str>,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct ApiCompatibilityManifestResponse {
    status: &'static str,
    schema_version: u8,
    read_only: bool,
    content_collected: bool,
    probe_performed: bool,
    handlers_executed: bool,
    surfaces: &'static [ApiCompatibilitySurface],
    omissions: &'static [ApiCompatibilityOmission],
    limitations: &'static [&'static str],
}

fn competitive_decentralization_gate() -> serde_json::Value {
    serde_json::json!({
        "license_required": false,
        "license_server_required": false,
        "activation_required": false,
        "license_check_performed": false,
        "mandatory_fee": false,
        "fee_route": "transparent_donation",
        "donation": {
            "default_enabled": true,
            "current_enabled": null,
            "default_percent": 2.0,
            "current_percent": null,
            "cycle_duration_s_default": 3600,
            "current_cycle_duration_s": null,
            "pool_visible": true,
            "disable_supported": true,
            "donation_off_test_status": "not_run",
            "current_state_source": "not_read_by_static_contract"
        },
        "offline_behavior": "local_first",
        "external_dependencies": [
            {
                "id": "user_pool",
                "purpose": "share_submission",
                "default_state": "user_configured",
                "required": "required_for_mining_only",
                "disable_impact": "setup, dashboard, diagnostics, and local APIs still work"
            },
            {
                "id": "optional_integrations",
                "purpose": "mqtt_webhook_solar_offgrid_integrations",
                "default_state": "disabled_or_user_configured",
                "required": "optional",
                "disable_impact": "core mining and safety unaffected"
            }
        ],
        "source_basis": ["public", "live_probe", "clean_room"],
        "repair_diagnostic": "read_only_default",
        "write_surfaces": [
            {
                "surface": "REST debug endpoints",
                "default": "mode_gated",
                "write_gate": "Hacker mode plus explicit confirmation where implemented",
                "audit_status": "present"
            },
            {
                "surface": "MCP raw hardware tools",
                "default": "expert_only",
                "write_gate": "requires follow-up raw-write gate audit before promotion",
                "audit_status": "partial"
            }
        ],
        "home_miner_safe": false,
        "home_miner_safe_status": "partial",
        "docs_link": "https://github.com/DCentralTech/DCENT_OS",
        "docs_link_status": "repo_path_not_served_by_dashboard",
        "recovery_link": "https://github.com/DCentralTech/DCENT_OS",
        "recovery_link_status": "repo_path_not_served_by_dashboard",
    })
}

fn competitive_readiness_feature(
    id: &'static str,
    label: &'static str,
    status: &'static str,
    priority: &'static str,
    competitor_reference: &'static str,
    home_miner_value: &'static str,
    current_behavior: &'static str,
    risk: &'static str,
    clean_room_path: &'static str,
    acceptance_test: &'static str,
    source_basis: &'static str,
    telemetry_source: &'static str,
    confidence: &'static str,
    blockers: &'static [&'static str],
    docs_link: &'static str,
) -> serde_json::Value {
    let promotion_allowed = status == "proven";
    let home_miner_safe_status = if promotion_allowed {
        "proven"
    } else {
        "blocked_or_partial"
    };

    serde_json::json!({
        "id": id,
        "label": label,
        "status": status,
        "priority": priority,
        "competitor_reference": competitor_reference,
        "home_miner_value": home_miner_value,
        "current_behavior": current_behavior,
        "risk": risk,
        "clean_room_path": clean_room_path,
        "acceptance_test": acceptance_test,
        "source_basis": source_basis,
        "telemetry_source": telemetry_source,
        "confidence": confidence,
        "blockers": blockers,
        "docs_link": docs_link,
        "recovery_link": docs_link,
        "license_required": false,
        "mandatory_fee": false,
        "promotion_allowed": promotion_allowed,
        "decentralization": {
            "license_required": false,
            "mandatory_fee": false,
            "fee_route": "transparent_donation_or_none",
            "offline_behavior": "local_first",
            "source_basis": [source_basis],
            "repair_diagnostic": "read_only_default",
            "home_miner_safe_status": home_miner_safe_status
        },
    })
}

///  W2 — render the canonical cross-firmware capability matrix
/// (from `dcentrald_api_types::firmware_stratum_matrix`) as JSON for
/// the competitive readiness dashboard widget.
fn firmware_matrix_response() -> serde_json::Value {
    use dcentrald_api_types::firmware_stratum_matrix::FIRMWARE_CAPABILITIES;
    let rows: Vec<serde_json::Value> = FIRMWARE_CAPABILITIES
        .iter()
        .map(|c| {
            serde_json::json!({
                "flavor": c.flavor,
                "stratum_binary": c.stratum_binary,
                "stratum_v1": c.stratum_v1,
                "stratum_v2": c.stratum_v2,
                "version_rolling": c.version_rolling,
                "version_rolling_mask": format!("0x{:08x}", c.version_rolling_mask),
                "suggest_difficulty": c.suggest_difficulty,
                "dev_fee_in_factory": c.dev_fee_in_factory,
                "dev_fee_runtime": c.dev_fee_runtime,
                "dev_fee_runtime_pct_low": c.dev_fee_runtime_pct_low,
                "dev_fee_runtime_pct_high": c.dev_fee_runtime_pct_high,
                "default_pool_url": c.default_pool_url,
                "default_worker": c.default_worker,
                "default_password": c.default_password,
            })
        })
        .collect();
    serde_json::json!({
        "schema": "dcentrald-api-types::firmware_stratum_matrix v1",
        "row_count": rows.len(),
        "rows": rows,
    })
}

/// W1.1 default-credential lockdown -- compute the SSH gate state from
/// the same files the on-device `S50dropbear` init script consults.
///
/// Returns one of: `"disabled"` | `"enabled-by-wizard"` | `"enabled-by-keys"`.
///
/// File contract (must stay in lockstep with
/// `br2_external_dcentos/board/*/rootfs-overlay/etc/init.d/S50dropbear`):
///   - `/data/dcent/.ssh-enabled`   -- gate flag, written by `dcent-enable-ssh`
///   - `/data/dcent/auth.json`      -- Argon2id wizard credential (W1.1 #1)
///   - `/data/dcent/authorized_keys` -- operator-uploaded keys (W1.1 #2)
///
/// Pure helper; takes paths so unit tests can drive every branch without
/// poking real `/data` on the host.
pub(crate) fn compute_setup_ssh_state(
    ssh_enabled_path: &std::path::Path,
    authorized_keys_path: &std::path::Path,
    auth_json_path: &std::path::Path,
) -> &'static str {
    if !ssh_enabled_path.exists() {
        return "disabled";
    }
    // Authorized-keys takes precedence: if the operator uploaded keys
    // through the dashboard, that's the explicit reason SSH is on.
    let keys_present = std::fs::metadata(authorized_keys_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if keys_present {
        return "enabled-by-keys";
    }
    if auth_json_path.exists() {
        return "enabled-by-wizard";
    }
    // Gate flag exists but neither evidence -- treat as disabled so the
    // dashboard never claims a state we can't justify.
    "disabled"
}

/// Default file-system view of the SSH gate (the daemon target paths).
fn current_setup_ssh_state() -> &'static str {
    compute_setup_ssh_state(
        std::path::Path::new("/data/dcent/.ssh-enabled"),
        std::path::Path::new("/data/dcent/authorized_keys"),
        std::path::Path::new("/data/dcent/auth.json"),
    )
}

fn build_competitive_readiness_response(now_ms: u64) -> serde_json::Value {
    let gate = competitive_decentralization_gate();
    let features = vec![
        competitive_readiness_feature(
            "decentralization_gate",
            "Decentralization Gate",
            "partial",
            "P0 invariant",
            "BraiinsOS/VNish/LuxOS commercial or proprietary surfaces",
            "Keeps DCENT_OS local-first, fee-transparent, repairable, and clean-room.",
            "Policy is documented and donation is visible; this read-only contract now makes the gate machine-readable.",
            "Parity pressure can introduce hidden dependencies, copied behavior, or misleading live claims.",
            "Require gate fields before promoting any competitive feature.",
            "Every promoted feature reports license, fee route, offline behavior, source basis, repair default, home safety, and docs link.",
            "clean_room_policy_and_firmware_manifest",
            "static_firmware_contract",
            "proven_policy_partial_runtime_audit",
            &["full network dependency audit not completed"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "s19jpro_139_native_mining",
            "S19j Pro .139 Native Mining",
            "blocked",
            "P0",
            "Stock Bitmain and BraiinsOS reliably bring up PIC/APW/UART before mining.",
            "Prevents home miners from being pushed onto an unreliable native path.",
            "13.7V rail is proven; chain UART RX remains 0 across ttyS1-4.",
            "Blind FPGA, PIC, APW, or UART writes can strand hardware or hide a stock-regression.",
            "Read-only FPGA/UIO state capture, then one gated relay experiment only with rollback proof.",
            "Cold boot records voltage, fans, relay regs, UART counters, accepted shares or rollback result.",
            "live_probe_and_dcentral_re_notes",
            "https://github.com/DCentralTech/DCENT_OS",
            "proven_blocker",
            &["FPGA relay write authority unknown", "UART RX zero", "accepted shares not proven"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "donation_transparency",
            "Transparent Donation Route",
            "partial",
            "P0 invariant",
            "Competitors commonly use license fees, mandatory fees, or commercial activation.",
            "Makes revenue routing visible and disableable for sovereign home miners.",
            "Donation config and active pool.donating surfaces exist; default-policy wording and donation-off route tests still need closure.",
            "Any hidden or ambiguous route breaks trust and decentralization.",
            "Keep donation explicit, bounded, disableable, and labelled as donation rather than a fee.",
            "Donation-off soak proves zero donation pool connects/submits; donation-on exposes percent, worker, cycle, and active state.",
            "firmware_config_and_stratum_code",
            "config/API/UI contract plus future soak",
            "proven_surfaces_missing_soak",
            &["donation-off route test not run", "default wording mismatch in older docs"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "watt_target_mining",
            "Watt Target Mining",
            "partial",
            "P1",
            "BraiinsOS and LuxOS expose watt-target operation.",
            "Lets home miners stay inside circuit and heat budgets.",
            "Power estimate, circuit cap, and watt-cap state exist; measured closed-loop watt PID is not proven across targets.",
            "Estimated power must not authorize upward tuning or residential circuit overrun.",
            "Expose readiness and allow only downward clamps until measured telemetry is trusted.",
            "Invalid or estimated-only telemetry blocks upward tuning; measured cap tests stay within derated circuit budget.",
            "clean_room_power_model_and_live_power_estimate",
            "status/power watch state plus future PDU/PMBus evidence",
            "partial",
            &["measured power authority incomplete", "cross-family soak missing"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "autotuning",
            "Autotuning",
            "blocked",
            "P0/P1",
            "BraiinsOS, VNish, and LuxOS ship production tuning loops.",
            "Improves efficiency without making home users hand-tune voltage/frequency.",
            "Autotuner architecture, visibility, saved profiles, and family capability gates exist; broad live activation is not proven.",
            "Bad frequency or voltage can lower hashrate, raise HW errors, or damage hardware.",
            "Promote one guarded S9/BM1387 canary before any wider family enablement.",
            "Canary tune holds thermal/circuit caps, raises no HW-error regression, and rolls back to last-known-good profile.",
            "clean_room_autotuner_code_and_hw_soak_plan",
            "autotuner visibility API plus future live canary",
            "partial_architecture_blocked_runtime",
            &["live tuning soak missing", "rollback not proven across families"],
            "docs/AUTOTUNER_DEEP_RESEARCH.md",
        ),
        competitive_readiness_feature(
            "pool_failover",
            "Pool Failover",
            "partial",
            "P1",
            "Stock, BraiinsOS, VNish, and LuxOS support failover behavior.",
            "Improves uptime without touching mining hardware.",
            "Pool failover code/config surfaces exist, but dashboard/runtime proof needs exact switch reason and pending-share accounting.",
            "Wrong failover can lose shares, flap pools, or hide rejected/unresolved work.",
            "Expose active pool index, switch reason, failure counters, and stale-job flushing proof.",
            "Mock primary fails; miner switches to failover, flushes stale jobs, preserves accounting, and returns only after stable primary.",
            "stratum_v1_clean_room_code",
            "stratum client tests plus future mock-pool integration",
            "partial",
            &["mock failover integration pending", "dashboard switch reason incomplete"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "home_safe_profiles",
            "Home Safe Profiles",
            "saved_only",
            "P1",
            "VNish/LuxOS expose profile-style UX; stock is simple and familiar.",
            "Gives beginners quiet, circuit-safe choices with expert override.",
            "Home mode, presets, night concepts, and safety caps exist; some profile/night writes are saved-only or not runtime-proven.",
            "UI must not claim live fan, watt, voltage, or frequency changes unless the daemon applies them.",
            "Return saved/applied/requires_restart/effective_source for home target, night, and profiles.",
            "Dashboard never claims active profile application unless API reports runtime adoption.",
            "home_mode_config_and_dashboard_contract",
            "config/API response contract",
            "proven_gap_no_hardware_change_needed",
            &["atomic profile apply contract missing", "night-mode runtime source mismatch"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "stock_liveness_supervisor",
            "Stock-Style Liveness Supervisor",
            "not_implemented",
            "P0/P1",
            "Stock Bitmain monitor loops restart miner processes when liveness fails.",
            "Prevents process-alive but non-mining states from persisting unattended.",
            "Crash wrapper and health APIs exist, but a dry-run miner-liveness supervisor and persistent event ring are not implemented.",
            "Active restarts must prove voltage-off, bounded restart loops, and fan-safe behavior before promotion.",
            "Start with dry-run read-only liveness events for :8080, :4028, share/nonce age, and upgrade state.",
            "Dry-run detects killed daemon, hung API, and stale mining without hardware writes or restart actions.",
            "stock_baseline_re_memory_and_local_scripts",
            "future local API/CGMiner poller",
            "known_gap",
            &["dry-run supervisor absent", "persistent event ring absent", "active restart proof absent"],
            "https://github.com/DCentralTech/DCENT_OS",
        ),
        competitive_readiness_feature(
            "fleet_management",
            "Fleet Management",
            "not_implemented",
            "P2",
            "LuxOS Commander-style fleet inventory and batch operations are mature.",
            "Supports home miners with several units without forcing cloud control.",
            "/api/fleet/discover is currently not implemented; autotuner fleet-profile export exists.",
            "Batch mutation can misconfigure or brick many miners if promoted early.",
            "Build local-first read-only inventory before any batch apply path.",
            "Fleet view lists local miners with model/version/status from validated live data; batch apply remains disabled.",
            "local_first_clean_room_api_plan",
            "future LAN inventory only",
            "known_gap",
            &["read-only discovery missing", "no batch rollback matrix"],
            "docs/COMPETITIVE_FEATURE_MATRIX.md",
        ),
    ];

    serde_json::json!({
        "schema": "dcentos.competitive.readiness.v1",
        "status": "partial",
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "telemetry_source": "static_firmware_contract",
        "source": "competitive_firmware_supremacy_ralph_wave2",
        "generated_at_s": now_ms / 1000,
        "fetched_at_ms": now_ms,
        "decentralization_gate": gate,
        "feature_count": features.len(),
        "features": features,
        // W1.1 default-credential lockdown -- read-only mirror of the
        // dropbear gate. Values: "disabled" | "enabled-by-wizard" |
        // "enabled-by-keys". The dashboard renders this so the operator
        // can tell whether SSH is reachable without trying it.
        "setup_ssh_state": current_setup_ssh_state(),
        //  W2 — cross-firmware capability matrix from
        // dcentrald-api-types::firmware_stratum_matrix. Lets the
        // dashboard render an 8-firmware comparison (Bitmain stock S9 /
        // VNish 3.9 S9 / VNish 2.0.4 S17 / VNish 1.2.7 / Bitmain stock
        // S19j / LuxOS 1.38 / BraiinsOS / DCENT_OS) for SV1/SV2/BIP310/
        // devfee/factory-pool fields. The corpus includes BraiinsOS (which
        // ships native SV2 + default version-rolling), so DCENT_OS is not
        // claimed as the only SV2 firmware. Pinned by api-types tests; no
        // live probe.
        "firmware_matrix": firmware_matrix_response(),
        "promotion_allowed_only_when": [
            "feature has Decentralization Gate fields",
            "status is proven for claimed live behavior",
            "hardware-affecting behavior has model-specific rollback proof",
            "dashboard copy matches API status and provenance"
        ],
        "limitations": [
            "This endpoint is a read-only readiness contract, not live telemetry.",
            "It does not probe pools, CGMiner, hardware, filesystem state, logs, cloud services, or network dependencies.",
            "Blocked and partial rows must not be rendered as enabled controls.",
            ".139 native mining remains blocked until live UART/share proof exists."
        ]
    })
}

const CONFIG_BACKUP_SOURCE_SPECS: &[ConfigBackupSourceSpec] = &[
    ConfigBackupSourceSpec {
        id: "persistent-config",
        label: "persistent daemon config",
        path: "/data/dcentrald.toml",
    },
    ConfigBackupSourceSpec {
        id: "factory-default-config",
        label: "factory default daemon config",
        path: "/etc/dcentrald.toml",
    },
];

const CONFIG_BACKUP_SECRET_KEY_PATTERNS: &[&str] = &[
    // W3 E-01: bare short substrings "pass"/"key" caused false positives
    // (`bypass`, `compass`, `monkey`, `keyboard`) which redacted — and
    // type-mangled — non-secret fields. Use specific forms instead; trailing-
    // segment substring matching still catches `wifi_password` / `api_token` /
    // `my_private_key` because the patterns below are long enough to be unambiguous.
    "password",
    "passphrase",
    "token",
    "secret",
    "private_key",
    "api_key",
    "apikey",
    "secret_key",
    "webhook.url",
    "webhook.telegram_bot_token",
    "mqtt.password",
    "pool.password",
    "donation.password",
];

const API_COMPATIBILITY_DCENT_ROUTES: &[ApiCompatibilityRouteEntry] = &[
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/status",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["DCENT-native contract; not a Bitmain/VNish OpenAPI clone."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/v1/capabilities",
        support: "implemented_shared_contract",
        mutates: false,
        compatibility: &["DCENT multi-family contract", "dcent-schema"],
        provenance: "mounted in rest::build_router and projected from MinerState plus HardwareInfo into dcent_schema::capability",
        unsupported_fields: &["live_install_execution", "tier_promotion"],
        limitations: &["Read-only descriptor only; support tier never promotes a SKU without exact evidence and grants only read-only caps when identity is incomplete."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/config/donation",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and backed by the shared config table",
        unsupported_fields: &[],
        limitations: &["Donation config only; full config writes continue through /api/config."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/config/donation",
        support: "implemented",
        mutates: true,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and validated by the same merged-config path as /api/config",
        unsupported_fields: &[],
        limitations: &["Persists the donation section and requires daemon restart or reconnect for mining-side changes to apply."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/stats",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Mode-gated detailed stats; unavailable modes return the existing access response."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/network/block",
        support: "implemented_manifest_only",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and backed by disabled-by-default local-node source manifest plus real recent_share_history pool job provenance",
        unsupported_fields: &[
            "block_height",
            "block_hash",
            "transaction_count",
            "fees_btc",
            "reward_btc",
            "mempool_fee_rates",
        ],
        limitations: &["Reports unavailable until a real local-node source is enabled and live RPC probing is implemented; Stratum jobs are not treated as block-height evidence."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/work/posture",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router and composed from MinerState plus recent_share_history",
        unsupported_fields: &["current_notify_age_s", "work_ring_occupancy", "dispatch_queue_depth"],
        limitations: &["Reports only already-published daemon state and recent real share events; it does not inspect dispatcher internals or infer current work from hashrate."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/pipeline/manifest",
        support: "implemented_manifest_only",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics", "CGMiner/Braiins/VNish/LuxOS parity planning"],
        provenance: "mounted in rest::build_router and declared from firmware source without dispatcher reads",
        unsupported_fields: &[
            "current_job_id",
            "last_notify_timestamp_ms",
            "work_ring_occupancy",
            "dispatch_queue_depth",
            "stale_nonce_drops_total",
            "unsupported_version_drops_total",
        ],
        limitations: &["Metadata-only manifest for a future nonblocking mining pipeline snapshot; it does not subscribe to mining_sync, read dispatcher internals, or infer live pipeline counters."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/pipeline/snapshot/schema",
        support: "implemented_schema_only",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics", "CGMiner/Braiins/VNish/LuxOS parity planning"],
        provenance: "mounted in rest::build_router and declared from the passive MiningPipelineSnapshot API type",
        unsupported_fields: &[
            "current_job_id",
            "last_notify_timestamp_ms",
            "work_ring_occupancy",
            "dispatch_queue_depth",
            "nonce_bursts_total",
            "stale_nonce_drops_total",
            "unsupported_version_drops_total",
            "local_validation_drops_total",
        ],
        limitations: &["Schema-only endpoint; no live snapshot publisher is enabled, no runtime state is read, and no dispatcher or hardware access is performed."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/history/shares",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and backed by recent_share_history",
        unsupported_fields: &[],
        limitations: &["Reports only stored real recent share events; it does not infer rows from aggregate counters."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/logs/manifest",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT diagnostics"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Metadata-only log source manifest; it does not return log contents."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/api-compatibility/manifest",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator inventory"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Firmware-declared manifest only; listed endpoints are not called or probed."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/compatibility/manifest",
        support: "implemented_alias",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator inventory"],
        provenance: "mounted in rest::build_router as an alias for /api/system/api-compatibility/manifest",
        unsupported_fields: &[],
        limitations: &["Alias route for tools that discover compatibility metadata under /api/compatibility."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/dashboard/version",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and backed by dashboard build metadata",
        unsupported_fields: &[],
        limitations: &["Dashboard self-detection metadata only; it does not inspect firmware state or hardware."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/dashboard/health",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard", "diagnostic banner"],
        provenance: "mounted in rest::build_router; lightweight daemon-liveness probe for the always-injected diagnostic banner (P0-6 / C-7)",
        unsupported_fields: &[],
        limitations: &["Daemon-served liveness only: a reachable daemon always reports alive. The dead/starting states come from server.py's always-local handler on :80 or the banner's fetch-failure path."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/competitive/readiness",
        support: "implemented_manifest_only",
        mutates: false,
        compatibility: &[
            "DCENT dashboard",
            "operator diagnostics",
            "Braiins/VNish/LuxOS parity planning",
        ],
        provenance: "mounted in rest::build_router and declared from static Competitive Firmware Supremacy RALPH contract",
        unsupported_fields: &[
            "live_competitor_probe",
            "donation_off_soak_result",
            "hardware_promotion",
        ],
        limitations: &[
            "Read-only readiness contract; it does not probe competitors, pools, hardware, logs, or filesystem state.",
        ],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/fleet/pool-stats",
        support: "implemented_local_only",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router and composed from local daemon pool counters",
        unsupported_fields: &["remote_fleet_members", "live_lan_probe"],
        limitations: &[
            "Local rollup only; it does not scan the LAN or contact other miners.",
        ],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/pipeline/snapshot",
        support: "implemented_read_only_gate",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router and reads only the optional snapshot watch receiver",
        unsupported_fields: &["live_dispatcher_probe", "hardware_smoke_promotion"],
        limitations: &[
            "Returns unavailable unless a nonblocking publisher is installed; it never polls hardware or dispatcher internals.",
        ],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/failure_modes",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator troubleshooting"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::failure_mode",
        unsupported_fields: &[],
        limitations: &["Static failure-mode catalog only; no live recovery action is executed."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/chain",
        support: "implemented_local_snapshot",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator troubleshooting"],
        provenance: "mounted in rest::build_router and classifies already-published chain state",
        unsupported_fields: &["automated_fixture_measurement"],
        limitations: &["Does not run a hashboard test or touch chain hardware."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/shares/local_rejects",
        support: "implemented_ring_snapshot",
        mutates: false,
        compatibility: &["DCENT diagnostics", "share validation analysis"],
        provenance: "mounted in rest::build_router and returns the bounded local-reject ring",
        unsupported_fields: &["pool_side_reject_log"],
        limitations: &["Reports only local validation rejects already published by the daemon."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/hardware/pic_info",
        support: "implemented_catalog_with_live_snapshot_seam",
        mutates: false,
        compatibility: &["DCENT diagnostics", "PIC/dsPIC planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::pic_firmware",
        unsupported_fields: &["live_pic_service_handle"],
        limitations: &[
            "Returns explicit not_wired live_per_slot status until a daemon-owned PicService snapshot handle is added.",
            "REST does not issue PIC I2C reads or writes.",
        ],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/hardware/thermal/bm1368/chip_temps",
        support: "implemented_status_contract",
        mutates: false,
        compatibility: &["DCENT diagnostics", "S21/BM1368 thermal planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::bm1368_temperature",
        unsupported_fields: &["live_bm1368_per_chip_temperature_readback"],
        limitations: &[
            "Returns unsupported for non-BM1368 chip families and not_proven for BM1368 until live target readback is wired.",
            "REST does not poll serial ASICs or infer per-chip temperatures from board sensors.",
        ],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/recovery_actions",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "LuxOS parity planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_recovery",
        unsupported_fields: &["live_recovery_execution"],
        limitations: &["Static recovery catalog only; destructive recovery routes are not invoked."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/boot_timeline",
        support: "implemented_observability",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router and combines canonical timeline with observed phase timestamps",
        unsupported_fields: &["boot_health_proof", "rollback_commit_proof"],
        limitations: &["Observed phases are management telemetry, not proof of expected version, rollback commit, or mining."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/history/audit",
        support: "implemented_ring_snapshot",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator forensics"],
        provenance: "mounted in rest::build_router and returns AppState audit-ring records",
        unsupported_fields: &["tamper_evident_storage"],
        // GROUP C (W8 parity): the ring is in-memory only (LOST on reboot).
        // The reboot-surviving read-back is GET /api/audit-log (mounted via
        // routes/audit_log.rs — not in this scanned-source manifest catalog,
        // same as other sub-module routes like /api/boot/phase).
        limitations: &["Bounded in-memory audit ring (LOST on reboot). For the reboot-surviving persistent log use GET /api/audit-log."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/hardware/psu_catalog",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "PSU replacement planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::psu_model",
        unsupported_fields: &["live_psu_probe"],
        limitations: &["Static PSU model catalog only; no PMBus/I2C operation is issued."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/cgminer/catalog",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["CGMiner", "pyasic", "hass-miner", "operator tooling"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::cgminer_catalog",
        unsupported_fields: &[],
        limitations: &["Catalog describes commands; it does not execute any CGMiner command."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/profiles/presets",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT dashboard", "autotuner planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::power_profile_preset",
        unsupported_fields: &["live_profile_application"],
        limitations: &["Static preset catalog only; does not apply frequency, voltage, or power settings."],
    },
    ApiCompatibilityRouteEntry {
        method: "PUT",
        path: "/api/autotuner/active",
        support: "implemented",
        mutates: true,
        compatibility: &["DCENT dashboard"],
        provenance: "mounted in rest::build_router and mapped onto persist_autotuner_mode plus the live autotuner command channel",
        unsupported_fields: &[],
        limitations: &["Persists the requested TunerMode immediately; live runtime application depends on the autotuner command channel acknowledgement."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/state_machine",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::{watchdog_policy,power_state,mining_loop_state}",
        unsupported_fields: &["live_fsm_state"],
        limitations: &["Static canonical policy thresholds only; not live FSM state; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/update_capability",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "supply-chain audit"],
        provenance: "mounted in rest::build_router and backed by crate::ota_signature + dcentrald-api-types::{luxos_update,ota_rollback_protection}",
        unsupported_fields: &["live_update_trigger"],
        limitations: &["Read-only integrity/rollback contract; issues no update, flash, or rollback action."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/error_vocab",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["pyasic", "hass-miner", "DCENT diagnostics"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::{luxos_error_vocab,braiinsos_miner_status}",
        unsupported_fields: &["live_error_stream"],
        limitations: &["Static cross-firmware vocabulary catalog; not a live error stream; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/ramp",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "pyasic"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::{ramp_curve,autotune_phase}",
        unsupported_fields: &["live_ramp_classification"],
        limitations: &["Static canonical ramp reference + autotune defaults; not a live ramp classification; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/stratum/protocol",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "pyasic", "protocol audit"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::{stratum_v1_messages,stratum_v2_messages}",
        unsupported_fields: &["live_pool_session"],
        limitations: &["Static protocol-support catalog; issues no pool connection or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/hardware/psu_bypass_matrix",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "PSU planning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::psu_bypass",
        unsupported_fields: &["live_psu_probe"],
        limitations: &["Static Loki-requirement/bypass-mode catalog; issues no PSU/I2C/hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/thermal/cold_environment",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "home / space-heater operators"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::cold_environment",
        unsupported_fields: &["live_ambient_sensor"],
        limitations: &["Static cold-environment policy + deterministic sample curve; no live sensor read or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/pools/failover_policy",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "pool reliability audit"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_pool_failover",
        unsupported_fields: &["live_failover_state"],
        limitations: &["Static failover policy reference; not the live runtime snapshot (see /api/pools.failover); issues no pool connection or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/tuning/constraints",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "tuning audit"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::braiinsos_constraints",
        unsupported_fields: &["live_tuning_state"],
        limitations: &["Static documented constraint catalog; changes no tuning state; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/diagnostics/sensor_outlier",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "thermal-safety audit"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::sensor_outlier",
        unsupported_fields: &["live_sensor_verdicts"],
        limitations: &["Static outlier-rejection policy + verdict taxonomy; no live sensor read or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/vnish_schema",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["pyasic", "fleet tooling", "firmware detection"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::vnish_settings",
        unsupported_fields: &["live_vnish_probe"],
        limitations: &["Static RE-derived VNish response-shape reference; touches no VNish unit, no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/luxos_architecture",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT recovery planning", "uninstall-to-stock", "forensics"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_system_architecture",
        unsupported_fields: &["live_mtd_probe"],
        limitations: &["Static RE-derived LuxOS layout reference; issues no flash, network, or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/thermal/cooling_modes",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "thermal audit", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::braiinsos_cooling_mode",
        unsupported_fields: &["live_cooling_state"],
        limitations: &["Static cooling-mode taxonomy; changes no cooling state; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/thermal/supervisor",
        support: "implemented_live",
        mutates: false,
        compatibility: &["DCENT diagnostics", "thermal-safety audit", "operator transparency"],
        provenance: "mounted in rest::build_router; reads the live ThermalSupervisor snapshot the daemon thermal loop publishes (Wave-G G1 / E3b) backed by dcentrald-thermal::supervisor::SupervisorSnapshot",
        unsupported_fields: &[],
        limitations: &["Preserves legacy enabled/board fields while adding configured_enabled, runtime_present, snapshot_available, and commissioning_state so disabled, pending_tick, running, and unsupported states are not conflated."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/power/dps",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "power audit", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::braiinsos_dps_configuration",
        unsupported_fields: &["live_dps_state"],
        limitations: &["Static DPS mode/threshold reference; changes no power state; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/network/config_schema",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT diagnostics", "operator transparency", "fleet provisioning"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::braiinsos_network_configuration",
        unsupported_fields: &["live_network_state"],
        limitations: &["Static network-config schema reference; changes no network state; issues no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/network/info",
        support: "implemented_read_only",
        mutates: false,
        compatibility: &["DCENT settings", "BraiinsOS parity", "fleet provisioning"],
        provenance: "mounted in routes::stock_parity; read-only network identity snapshot",
        unsupported_fields: &["live_static_ip_write", "interface_reconfiguration"],
        limitations: &["Read-only network identity and link-state snapshot; missing OS fields degrade to empty strings rather than inferred values."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/network/hostname",
        support: "implemented_safe_write",
        mutates: true,
        compatibility: &["DCENT settings", "fleet provisioning", "operator identity"],
        provenance: "mounted in routes::stock_parity; persists [general].hostname through the daemon atomic config writer",
        unsupported_fields: &["static_ip", "netmask", "gateway", "dns_servers", "live_hostname_apply"],
        limitations: &["Persists hostname config only; does not reconfigure active interfaces or apply static IP settings live."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/miner/type",
        support: "implemented_read_only",
        mutates: false,
        compatibility: &["pyasic", "fleet tooling", "Bitmain CGI parity"],
        provenance: "mounted in routes::stock_parity and composed from AppState plus system identity",
        unsupported_fields: &[],
        limitations: &["Concise identity view only; it does not probe hardware while serving the request."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/log/backup",
        support: "implemented_read_only",
        mutates: false,
        compatibility: &["DCENT diagnostics", "Bitmain CGI parity", "support bundle"],
        provenance: "mounted in routes::stock_parity and redacted with the config-backup secret policy",
        unsupported_fields: &["unredacted_logs"],
        limitations: &["Text support bundle only; secret, wallet, and credential-bearing values are redacted before response."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/luxos_web_map",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT migration planning", "forensics", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_web_pages",
        unsupported_fields: &["live_luxos_probe"],
        limitations: &["Static RE-derived LuxOS web-UI surface map; touches no LuxOS unit, no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/proto_wire_types",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["cross-firmware tooling", "gRPC interop", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::braiinsos_proto_wire_types",
        unsupported_fields: &["live_grpc_session"],
        limitations: &["Static proto unit-type reference; issues no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/luxos_responses",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["pyasic", "hass-miner", "fleet tooling"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_response_payloads",
        unsupported_fields: &["live_luxos_probe"],
        limitations: &["Static RE-derived LuxOS CGMiner-compat response-shape reference; touches no LuxOS unit, no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/luxos_status_codes",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["pyasic", "fleet tooling", "operator transparency"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_rest_envelope",
        unsupported_fields: &["live_luxos_probe"],
        limitations: &["Static RE-derived LuxOS status-code reference; touches no LuxOS unit, no network or hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/firmware/vnish_overlay",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT migration planning", "forensics", "uninstall-to-stock"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::vnish_overlay_layout",
        unsupported_fields: &["live_vnish_probe"],
        limitations: &["Static RE-derived VNish overlay/recovery reference; touches no VNish unit, no flash/network/hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/hardware/thermal/sensors",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["DCENT dashboard", "thermal diagnostics"],
        provenance: "mounted in rest::build_router and backed by dcentrald-api-types::luxos_sensor_topology",
        unsupported_fields: &["live_sensor_readback"],
        limitations: &["Static sensor-topology catalog only; live thermal control is unchanged."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/chips",
        support: "implemented_local_snapshot",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics", "chip-health analysis"],
        provenance: "mounted in rest::build_router and built from build_chip_health_snapshot over already-published chain state",
        unsupported_fields: &["live_per_chip_probe"],
        limitations: &["Mode-gated read-only chip-health snapshot; classifies already-published chain state and does not probe ASICs."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/env/recipe",
        support: "implemented_local_snapshot",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router; Wave-55a `a lab unit`-class XIL surface reading process env + /etc/dcentos platform files",
        unsupported_fields: &[],
        limitations: &["Read-only Wave-54 recipe-intact report (applied / missing / forbidden env + fingerprint); on non-`a lab unit` units is_xil_25_class is false. Reports env+fingerprint only; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/chain/presence",
        support: "implemented_local_snapshot",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router; Wave-55a surface composed from the MinerState::chains snapshot the daemon already maintains",
        unsupported_fields: &["live_chain_probe"],
        limitations: &["Read-only per-chain chips_responding/expected + chip-rail mv_actual/mv_target; reports only already-published chain state and does not poll ASICs."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/mining/handoff/state",
        support: "implemented_local_snapshot",
        mutates: false,
        compatibility: &["DCENT dashboard", "operator diagnostics"],
        provenance: "mounted in rest::build_router; Wave-55a handoff-mode classifier over MinerState + runtime_health_snapshot + platform fingerprint",
        unsupported_fields: &[],
        limitations: &["Read-only classifier (handoff_mining / bosminer_only / standalone / idle); ac_cycle_recommended is a best-effort heuristic, not a control action; issues no hardware I/O."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/re/catalog",
        support: "implemented_catalog",
        mutates: false,
        compatibility: &["RE catalog", "operator diagnostics", "DCENT dashboard"],
        provenance: "mounted in rest::build_router and backed by HAL-free dcentrald-api-types catalogs",
        unsupported_fields: &["hardware_probe", "control_action"],
        limitations: &["Read-only reverse-engineering catalog; no hardware, config, filesystem, or mining-control side effects."],
    },
];

const API_COMPATIBILITY_PYASIC_ROUTES: &[ApiCompatibilityRouteEntry] = &[
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/info",
        support: "implemented",
        mutates: false,
        compatibility: &["ESP-Miner", "AxeOS", "pyasic"],
        provenance: "mounted in rest::build_router; response includes field_sources and unsupported_metrics",
        unsupported_fields: &["bestDiff", "bestSessionDiff", "vrTemp"],
        limitations: &["Compatibility-only unsupported metrics are returned with explicit provenance instead of fabricated values."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/asic",
        support: "implemented",
        mutates: false,
        compatibility: &["AxeOS", "pyasic"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Per-ASIC fields reflect the current DCENT_OS runtime state contract."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/system/identify",
        support: "implemented",
        mutates: true,
        compatibility: &["AxeOS locate"],
        provenance: "mounted in rest::build_router and routed to the LED locate handler",
        unsupported_fields: &[],
        limitations: &["Side-effecting locate action; this manifest declares the route but never calls it."],
    },
];

const API_COMPATIBILITY_V1_ROUTES: &[ApiCompatibilityRouteEntry] = &[
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/upgrade/status",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT firmware status"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Read-only status view of staged firmware and boot-commit state."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/system/update/status",
        support: "implemented_alias",
        mutates: false,
        compatibility: &["DCENT firmware status"],
        provenance: "mounted in rest::build_router as an update-status alias",
        unsupported_fields: &[],
        limitations: &["Alias for the read-only upgrade status handler."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/v1/system/upgrade/status",
        support: "implemented_alias",
        mutates: false,
        compatibility: &["/api/v1 firmware status"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Alias for the read-only upgrade status handler."],
    },
    ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/api/v1/firmware/update/status",
        support: "implemented_alias",
        mutates: false,
        compatibility: &["/api/v1 firmware status"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Alias for the read-only upgrade status handler."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/system/upgrade",
        support: "implemented",
        mutates: true,
        compatibility: &["DCENT signed firmware update"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Side-effecting signed firmware upload/apply path; this manifest declares the route but never calls it."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/v1/system/upgrade",
        support: "implemented_alias",
        mutates: true,
        compatibility: &["/api/v1 firmware update"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Alias for the signed firmware upload/apply path; this manifest declares the route but never calls it."],
    },
    ApiCompatibilityRouteEntry {
        method: "POST",
        path: "/api/v1/firmware/update",
        support: "implemented_alias",
        mutates: true,
        compatibility: &["/api/v1 firmware update"],
        provenance: "mounted in rest::build_router",
        unsupported_fields: &[],
        limitations: &["Alias for the signed firmware upload/apply path; this manifest declares the route but never calls it."],
    },
];

const API_COMPATIBILITY_CGMINER_COMMANDS: &[ApiCompatibilityCommandEntry] = &[
    ApiCompatibilityCommandEntry {
        name: "summary",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[
            "Compatibility response includes DCENT field provenance for unsupported counters.",
        ],
    },
    ApiCompatibilityCommandEntry {
        name: "stats",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[
            "Reports the current DCENT chain/stat contract through CGMiner-shaped JSON.",
        ],
    },
    ApiCompatibilityCommandEntry {
        name: "pools",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[
            "Reports configured pool view; pool management commands remain explicitly unsupported.",
        ],
    },
    ApiCompatibilityCommandEntry {
        name: "devs",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &["Reports chain-level devices from the runtime state."],
    },
    ApiCompatibilityCommandEntry {
        name: "version",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[],
    },
    ApiCompatibilityCommandEntry {
        name: "coin",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[],
    },
    ApiCompatibilityCommandEntry {
        name: "config",
        support: "implemented",
        mutates: false,
        provenance: "recognized by cgminer::handle_command",
        limitations: &[],
    },
    ApiCompatibilityCommandEntry {
        name: "switchpool",
        support: "recognized_unsupported",
        mutates: false,
        provenance: "recognized by cgminer::handle_command and returns explicit CGMiner error 15",
        limitations: &["Pool switching is not performed."],
    },
    ApiCompatibilityCommandEntry {
        name: "enablepool",
        support: "recognized_unsupported",
        mutates: false,
        provenance: "recognized by cgminer::handle_command and returns explicit CGMiner error 15",
        limitations: &["Pool enable is not performed."],
    },
    ApiCompatibilityCommandEntry {
        name: "disablepool",
        support: "recognized_unsupported",
        mutates: false,
        provenance: "recognized by cgminer::handle_command and returns explicit CGMiner error 15",
        limitations: &["Pool disable is not performed."],
    },
    ApiCompatibilityCommandEntry {
        name: "addpool",
        support: "implemented",
        mutates: true,
        provenance: "cgminer::handle_command routes addpool to rest::grpc_bridge_set_pools (validate_and_write_pool_config core: <=3 pools, V1-URL validation, atomic TOML write)",
        limitations: &[
            "Writes/extends the configured pool set; does not hot-switch the active pool (failover is priority/FSM-driven).",
        ],
    },
    ApiCompatibilityCommandEntry {
        name: "restart",
        support: "implemented",
        mutates: true,
        provenance: "cgminer::handle_command routes restart to rest::grpc_bridge_reboot (trigger_daemon_restart — respawns the daemon, preserving fan management)",
        limitations: &["Restarts the dcentrald daemon, not a full SoC reboot."],
    },
    ApiCompatibilityCommandEntry {
        name: "quit",
        support: "recognized_unsupported",
        mutates: false,
        provenance: "recognized by cgminer::handle_command and returns explicit CGMiner error 15",
        limitations: &[
            "Daemon shutdown is not performed through the CGMiner TCP compatibility API.",
        ],
    },
];

const API_COMPATIBILITY_WEBSOCKET_ROUTES: &[ApiCompatibilityRouteEntry] =
    &[ApiCompatibilityRouteEntry {
        method: "GET",
        path: "/ws",
        support: "implemented",
        mutates: false,
        compatibility: &["DCENT dashboard"],
        provenance:
            "mounted by dcentrald-api::lib when composing REST, WebSocket, and dashboard routes",
        unsupported_fields: &[],
        limitations: &["Streaming contract, not a REST endpoint."],
    }];

const API_COMPATIBILITY_SURFACES: &[ApiCompatibilitySurface] = &[
    ApiCompatibilitySurface {
        id: "dcent-rest",
        label: "DCENT REST API",
        protocol: "http-json",
        default_port: Some(8080),
        default_bind: Some("0.0.0.0"),
        compatibility: &["DCENT dashboard", "operator tooling"],
        routes: API_COMPATIBILITY_DCENT_ROUTES,
        commands: &[],
        limitations: &["This is a declared subset of production-relevant REST routes, not a full OpenAPI export."],
    },
    ApiCompatibilitySurface {
        id: "pyasic-axeos-rest",
        label: "AxeOS / ESP-Miner discovery REST",
        protocol: "http-json",
        default_port: Some(8080),
        default_bind: Some("0.0.0.0"),
        compatibility: &["pyasic", "AxeOS", "ESP-Miner"],
        routes: API_COMPATIBILITY_PYASIC_ROUTES,
        commands: &[],
        limitations: &["Compatibility-only fields with unavailable source data are marked as unsupported instead of guessed."],
    },
    ApiCompatibilitySurface {
        id: "v1-firmware-aliases",
        label: "/api/v1 firmware aliases",
        protocol: "http-json",
        default_port: Some(8080),
        default_bind: Some("0.0.0.0"),
        compatibility: &["DCENT firmware update clients"],
        routes: API_COMPATIBILITY_V1_ROUTES,
        commands: &[],
        limitations: &["Only the mounted firmware status/update aliases are declared; this is not a full VNish /api/v1 surface."],
    },
    ApiCompatibilitySurface {
        id: "cgminer-tcp",
        label: "CGMiner TCP API",
        protocol: "cgminer-tcp-json",
        default_port: Some(4028),
        default_bind: Some("127.0.0.1 unless cgminer_bind_lan=true"),
        compatibility: &["CGMiner", "pyasic", "hass-miner"],
        routes: &[],
        commands: API_COMPATIBILITY_CGMINER_COMMANDS,
        limitations: &["Unauthenticated TCP compatibility API is localhost-only by default unless explicitly configured for LAN binding."],
    },
    ApiCompatibilitySurface {
        id: "websocket",
        label: "DCENT WebSocket stream",
        protocol: "websocket-json",
        default_port: Some(8080),
        default_bind: Some("0.0.0.0"),
        compatibility: &["DCENT dashboard"],
        routes: API_COMPATIBILITY_WEBSOCKET_ROUTES,
        commands: &[],
        limitations: &["The manifest declares that the WebSocket route is mounted; it does not open a socket."],
    },
];

const API_COMPATIBILITY_OMISSIONS: &[ApiCompatibilityOmission] = &[
    ApiCompatibilityOmission {
        path: None,
        surface: Some("full VNish OpenAPI /api/v1"),
        reason: "Only mounted firmware/update aliases are declared; a full VNish-style /api/v1 surface is not implemented.",
    },
    ApiCompatibilityOmission {
        path: None,
        surface: Some("Bitmain proprietary UI/API clone"),
        reason: "DCENT_OS exposes compatibility contracts where implemented, not proprietary Bitmain firmware internals.",
    },
];

fn config_backup_source_entry(
    spec: ConfigBackupSourceSpec,
    active_path: &str,
    writable_path: &str,
) -> ConfigBackupSourceEntry {
    match std::fs::metadata(spec.path) {
        Ok(metadata) => ConfigBackupSourceEntry {
            id: spec.id,
            label: spec.label,
            path: spec.path,
            active: spec.path == active_path,
            writable_target: spec.path == writable_path,
            metadata_status: "available".to_string(),
            exists: true,
            size_bytes: Some(metadata.len()),
            modified_ms: metadata.modified().ok().and_then(|modified| {
                modified
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_millis())
            }),
        },
        Err(error) => ConfigBackupSourceEntry {
            id: spec.id,
            label: spec.label,
            path: spec.path,
            active: spec.path == active_path,
            writable_target: spec.path == writable_path,
            metadata_status: if error.kind() == std::io::ErrorKind::NotFound {
                "missing".to_string()
            } else {
                format!("metadata_unavailable: {}", error.kind())
            },
            exists: false,
            size_bytes: None,
            modified_ms: None,
        },
    }
}

fn build_config_backup_manifest_response() -> ConfigBackupManifestResponse {
    let active_path = get_config_path();
    let writable_path = get_writable_config_path();

    ConfigBackupManifestResponse {
        status: "ok",
        read_only: true,
        content_collected: false,
        // COMP-1: the dedicated `GET /api/config/export` + `POST /api/config/import`
        // endpoints provide a redacted, re-importable daemon-config backup/restore
        // (LuxOS/Braiins parity). Restore is SUPPORTED but NOT lossless across
        // units: secrets, worker/payout addresses, and credential URLs are redacted
        // out of the export and are only restored keep-existing on the SAME unit;
        // on a different/wiped unit they must be re-entered (see redaction notes).
        // This manifest endpoint stays metadata-only, but advertises the capability.
        restore_supported: true,
        daemon_config_export_supported: true,
        dashboard_preferences_export_supported: true,
        sources: CONFIG_BACKUP_SOURCE_SPECS
            .iter()
            .copied()
            .map(|spec| config_backup_source_entry(spec, active_path, writable_path))
            .collect(),
        redaction_policy: ConfigBackupRedactionPolicy {
            content_included: false,
            secret_key_patterns: CONFIG_BACKUP_SECRET_KEY_PATTERNS,
            notes: &[
                "This manifest does not include TOML contents.",
                // DEVOPS-009 / COMP-1: the redactor exists and is wired into the
                // export. `GET /api/config/export` runs `redact_secrets_in_toml_table`
                // plus wallet/credential-URL redaction before serializing — it masks
                // pool.password / mqtt.password / tokens / keys / worker wallet
                // addresses / credential-bearing pool URLs (the secret-key list
                // advertised here, extended with wallet+URL handling).
                "GET /api/config/export returns the full effective config with secrets, wallet/payout addresses (worker/fallback_worker/coinbase_output_address), and credential URLs redacted (helper: redact_config_table_for_export). Those redacted values are NOT included in the export.",
                "POST /api/config/import validates (fail-closed) then MERGES the uploaded sections onto the current config (omitted sections are preserved). Redaction-placeholder values are kept-existing, so a SAME-UNIT round-trip never overwrites a stored secret with the mask; restoring onto a DIFFERENT/wiped unit drops the redacted secrets/addresses/credential URLs and they must be re-entered.",
            ],
        },
        limitations: &[
            "This endpoint reports daemon config source metadata only.",
            "It does not return, parse, validate, import, restore, or write configuration values; use /api/config/export and /api/config/import for that.",
            "Dashboard preference export is browser-local and is not a full firmware backup.",
        ],
    }
}

/// GET /api/config/backup/manifest -- Metadata-only config backup readiness.
///
/// Reports daemon config source availability and backup policy without exposing
/// TOML contents or adding a restore/import path.
async fn get_config_backup_manifest() -> impl IntoResponse {
    Json(build_config_backup_manifest_response())
}

// ─── COMP-1: config export / import (LuxOS/Braiins parity) ──────────────────

/// Top-level config sections accepted by `POST /api/config/import`.
///
/// MUST mirror `dcentrald::config::DcentraldConfig`'s named sections. The daemon
/// parses the persisted config with `#[serde(deny_unknown_fields)]` on every
/// struct, so an unknown top-level section would CRASH-LOOP the daemon at the
/// next restart. The import path therefore fails closed on any section not in
/// this allowlist (it can never persist a config that bricks startup).
const CONFIG_IMPORT_ALLOWED_SECTIONS: &[&str] = &[
    "general",
    "logging",
    "pool",
    "mining",
    "power",
    "thermal",
    "api",
    "network_block",
    "donation",
    "mqtt",
    "watchdog",
    "hash_on_disconnect",
    "mode",
    // Legacy top-level [heater] block still present on older flashed S9 images
    // (DcentraldConfig keeps a `#[serde(rename = "heater")]` field for it).
    "heater",
    "autotuner",
    "autotune",
    "led",
    "sv2",
    "job_declaration",
    "webhook",
    "psu",
    "hashboard",
    "stratum_proxy",
    "bridge",
];

/// Read a TOML value as i64, accepting integers or whole-number floats.
fn toml_value_as_i64(v: &toml::Value) -> Option<i64> {
    v.as_integer().or_else(|| v.as_float().map(|f| f as i64))
}

/// Read a TOML value as f64, accepting floats or integers.
fn toml_value_as_f64(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Masked display form of a pool worker (the worker is the operator's BTC
/// payout address on V1 solo). Single chokepoint so every `/api/pools` worker
/// emission stays masked per the wallet-mask rule.
fn pool_worker_display(worker: &str) -> String {
    dcentrald_common::wallet_mask::mask_wallet(worker)
}

/// COMP-1 export redactor: run the canonical secret-key redactor, then ALSO
/// redact operator wallet/payout addresses and credential-bearing URLs — none of
/// which `key_is_secret` covers (a bare `worker`/`url` is not a "secret key").
///
/// FIX 1 (2026-06-20): the wallet/URL pass is now key-PATTERN based and recurses
/// through every nested table + array-of-tables, so it can never again leak a
/// secret hiding under a renamed/nested key (the prior version only matched the
/// EXACT keys `worker` and `url`):
///   - WALLET/ADDRESS keys (`worker`, `fallback_worker`,
///     `coinbase_output_address`) → replaced with the redaction placeholder,
///     exactly like `worker` has always been (re-importable via keep-existing).
///     These are operator BTC payout/account addresses (config.rs surfaces them
///     as pool.worker, donation.worker, donation.fallback_worker,
///     job_declaration.coinbase_output_address).
///   - CREDENTIAL-URL keys (`url`, any `*_url` — sv2_url / pool_url /
///     fallback_pool_url / bitcoind_rpc_url / cgminer_scrape_url … — and the
///     MQTT `broker`) → run through `sanitize_pool_url`, which strips ONLY an
///     inline `user:pass@` credential and leaves a clean URL byte-for-byte. A
///     clean (no-credential) URL therefore round-trips verbatim; the credential
///     part is dropped entirely (not restorable, must be re-entered on restore).
///
/// Existing password/token/secret redaction (the `key_is_secret` chokepoint) is
/// run first and left intact.
fn redact_config_table_for_export(table: &mut toml::Table) {
    redact_secrets_in_toml_table(table);
    redact_wallets_and_credential_urls(table);
}

/// Local key names whose VALUE is an operator wallet / payout / account address.
/// Masked with the redaction placeholder (re-importable via keep-existing),
/// exactly like `worker` has always been. Kept in lockstep with the import-side
/// keep-existing handling (FIX 2). Match is on the LOCAL (last-segment) key name.
pub(crate) const WALLET_ADDRESS_KEYS: &[&str] =
    &["worker", "fallback_worker", "coinbase_output_address"];

/// `true` when `key` (a local key name) addresses a value that is — or may carry
/// — an inline-credential URL: the bare `url`, any `*_url` (sv2_url / pool_url /
/// fallback_pool_url / bitcoind_rpc_url / cgminer_scrape_url / …), or the MQTT
/// `broker`. Such values are sanitized (inline `user:pass@` stripped) on export.
pub(crate) fn key_is_credential_url(key: &str) -> bool {
    key == "url" || key.ends_with("_url") || key == "broker"
}

fn redact_wallets_and_credential_urls(table: &mut toml::Table) {
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        match table.get_mut(&key) {
            Some(toml::Value::String(s)) if !s.is_empty() => {
                if WALLET_ADDRESS_KEYS.contains(&key.as_str()) {
                    *s = SECRET_REDACTION_PLACEHOLDER.to_string();
                } else if key_is_credential_url(&key) {
                    // Strip ONLY inline `user:pass@` credentials; a clean URL is
                    // left byte-for-byte so the export round-trips verbatim.
                    let sanitized = dcentrald_stratum::pool_api::sanitize_pool_url(s);
                    if sanitized != *s {
                        *s = sanitized;
                    }
                }
            }
            Some(toml::Value::Table(inner)) => redact_wallets_and_credential_urls(inner),
            Some(toml::Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let toml::Value::Table(inner) = item {
                        redact_wallets_and_credential_urls(inner);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Build the redacted, re-importable config export payload.
fn build_config_export() -> std::result::Result<serde_json::Value, String> {
    let mut table = load_config_table_for_write()?;
    redact_config_table_for_export(&mut table);
    let config_toml =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    let schema_version = table
        .get("general")
        .and_then(|g| g.as_table())
        .and_then(|g| g.get("schema_version"))
        .and_then(|v| v.as_integer());
    let exported_at_ms: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(serde_json::json!({
        "status": "ok",
        "redacted": true,
        "reimportable": true,
        "secret_placeholder": SECRET_REDACTION_PLACEHOLDER,
        "secret_key_patterns": CONFIG_BACKUP_SECRET_KEY_PATTERNS,
        "schema_version": schema_version,
        "exported_at_ms": exported_at_ms,
        "config_toml": config_toml,
        "notes": [
            "Full effective daemon config. All secrets (passwords/tokens/keys) and operator wallet/payout addresses (worker/fallback_worker/coinbase_output_address) are replaced with the redaction placeholder; credential URLs keep only their host (any inline user:pass@ is stripped).",
            "NOT a complete copy: these redacted values are NOT in the export. Re-importing onto the SAME unit restores them keep-existing (the placeholder is replaced from the running config). Restoring onto a DIFFERENT or wiped unit DROPS them — secrets, worker/payout addresses, and any credential URLs must be re-entered there.",
            "Re-import via POST /api/config/import. Import MERGES the uploaded sections onto the current config: sections you omit are preserved, and placeholder values are kept-existing so a round-trip never overwrites a stored secret with the mask.",
        ],
    }))
}

/// GET /api/config/export — full effective daemon config, secrets/wallet/URL
/// redacted, in a re-importable form (COMP-1, LuxOS/Braiins parity).
async fn get_config_export(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match build_config_export() {
        Ok(value) => {
            push_rest_audit_free(
                &state,
                "config_export",
                "Config exported (secrets, wallet workers, and credential URLs redacted)",
            );
            Json(value).into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

/// Body for `POST /api/config/import`: a full daemon config as a TOML document
/// (exactly the `config_toml` field produced by `GET /api/config/export`).
#[derive(Debug, Deserialize)]
struct ConfigImportPayload {
    config_toml: String,
}

/// Top-level sections present in the imported table that are NOT in the import
/// allowlist (would crash-loop the deny-unknown-fields daemon at restart).
fn disallowed_import_sections(table: &toml::Table) -> Vec<String> {
    table
        .keys()
        .filter(|k| !CONFIG_IMPORT_ALLOWED_SECTIONS.contains(&k.as_str()))
        .cloned()
        .collect()
}

fn validate_mining_write(table: &toml::Table) -> std::result::Result<(), String> {
    let Some(mining) = table.get("mining").and_then(|v| v.as_table()) else {
        return Ok(());
    };
    const MINING_VOLTAGE_MV_MIN: i64 = 5000;
    const MINING_VOLTAGE_MV_MAX: i64 = 20000;
    const MINING_FREQ_MHZ_MIN: i64 = 50;
    const MINING_FREQ_MHZ_MAX: i64 = 1200;
    if let Some(mv) = mining.get("voltage_mv").and_then(toml_value_as_i64) {
        if mv > 0 && !(MINING_VOLTAGE_MV_MIN..=MINING_VOLTAGE_MV_MAX).contains(&mv) {
            return Err(format!(
                "mining.voltage_mv {} out of range. Valid: {}-{} mV.",
                mv, MINING_VOLTAGE_MV_MIN, MINING_VOLTAGE_MV_MAX
            ));
        }
        const MINING_VOLTAGE_MV_AM2_CEILING: i64 = 14_500;
        if mv > MINING_VOLTAGE_MV_AM2_CEILING {
            return Err(format!(
                "mining.voltage_mv ({}) exceeds the {} mV am2 chip-rail ceiling. An \
                 imported config cannot safely know the target platform; the am2 \
                 beta-gate hardware refuses any chip-rail above {} mV (risk of dsPIC \
                 corruption / EEPROM damage - see .74 hb2 incident 2026-04-29). \
                 Reduce mining.voltage_mv to <= {}.",
                mv,
                MINING_VOLTAGE_MV_AM2_CEILING,
                MINING_VOLTAGE_MV_AM2_CEILING,
                MINING_VOLTAGE_MV_AM2_CEILING
            ));
        }
    }
    if let Some(freq) = mining.get("frequency_mhz").and_then(toml_value_as_i64) {
        if freq > 0 && !(MINING_FREQ_MHZ_MIN..=MINING_FREQ_MHZ_MAX).contains(&freq) {
            return Err(format!(
                "mining.frequency_mhz {} out of range. Valid: {}-{} MHz.",
                freq, MINING_FREQ_MHZ_MIN, MINING_FREQ_MHZ_MAX
            ));
        }
    }
    if let Some(0) = mining.get("serial_chip_count").and_then(toml_value_as_i64) {
        return Err(
            "mining.serial_chip_count must be >= 1 (0 chips is not a valid chain geometry; \
             it divide-by-zero panics chain address assignment at init)"
                .to_string(),
        );
    }
    Ok(())
}

/// Sanity-bounds validation mirroring the platform-independent subset of
/// `dcentrald::config::DcentraldConfig::validate()`.
///
/// We CANNOT call `DcentraldConfig::validate()` here: the daemon crate
/// (`dcentrald`) depends on `dcentrald-api`, not the reverse, so importing the
/// validator would be a dependency cycle. This replicates the validator's
/// platform-independent, security-relevant checks so an obviously-bad value is
/// rejected BEFORE it is persisted to `/data` (the full validator still runs on
/// the next daemon start, but it REJECTS rather than clamps — persisting a bad
/// value would risk a start-time crash-loop). Each check is defensive: it only
/// fires when the key is present and parseable.
fn validate_imported_config_table(table: &toml::Table) -> std::result::Result<(), String> {
    if let Some(thermal) = table.get("thermal").and_then(|v| v.as_table()) {
        let target = thermal.get("target_temp_c").and_then(toml_value_as_i64);
        let hot = thermal.get("hot_temp_c").and_then(toml_value_as_i64);
        let dangerous = thermal.get("dangerous_temp_c").and_then(toml_value_as_i64);
        if let (Some(t), Some(h)) = (target, hot) {
            if t >= h {
                return Err(format!(
                    "thermal.target_temp_c ({}) must be less than thermal.hot_temp_c ({})",
                    t, h
                ));
            }
        }
        if let (Some(h), Some(d)) = (hot, dangerous) {
            if h >= d {
                return Err(format!(
                    "thermal.hot_temp_c ({}) must be less than thermal.dangerous_temp_c ({})",
                    h, d
                ));
            }
        }
        if let Some(d) = dangerous {
            if d > 90 {
                return Err(format!(
                    "thermal.dangerous_temp_c ({}) must be <= 90 (residential safety limit)",
                    d
                ));
            }
        }
        // FIX 3: thermal.pid_interval_s feeds Duration::from_secs_f32 on the PID
        // loop (config.rs validate() ~:1075). A non-finite/overflowing value
        // PANICS (panic=abort → daemon aborts with boards powered); <=0 or >60 s
        // defeats thermal cadence. Reject before persist so the daemon can't
        // crash-loop at the next start. Default is 5.0.
        if let Some(pid) = thermal.get("pid_interval_s").and_then(toml_value_as_f64) {
            if !pid.is_finite() || pid <= 0.0 || pid > 60.0 {
                return Err(format!(
                    "thermal.pid_interval_s ({}) must be finite and in (0, 60] s — a \
                     non-finite/<=0/too-large value panics or defeats the thermal PID \
                     loop (config.rs validate()). Default is 5.0.",
                    pid
                ));
            }
        }
    }
    if let Some(o) = table
        .get("mode")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("hacker"))
        .and_then(|v| v.as_table())
        .and_then(|h| h.get("dangerous_temp_override"))
        .and_then(toml_value_as_i64)
    {
        if o > 90 {
            return Err(format!(
                "mode.hacker.dangerous_temp_override ({}) must be <= 90 (residential safety limit)",
                o
            ));
        }
    }
    // FIX 3: mode.active gates the quiet/home thermal posture but is coerced to
    // Standard by every consumer's `_ =>` arm (config.rs validate() ~:1098), so a
    // typo silently discards operator intent. Reject anything outside the known
    // set before persist. ("mining" is the autotuner alias for standard.)
    if let Some(active) = table
        .get("mode")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("active"))
        .and_then(|v| v.as_str())
    {
        let normalized = active.trim().to_ascii_lowercase();
        if !matches!(
            normalized.as_str(),
            "home" | "standard" | "hacker" | "mining"
        ) {
            return Err(format!(
                "mode.active ('{}') is not a known mode — expected one of \
                 home | standard | hacker (alias: mining). An unknown value is \
                 silently coerced to Standard, discarding operator intent on the \
                 field that gates the quiet/home thermal posture.",
                active
            ));
        }
    }
    if let Some(http_bind) = table
        .get("api")
        .and_then(|v| v.as_table())
        .and_then(|api| api.get("http_bind"))
        .and_then(|v| v.as_str())
    {
        if http_bind.parse::<std::net::IpAddr>().is_err() {
            return Err(format!(
                "api.http_bind ('{}') must be an IP address such as 0.0.0.0, 127.0.0.1, or ::",
                http_bind
            ));
        }
    }
    if let Some(donation) = table.get("donation").and_then(|v| v.as_table()) {
        if let Some(p) = donation.get("percent").and_then(toml_value_as_f64) {
            if !(0.0..=5.0).contains(&p) {
                return Err(format!(
                    "donation.percent ({}) must be between 0.0 and 5.0",
                    p
                ));
            }
        }
        if let Some(c) = donation.get("cycle_duration_s").and_then(toml_value_as_i64) {
            if !(60..=86400).contains(&c) {
                return Err(format!(
                    "donation.cycle_duration_s ({}) must be between 60 and 86400",
                    c
                ));
            }
        }
    }
    if let Some(power) = table.get("power").and_then(|v| v.as_table()) {
        let target = power.get("target_watts").and_then(toml_value_as_i64);
        let max = power.get("max_watts").and_then(toml_value_as_i64);
        if let (Some(t), Some(m)) = (target, max) {
            if t > m {
                return Err(format!(
                    "power.target_watts ({}) cannot exceed power.max_watts ({})",
                    t, m
                ));
            }
        }
        if let Some(v) = power
            .get("psu_override")
            .and_then(|v| v.as_table())
            .and_then(|o| o.get("voltage_v"))
            .and_then(toml_value_as_f64)
        {
            if v <= 5.0 || v > 20.0 {
                return Err(format!(
                    "power.psu_override.voltage_v ({:.2}) is outside the sane PSU rail range (5.0-20.0 V)",
                    v
                ));
            }
        }
    }
    if let Some(mining) = table.get("mining").and_then(|v| v.as_table()) {
        const MINING_VOLTAGE_MV_MIN: i64 = 5000;
        const MINING_VOLTAGE_MV_MAX: i64 = 20000;
        const MINING_FREQ_MHZ_MIN: i64 = 50;
        const MINING_FREQ_MHZ_MAX: i64 = 1200;
        if let Some(mv) = mining.get("voltage_mv").and_then(toml_value_as_i64) {
            if mv > 0 && !(MINING_VOLTAGE_MV_MIN..=MINING_VOLTAGE_MV_MAX).contains(&mv) {
                return Err(format!(
                    "mining.voltage_mv {} out of range. Valid: {}-{} mV.",
                    mv, MINING_VOLTAGE_MV_MIN, MINING_VOLTAGE_MV_MAX
                ));
            }
            // FIX 3: the am2 beta-gate hardware refuses any chip-rail target above
            // 14_500 mV (config.rs validate() ~:990 — the .74 hb2 EEPROM corruption
            // envelope). An imported config CANNOT safely know the target platform,
            // so reject >14_500 UNCONDITIONALLY before persist; otherwise the
            // daemon's platform-aware validate() would crash-loop an am2 unit at the
            // next start. This is stricter than the generic 5000-20000 envelope
            // above and applies regardless of detected platform.
            const MINING_VOLTAGE_MV_AM2_CEILING: i64 = 14_500;
            if mv > MINING_VOLTAGE_MV_AM2_CEILING {
                return Err(format!(
                    "mining.voltage_mv ({}) exceeds the {} mV am2 chip-rail ceiling. An \
                     imported config cannot safely know the target platform; the am2 \
                     beta-gate hardware refuses any chip-rail above {} mV (risk of dsPIC \
                     corruption / EEPROM damage — see .74 hb2 incident 2026-04-29). \
                     Reduce mining.voltage_mv to <= {}.",
                    mv,
                    MINING_VOLTAGE_MV_AM2_CEILING,
                    MINING_VOLTAGE_MV_AM2_CEILING,
                    MINING_VOLTAGE_MV_AM2_CEILING
                ));
            }
        }
        if let Some(freq) = mining.get("frequency_mhz").and_then(toml_value_as_i64) {
            if freq > 0 && !(MINING_FREQ_MHZ_MIN..=MINING_FREQ_MHZ_MAX).contains(&freq) {
                return Err(format!(
                    "mining.frequency_mhz {} out of range. Valid: {}-{} MHz.",
                    freq, MINING_FREQ_MHZ_MIN, MINING_FREQ_MHZ_MAX
                ));
            }
        }
        if let Some(0) = mining.get("serial_chip_count").and_then(toml_value_as_i64) {
            return Err(
                "mining.serial_chip_count must be >= 1 (0 chips is not a valid chain geometry; \
                 it divide-by-zero panics chain address assignment at init)"
                    .to_string(),
            );
        }
    }
    // FIX 3: watchdog cadence sanity (config.rs validate() ~:1118-1142). The
    // kicker period is fed verbatim into tokio::time::interval, which PANICS on a
    // zero period (panic=abort → daemon aborts right after the hardware watchdog
    // is armed → the SoC auto-reboots once timeout_s elapses). A kick interval at
    // or above the timeout means even a healthy daemon can't kick in time, so the
    // SoC reboots a working unit. Only enforced when the watchdog is (or defaults
    // to) enabled — WatchdogConfig::enabled defaults to true (config.rs:5203), so
    // a present [watchdog] section that omits `enabled` is treated as enabled.
    if let Some(watchdog) = table.get("watchdog").and_then(|v| v.as_table()) {
        let enabled = watchdog
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if enabled {
            let kick = watchdog.get("kick_interval_s").and_then(toml_value_as_i64);
            let timeout = watchdog.get("timeout_s").and_then(toml_value_as_i64);
            if let Some(k) = kick {
                if k <= 0 {
                    return Err(format!(
                        "watchdog.kick_interval_s ({}) must be > 0 when watchdog.enabled is \
                         true — a zero/negative kick interval panics the watchdog kicker \
                         (tokio::time::interval rejects a zero period), which on a \
                         panic=abort build aborts the daemon right after the hardware \
                         watchdog is armed. Default is 5.",
                        k
                    ));
                }
                if let Some(t) = timeout {
                    if k >= t {
                        return Err(format!(
                            "watchdog.kick_interval_s ({}) must be less than watchdog.timeout_s \
                             ({}) — a kick interval at or above the hardware timeout means even a \
                             healthy daemon cannot kick before the watchdog fires, so the SoC \
                             reboots a working unit. Default is kick=5 / timeout=30.",
                            k, t
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// COMP-1 keep-existing: a re-imported export carries the redaction placeholder
/// for every secret AND for the wallet/payout addresses that FIX 1 masks
/// (`worker`, `fallback_worker`, `coinbase_output_address`). Restore each from
/// the current stored config so we NEVER persist the literal mask and never
/// clobber a real secret. A placeholder with no current counterpart is dropped.
///
/// FIX 2 (2026-06-20): this resolver is intentionally key-AGNOSTIC — it restores
/// ANY string value that equals the placeholder, matched to the current config by
/// its exact structural position (same nested table / same array index / same key
/// name). That inherently mirrors FIX 1's generalized key set (the masked
/// wallet/secret keys are all placeholders, restored by position) PLUS every
/// `key_is_secret` key, with no per-key list to drift out of sync. Credential
/// URLs are NOT placeholders after export (FIX 1 sanitizes them — only the inline
/// `user:pass@` is stripped and the clean host round-trips verbatim), so they are
/// left untouched here; their stripped credentials are not restorable and must be
/// re-entered (a placeholder a caller DOES put on a `url`/`*_url`/`broker` key is
/// still honoured by the generic restore below).
fn resolve_import_redaction_placeholders(imported: &mut toml::Table, current: &toml::Table) {
    let keys: Vec<String> = imported.keys().cloned().collect();
    let mut remove_keys: Vec<String> = Vec::new();
    for key in keys {
        let current_val = current.get(&key);
        match imported.get_mut(&key) {
            Some(toml::Value::String(s)) if s.as_str() == SECRET_REDACTION_PLACEHOLDER => {
                match current_val.and_then(|v| v.as_str()) {
                    Some(existing) => *s = existing.to_string(),
                    None => remove_keys.push(key.clone()),
                }
            }
            Some(toml::Value::Table(inner)) => {
                if let Some(cur_inner) = current_val.and_then(|v| v.as_table()) {
                    resolve_import_redaction_placeholders(inner, cur_inner);
                } else {
                    drop_placeholder_strings(inner);
                }
            }
            Some(toml::Value::Array(items)) => {
                let cur_items = current_val.and_then(|v| v.as_array());
                for (idx, item) in items.iter_mut().enumerate() {
                    if let toml::Value::Table(inner) = item {
                        match cur_items
                            .and_then(|arr| arr.get(idx))
                            .and_then(|v| v.as_table())
                        {
                            Some(cur_inner) => {
                                resolve_import_redaction_placeholders(inner, cur_inner)
                            }
                            None => drop_placeholder_strings(inner),
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for key in &remove_keys {
        imported.remove(key);
    }
}

/// Remove any string value equal to the redaction placeholder (used when an
/// imported section has no counterpart in the current config — there is nothing
/// to keep-existing from, so the mask must never be persisted as a real value).
fn drop_placeholder_strings(table: &mut toml::Table) {
    let keys: Vec<String> = table.keys().cloned().collect();
    let mut remove_keys: Vec<String> = Vec::new();
    for key in keys {
        match table.get_mut(&key) {
            Some(toml::Value::String(s)) if s.as_str() == SECRET_REDACTION_PLACEHOLDER => {
                remove_keys.push(key.clone())
            }
            Some(toml::Value::Table(inner)) => drop_placeholder_strings(inner),
            Some(toml::Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let toml::Value::Table(inner) = item {
                        drop_placeholder_strings(inner);
                    }
                }
            }
            _ => {}
        }
    }
    for key in &remove_keys {
        table.remove(key);
    }
}

/// Outcome of a successful [`apply_config_import`]: which top-level sections were
/// overlaid from the upload (`applied`) and which existing sections the merge
/// preserved because the upload omitted them (`preserved`). Both sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigImportOutcome {
    applied: Vec<String>,
    preserved: Vec<String>,
}

/// FIX 4 / SW-13 section-preservation merge: overlay each top-level section
/// present in `imported` onto a clone of `current`, preserving every current
/// section the upload omitted. A wholesale persist of `imported` alone would
/// silently DROP every unmentioned section (data loss) — this is the same
/// read-modify-write section-preservation invariant the rest of this file's
/// config writers use. Overlay is at top-level-section granularity (an imported
/// section REPLACES the corresponding current section; that mirrors the
/// full-section export and matches operator expectations). Pure → host-testable.
/// Returns `(merged_table, applied_sorted, preserved_sorted)`.
fn merge_imported_config_sections(
    current: &toml::Table,
    imported: &toml::Table,
) -> (toml::Table, Vec<String>, Vec<String>) {
    let mut merged = current.clone();
    let mut applied: Vec<String> = Vec::with_capacity(imported.len());
    for (key, value) in imported.iter() {
        merged.insert(key.clone(), value.clone());
        applied.push(key.clone());
    }
    let mut preserved: Vec<String> = current
        .keys()
        .filter(|k| !imported.contains_key(k.as_str()))
        .cloned()
        .collect();
    applied.sort();
    preserved.sort();
    (merged, applied, preserved)
}

/// Parse + validate (fail-closed) + keep-existing-resolve + MERGE onto the current
/// effective config + atomically persist an imported config. Returns which
/// top-level sections were applied and which existing ones were preserved.
fn apply_config_import(
    config_toml: &str,
) -> std::result::Result<ConfigImportOutcome, ConfigPersistenceError> {
    // 1. Parse — fail closed on malformed TOML.
    let mut imported: toml::Table = toml::from_str(config_toml)
        .map_err(|e| ConfigPersistenceError::bad_request(format!("Invalid config TOML: {}", e)))?;

    // 2. Reject unknown top-level sections (daemon deny_unknown_fields → restart
    //    crash-loop). Fail closed.
    let unknown = disallowed_import_sections(&imported);
    if !unknown.is_empty() {
        return Err(ConfigPersistenceError::bad_request(format!(
            "Disallowed config sections: {}. Allowed: {}",
            unknown.join(", "),
            CONFIG_IMPORT_ALLOWED_SECTIONS.join(", "),
        )));
    }

    // 3. Sanity-bounds validation (mirror of DcentraldConfig::validate()). Fail
    //    closed before any write.
    validate_imported_config_table(&imported).map_err(ConfigPersistenceError::bad_request)?;

    // 4. Keep-existing: resolve redaction placeholders against the current config
    //    so a re-imported export never overwrites a real secret with the mask.
    // RELIAB-2b: hold the config-write lock across load→merge→write so a
    // concurrent config writer can't interleave and cause a lost update. This is
    // a synchronous fn — the guard drops on return, never across an `.await`.
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let current = load_config_table_for_write().unwrap_or_default();
    resolve_import_redaction_placeholders(&mut imported, &current);

    // 5. FIX 4: MERGE the uploaded sections onto the current effective config so
    //    sections the upload omitted are PRESERVED (a wholesale persist of the
    //    upload would drop them — SW-13 section-preservation invariant).
    let (mut merged, applied, preserved) = merge_imported_config_sections(&current, &imported);

    // 6. Stamp schema + persist atomically via the canonical write path.
    migrate_config_schema(&mut merged);
    let config_path = get_writable_config_path();
    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ConfigPersistenceError::from_io("Failed to create config directory", e))?;
    }
    let output = toml::to_string_pretty(&merged).map_err(|e| {
        ConfigPersistenceError::bad_request(format!("Failed to serialize config: {}", e))
    })?;
    atomic_write(config_path, output)
        .map_err(|e| ConfigPersistenceError::from_io("Failed to write config", e))?;

    Ok(ConfigImportOutcome { applied, preserved })
}

/// POST /api/config/import — validate (fail-closed) then atomically persist an
/// exported config. Restart required to apply (COMP-1, LuxOS/Braiins parity).
async fn post_config_import(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConfigImportPayload>,
) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/config/import",
    ) {
        return response;
    }

    match apply_config_import(&body.config_toml) {
        Ok(outcome) => {
            push_rest_audit_free(
                &state,
                "config_import",
                format!(
                    "Config imported and validated ({} sections applied, {} preserved, restart required)",
                    outcome.applied.len(),
                    outcome.preserved.len()
                ),
            );
            Json(serde_json::json!({
                "status": "ok",
                "restart_required": true,
                "message": "Configuration imported, value-validated, and merged onto the current config (omitted sections preserved). Restart dcentrald to apply. NOTE: unknown/typo'd NESTED keys are only fully checked at the next daemon start — if one is present the daemon parks in management-only mode (mining disabled, config NOT lost) until corrected.",
                "applied_sections": outcome.applied,
                "preserved_sections": outcome.preserved,
                // Back-compat alias: `sections` mirrors the applied list.
                "sections": outcome.applied,
            }))
            .into_response()
        }
        Err(error) => error.into_response(),
    }
}

fn build_api_compatibility_manifest_response() -> ApiCompatibilityManifestResponse {
    ApiCompatibilityManifestResponse {
        status: "ok",
        schema_version: 1,
        read_only: true,
        content_collected: false,
        probe_performed: false,
        handlers_executed: false,
        surfaces: API_COMPATIBILITY_SURFACES,
        omissions: API_COMPATIBILITY_OMISSIONS,
        limitations: &[
            "This manifest is declared by firmware source code.",
            "It does not call, probe, test, upload, write, reboot, or open the listed endpoints.",
            "Support values describe mounted or recognized compatibility surfaces, not live endpoint health.",
            "Side-effecting routes are identified so tools can avoid unsafe automatic calls.",
        ],
    }
}

/// GET /api/system/api-compatibility/manifest -- Read-only API compatibility manifest.
///
/// Reports declared REST, /api/v1 alias, WebSocket, and CGMiner compatibility
/// surfaces without calling handlers, probing endpoints, or collecting runtime data.
async fn get_api_compatibility_manifest() -> impl IntoResponse {
    Json(build_api_compatibility_manifest_response())
}

/// pyasic / fleet-tool friendly field names mapped onto canonical DCENT routes
/// (P3-1 audit note). Most fleet-integration "404s" are name mismatches, so this
/// lets a tool resolve a friendly name to the real mounted route. Every target
/// here MUST be a real mounted route — the index test cross-checks each value
/// against `mounted_route_paths_from_source()` so a stale alias can't ship.
const API_INDEX_ROUTE_ALIASES: &[(&str, &str)] = &[
    ("fans", "/api/status"),
    ("autotune_status", "/api/autotuner/status"),
    ("thermal", "/api/thermal/supervisor"),
    ("pools.failover", "/api/pools/failover_policy"),
    ("tuning_profiles", "/api/profiles"),
];

/// Builds the machine-readable route catalog for `GET /api/index`.
///
/// Reuses the firmware-declared compatibility manifest
/// (`build_api_compatibility_manifest_response`) as the single source of route
/// truth — it does NOT hand-maintain a second route list — and flattens it into
/// a minimal OpenAPI-ish catalog (surfaces + routes + commands) plus the small
/// pyasic-friendly alias map. Read-only: it does not call, probe, or open any of
/// the listed endpoints.
fn build_api_index_response() -> serde_json::Value {
    let manifest = build_api_compatibility_manifest_response();

    let mut surfaces: Vec<serde_json::Value> = Vec::new();
    let mut routes: Vec<serde_json::Value> = Vec::new();
    let mut commands: Vec<serde_json::Value> = Vec::new();
    for surface in manifest.surfaces {
        surfaces.push(serde_json::json!({
            "id": surface.id,
            "label": surface.label,
            "protocol": surface.protocol,
            "default_port": surface.default_port,
            "default_bind": surface.default_bind,
            "route_count": surface.routes.len(),
            "command_count": surface.commands.len(),
        }));
        for route in surface.routes {
            routes.push(serde_json::json!({
                "method": route.method,
                "path": route.path,
                "surface": surface.id,
                "support": route.support,
                "mutates": route.mutates,
            }));
        }
        for command in surface.commands {
            commands.push(serde_json::json!({
                "name": command.name,
                "surface": surface.id,
                "support": command.support,
                "mutates": command.mutates,
            }));
        }
    }

    let aliases: serde_json::Value = API_INDEX_ROUTE_ALIASES
        .iter()
        .map(|(name, path)| {
            (
                (*name).to_string(),
                serde_json::Value::String((*path).to_string()),
            )
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();

    let surface_count = surfaces.len();
    let route_count = routes.len();
    let command_count = commands.len();

    serde_json::json!({
        "status": "ok",
        "schema": "dcentos.api.index.v1",
        "api_contract_version": dcentrald_api_types::API_CONTRACT_VERSION,
        "read_only": true,
        "generated_from": "/api/system/api-compatibility/manifest",
        "surface_count": surface_count,
        "route_count": route_count,
        "command_count": command_count,
        "surfaces": surfaces,
        "routes": routes,
        "commands": commands,
        "aliases": aliases,
        "limitations": [
            "Catalog is declared by firmware source; listed routes are not called or probed.",
            "Aliases map common fleet-tool field names onto canonical DCENT routes; they are documentation, not separate endpoints.",
        ],
    })
}

/// GET /api/index -- Machine-readable route catalog for fleet tooling.
///
/// P3-1 (Omega Plan): there was no machine-readable API index, so external
/// fleet tools had to hardcode route paths and most "404s" were name mismatches.
/// Emits a JSON catalog of the mounted compatibility surfaces (derived from the
/// same data as `/api/system/api-compatibility/manifest`) plus pyasic-friendly
/// name aliases. Read-only; lists routes without calling or probing them.
async fn get_api_index() -> impl IntoResponse {
    Json(build_api_index_response())
}

/// GET /api/competitive/readiness -- Read-only competitive readiness contract.
///
/// Reports  competitive feature status and Decentralization Gate fields
/// without probing hardware, pools, logs, filesystems, cloud services, or other
/// endpoint handlers.
async fn get_competitive_readiness(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/competitive/readiness", mode)
    {
        return resp.into_response();
    }

    Json(build_competitive_readiness_response(unix_time_ms())).into_response()
}

/// Write key-value pairs to a TOML section. Creates section if missing.
/// Uses read-modify-write pattern to preserve existing config.
pub(crate) fn write_toml_section(
    section: &str,
    entries: &[(&str, toml::Value)],
) -> std::result::Result<(), String> {
    // RELIAB-2b: serialize the whole load→modify→write so a concurrent config
    // writer can't interleave and drop this section's change (lost update).
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_path = get_writable_config_path();
    let mut table = load_config_table_for_write()?;

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let sec = table
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(ref mut sec_table) = sec {
        for (key, value) in entries {
            sec_table.insert(key.to_string(), value.clone());
        }
    }

    let output =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

const WEBHOOK_SUPPORTED_EVENTS: &[&str] = &[
    // AlertEvent-sourced names (thermal/hashboard alerts).
    "emergency_shutdown",
    "fan_failure",
    "pool_disconnected",
    "mining_stopped",
    "hashboard_offline",
    "thermal_restart",
    // HLA-10 / PH-3 hashrate alerts — AlertEvent + WebhookEvent superset names.
    "hashrate_degraded",
    "hashrate_recovery_exhausted",
    // W11 C-1: WebhookEvent-sourced names — MUST stay in sync with
    // `WebhookEvent::event_name()` in dcentrald-api/src/webhook.rs, else the
    // /api/config/webhook allow-list validation rejects a config that names them.
    // (mining_stopped + pool_disconnected already listed above.)
    "mining_started",
    "pool_failover",
    "thermal_safety",
    "share_milestone",
    "lucky_share",
    "ota",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WebhookConfigPayload {
    enabled: bool,
    #[serde(default)]
    url: String,
    #[serde(default)]
    events: Vec<String>,
    /// Delivery channel format (generic / discord / slack / telegram).
    #[serde(default)]
    format: crate::webhook::WebhookFormat,
    /// Telegram bot token (only used when `format == telegram`). SECRET — masked
    /// in the GET response and treated keep-existing-on-redaction on write.
    #[serde(default)]
    telegram_bot_token: String,
    /// Telegram chat id (only used when `format == telegram`).
    #[serde(default)]
    telegram_chat_id: String,
}

/// Parse a `[webhook].format` string into the typed enum (default `generic`).
fn parse_webhook_format(s: &str) -> crate::webhook::WebhookFormat {
    match s.trim().to_ascii_lowercase().as_str() {
        "discord" => crate::webhook::WebhookFormat::Discord,
        "slack" => crate::webhook::WebhookFormat::Slack,
        "telegram" => crate::webhook::WebhookFormat::Telegram,
        _ => crate::webhook::WebhookFormat::Generic,
    }
}

/// The on-disk `[webhook].format` string for a typed format value.
fn webhook_format_str(format: crate::webhook::WebhookFormat) -> &'static str {
    match format {
        crate::webhook::WebhookFormat::Generic => "generic",
        crate::webhook::WebhookFormat::Discord => "discord",
        crate::webhook::WebhookFormat::Slack => "slack",
        crate::webhook::WebhookFormat::Telegram => "telegram",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MqttConfigPayload {
    enabled: bool,
    broker: String,
    topic_prefix: String,
    discovery: bool,
    username: String,
    password: String,
    publish_interval_s: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MqttConfigResponse {
    enabled: bool,
    broker: String,
    topic_prefix: String,
    discovery: bool,
    username: String,
    password: String,
    publish_interval_s: u16,
    restart_required: bool,
    runtime_message: String,
}

impl Default for MqttConfigPayload {
    fn default() -> Self {
        Self {
            enabled: false,
            broker: "mqtt://localhost:1883".to_string(),
            topic_prefix: "dcentrald".to_string(),
            discovery: true,
            username: String::new(),
            password: String::new(),
            publish_interval_s: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct WebhookConfigResponse {
    enabled: bool,
    url: String,
    events: Vec<String>,
    supported_events: Vec<&'static str>,
    restart_required: bool,
    format: crate::webhook::WebhookFormat,
    /// Masked: set => "<redacted>", unset => "". Never the real token.
    telegram_bot_token: String,
    telegram_chat_id: String,
}

fn normalize_webhook_events(events: &[String]) -> Vec<String> {
    if events.is_empty() {
        WEBHOOK_SUPPORTED_EVENTS
            .iter()
            .map(|event| (*event).to_string())
            .collect()
    } else {
        events.to_vec()
    }
}

fn read_webhook_config(table: &toml::Table) -> WebhookConfigPayload {
    let Some(webhook) = table.get("webhook").and_then(|value| value.as_table()) else {
        return WebhookConfigPayload {
            enabled: false,
            url: String::new(),
            events: normalize_webhook_events(&[]),
            ..Default::default()
        };
    };

    let raw_events = webhook
        .get("events")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    WebhookConfigPayload {
        enabled: webhook
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        url: webhook
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        events: normalize_webhook_events(&raw_events),
        format: webhook
            .get("format")
            .and_then(|value| value.as_str())
            .map(parse_webhook_format)
            .unwrap_or_default(),
        telegram_bot_token: webhook
            .get("telegram_bot_token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        telegram_chat_id: webhook
            .get("telegram_chat_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

fn validate_webhook_config(config: &WebhookConfigPayload) -> std::result::Result<(), String> {
    if config.enabled {
        match config.format {
            // Telegram delivers via the Bot API (token + chat id), not a URL.
            crate::webhook::WebhookFormat::Telegram => {
                if config.telegram_bot_token.trim().is_empty()
                    || config.telegram_chat_id.trim().is_empty()
                {
                    return Err(
                        "Telegram bot token and chat id are required when the Telegram webhook format is enabled"
                            .to_string(),
                    );
                }
            }
            // Generic / Discord / Slack all POST to the configured URL.
            crate::webhook::WebhookFormat::Generic
            | crate::webhook::WebhookFormat::Discord
            | crate::webhook::WebhookFormat::Slack => {
                if config.url.trim().is_empty() {
                    return Err(
                        "Webhook URL is required when webhook notifications are enabled"
                            .to_string(),
                    );
                }
            }
        }
    }

    let invalid_events: Vec<String> = config
        .events
        .iter()
        .filter(|event| !WEBHOOK_SUPPORTED_EVENTS.contains(&event.as_str()))
        .cloned()
        .collect();
    if !invalid_events.is_empty() {
        return Err(format!(
            "Unsupported webhook events: {}",
            invalid_events.join(", ")
        ));
    }

    Ok(())
}

fn webhook_config_response(config: WebhookConfigPayload) -> WebhookConfigResponse {
    WebhookConfigResponse {
        enabled: config.enabled,
        // SEC (W20 / parity #59,#66): the webhook URL embeds the delivery
        // secret (Slack/Discord token). `webhook.url` is classified secret in
        // CONFIG_BACKUP_SECRET_KEY_PATTERNS; this read/echo path was returning
        // it in cleartext. Mask it (set => "<redacted>", unset => "").
        // `post_webhook_config` treats the placeholder as keep-existing so a
        // dashboard round-trip never overwrites the real URL with its mask.
        url: redact_password(&config.url),
        events: normalize_webhook_events(&config.events),
        supported_events: WEBHOOK_SUPPORTED_EVENTS.to_vec(),
        restart_required: false,
        format: config.format,
        // SEC: the Telegram bot token is a delivery secret (matches the
        // CONFIG_BACKUP_SECRET_KEY_PATTERNS "token" rule). Mask it like the URL
        // (set => "<redacted>", unset => ""); `post_webhook_config` treats the
        // placeholder as keep-existing so a round-trip never persists the mask.
        telegram_bot_token: redact_password(&config.telegram_bot_token),
        // The chat id is an addressing value, not a secret — echoed verbatim.
        telegram_chat_id: config.telegram_chat_id,
    }
}

fn mqtt_runtime_message() -> String {
    "MQTT settings are saved to dcentrald.toml immediately. The running daemon polls for [mqtt] changes and restarts the lightweight publisher task within a few seconds.".to_string()
}

fn read_mqtt_config(table: &toml::Table) -> MqttConfigPayload {
    let mut config = MqttConfigPayload::default();
    let Some(mqtt) = table.get("mqtt").and_then(|value| value.as_table()) else {
        return config;
    };

    config.enabled = mqtt
        .get("enabled")
        .and_then(|value| value.as_bool())
        .unwrap_or(config.enabled);
    config.broker = mqtt
        .get("broker")
        .and_then(|value| value.as_str())
        .unwrap_or(config.broker.as_str())
        .to_string();
    config.topic_prefix = mqtt
        .get("topic_prefix")
        .or_else(|| mqtt.get("topicPrefix"))
        .and_then(|value| value.as_str())
        .unwrap_or(config.topic_prefix.as_str())
        .to_string();
    config.discovery = mqtt
        .get("discovery")
        .and_then(|value| value.as_bool())
        .unwrap_or(config.discovery);
    config.username = mqtt
        .get("username")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    config.password = mqtt
        .get("password")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    config.publish_interval_s = mqtt
        .get("publish_interval_s")
        .or_else(|| mqtt.get("publishIntervalS"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(config.publish_interval_s);

    config
}

fn mqtt_config_response(config: MqttConfigPayload) -> MqttConfigResponse {
    MqttConfigResponse {
        enabled: config.enabled,
        broker: config.broker,
        topic_prefix: config.topic_prefix,
        discovery: config.discovery,
        username: config.username,
        // SEC (W20 / parity #59,#66): never echo the stored MQTT broker
        // password to a client. The codebase classifies `mqtt.password` as a
        // secret (CONFIG_BACKUP_SECRET_KEY_PATTERNS); this read/echo path was
        // returning it in cleartext. Mask it (set => "<redacted>", unset =>
        // ""). `persist_mqtt_config` treats the placeholder as keep-existing
        // so a dashboard round-trip never overwrites the real secret.
        password: redact_password(&config.password),
        publish_interval_s: config.publish_interval_s,
        restart_required: false,
        runtime_message: mqtt_runtime_message(),
    }
}

fn validate_mqtt_config(config: &MqttConfigPayload) -> std::result::Result<(), String> {
    if config.enabled {
        if config.broker.trim().is_empty() {
            return Err("MQTT broker is required when MQTT publishing is enabled".to_string());
        }
        if config.topic_prefix.trim().is_empty() {
            return Err(
                "MQTT topic prefix is required when MQTT publishing is enabled".to_string(),
            );
        }
    }

    if !config.broker.trim().is_empty() {
        crate::mqtt::parse_broker_url(&config.broker)
            .map_err(|error| format!("Invalid MQTT broker: {}", error))?;
    }

    if config.publish_interval_s == 0 {
        return Err("MQTT publish interval must be at least 1 second".to_string());
    }
    if config.publish_interval_s > 3600 {
        return Err("MQTT publish interval must be 3600 seconds or less".to_string());
    }

    let username_set = !config.username.trim().is_empty();
    let password_set = !config.password.is_empty();
    if username_set ^ password_set {
        return Err(
            "MQTT username and password must either both be set or both be empty".to_string(),
        );
    }

    Ok(())
}

/// MQTT-1: resolve a kept (masked) MQTT password against the stored config.
///
/// The GET response ([`mqtt_config_response`]) masks `mqtt.password` to
/// [`SECRET_REDACTION_PLACEHOLDER`]. A dashboard "Test connection" round-trip
/// that didn't change the password re-POSTs that placeholder. Running the broker
/// test with the literal mask would test the wrong credential (the string
/// `"<redacted>"`) instead of the real saved secret. Resolve the placeholder
/// back to the stored value so the test exercises the actual credentials; any
/// other value (including empty) is used verbatim. Mirrors
/// [`persist_mqtt_config`]'s keep-existing-on-redaction handling.
fn resolve_mqtt_test_password(submitted: &str, stored_table: &toml::Table) -> String {
    if submitted == SECRET_REDACTION_PLACEHOLDER {
        read_mqtt_config(stored_table).password
    } else {
        submitted.to_string()
    }
}

fn persist_mqtt_config(config: &MqttConfigPayload) -> std::result::Result<(), String> {
    // RELIAB-2b: serialize the whole load→modify→write so a concurrent config
    // writer can't interleave and drop this section's change (lost update).
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_path = get_writable_config_path();
    let mut table = load_config_table_for_write()?;

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let mqtt = table
        .entry("mqtt".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let mqtt_table = mqtt
        .as_table_mut()
        .ok_or_else(|| "[mqtt] is not a TOML table".to_string())?;

    mqtt_table.insert("enabled".to_string(), toml::Value::Boolean(config.enabled));
    mqtt_table.insert(
        "broker".to_string(),
        toml::Value::String(config.broker.clone()),
    );
    mqtt_table.insert(
        "topic_prefix".to_string(),
        toml::Value::String(config.topic_prefix.clone()),
    );
    mqtt_table.insert(
        "discovery".to_string(),
        toml::Value::Boolean(config.discovery),
    );
    mqtt_table.insert(
        "publish_interval_s".to_string(),
        toml::Value::Integer(i64::from(config.publish_interval_s)),
    );

    if config.username.trim().is_empty() {
        mqtt_table.remove("username");
    } else {
        mqtt_table.insert(
            "username".to_string(),
            toml::Value::String(config.username.trim().to_string()),
        );
    }

    // Keep-existing-on-redaction (W20 SEC): the GET response masks the stored
    // password to SECRET_REDACTION_PLACEHOLDER, so a dashboard round-trip that
    // didn't change it re-POSTs the placeholder. Treat the placeholder as
    // "leave the stored password untouched" (the loaded table already holds
    // the real value) rather than overwriting the secret with its mask. An
    // empty value still clears; any other value still sets.
    if config.password == SECRET_REDACTION_PLACEHOLDER {
        // keep whatever the loaded config table already holds for [mqtt].password
    } else if config.password.is_empty() {
        mqtt_table.remove("password");
    } else {
        mqtt_table.insert(
            "password".to_string(),
            toml::Value::String(config.password.clone()),
        );
    }

    let output =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

fn webhook_miner_name(table: &toml::Table) -> String {
    table
        .get("general")
        .and_then(|value| value.as_table())
        .and_then(|general| general.get("hostname"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("dcentos")
        .to_string()
}

/// Build the `(target_url, body)` for a synthetic "test" notification, reshaped
/// for the configured channel exactly as the live dispatcher would deliver it.
///
/// The synthetic event is a `mining_stopped` / `dashboard_test` alert, so the
/// Generic body stays byte-identical to the historical test payload while
/// `discord` / `slack` / `telegram` validate the reshaped delivery (and, for
/// Telegram, the Bot API endpoint URL). The event carries no secret, so
/// `redact()` is a no-op here — it is called only to honour the contract that
/// redaction always precedes `render_text`.
fn webhook_test_payload(
    format: crate::webhook::WebhookFormat,
    miner: &str,
    url: &str,
    telegram_bot_token: &str,
    telegram_chat_id: &str,
) -> (String, serde_json::Value) {
    let mut event = crate::webhook::WebhookEvent::MiningStopped {
        reason: "dashboard_test".to_string(),
    };
    event.redact();
    crate::webhook::payload_for(
        format,
        miner,
        url,
        Some(telegram_bot_token),
        Some(telegram_chat_id),
        &event,
    )
}

pub(crate) fn persist_power_calibration(
    calibration: Option<&dcentrald_autotuner::PowerCalibration>,
) -> std::result::Result<(), String> {
    // RELIAB-2b: serialize the whole load→modify→write so a concurrent config
    // writer can't interleave and drop this section's change (lost update).
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_path = get_writable_config_path();
    let mut table = load_config_table_for_write()?;

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let power = table
        .entry("power".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));

    let power_table = power
        .as_table_mut()
        .ok_or_else(|| "[power] is not a TOML table".to_string())?;

    if let Some(calibration) = calibration {
        let value = toml::Value::try_from(calibration.clone())
            .map_err(|e| format!("Failed to serialize calibration: {}", e))?;
        power_table.insert("calibration".to_string(), value);
    } else {
        power_table.remove("calibration");
    }

    let output =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

fn json_to_toml(value: &serde_json::Value) -> std::result::Result<toml::Value, String> {
    match value {
        serde_json::Value::Null => {
            Err("null values are not supported in config updates".to_string())
        }
        serde_json::Value::Bool(v) => Ok(toml::Value::Boolean(*v)),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = v.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err(format!("unsupported numeric value: {}", v))
            }
        }
        serde_json::Value::String(v) => Ok(toml::Value::String(v.clone())),
        serde_json::Value::Array(values) => {
            let mut out = Vec::with_capacity(values.len());
            for item in values {
                out.push(json_to_toml(item)?);
            }
            Ok(toml::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::Table::new();
            for (key, value) in map {
                table.insert(key.clone(), json_to_toml(value)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}

fn merge_toml_value(dst: &mut toml::Value, src: toml::Value) {
    match (dst, src) {
        (toml::Value::Table(dst_table), toml::Value::Table(src_table)) => {
            for (key, src_value) in src_table {
                if let Some(dst_value) = dst_table.get_mut(&key) {
                    merge_toml_value(dst_value, src_value);
                } else {
                    dst_table.insert(key, src_value);
                }
            }
        }
        (dst_value, src_value) => {
            *dst_value = src_value;
        }
    }
}

/// Compute seconds since a Unix epoch timestamp (0 = never → returns 0).
fn secs_since(epoch_secs: u64) -> u64 {
    if epoch_secs == 0 {
        return 0;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(epoch_secs)
}

fn psu_diag_gpio_payload(hw: &crate::HardwareInfo) -> serde_json::Value {
    let output_enabled = dcentrald_hal::platform::amlogic::is_psu_enabled();

    serde_json::json!({
        "detected": true,
        "model": hw.psu_model,
        "fw_version": hw.psu_fw_version,
        "transport": "gpio",
        "control_mode": "gpio_enable",
        "output_enabled": output_enabled,
        "voltage_range": hw.psu_voltage_range,
        "voltage_in": null,
        "voltage_out": null,
        "current_a": null,
        "power_w": null,
        "temp_c": null,
        "supports_output_gate": true,
        "supports_voltage_set": false,
        "supports_watchdog": false,
        "message": "GPIO PSU control is available on this Amlogic platform. Live PSU voltage/current/power telemetry is not exposed here, and APW voltage programming is not wired for the GPIO-only path.",
    })
}

fn psu_diag_kernel_payload(
    hw: &crate::HardwareInfo,
    psu: &mut dcentrald_hal::psu::PsuController,
) -> serde_json::Value {
    let fw_version = psu.get_version().ok();
    let model = fw_version
        .as_deref()
        .map(dcentrald_hal::psu::PsuController::model_name_from_version);
    let voltage_range = fw_version
        .as_deref()
        .and_then(dcentrald_hal::psu::PsuController::format_voltage_range);
    let output_enabled = psu.read_state().ok();
    let voltage_out = psu.measure_voltage().ok();
    let supports_output_gate = hw.control_board.starts_with("Zynq am2-s17");
    let output_gate_enabled = if supports_output_gate {
        Some(dcentrald_hal::platform::zynq::is_psu_output_enabled())
    } else {
        None
    };

    serde_json::json!({
        "detected": true,
        "model": model,
        "fw_version": fw_version,
        "transport": psu.transport_name(),
        "control_mode": "bitmain_apw_i2c",
        "output_enabled": output_enabled,
        "output_gate_enabled": output_gate_enabled,
        "voltage_range": voltage_range,
        "voltage_in": null,
        "voltage_out": voltage_out,
        "current_a": null,
        "power_w": null,
        "temp_c": null,
        "supports_output_gate": supports_output_gate,
        "supports_voltage_set": true,
        "supports_watchdog": true,
        "message": if supports_output_gate {
            "Smart APW runtime control is available on the Bitmain I2C path, and this Zynq board also exposes a GPIO PSU output gate. Current, power, and temperature telemetry are not yet exposed."
        } else {
            "Smart APW runtime control is available on the Bitmain I2C path. This implementation currently reads firmware, output state, and output voltage only; current, power, and temperature telemetry are not exposed."
        },
    })
}

/// Mode middleware is applied to gate debug endpoints.
///
/// NOTE: Does NOT call `.with_state()` — the caller must apply state
/// after merging all routes (REST, WebSocket, dashboard).
pub fn build_router() -> Router<Arc<AppState>> {
    Router::new()
        // Core status endpoints (all modes)
        .route("/api/status", get(get_status))
        .route("/api/network/block", get(get_network_block))
        .route("/api/mining/work/posture", get(get_mining_work_posture))
        .route(
            "/api/mining/pipeline/manifest",
            get(get_mining_pipeline_manifest),
        )
        .route(
            "/api/mining/pipeline/snapshot/schema",
            get(get_mining_pipeline_snapshot_schema),
        )
        .route(
            "/api/mining/pipeline/snapshot",
            get(get_mining_pipeline_snapshot),
        )
        .route("/api/thermal/posture", get(get_thermal_posture))
        .route("/api/thermal/supervisor", get(get_thermal_supervisor))
        .route("/api/fleet/miners", get(get_fleet_miners))
        .route("/api/fleet/discover", post(post_fleet_discover))
        .route("/api/fleet/pool-stats", get(get_fleet_pool_stats))
        .route("/api/pools", get(get_pools).post(post_pools))
        .route("/api/pools/test", post(post_pools_test))
        .route("/api/config", get(get_config).post(post_config))
        .route(
            "/api/config/donation",
            get(get_config_donation).post(post_config_donation),
        )
        .route(
            "/api/config/shared",
            get(get_shared_config).post(post_shared_config),
        )
        .route(
            "/api/config/backup/manifest",
            get(get_config_backup_manifest),
        )
        // COMP-1 (LuxOS/Braiins parity): full daemon config backup/restore.
        // GET /api/config/export — full effective config, all secrets +
        //   wallet workers + credential-bearing pool URLs redacted, re-importable.
        // POST /api/config/import — validate (fail-closed) then atomically persist
        //   an exported config; redaction placeholders are kept-existing.
        .route("/api/config/export", get(get_config_export))
        .route("/api/config/import", post(post_config_import))
        .route(
            "/api/system/api-compatibility/manifest",
            get(get_api_compatibility_manifest),
        )
        .route(
            "/api/compatibility/manifest",
            get(get_api_compatibility_manifest),
        )
        // P3-1 (Omega Plan): machine-readable route catalog for fleet tooling,
        // derived from the compatibility manifest data + pyasic-friendly aliases.
        .route("/api/index", get(get_api_index))
        .route("/api/competitive/readiness", get(get_competitive_readiness))
        .route("/api/system/info", get(get_system_info))
        .route("/api/v1/capabilities", get(get_capabilities))
        .route("/api/system/health", get(get_system_health))
        //  HIGH-1/2/3 (2026-05-24) — `a lab unit`-class XIL
        // bosminer-handoff surfaces. Read-only; no auth gate (Gate-1 Q3
        // = match existing dev-firmware no-auth posture). On non-`a lab unit`
        // units these endpoints still respond but `is_xil_25_class` is
        // false so dashboard components render empty.
        .route("/api/env/recipe", get(get_env_recipe))
        .route("/api/mining/chain/presence", get(get_chain_presence))
        .route("/api/mining/handoff/state", get(get_mining_handoff_state))
        .route("/api/system/asic", get(get_system_asic))
        .route("/api/system/update/metadata", get(get_update_metadata))
        .route("/api/system/restart", post(post_action_restart))
        .route("/api/system/identify", post(post_system_identify))
        // W5.1 (2026-05-07): dashboard self-detection. Returns the
        // (built_at, sha256, version) of the on-disk dashboard SPA so
        // the React client can detect when it is running against a
        // stale daemon (or vice versa) and prompt a hard reload.
        .route("/api/dashboard/version", get(get_dashboard_version))
        // P0-6 (Omega Plan, C-7): the always-injected diagnostic banner polls
        // `/api/dashboard/health`. When the SPA is served by server.py on :80
        // that path is answered locally (works even when the daemon is dead).
        // When the SPA is served DIRECTLY by the daemon on :8080 the route did
        // not exist, so the poll 404'd. Registering it here means a reachable
        // daemon answers `alive` (no false DEAD bar) while an unreachable daemon
        // makes the fetch fail so the banner surfaces the bar.
        .route("/api/dashboard/health", get(get_dashboard_health))
        .route("/api/swarm", get(get_swarm_status))
        .route("/api/swarm/room-temp", post(post_swarm_room_temp))
        // Read-only MCP mount: initialize, tools/list, and status/device/swarm
        // tools only. Mutating MCP tools from the shared profile are rejected by
        // the handler, and normal auth still wraps this route.
        .route("/mcp", post(post_mcp))
        // Control actions (all modes)
        .route("/api/action/restart", post(post_action_restart))
        .route("/api/action/reboot", post(post_action_reboot))
        .route("/api/action/sleep", post(post_action_sleep))
        .route("/api/action/wake", post(post_action_wake))
        // Fan control (all modes)
        .route("/api/fan", post(post_fan))
        // PSU override (all modes)
        .route(
            "/api/config/mqtt",
            get(get_mqtt_config).post(post_mqtt_config),
        )
        .route("/api/config/mqtt/test", post(post_mqtt_test))
        // MQTT/HA integration status (observability; P2-10)
        .route("/api/mqtt/status", get(get_mqtt_status))
        .route(
            "/api/config/psu-override",
            get(get_psu_override).post(post_psu_override),
        )
        .route(
            "/api/config/webhook",
            get(get_webhook_config).post(post_webhook_config),
        )
        .route("/api/config/webhook/test", post(post_webhook_test))
        .route(
            "/api/config/power-calibration",
            get(get_power_calibration).post(post_power_calibration),
        )
        // Off-grid / Direct DC (all modes)
        .route(
            "/api/offgrid/config",
            get(get_offgrid_config).post(post_offgrid_config),
        )
        .route("/api/offgrid/status", get(get_offgrid_status))
        .route("/api/offgrid/presets", get(get_offgrid_presets))
        .route("/api/offgrid/test", post(post_offgrid_test))
        .route(
            "/api/solar/config",
            get(get_solar_config).post(post_solar_config),
        )
        .route("/api/solar/status", get(get_solar_status))
        .route(
            "/api/solar/verification-history",
            get(get_solar_verification_history),
        )
        .route("/api/solar/test", post(post_solar_test))
        // TOU / Power schedule (all modes)
        .route(
            "/api/tou/schedule",
            get(get_tou_schedule).post(post_tou_schedule),
        )
        // Standard + Hacker mode endpoints
        .route("/api/stats", get(get_stats))
        .route("/api/system/stats", get(get_system_stats))
        .route("/api/system/upgrade/status", get(get_system_upgrade_status))
        .route("/api/system/update/status", get(get_system_upgrade_status))
        .route(
            "/api/v1/system/upgrade/status",
            get(get_system_upgrade_status),
        )
        .route(
            "/api/v1/firmware/update/status",
            get(get_system_upgrade_status),
        )
        .route("/api/history", get(get_history))
        .route("/api/history/shares", get(get_share_history))
        .route("/api/profiles", get(get_profiles).post(post_profiles))
        // GROUP-B: read-only canonical 21-step BM1362 silicon table
        // (LuxOS CGMiner `profiles` code 323 equivalent). Distinct from the
        // user-managed `/api/profiles/silicon/*` import surface below and the
        // per-SKU `/api/miner/pvt-table`: this returns the full
        // characterization ladder (step -16..+4) straight from
        // `dcentrald-silicon-profiles::bm1362::BM1362_TABLE`.
        .route("/api/profiles/silicon-table", get(get_silicon_table))
        // Home mode endpoints (all modes)
        .route("/api/home/status", get(get_home_status))
        .route("/api/home/target", post(post_home_target))
        .route("/api/home/presets", get(get_home_presets))
        .route("/api/home/room-temp", post(post_home_room_temp))
        .route("/api/home/history", get(get_home_history))
        .route(
            "/api/home/night-mode",
            get(get_home_night_mode).post(post_home_night_mode),
        )
        // Hacker debug endpoints (Hacker mode only)
        .route(
            "/api/debug/registers",
            get(get_debug_registers).post(post_debug_registers),
        )
        .route("/api/debug/psu/control", post(post_debug_psu_control))
        .route("/api/debug/i2c", get(get_debug_i2c).post(post_debug_i2c))
        .route("/api/debug/asic-command", post(post_debug_asic_command))
        .route("/api/debug/pid-state", get(get_debug_pid_state))
        .route("/api/debug/pid-params", post(post_debug_pid_params))
        .route("/api/debug/log", get(get_debug_log))
        .route("/api/debug/chip/frequency", post(post_debug_chip_frequency))
        .route("/api/debug/chip/voltage", post(post_debug_chip_voltage))
        // Autotuner endpoints (Standard + Hacker modes)
        .route("/api/autotuner/status", get(get_autotuner_status))
        .route("/api/autotuner/state", get(get_autotuner_status))
        .route("/api/autotuner/target", get(get_autotuner_target))
        .route("/api/autotuner/visibility", get(get_autotuner_visibility))
        .route(
            "/api/autotuner/saved-status",
            get(get_autotuner_saved_status),
        )
        .route(
            "/api/autotuner/tuned_profiles",
            get(get_autotuner_tuned_profiles),
        )
        .route(
            "/api/autotuner/silicon-report",
            get(get_autotuner_silicon_report),
        )
        .route("/api/autotuner/chip-health", get(get_autotuner_chip_health))
        .route("/api/autotuner/active", put(put_autotuner_active))
        // Wave D (RE-010 closure, 2026-05-19): live drill-in per-chip telemetry.
        // Same data path as POST /api/diagnostics/chip-health/start but as a
        // GET (request-time snapshot, no persisted artifact). LuxOS parity
        // feature
        // CORPUS_RESOLUTIONS.md §RE-010.
        .route("/api/chips", get(get_chips))
        .route(
            "/api/autotuner/fleet-profile/export",
            post(post_autotuner_fleet_export),
        )
        .route(
            "/api/autotuner/increment_hashrate_target",
            post(post_autotuner_increment_hashrate_target),
        )
        .route(
            "/api/autotuner/decrement_hashrate_target",
            post(post_autotuner_decrement_hashrate_target),
        )
        .route(
            "/api/autotuner/increment_power_target",
            post(post_autotuner_increment_power_target),
        )
        .route(
            "/api/autotuner/decrement_power_target",
            post(post_autotuner_decrement_power_target),
        )
        .route(
            "/api/autotuner/set_default_hashrate_target",
            post(post_autotuner_set_default_hashrate_target),
        )
        // New autotuner endpoints (best-in-class features)
        .route("/api/autotuner/efficiency", get(get_autotuner_efficiency))
        .route("/api/autotuner/telemetry", get(get_autotuner_telemetry))
        .route(
            "/api/autotuner/telemetry/csv",
            get(get_autotuner_telemetry_csv),
        )
        .route(
            "/api/autotuner/profitability",
            post(post_autotuner_profitability),
        )
        .route(
            "/api/autotuner/noise-profile",
            post(post_autotuner_noise_profile),
        )
        .route(
            "/api/autotuner/room-temp-factor",
            post(post_autotuner_room_temp_factor),
        )
        // LED control endpoints (all modes)
        .route("/api/led/status", get(get_led_status))
        .route("/api/led/pattern", post(post_led_pattern))
        .route("/api/led/locate", post(post_led_locate))
        .route("/api/led/locate/stop", post(post_led_locate_stop))
        .route("/api/led/patterns", get(get_led_patterns))
        .route("/api/led/config", get(get_led_config).post(post_led_config))
        // Authentication endpoints (unauthenticated)
        .route("/api/auth/status", get(get_auth_status))
        .route("/api/auth/setup", post(post_auth_setup))
        .route("/api/auth/session", post(post_auth_session))
        .route("/api/auth/ws-ticket", post(post_auth_ws_ticket))
        .route(
            "/api/auth/session/current",
            delete(delete_auth_session_current),
        )
        // Safety warning endpoints (all modes)
        .route("/api/safety/warnings", get(get_safety_warnings))
        .route("/api/safety/acknowledge", post(post_safety_acknowledge))
        // First-boot setup wizard endpoints
        .route("/api/setup/status", get(get_setup_status))
        .route("/api/setup/step1-safety", post(post_setup_safety))
        .route("/api/setup/step2-circuit", post(post_setup_circuit))
        .route("/api/setup/step3-password", post(post_setup_password))
        .route("/api/setup/step4-mode", post(post_setup_mode))
        .route("/api/setup/step5-pool", post(post_setup_pool))
        // P2-4 (§4.E): capture the electricity rate/currency + quiet-hours at
        // setup. Economics persists to `[home]` (single source of truth);
        // quiet-hours reuses the live `[home.night_mode]` writer (same handler
        // as /api/home/night-mode — start/end hours + PWM≤30 cap + power cut).
        .route("/api/setup/step-economics", post(post_setup_economics))
        .route("/api/setup/quiet-hours", post(post_home_night_mode))
        .route("/api/setup/test-pool", post(post_pools_test))
        .route("/api/setup/skip-password", post(post_setup_skip_password))
        .route("/api/setup/skip-safety", post(post_setup_skip_safety))
        .route("/api/setup/complete", post(post_setup_complete))
        // System upgrade endpoint (signed sysupgrade tar upload)
        .route(
            "/api/system/upgrade",
            post(post_system_upgrade).layer(DefaultBodyLimit::max(SYSTEM_UPGRADE_MAX_UPLOAD_BYTES)),
        )
        .route(
            "/api/v1/system/upgrade",
            post(post_system_upgrade).layer(DefaultBodyLimit::max(SYSTEM_UPGRADE_MAX_UPLOAD_BYTES)),
        )
        .route(
            "/api/v1/firmware/update",
            post(post_system_upgrade).layer(DefaultBodyLimit::max(SYSTEM_UPGRADE_MAX_UPLOAD_BYTES)),
        )
        // Prometheus metrics endpoint
        .route("/metrics", get(get_metrics))
        // Diagnostic endpoints (all modes)
        .route(
            "/api/diagnostics/hashreport/start",
            post(post_diag_hashreport_start),
        )
        .route(
            "/api/diagnostics/hashreport/cancel",
            post(post_diag_hashreport_cancel),
        )
        .route(
            "/api/diagnostics/hashreport/status",
            get(get_diag_hashreport_status),
        )
        .route(
            "/api/diagnostics/hashreport/result",
            get(get_diag_hashreport_result),
        )
        .route(
            "/api/diagnostics/hashreport/report",
            get(get_diag_hashreport_report),
        )
        .route(
            "/api/diagnostics/chip-health/start",
            post(post_diag_chiphealth_start),
        )
        .route(
            "/api/diagnostics/chip-health/status",
            get(get_diag_chiphealth_status),
        )
        .route(
            "/api/diagnostics/chip-health/result",
            get(get_diag_chiphealth_result),
        )
        .route(
            "/api/diagnostics/chip-health/report",
            get(get_diag_chiphealth_report),
        )
        .route(
            "/api/diagnostics/board-health/start",
            post(post_diag_boardhealth_start),
        )
        .route(
            "/api/diagnostics/board-health/status",
            get(get_diag_boardhealth_status),
        )
        .route(
            "/api/diagnostics/board-health/result",
            get(get_diag_boardhealth_result),
        )
        .route(
            "/api/diagnostics/board-health/report",
            get(get_diag_boardhealth_report),
        )
        .route(
            "/api/diagnostics/reports/recent",
            get(get_diag_recent_reports),
        )
        .route(
            "/api/diagnostics/logs/manifest",
            get(get_diagnostics_log_manifest),
        )
        .route(
            "/api/diagnostics/troubleshoot/network",
            get(get_diag_network),
        )
        .route("/api/diagnostics/troubleshoot/psu", get(get_diag_psu))
        .route("/api/diagnostics/troubleshoot/fpga", get(get_diag_fpga))
        .route(
            "/api/diagnostics/troubleshoot/asic-comm",
            get(get_diag_asic_comm),
        )
        .route(
            "/api/diagnostics/troubleshoot/i2c-scan",
            get(get_diag_i2c_scan),
        )
        //  W1: api-types failure_mode catalog (read-only).
        .route(
            "/api/diagnostics/failure_modes",
            get(get_diag_failure_modes),
        )
        //  W4: api-types hashboard_diagnostics::classify_chain
        // verdict for the requested chain id.
        .route("/api/diagnostics/chain", get(get_diag_chain))
        //  W1: share_validation local-reject ring snapshot.
        .route(
            "/api/diagnostics/shares/local_rejects",
            get(get_diag_share_local_rejects),
        )
        //  W3: PIC firmware variant catalog (read-only).
        .route("/api/hardware/pic_info", get(get_hardware_pic_info))
        // S21/BM1368 per-chip temperature readback contract (read-only).
        .route(
            "/api/hardware/thermal/bm1368/chip_temps",
            get(get_hardware_bm1368_chip_temps),
        )
        //  W4: LuxOS recovery actions catalog (read-only).
        .route(
            "/api/diagnostics/recovery_actions",
            get(get_diag_recovery_actions),
        )
        //  W5: DCENT_OS boot timeline + observed phase timestamps.
        .route("/api/system/boot_timeline", get(get_system_boot_timeline))
        //  W2: audit_log ring snapshot.
        .route("/api/history/audit", get(get_history_audit))
        //  W3: PSU model catalog (read-only).
        .route("/api/hardware/psu_catalog", get(get_hardware_psu_catalog))
        //  W4: cgminer command catalog (read-only).
        .route("/api/cgminer/catalog", get(get_cgminer_catalog))
        //  W2: power-profile preset catalog (read-only).
        .route("/api/profiles/presets", get(get_profile_presets))
        //  W3: thermal-sensor topology catalog (read-only).
        .route(
            "/api/hardware/thermal/sensors",
            get(get_hardware_thermal_sensors),
        )
        // : state-machine policy catalog (read-only).
        .route(
            "/api/diagnostics/state_machine",
            get(get_diagnostics_state_machine),
        )
        // : OTA update-capability transparency (read-only).
        .route(
            "/api/system/update_capability",
            get(get_system_update_capability),
        )
        // : cross-firmware status/error vocabulary (read-only).
        .route(
            "/api/diagnostics/error_vocab",
            get(get_diagnostics_error_vocab),
        )
        // : boot-to-mining ramp reference (read-only).
        .route("/api/mining/ramp", get(get_mining_ramp))
        // : Stratum protocol-support transparency (read-only).
        .route("/api/stratum/protocol", get(get_stratum_protocol))
        // : PSU-bypass / Loki-requirement matrix (read-only).
        .route(
            "/api/hardware/psu_bypass_matrix",
            get(get_hardware_psu_bypass_matrix),
        )
        // : cold-environment target auto-adjust (read-only).
        .route(
            "/api/thermal/cold_environment",
            get(get_thermal_cold_environment),
        )
        // : pool-failover policy reference (read-only).
        .route("/api/pools/failover_policy", get(get_pools_failover_policy))
        // : tuning-constraint catalog (read-only).
        .route("/api/tuning/constraints", get(get_tuning_constraints))
        // : temp-sensor outlier-rejection policy (read-only).
        .route(
            "/api/diagnostics/sensor_outlier",
            get(get_diagnostics_sensor_outlier),
        )
        // : VNish REST response-shape reference (read-only).
        .route("/api/firmware/vnish_schema", get(get_firmware_vnish_schema))
        // : LuxOS system-architecture reference (read-only).
        .route(
            "/api/firmware/luxos_architecture",
            get(get_firmware_luxos_architecture),
        )
        // : cooling-mode taxonomy reference (read-only).
        .route("/api/thermal/cooling_modes", get(get_thermal_cooling_modes))
        // : Dynamic-Power-Scaling reference (read-only).
        .route("/api/power/dps", get(get_power_dps))
        // : network-configuration schema reference (read-only).
        .route("/api/network/config_schema", get(get_network_config_schema))
        // : LuxOS web-UI surface map reference (read-only).
        .route(
            "/api/firmware/luxos_web_map",
            get(get_firmware_luxos_web_map),
        )
        // : BraiinsOS proto wire-type reference (read-only).
        .route(
            "/api/firmware/proto_wire_types",
            get(get_firmware_proto_wire_types),
        )
        // : LuxOS CGMiner-compat response shapes (read-only).
        .route(
            "/api/firmware/luxos_responses",
            get(get_firmware_luxos_responses),
        )
        // : LuxOS REST status-code reference (read-only).
        .route(
            "/api/firmware/luxos_status_codes",
            get(get_firmware_luxos_status_codes),
        )
        // : VNish-on-stock overlay layout reference (read-only).
        .route(
            "/api/firmware/vnish_overlay",
            get(get_firmware_vnish_overlay),
        )
        //  W8-D: silicon-profile import endpoints under
        // /api/profiles/silicon/* (list, import, reload, active,
        // get/delete by id). The pre-existing autotuner-mode
        // /api/profiles GET+POST routes above are unchanged so
        // dashboard consumers keep working. Spec:
        // plans/wave4-profile-import-infrastructure.md §E.
        .merge(crate::routes::profiles::router())
        //  W8-F: Restore-to-Stock backend at
        // /api/system/restore-to-stock/{preflight,status}. Multi-step
        // destructive flow (NAND backup + safety preflight + operator
        // serial typed-confirm + dry-run by default). Industry pattern
        // parity with VNish/LuxOS/BraiinsOS stock-revert buttons; we
        // refuse to flash any tarball that contains SECURE_BOOT_SET
        // (no-override), Hashcore root hash, atlas SSH key,
        // hotelfee.json, daemons:22322 listener, or DTU phone-home.
        // See routes/restore_to_stock.rs.
        .merge(crate::routes::restore_to_stock::router())
        // Read-only reverse-engineering catalog endpoints backed by
        // HAL-free dcentrald-api-types constants.
        .merge(crate::routes::re_catalog::router())
        // W5.2 — SV2 protocol endpoints (status, handshake, messages).
        // See routes/sv2.rs.
        .merge(crate::routes::sv2::router())
        // W5.2 — SV2 Job Declaration endpoints (status, config,
        // test-connection). See routes/jd.rs.
        .merge(crate::routes::jd::router())
        // W9.5 — DungeonMaster donation pool public-info endpoint.
        // Read-only, intentionally public disclosure of the donation
        // pool URL + payout address + block-explorer link so operators
        // can independently verify on-chain payout history. See
        // routes/donation.rs. Per swarm review: trust-but-verify.
        .merge(crate::routes::donation::router())
        // W9.4 — J/TH calibration loop (operator wattmeter source-of-truth).
        // POST /api/perf/calibrate accepts a wall-meter reading and bakes it
        // into the persisted PowerCalibration as `operator_confirmed=true`.
        // GET /api/perf/efficiency returns `(j_per_th, source, confidence,
        // measured_at)` for the dashboard EarningsPage source-tagged J/TH.
        // See routes/perf.rs.
        .merge(crate::routes::perf::router())
        // W11.12 — stock-CGI parity (RE2 §15.2 + competing-firmware features).
        // GET /api/network/info  — hostname/MAC/IPv4/v6/gateway/DNS/link.
        // GET /api/miner/type    — concise hardware identity for fleet tools.
        // GET /api/log/backup    — redacted text/plain log bundle for support.
        // Read-only; degrades gracefully when sources are missing. See
        // routes/stock_parity.rs.
        .merge(crate::routes::stock_parity::router())
        // W13.D1 — `/api/miner/pvt-table` returns the full per-SKU PVT
        // freq/voltage table for the detected hashboard. Sourced from
        // `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku`. See
        // routes/pvt_table.rs.
        .merge(crate::routes::pvt_table::router())
        // W13.D1 — `/api/boot/phase` (200 with the real phase only while a
        // cold-boot orchestrator is publishing; 404 when the tracker was
        // never started so the dashboard hides the strip instead of showing a
        // false "Booting" on a healthy mining unit) +
        // `/api/boot/timeline` (dev-mode gated on
        // `ApiConfig::expose_boot_timeline`). 6-substate CV1835
        // taxonomy + generic 3-substate fallback. See
        // routes/boot_phase.rs.
        .merge(crate::routes::boot_phase::router())
        // 2026-05-17 — `POST /api/autotuner/quota` hashrate split-quota
        // planner (ePIC UMC OS V1.18.2 analog). Resolves a fraction /
        // absolute_ths quota to the equivalent wattage target + the
        // canonical `[autotune] mode = "hashrate-quota"` block. The
        // daemon's `TunerMode::HashrateQuota` delegates the real tick
        // path to the gated `PowerTargetController`; this endpoint only
        // plans and never mutates the running tuner. See
        // routes/autotuner_quota.rs.
        .merge(crate::routes::autotuner_quota::router())
        // GROUP C (W8 parity) — `GET /api/audit-log`. Paginated, redacted
        // read-back of the PERSISTENT, reboot-surviving NDJSON audit log
        // (default `/data/audit.log`). Distinct from `/api/history/audit`,
        // which reads the volatile in-memory ring (lost on reboot). See
        // routes/audit_log.rs.
        .merge(crate::routes::audit_log::router())
        // A03 (2026-06-10 knowledge-goldmine, finding s5-luxminer CAND-01) —
        // `GET /api/metrics/rolling` + `/api/metrics/rolling.csv`. Read-only
        // 3-tier (5s/1m/5m) rolling-average ring, LuxOS `/metrics` parity.
        // See routes/rolling_metrics.rs.
        .merge(crate::routes::rolling_metrics::router())
        // A06 (2026-06-10 knowledge-goldmine, finding s5-luxminer CAND-04) —
        // `GET /api/chips/health`. Flat, read-only per-chip hashrate-ratio /
        // error-rate diagnostics array. See routes/chip_health.rs.
        .merge(crate::routes::chip_health::router())
        // A04 (2026-06-10 knowledge-goldmine, finding s5-luxminer API-07) —
        // `GET /api/profile/download` + `POST /api/profile/upload`. V/F
        // profile save/restore (byte-exact round trip). See
        // routes/vf_profile.rs.
        .merge(crate::routes::vf_profile::router())
        .layer(axum::middleware::map_response(normalize_api_error_response))
}

// ─── Query Parameter Types ─────────────────────────────────────────────

/// Query parameters for test status/result lookups.
#[derive(Debug, Deserialize)]
pub struct TestIdQuery {
    pub test_id: String,
}

/// Query parameters for report format.
#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub test_id: String,
    pub format: Option<String>,
}

/// Query parameters for recent report listings.
#[derive(Debug, Deserialize)]
pub struct RecentReportsQuery {
    pub limit: Option<usize>,
}

/// Query parameters for debug register access.
#[derive(Debug, Deserialize)]
pub struct RegisterQuery {
    pub chain: u8,
    pub offset: String,
    pub count: Option<u8>,
}

/// Query parameters for `GET /api/chips` (Wave D RE-010 closure, 2026-05-19).
///
/// Optional `chain` filter to scope the per-chip snapshot to a single chain;
/// omit for all chains (default).
#[derive(Debug, Deserialize)]
pub struct ChipsQuery {
    pub chain: Option<u8>,
}

/// Query parameters for debug I2C access.
#[derive(Debug, Deserialize)]
pub struct I2cQuery {
    pub bus: u8,
    pub addr: String,
    pub reg: Option<String>,
}

/// Query parameters for debug log access.
#[derive(Debug, Deserialize)]
pub struct LogQuery {
    /// Number of lines to return (default 100, max 1000).
    pub lines: Option<usize>,
    /// Substring filter pattern (plain text, not regex).
    pub grep: Option<String>,
}

// ─── Request Body Types ────────────────────────────────────────────────

/// Request body for pool configuration.
#[derive(Debug, Deserialize)]
pub struct PoolRequest {
    pub url: String,
    pub worker: String,
    pub password: String,
    pub priority: Option<u8>,
    pub protocol: Option<String>,
    pub sv2_url: Option<String>,
    pub split_bps: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct HashrateSplitRequest {
    pub enabled: Option<bool>,
    pub secondary_pool_index: Option<usize>,
    pub secondary_pct: Option<u8>,
    pub cycle_duration_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PoolConfigRequest {
    Single(PoolRequest),
    Multiple {
        pools: Vec<PoolRequest>,
        hashrate_split: Option<HashrateSplitRequest>,
    },
}

impl PoolConfigRequest {
    fn into_parts(self) -> (Vec<PoolRequest>, Option<HashrateSplitRequest>) {
        match self {
            Self::Single(pool) => (vec![pool], None),
            Self::Multiple {
                pools,
                hashrate_split,
            } => (pools, hashrate_split),
        }
    }
}

#[derive(Debug, Serialize)]
struct ConfiguredPoolInfo {
    url: String,
    worker: String,
    priority: u8,
    protocol: Option<String>,
    sv2_url: Option<String>,
    split_bps: Option<u16>,
}

fn read_configured_pool() -> Option<ConfiguredPoolInfo> {
    read_configured_pools().into_iter().next()
}

fn configured_pool_from_table(pool: &toml::Table, priority: u8) -> Option<ConfiguredPoolInfo> {
    let url = pool.get("url")?.as_str()?.trim().to_string();
    if url.is_empty() {
        return None;
    }

    Some(ConfiguredPoolInfo {
        url,
        worker: pool
            .get("worker")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        priority,
        protocol: pool
            .get("protocol")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        sv2_url: pool
            .get("sv2_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        split_bps: pool
            .get("split_bps")
            .and_then(|v| v.as_integer())
            .and_then(|value| u16::try_from(value).ok()),
    })
}

fn read_configured_pools() -> Vec<ConfiguredPoolInfo> {
    let Ok(contents) = std::fs::read_to_string(get_config_path()) else {
        return Vec::new();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&contents) else {
        return Vec::new();
    };
    let Some(pool) = table.get("pool").and_then(|value| value.as_table()) else {
        return Vec::new();
    };

    let mut pools = Vec::new();
    if let Some(primary) = configured_pool_from_table(pool, 0) {
        pools.push(primary);
    }
    if let Some(failover1) = pool
        .get("failover1")
        .and_then(|value| value.as_table())
        .and_then(|value| configured_pool_from_table(value, 1))
    {
        pools.push(failover1);
    }
    if let Some(failover2) = pool
        .get("failover2")
        .and_then(|value| value.as_table())
        .and_then(|value| configured_pool_from_table(value, 2))
    {
        pools.push(failover2);
    }
    pools
}

fn normalize_pool_requests(
    mut pools: Vec<PoolRequest>,
) -> std::result::Result<Vec<PoolRequest>, String> {
    pools.retain(|pool| !pool.url.trim().is_empty());
    if pools.is_empty() {
        return Err("At least one pool URL is required".to_string());
    }
    if pools.len() > 3 {
        return Err("DCENT_OS supports up to 3 dashboard-configured pools".to_string());
    }

    pools.sort_by_key(|pool| pool.priority.unwrap_or(u8::MAX));
    for (priority, pool) in pools.iter_mut().enumerate() {
        pool.priority = Some(priority as u8);
    }
    Ok(pools)
}

fn set_optional_pool_string(pool_table: &mut toml::Table, key: &str, value: Option<&str>) {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => {
            pool_table.insert(key.into(), toml::Value::String(value.to_string()));
        }
        _ => {
            pool_table.remove(key);
        }
    }
}

fn set_optional_pool_integer(pool_table: &mut toml::Table, key: &str, value: Option<u16>) {
    match value {
        Some(value) => {
            pool_table.insert(key.into(), toml::Value::Integer(i64::from(value)));
        }
        None => {
            pool_table.remove(key);
        }
    }
}

fn write_pool_to_table(pool_table: &mut toml::Table, pool: &PoolRequest) {
    pool_table.insert(
        "url".into(),
        toml::Value::String(pool.url.trim().to_string()),
    );
    pool_table.insert("worker".into(), toml::Value::String(pool.worker.clone()));
    pool_table.insert(
        "password".into(),
        toml::Value::String(pool.password.clone()),
    );
    pool_table.remove("priority");
    set_optional_pool_string(pool_table, "protocol", pool.protocol.as_deref());
    set_optional_pool_string(pool_table, "sv2_url", pool.sv2_url.as_deref());
    set_optional_pool_integer(pool_table, "split_bps", pool.split_bps);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolAuditSlot {
    url: String,
    worker: String,
    password: String,
    protocol: Option<String>,
    sv2_url: Option<String>,
    split_bps: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PoolAuditSnapshot {
    pools: Vec<PoolAuditSlot>,
    routing_mode: Option<String>,
    split_cycle_duration_s: Option<u64>,
}

fn optional_trimmed_string(value: Option<&toml::Value>) -> Option<String> {
    value
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn pool_audit_slot_from_table(pool: &toml::Table) -> Option<PoolAuditSlot> {
    let url = optional_trimmed_string(pool.get("url"))?;
    Some(PoolAuditSlot {
        url,
        worker: pool
            .get("worker")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        password: pool
            .get("password")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        protocol: optional_trimmed_string(pool.get("protocol")),
        sv2_url: optional_trimmed_string(pool.get("sv2_url")),
        split_bps: pool
            .get("split_bps")
            .and_then(|value| value.as_integer())
            .and_then(|value| u16::try_from(value).ok()),
    })
}

fn read_pool_audit_snapshot() -> PoolAuditSnapshot {
    let Ok(contents) = std::fs::read_to_string(get_config_path()) else {
        return PoolAuditSnapshot::default();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&contents) else {
        return PoolAuditSnapshot::default();
    };
    let Some(pool) = table.get("pool").and_then(|value| value.as_table()) else {
        return PoolAuditSnapshot::default();
    };

    let mut pools = Vec::new();
    if let Some(primary) = pool_audit_slot_from_table(pool) {
        pools.push(primary);
    }
    for key in ["failover1", "failover2"] {
        if let Some(slot) = pool
            .get(key)
            .and_then(|value| value.as_table())
            .and_then(pool_audit_slot_from_table)
        {
            pools.push(slot);
        }
    }

    PoolAuditSnapshot {
        pools,
        routing_mode: optional_trimmed_string(pool.get("routing_mode")),
        split_cycle_duration_s: pool
            .get("split_cycle_duration_s")
            .and_then(|value| value.as_integer())
            .and_then(|value| u64::try_from(value).ok()),
    }
}

fn pool_audit_snapshot_from_request(
    pools: &[PoolRequest],
    hashrate_split: Option<&NormalizedHashrateSplit>,
) -> PoolAuditSnapshot {
    PoolAuditSnapshot {
        pools: pools
            .iter()
            .map(|pool| PoolAuditSlot {
                url: pool.url.trim().to_string(),
                worker: pool.worker.clone(),
                password: pool.password.clone(),
                protocol: pool
                    .protocol
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                sv2_url: pool
                    .sv2_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                split_bps: pool.split_bps,
            })
            .collect(),
        routing_mode: hashrate_split.map(|_| "weighted_split".to_string()),
        split_cycle_duration_s: hashrate_split.map(|split| split.cycle_duration_s),
    }
}

fn pool_slot_audit_prefix(index: usize) -> &'static str {
    match index {
        0 => "pool.primary",
        1 => "pool.failover1",
        2 => "pool.failover2",
        _ => "pool.extra",
    }
}

fn push_pool_field_change(changed_fields: &mut Vec<String>, prefix: &str, field: &str) {
    changed_fields.push(format!("{prefix}.{field}"));
}

fn pool_config_changed_fields(
    before: &PoolAuditSnapshot,
    after: &PoolAuditSnapshot,
) -> Vec<String> {
    let mut changed_fields = Vec::new();
    let max_len = before.pools.len().max(after.pools.len());

    for index in 0..max_len {
        let prefix = pool_slot_audit_prefix(index);
        match (before.pools.get(index), after.pools.get(index)) {
            (None, Some(_)) => push_pool_field_change(&mut changed_fields, prefix, "added"),
            (Some(_), None) => push_pool_field_change(&mut changed_fields, prefix, "removed"),
            (Some(old), Some(new)) => {
                if old.url != new.url {
                    push_pool_field_change(&mut changed_fields, prefix, "url");
                }
                if old.worker != new.worker {
                    push_pool_field_change(&mut changed_fields, prefix, "worker");
                }
                if old.password != new.password {
                    push_pool_field_change(&mut changed_fields, prefix, "password");
                }
                if old.protocol != new.protocol {
                    push_pool_field_change(&mut changed_fields, prefix, "protocol");
                }
                if old.sv2_url != new.sv2_url {
                    push_pool_field_change(&mut changed_fields, prefix, "sv2_url");
                }
                if old.split_bps != new.split_bps {
                    push_pool_field_change(&mut changed_fields, prefix, "split_bps");
                }
            }
            (None, None) => {}
        }
    }

    if before.routing_mode != after.routing_mode {
        changed_fields.push("pool.routing_mode".to_string());
    }
    if before.split_cycle_duration_s != after.split_cycle_duration_s {
        changed_fields.push("pool.split_cycle_duration_s".to_string());
    }

    changed_fields
}

fn pool_config_write_audit_event(
    pool_count: usize,
    changed_fields: Vec<String>,
) -> dcentrald_api_types::audit_log::AuditEvent {
    dcentrald_api_types::audit_log::AuditEvent::PoolConfigWrite {
        pool_count: u8::try_from(pool_count).unwrap_or(u8::MAX),
        changed_fields,
        secret_fields_redacted: vec!["pool.*.worker".to_string(), "pool.*.password".to_string()],
    }
}

fn parse_pool_host_port(url: &str) -> std::result::Result<(String, u16), String> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("stratum+tcp://")
        .or_else(|| trimmed.strip_prefix("stratum+tls://"))
        .or_else(|| trimmed.strip_prefix("stratum+ssl://"))
        .or_else(|| trimmed.strip_prefix("stratum2+tcp://"))
        .or_else(|| trimmed.strip_prefix("stratum2+ssl://"))
        .unwrap_or(trimmed);
    let (host, port_str) = without_scheme
        .rsplit_once(':')
        .ok_or_else(|| "Pool URL must include host:port".to_string())?;
    if host.is_empty() {
        return Err("Pool URL is missing a host".to_string());
    }
    let port = port_str
        .parse::<u16>()
        .map_err(|_| "Pool URL has an invalid port".to_string())?;
    Ok((host.to_string(), port))
}

fn validate_pool_url_support(url: &str) -> std::result::Result<(), String> {
    dcentrald_stratum::url_validator::validate_v1_pool_url(url)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn diagnostic_pool_dns_host(pool_url: &str) -> String {
    let sanitized = dcentrald_stratum::pool_api::sanitize_pool_url(pool_url);
    parse_pool_host_port(&sanitized)
        .map(|(host, _port)| host)
        .unwrap_or_else(|_| "public-pool.io".to_string())
}

struct NormalizedHashrateSplit {
    primary_bps: u16,
    secondary_bps: u16,
    cycle_duration_s: u64,
}

fn pool_request_is_v1_only(pool: &PoolRequest) -> bool {
    let protocol = pool
        .protocol
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let v1_protocol = protocol.is_empty() || protocol == "sv1" || protocol == "v1";
    v1_protocol
        && pool
            .sv2_url
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
}

fn normalize_hashrate_split_request(
    split: Option<HashrateSplitRequest>,
    pools: &[PoolRequest],
) -> std::result::Result<Option<NormalizedHashrateSplit>, String> {
    let Some(split) = split else {
        return Ok(None);
    };
    if !split.enabled.unwrap_or(false) {
        return Ok(None);
    }
    if pools.len() != 2 {
        return Err(
            "Hashrate splitting currently supports exactly two configured pools: pool1 and pool2"
                .to_string(),
        );
    }
    if split.secondary_pool_index.unwrap_or(1) != 1 {
        return Err("Hashrate splitting currently routes pool1/pool2 only".to_string());
    }

    let secondary_pct = split.secondary_pct.unwrap_or(20);
    if secondary_pct == 0 || secondary_pct >= 100 {
        return Err("Hashrate split secondary_pct must be between 1 and 99".to_string());
    }
    let secondary_bps = u16::from(secondary_pct) * 100;
    let primary_bps = 10_000u16.saturating_sub(secondary_bps);
    let cycle_duration_s = split.cycle_duration_s.unwrap_or(1800);
    if !(120..=86400).contains(&cycle_duration_s) {
        return Err("Hashrate split cycle_duration_s must be between 120 and 86400".to_string());
    }
    let min_window_s = (cycle_duration_s * u64::from(primary_bps.min(secondary_bps))) / 10_000;
    if min_window_s < 60 {
        return Err(format!(
            "Hashrate split route windows must be at least 60s; current smallest window is {}s",
            min_window_s
        ));
    }
    if !pool_request_is_v1_only(&pools[0]) || !pool_request_is_v1_only(&pools[1]) {
        return Err(
            "Hashrate splitting is V1-only in this build; remove SV2 URLs and use Stratum V1 pools"
                .to_string(),
        );
    }

    Ok(Some(NormalizedHashrateSplit {
        primary_bps,
        secondary_bps,
        cycle_duration_s,
    }))
}

fn redact_worker(worker: &str) -> String {
    let trimmed = worker.trim();
    if trimmed.len() <= 18 {
        return trimmed.to_string();
    }
    format!("{}...{}", &trimmed[..10], &trimmed[trimmed.len() - 6..])
}

/// DEVOPS-009: placeholder substituted for any secret-bearing value when a
/// daemon config is exported / bundled. Matches the support-bundle redactor's
/// `<redacted>` token (see `routes::stock_parity`) so the two surfaces look
/// identical in a support ticket.
pub(crate) const SECRET_REDACTION_PLACEHOLDER: &str = "<redacted>";

/// DEVOPS-009: returns `true` when a TOML key name looks like it carries a
/// secret. Key-name match (case-insensitive) against the canonical
/// `CONFIG_BACKUP_SECRET_KEY_PATTERNS` list — the SAME list the config-backup
/// manifest advertises and the support-bundle redactor uses, so the policy is
/// declared once. A pattern matches if the (last path segment of the) key
/// equals it OR contains it as a substring (so `wifi_password`, `api_token`,
/// `private_key` all redact).
pub(crate) fn key_is_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    // Compare both the full dotted path and its trailing segment so dotted
    // patterns ("pool.password", "webhook.url") and bare patterns ("password")
    // both hit. The trailing segment carries substring matches like
    // `wifi_password` / `api_token` / `private_key`.
    let last = lower.rsplit('.').next().unwrap_or(lower.as_str());
    CONFIG_BACKUP_SECRET_KEY_PATTERNS.iter().any(|pat| {
        let p = *pat;
        // Dotted patterns ("webhook.url") match the full path exactly; bare
        // patterns ("password") match the trailing segment by substring.
        if p.contains('.') {
            lower == p
        } else {
            last == p || last.contains(p)
        }
    })
}

fn redact_secret_kv_in_log_line(line: &str) -> String {
    if let Some(eq) = line.find('=') {
        let key = line[..eq].trim();
        if !key.is_empty() && key_is_secret(key) {
            let leading: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            return format!("{leading}{key}={SECRET_REDACTION_PLACEHOLDER}");
        }
    }

    if let Some(colon) = line.find(':') {
        let before = &line[..colon];
        if let Some(start) = before.find('"') {
            if let Some(end) = before[start + 1..].find('"') {
                let key = &before[start + 1..start + 1 + end];
                if !key.is_empty() && key_is_secret(key) {
                    return format!(
                        "{} \"{}\"",
                        &line[..colon + 1],
                        SECRET_REDACTION_PLACEHOLDER
                    );
                }
            }
        }
    }

    line.to_string()
}

fn scrub_debug_log_line(line: &str) -> String {
    let masked = dcentrald_common::wallet_mask::mask_in_string(line);
    redact_secret_kv_in_log_line(masked.as_ref())
}

/// DEVOPS-009: redact a single password-like value. Empty values stay empty
/// (an unset password must not look "set" after redaction); any non-empty
/// value becomes the placeholder. Pure helper so it's trivially host-testable.
pub(crate) fn redact_password(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        SECRET_REDACTION_PLACEHOLDER.to_string()
    }
}

/// DEVOPS-009: recursively redact every secret-bearing value in a TOML table
/// IN PLACE.
///
/// This is the single chokepoint a future daemon-config export / support
/// bundle MUST run before serializing config to disk or to a download. It is
/// strictly *safer*: it can only mask values, never reveal them, so it is
/// promoted to a real default (no behaviour gate). It walks nested tables and
/// arrays-of-tables so `[pool]`, `[[pools]]`, `[mqtt]`, `[donation]`,
/// `[api]`, `[webhook]` secrets are all caught regardless of nesting depth.
///
/// String secrets become [`SECRET_REDACTION_PLACEHOLDER`]; non-string secrets
/// (rare — a numeric "key") are removed entirely so no value leaks. Empty
/// strings are left empty so an unset secret doesn't masquerade as set.
pub(crate) fn redact_secrets_in_toml_table(table: &mut toml::Table) {
    redact_secrets_in_toml_table_at(table, "");
}

/// Path-aware recursion for [`redact_secrets_in_toml_table`]. `prefix` is the
/// dotted path of the parent table ("" at the root, "webhook" inside
/// `[webhook]`) so dotted secret patterns like `webhook.url` match precisely
/// while bare patterns still match the local key.
fn redact_secrets_in_toml_table_at(table: &mut toml::Table, prefix: &str) {
    // Collect keys first to avoid borrow conflicts while mutating.
    let keys: Vec<String> = table.keys().cloned().collect();
    // W3 E-01: non-string secret-keyed values are REMOVED, not replaced with a
    // String placeholder (which would type-mangle a re-parsed config). The arm
    // below holds a `get_mut` borrow, so collect and remove after the loop.
    let mut secret_nonstring_keys: Vec<String> = Vec::new();
    for key in keys {
        let dotted = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        // A value is secret if its local key OR its full dotted path matches.
        let secret = key_is_secret(&key) || key_is_secret(&dotted);
        match table.get_mut(&key) {
            Some(toml::Value::String(s)) if secret => {
                *s = redact_password(s);
            }
            Some(_value) if secret => {
                // Non-string secret-keyed value (e.g. an integer token): remove
                // it entirely so nothing leaks AND the value type is not mangled
                // (replacing with a String placeholder broke config round-trip).
                // A `get_mut` borrow is held here, so defer the removal.
                secret_nonstring_keys.push(key.clone());
            }
            Some(toml::Value::Table(inner)) => {
                redact_secrets_in_toml_table_at(inner, &dotted);
            }
            Some(toml::Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let toml::Value::Table(inner) = item {
                        redact_secrets_in_toml_table_at(inner, &dotted);
                    }
                }
            }
            _ => {}
        }
    }
    // W3 E-01: drop the non-string secret-keyed values collected above so no
    // value leaks and no type is mangled.
    for key in &secret_nonstring_keys {
        table.remove(key);
    }
}

fn donation_failover_contract(miner: &crate::MinerState) -> serde_json::Value {
    let table = std::fs::read_to_string(get_config_path())
        .ok()
        .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok());
    let donation = table
        .as_ref()
        .and_then(|table| table.get("donation"))
        .and_then(|value| value.as_table());

    let enabled = donation
        .and_then(|table| table.get("enabled"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let percent = donation
        .and_then(|table| table.get("percent"))
        .and_then(|value| value.as_float())
        .unwrap_or(2.0);
    let cycle_duration_s = donation
        .and_then(|table| table.get("cycle_duration_s"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(3600);
    let pool_url = donation
        .and_then(|table| table.get("pool_url"))
        .and_then(|value| value.as_str())
        .unwrap_or("stratum+tcp://pool.d-central.tech:3333");
    let fallback_enabled = donation
        .and_then(|table| table.get("fallback_enabled"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let fallback_pool_url = donation
        .and_then(|table| table.get("fallback_pool_url"))
        .and_then(|value| value.as_str())
        .unwrap_or("stratum+tcp://stratum.braiins.com:3333");
    let fallback_worker = donation
        .and_then(|table| table.get("fallback_worker"))
        .and_then(|value| value.as_str())
        .unwrap_or("DungeonMaster");
    let donation_host = parse_pool_host_port(pool_url)
        .map(|(host, _)| host)
        .unwrap_or_else(|_| "unavailable".to_string());
    let fallback_host = parse_pool_host_port(fallback_pool_url)
        .map(|(host, _)| host)
        .unwrap_or_else(|_| "unavailable".to_string());

    serde_json::json!({
        "enabled": enabled,
        "active": miner.pool.donating,
        "percent": percent,
        "cycle_duration_s": cycle_duration_s,
        "cycle_remaining_s": serde_json::Value::Null,
        "pool_visible": true,
        "pool_host": donation_host,
        "fallback_enabled": fallback_enabled,
        "fallback_pool_host": fallback_host,
        "fallback_worker_redacted": redact_worker(fallback_worker),
        "fallback_policy": "primary_donation_pool_then_visible_braiins_account",
        "disable_supported": true,
        "excluded_from_user_failover": true,
        "telemetry_source": "config_plus_miner_state_pool",
    })
}

/// RE-013: compact, open-source-transparency donation block for `/api/status`.
///
/// Surfaces BOTH the primary donation pool AND the visible Braiins-worker
/// fallback (`fallback_worker`, default `DungeonMaster`) so the
/// devfee-transparency contract is honest at the top-level status surface, not
/// just `/api/pools.failover` / `/api/donation/info`. Pure (takes the parsed
/// donation sub-table) so it's host-testable. Worker names are redacted for
/// privacy via `redact_worker`. `active` reflects whether the donation slice
/// is currently routing (from live `MinerState`).
fn donation_transparency_from_table(
    donation: Option<&toml::Table>,
    active: bool,
) -> serde_json::Value {
    let enabled = donation
        .and_then(|t| t.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let percent = donation
        .and_then(|t| t.get("percent"))
        .and_then(|v| v.as_float())
        .unwrap_or(2.0);
    let pool_url = donation
        .and_then(|t| t.get("pool_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("stratum+tcp://pool.d-central.tech:3333");
    let worker = donation
        .and_then(|t| t.get("worker"))
        .and_then(|v| v.as_str())
        .unwrap_or("DungeonMaster");
    let fallback_enabled = donation
        .and_then(|t| t.get("fallback_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let fallback_pool_url = donation
        .and_then(|t| t.get("fallback_pool_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("stratum+tcp://stratum.braiins.com:3333");
    let fallback_worker = donation
        .and_then(|t| t.get("fallback_worker"))
        .and_then(|v| v.as_str())
        .unwrap_or("DungeonMaster");

    serde_json::json!({
        "enabled": enabled,
        "active": active,
        "percent": percent,
        "disable_supported": true,
        "is_devfee": false,
        "label": "donation",
        // Primary donation route.
        "pool_url": pool_url,
        "worker_redacted": redact_worker(worker),
        // RE-013: the visible Braiins-worker fallback, surfaced for
        // transparency (no hidden devfee route).
        "fallback_enabled": fallback_enabled,
        "fallback_pool_url": fallback_pool_url,
        "fallback_worker_redacted": redact_worker(fallback_worker),
        "fallback_policy": "primary_donation_pool_then_visible_braiins_account",
        "note": "Voluntary, transparent, fully disableable. The fallback routes the donation slice to a visible Braiins Pool worker only when the primary donation endpoint is unavailable; it never extends the configured percentage and is excluded from user-pool failover.",
    })
}

fn build_hashrate_split_contract(
    miner: &crate::MinerState,
    configured_pools: &[ConfiguredPoolInfo],
) -> serde_json::Value {
    let table = std::fs::read_to_string(get_config_path())
        .ok()
        .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok());
    let pool = table
        .as_ref()
        .and_then(|table| table.get("pool"))
        .and_then(|value| value.as_table());
    let donation = table
        .as_ref()
        .and_then(|table| table.get("donation"))
        .and_then(|value| value.as_table());

    let routing_mode = pool
        .and_then(|table| table.get("routing_mode"))
        .and_then(|value| value.as_str())
        .unwrap_or("failover");
    let enabled = routing_mode == "weighted_split" && configured_pools.len() >= 2;
    let primary_bps = pool
        .and_then(|table| table.get("split_bps"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| configured_pools.first().and_then(|pool| pool.split_bps))
        .unwrap_or(8000);
    let secondary_bps = pool
        .and_then(|table| table.get("failover1"))
        .and_then(|value| value.as_table())
        .and_then(|table| table.get("split_bps"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| configured_pools.get(1).and_then(|pool| pool.split_bps))
        .unwrap_or_else(|| 10_000u16.saturating_sub(primary_bps));
    let cycle_duration_s = pool
        .and_then(|table| table.get("split_cycle_duration_s"))
        .and_then(|value| value.as_integer())
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(1800);

    let donation_enabled = donation
        .and_then(|table| table.get("enabled"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let donation_pct = donation
        .and_then(|table| table.get("percent"))
        .and_then(|value| value.as_float())
        .unwrap_or(2.0);
    let user_allocation_pct = if donation_enabled {
        (100.0 - donation_pct).clamp(0.0, 100.0)
    } else {
        100.0
    };
    let primary_pct = f64::from(primary_bps) / 100.0;
    let secondary_pct = f64::from(secondary_bps) / 100.0;
    let runtime = &miner.pool.hashrate_split;

    serde_json::json!({
        "schema": "dcentos.hashrate_split.v1",
        "enabled": enabled,
        "runtime_active": runtime.active,
        "routing_mode": routing_mode,
        "algorithm": "weighted_time_slice_v1",
        "v1_only": true,
        "simultaneous_clients": false,
        "primary_pool_index": 0,
        "secondary_pool_index": 1,
        "active_route": runtime.active_route,
        "active_pool_index": runtime.active_pool_index,
        "active_pool_priority": runtime.active_pool_priority,
        "primary_bps": primary_bps,
        "secondary_bps": secondary_bps,
        "primary_pct": primary_pct,
        "secondary_pct": secondary_pct,
        "cycle_duration_s": cycle_duration_s,
        "cycle_remaining_s": runtime.cycle_remaining_s,
        "switch_count": runtime.switch_count,
        "secondary_shares": runtime.secondary_shares,
        "donation_composed": donation_enabled,
        "donation_pct": if donation_enabled { serde_json::json!(donation_pct) } else { serde_json::Value::Null },
        "configured_effective_primary_pct": primary_pct * user_allocation_pct / 100.0,
        "configured_effective_secondary_pct": secondary_pct * user_allocation_pct / 100.0,
        "requires_restart_or_reconnect": true,
        "hardware_writes": false,
        "dispatcher_flush_on_switch": true,
        "source_basis": ["local_config", "stratum_v1_runtime_state"],
        "telemetry_source": runtime.telemetry_source,
    })
}

fn build_pool_failover_contract(
    miner: &crate::MinerState,
    configured_pools: &[ConfiguredPoolInfo],
) -> serde_json::Value {
    let runtime = &miner.pool.failover;
    let configured_count = configured_pools.len();
    let split_active_index = if miner.pool.hashrate_split.enabled
        && miner.pool.hashrate_split.active
        && miner.pool.hashrate_split.active_pool_index < configured_count
    {
        Some(miner.pool.hashrate_split.active_pool_index)
    } else {
        None
    };
    let active_index = split_active_index.or_else(|| {
        if runtime.configured_pool_count > 0 && runtime.active_pool_index < configured_count {
            Some(runtime.active_pool_index)
        } else {
            configured_pools
                .iter()
                .position(|pool| pool.url == miner.pool.url)
        }
    });
    let active_index_value = active_index.unwrap_or(runtime.active_pool_index);
    let active_pool = active_index.and_then(|index| configured_pools.get(index));
    let active_url = active_pool
        .map(|pool| pool.url.clone())
        .or_else(|| (!runtime.active_pool_url.is_empty()).then(|| runtime.active_pool_url.clone()))
        .unwrap_or_else(|| miner.pool.url.clone());
    // TEL-002: sanitize before the URL is both parsed for the host AND emitted
    // raw in the pool-failover contract body — strips any inline `user:pass@`
    // credentials a Stratum V1 URL might carry, matching every other pool-URL
    // surface (the analogous shared_primary_pool path already sanitizes).
    let active_url = dcentrald_stratum::pool_api::sanitize_pool_url(&active_url);
    let active_host = parse_pool_host_port(&active_url)
        .map(|(host, _)| host)
        .unwrap_or_else(|_| "unavailable".to_string());
    let role = if miner.pool.donating {
        "donation"
    } else if configured_count == 0 || miner.pool.status.eq_ignore_ascii_case("disabled") {
        "disabled"
    } else if miner.pool.hashrate_split.enabled
        && miner.pool.hashrate_split.active
        && active_index_value == 1
    {
        "user_split_secondary"
    } else if miner.pool.hashrate_split.enabled && miner.pool.hashrate_split.active {
        "user_split_primary"
    } else if active_index_value == 0 {
        "user_primary"
    } else if active_index_value < configured_count {
        "user_failover"
    } else if miner.pool.status.eq_ignore_ascii_case("disconnected")
        || miner.pool.status.eq_ignore_ascii_case("connecting")
    {
        "reconnecting"
    } else {
        "unknown"
    };

    let pools = configured_pools
        .iter()
        .enumerate()
        .map(|(index, pool)| {
            serde_json::json!({
                "index": index,
                "priority": pool.priority,
                "url": dcentrald_stratum::pool_api::sanitize_pool_url(&pool.url),
                "worker_redacted": redact_worker(&pool.worker),
                "configured": true,
                "active": active_index == Some(index),
                "status": if active_index == Some(index) { miner.pool.status.clone() } else { "configured".to_string() },
                "protocol": pool.protocol.clone().unwrap_or_else(|| "sv1".to_string()),
                "telemetry_source": if active_index == Some(index) { "runtime_state" } else { "local_config" },
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "schema": "dcentos.pool_failover.v1",
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "external_calls": false,
        "license_required": false,
        "license_server_required": false,
        "activation_required": false,
        "mandatory_fee": false,
        "fee_route": if miner.pool.donating { "transparent_donation" } else { "none" },
        "local_first": true,
        "secrets_included": false,
        "redacted_fields": ["password", "token", "authorization"],
        "configured_pool_count": configured_count,
        "active_pool_index": active_index_value,
        "active_pool_priority": active_index_value + 1,
        "active_pool_url": active_url,
        "active_pool_host": active_host,
        "active_worker_redacted": active_pool.map(|pool| redact_worker(&pool.worker)).unwrap_or_default(),
        "active_route_kind": if miner.pool.donating { "donation" } else if miner.pool.hashrate_split.enabled && miner.pool.hashrate_split.active { "user_split" } else { "user" },
        "current_pool_role": role,
        "pools": pools,
        "switch_count": runtime.switch_count,
        "last_switch_reason": runtime.last_switch_reason.clone(),
        "last_failure_reason": runtime.last_failure_reason.clone(),
        "last_failure_pool_index": runtime.last_failure_pool_index,
        "consecutive_failures": runtime.consecutive_failures,
        "backoff_ms": runtime.backoff_ms,
        "return_to_primary_policy": "anti_flap_cooldown_window",
        "primary_stable_since_ms": serde_json::Value::Null,
        "return_blocked_reason": serde_json::Value::Null,
        "stale_jobs_flushed_on_switch": runtime.stale_jobs_flushed_on_switch,
        "last_flush_at_ms": serde_json::Value::Null,
        "flush_event_id": if runtime.stale_jobs_flushed_on_switch { runtime.event.clone() } else { "unavailable".to_string() },
        "pending_submit_correlations": serde_json::Value::Null,
        "pending_submit_correlations_cleared": runtime.pending_submit_correlations_cleared,
        "oldest_pending_submit_age_ms": serde_json::Value::Null,
        "pending_share_preserved": runtime.pending_share_preserved,
        "shares_unresolved": runtime.shares_unresolved,
        "pending_submit_dropped": runtime.pending_submit_dropped,
        "shares_dropped_while_disconnected": serde_json::Value::Null,
        "unresolved_submit_count": runtime.shares_unresolved,
        "donation": donation_failover_contract(miner),
        "hashrate_split": build_hashrate_split_contract(miner, configured_pools),
        "source_basis": ["clean_room", "stratum_v1_runtime_state", "local_config"],
        "telemetry_source": runtime.telemetry_source,
        "last_update_ms": unix_time_ms(),
        "stale_after_ms": 60_000,
        "stale": false,
        "repair_diagnostic": "read_only_default",
        "docs_link": "https://github.com/DCentralTech/DCENT_OS",
        "recovery_link": "https://github.com/DCentralTech/DCENT_OS",
        "limitations": [
            "Read-only failover contract; it does not connect to pools, test pools, switch pools, write config, or touch hardware.",
            "No-notify timeout failover, reject-rate failover, and stable primary return are not implemented yet.",
            "Pending submit ages and disconnected-share drop counters are not yet fully published by the Stratum task."
        ]
    })
}

/// Request body for home power target.
#[derive(Debug, Deserialize)]
pub struct HomeTargetRequest {
    pub preset: Option<String>,
    pub watts: Option<u32>,
}

/// Request body for room temperature.
#[derive(Debug, Deserialize)]
pub struct RoomTempRequest {
    pub temp_c: f32,
}

/// Request body for night mode configuration.
#[derive(Debug, Deserialize)]
pub struct NightModeRequest {
    pub enabled: bool,
    pub start_hour: Option<u8>,
    pub end_hour: Option<u8>,
    pub max_fan_pwm: Option<u8>,
    pub power_reduction_pct: Option<u8>,
}

/// Request body for debug register write.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterWriteRequest {
    pub chain: u8,
    pub offset: String,
    pub value: String,
    pub confirm: Option<bool>,
}

/// Request body for debug I2C write.
#[derive(Debug, Serialize, Deserialize)]
pub struct I2cWriteRequest {
    pub bus: u8,
    pub addr: String,
    pub data: Vec<u8>,
    pub confirm: Option<bool>,
}

/// Request body for debug ASIC command.
#[derive(Debug, Serialize, Deserialize)]
pub struct AsicCommandRequest {
    pub chain: u8,
    pub command: String,
    pub chip: Option<u8>,
    pub register: Option<String>,
    pub confirm: Option<bool>,
}

/// Request body for PID parameter tuning.
#[derive(Debug, Serialize, Deserialize)]
pub struct PidParamsRequest {
    pub kp: Option<f64>,
    pub ki: Option<f64>,
    pub kd: Option<f64>,
    pub setpoint: Option<f64>,
    pub confirm: Option<bool>,
}

/// Request body for per-chip frequency setting.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChipFrequencyRequest {
    pub chain: u8,
    pub chip: u8,
    pub freq_mhz: u16,
    pub confirm: Option<bool>,
}

/// Request body for per-chain voltage setting.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChipVoltageRequest {
    pub chain: u8,
    pub pic_value: u8,
    pub confirm: Option<bool>,
}

/// Request body for smart PSU control.
#[derive(Debug, Serialize, Deserialize)]
pub struct PsuControlRequest {
    pub action: String,
    pub voltage_v: Option<f64>,
    pub confirm: Option<bool>,
}

/// Request body for diagnostic test start.
#[derive(Debug, Deserialize)]
pub struct DiagStartRequest {
    pub chain: Option<u8>,
    pub duration_minutes: Option<u8>,
}

/// Request body for diagnostic cancellation.
#[derive(Debug, Deserialize)]
pub struct DiagCancelRequest {
    pub test_id: String,
}

// ─── Core Status Handlers ──────────────────────────────────────────────

const DEFAULT_THERMAL_TARGET_TEMP_C: u8 = 55;
const DEFAULT_THERMAL_HOT_TEMP_C: u8 = 65;
const DEFAULT_THERMAL_DANGEROUS_TEMP_C: u8 = 75;
const DEFAULT_THERMAL_FAN_MIN_PWM: u8 = 0;
const DEFAULT_THERMAL_FAN_MAX_PWM: u8 = 30;
const DEFAULT_THERMAL_HYSTERESIS_C: u8 = 3;

#[derive(Clone, Copy)]
struct ThermalPostureThresholds {
    target_temp_c: u8,
    hot_temp_c: u8,
    dangerous_temp_c: u8,
    fan_min_pwm: u8,
    fan_max_pwm: u8,
    hysteresis_c: u8,
}

fn thermal_config_u8(table: &toml::Table, key: &str, default: u8) -> u8 {
    table
        .get("thermal")
        .and_then(|value| value.as_table())
        .and_then(|thermal| thermal.get(key))
        .and_then(|value| value.as_integer())
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(default)
}

fn read_thermal_posture_thresholds() -> (ThermalPostureThresholds, &'static str, String) {
    match load_config_table_for_write() {
        Ok(table) => (
            ThermalPostureThresholds {
                target_temp_c: thermal_config_u8(
                    &table,
                    "target_temp_c",
                    DEFAULT_THERMAL_TARGET_TEMP_C,
                ),
                hot_temp_c: thermal_config_u8(&table, "hot_temp_c", DEFAULT_THERMAL_HOT_TEMP_C),
                dangerous_temp_c: thermal_config_u8(
                    &table,
                    "dangerous_temp_c",
                    DEFAULT_THERMAL_DANGEROUS_TEMP_C,
                ),
                fan_min_pwm: thermal_config_u8(&table, "fan_min_pwm", DEFAULT_THERMAL_FAN_MIN_PWM),
                fan_max_pwm: thermal_config_u8(&table, "fan_max_pwm", DEFAULT_THERMAL_FAN_MAX_PWM),
                hysteresis_c: thermal_config_u8(
                    &table,
                    "hysteresis_c",
                    DEFAULT_THERMAL_HYSTERESIS_C,
                ),
            },
            "active_config_or_dcentos_defaults",
            "Read from active TOML when available; missing fields use DCENT_OS daemon defaults."
                .to_string(),
        ),
        Err(err) => (
            ThermalPostureThresholds {
                target_temp_c: DEFAULT_THERMAL_TARGET_TEMP_C,
                hot_temp_c: DEFAULT_THERMAL_HOT_TEMP_C,
                dangerous_temp_c: DEFAULT_THERMAL_DANGEROUS_TEMP_C,
                fan_min_pwm: DEFAULT_THERMAL_FAN_MIN_PWM,
                fan_max_pwm: DEFAULT_THERMAL_FAN_MAX_PWM,
                hysteresis_c: DEFAULT_THERMAL_HYSTERESIS_C,
            },
            "dcentos_defaults_config_unreadable",
            format!(
                "Active TOML could not be read or parsed; using DCENT_OS defaults for display only: {}",
                err
            ),
        ),
    }
}

fn classify_thermal_posture(
    max_temp_c: Option<f32>,
    thermal_related_limit: bool,
    fan_tach_suspect: bool,
    thresholds: ThermalPostureThresholds,
) -> (&'static str, &'static str) {
    if fan_tach_suspect {
        return (
            "sensor_limited",
            "Fan PWM is active while tachometer RPM is zero or unavailable.",
        );
    }

    match max_temp_c {
        None => (
            "unknown",
            "No non-zero chain temperature telemetry is available.",
        ),
        Some(temp) if temp >= thresholds.dangerous_temp_c as f32 => (
            "critical",
            "Observed chain temperature is at or above the configured dangerous threshold.",
        ),
        Some(temp) if temp >= thresholds.hot_temp_c as f32 => (
            "hot",
            "Observed chain temperature is at or above the configured hot threshold.",
        ),
        Some(_) if thermal_related_limit => (
            "limited",
            "Dispatcher reports a thermal, sensor, or fan-clamp runtime limit.",
        ),
        Some(temp) if temp >= thresholds.target_temp_c as f32 => (
            "watch",
            "Observed chain temperature is at or above target but below hot.",
        ),
        Some(_) => (
            "ok",
            "Observed chain temperature is below the configured target threshold.",
        ),
    }
}

/// GET /api/status -- Overall miner status.
///
/// Polled by dashboard every 5 seconds. Returns hashrate, temperatures,
/// fan speed, pool status, uptime, chain data.
async fn get_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let mode = *state.mode_rx.borrow();
    let power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let power_projection = (&power, &miner, &hardware);
    let targeting = build_power_targeting_state(mode, &power_projection);

    // P0-2 (C-2 / D-1 / D-2): project per-chain hashrate + voltage with explicit
    // provenance. The raw daemon `ChainState` carries a per-chain hashrate that
    // can lag the live topline (every chain reads 0.0 under a live ~1.1 TH/s
    // aggregate on a freshly-warmed S9 — the `.100` audit) and a `voltage_mv`
    // that is a COMMANDED DAC value, never a measured rail (S9 has no per-chain
    // voltage ADC). See [].
    let chip_id = chip_type_to_chip_id(&hardware.chip_type).unwrap_or(0x1387);
    let default_voltage_mv = dcentrald_asic::drivers::MinerProfile::for_chip(chip_id)
        .map(|profile| profile.default_voltage_mv)
        .unwrap_or(0);
    // AT-3: fed by the live measured-rail slot (empty/no-op until the operator
    // opts the am2 hybrid path into the quiet-window 0x3A read; then a fresh,
    // plausible reading tags that chain's voltage_source `measured`).
    let chain_telemetry =
        (&miner.chains, miner.hashrate_ghs, default_voltage_mv);

    let chains: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .zip(chain_telemetry.iter())
        .map(|(c, proj)| {
            serde_json::json!({
                "id": c.id,
                "chips": c.chips,
                "frequency_mhz": proj.frequency_mhz,
                "frequency_source": proj.frequency_source,
                // D-2: COMMANDED DAC value (S9 has no per-chain ADC). Tagged via
                // `voltage_source` so the dashboard never presents a commanded
                // value as a measured rail reading, and never a bare 0.
                "voltage_mv": proj.voltage_mv,
                "voltage_source": proj.voltage_source,
                "temp_c": c.temp_c,
                // BUG-11: provenance of temp_c so the dashboard can honestly
                // label a die-temp fallback (S9 board sensors silent) vs a real
                // board sensor reading, and never mistake a fallback for an
                // unpowered board.
                "temp_source": c.temp_source,
                // D-1: real per-chain hashrate when published, else the live
                // topline split across responding chains. `hashrate_source`
                // tags which, so a live topline never shows bare per-chain 0s.
                "hashrate_ghs": proj.hashrate_ghs,
                "hashrate_source": proj.hashrate_source,
                "errors": c.errors,
                "status": c.status,
            })
        })
        .collect();

    // W6.3 / W6.4 / P1-3 (D-9): nominal vs effective TH/s.
    //
    // `nominal` is the RATED nameplate capacity — the real installed silicon
    // (live chip counts) at the chip's rated clock, from the chip profile. It was
    // previously set to the live MEASURED hashrate, which made "% of rated"
    // (= effective / nominal) collapse to the acceptance percentage and never
    // reflect rated capacity. `effective` is the share-paid hashrate — what the
    // ASIC measured, scaled by rolling pool acceptance — so it must keep using
    // the MEASURED value, not the rated one. Both are computed at read time so
    // they track `miner.hashrate_ghs` / the chain population automatically.
    let measured_ths = miner.hashrate_ghs / 1000.0;
    let live_chips: u64 = miner.chains.iter().map(|c| c.chips as u64).sum();
    let nominal_ths = dcentrald_asic::drivers::MinerProfile::for_chip(chip_id)
        .map(|profile| rated_nominal_ths(profile, live_chips))
        // Unknown chip: no rated capacity to report — fall back to the measured
        // value so the field stays a real number rather than a fabricated 0.
        .unwrap_or(measured_ths);
    let effective_hashrate_ths = measured_ths * (miner.pool.rolling_acceptance_pct_30min / 100.0);

    // RE-013: surface the donation config (including the visible Braiins-worker
    // fallback) on the top-level status surface for devfee transparency.
    let donation_block = {
        // P3-2: post-write-fresh in-memory config cache. Wrapped in `Some` so
        // the absent-file path still yields no donation block (empty table ⇒
        // `get("donation")` is None), matching the prior read_to_string().ok().
        let table = Some(state.config_cache.snapshot());
        let donation = table
            .as_ref()
            .and_then(|t| t.get("donation"))
            .and_then(|v| v.as_table());
        donation_transparency_from_table(donation, miner.pool.donating)
    };

    Json(serde_json::json!({
        "hashrate_ghs": miner.hashrate_ghs,
        "hashrate_5s_ghs": miner.hashrate_5s_ghs,
        // W6.3 dashboard surface — nominal vs effective TH/s.
        "nominal_hashrate_ths": nominal_ths,
        "effective_hashrate_ths": effective_hashrate_ths,
        "acceptance_pct_30min": miner.pool.rolling_acceptance_pct_30min,
        "acceptance_count_30min": miner.pool.rolling_acceptance_count_30min,
        "acceptance_source_30min": miner.pool.rolling_acceptance_source,
        // TEL-3: RESERVED / always-`null`. No state publisher wires this field
        // (every `PoolState` is constructed with `None` and
        // `apply_quality_snapshot` leaves it untouched). The live worst-chip
        // HW-error data flows only on the separate autotuner chip-health channel
        // (`GET /api/autotuner/chip-health` + the `autotuner_chip_health` WS
        // message). Emitted as a stable `null` placeholder for API shape — do
        // NOT read as live telemetry. See `PoolState::worst_chip_hw_err_rate`.
        "worst_chip_hw_err_rate": miner.pool.worst_chip_hw_err_rate,
        "accepted": miner.accepted,
        "rejected": miner.rejected,
        "uptime_s": miner.uptime_s,
        "firmware_version": miner.firmware_version,
        "mode": mode,
        "chains": chains,
        "fans": {
            "pwm": miner.fans.pwm,
            "rpm": miner.fans.rpm,
            "per_fan": miner.fans.per_fan.iter().map(|f| serde_json::json!({
                "id": f.id, "rpm": f.rpm, "pwm_percent": f.pwm_percent,
            })).collect::<Vec<_>>(),
        },
        "pool": {
            // SEC (W20 / parity #66): stratum URLs can embed inline credentials
            // (stratum+tcp://worker:pass@host). Strip them for the status
            // display surface (the daemon reconnects via the real config URL,
            // never this display copy). Mirrors get_pools, which already sanitizes.
            "url": dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
            "status": miner.pool.status,
            "difficulty": miner.pool.difficulty,
            "pool_target_difficulty": miner.pool.difficulty,
            "last_share_s": secs_since(miner.pool.last_share_at),
            "share_efficiency": miner.pool.share_efficiency,
            "protocol": miner.pool.protocol,
            "encrypted": miner.pool.encrypted,
            "encrypted_source": miner.pool.encrypted_source,
            "sv2_session": miner.pool.sv2_session,
            "sv2_session_source": miner.pool.sv2_session_source,
            "donating": miner.pool.donating,
            "donating_source": miner.pool.donating_source,
            "donation_active_url": dcentrald_stratum::pool_api::sanitize_pool_url(
                &miner.pool.donation_active_url
            ),
            "donation_active_worker": dcentrald_common::wallet_mask::mask_wallet(
                &miner.pool.donation_active_worker
            ),
            "donation_pool_index": miner.pool.donation_pool_index,
            "auto_fallback_active": miner.pool.auto_fallback_active,
            "auto_fallback_source": miner.pool.auto_fallback_source,
            "auto_retry_sv2_after_s": miner.pool.auto_retry_sv2_after_s,
            "auto_fallback_reason": miner.pool.auto_fallback_reason,
            "failover": miner.pool.failover.clone(),
            "failover_source": miner.pool.failover_source,
            "hashrate_split": miner.pool.hashrate_split.clone(),
            "hashrate_split_source": miner.pool.hashrate_split_source,
            "latency_ms": miner.pool.latency_ms,
            "latency_ms_source": miner.pool.latency_ms_source,
            "reject_reason_counts": miner.pool.reject_reason_counts,
            "reject_reason_counts_source": miner.pool.reject_reason_counts_source,
            // W6.3 — rolling acceptance fields mirrored under `pool` for
            // dashboards that organize stats per-pool.
            "rolling_acceptance_pct_30min": miner.pool.rolling_acceptance_pct_30min,
            "rolling_acceptance_count_30min": miner.pool.rolling_acceptance_count_30min,
            "rolling_acceptance_source": miner.pool.rolling_acceptance_source,
            // TEL-3: SAME reserved always-`null` field as the top-level copy —
            // not live telemetry (no publisher wires it). See above.
            "worst_chip_hw_err_rate": miner.pool.worst_chip_hw_err_rate,
        },
        "share_efficiency": miner.pool.share_efficiency,
        "power": build_status_power_section(&power_projection, &power, targeting),
        // RE-013: transparent donation surface (primary + visible Braiins
        // fallback worker). Open-source devfee transparency.
        "donation": donation_block,
    }))
}

/// GET /api/fleet/miners -- Minimal local fleet inventory.
///
/// Current DCENT_OS only has a local single-miner state publisher. This endpoint
/// exposes that local state using the fleet-dashboard contract and avoids LAN
/// probing or daemon-to-daemon dependencies.
async fn get_fleet_miners(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // PR-048b: reuse the EXACT /9H plumbing the single-miner path uses.
    // `build_mining_pipeline_snapshot_response` already returns an
    // `unavailable()` snapshot (both difficulty fields `None`) when no
    // publisher receiver is installed, so the fleet row degrades to
    // `(None, None)` — no new computation, no fabricated achieved value.
    let now_ms = unix_time_ms();
    let snapshot = build_mining_pipeline_snapshot_response(
        state.mining_pipeline_snapshot_rx.as_ref(),
        now_ms,
        state.mining_pipeline_snapshot_stale_after_ms,
    );

    Json(build_local_fleet_miners_response(
        &miner,
        &hw,
        local_hostname(),
        eth0_ipv4(),
        now_ms,
        snapshot.last_share_target_difficulty,
        snapshot.last_share_achieved_difficulty,
    ))
}

/// GET /api/thermal/posture -- Read-only thermal and power posture summary.
///
/// Read-only thermal supervisor runtime truth view.
fn thermal_supervisor_snapshot_response_value(
    snap: dcentrald_thermal::supervisor::SupervisorSnapshot,
    status: crate::ThermalSupervisorRuntimeStatus,
) -> serde_json::Value {
    let mut value = serde_json::to_value(snap).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.extend([
            ("configured_enabled".into(), serde_json::json!(status.configured_enabled)),
            ("runtime_present".into(), serde_json::json!(status.runtime_present)),
            ("snapshot_available".into(), serde_json::json!(status.snapshot_available)),
            ("commissioning_state".into(), serde_json::json!(status.commissioning_state)),
        ]);
    }
    value
}

async fn get_thermal_supervisor() -> impl IntoResponse {
    match crate::thermal_supervisor_snapshot() {
        Some(snap) => {
            let status = crate::thermal_supervisor_runtime_status();
            Json(thermal_supervisor_snapshot_response_value(snap, status)).into_response()
        }
        None => {
            let status = crate::thermal_supervisor_runtime_status();
            Json(serde_json::json!({
                "enabled": status.configured_enabled && status.snapshot_available,
                "configured_enabled": status.configured_enabled,
                "runtime_present": status.runtime_present,
                "snapshot_available": status.snapshot_available,
                "commissioning_state": status.commissioning_state,
                "uptime_secs": 0,
                "secs_since_last_step": 0,
                "board_states": [],
                "fan_max_pwm": serde_json::Value::Null,
                "note": match status.commissioning_state {
                    "disabled" => "thermal supervisor runtime present but disabled by configuration",
                    "pending_tick" => "thermal supervisor configured/enabled but no live tick has produced a snapshot yet",
                    "unsupported" => "thermal supervisor runtime channel is not installed on this daemon path",
                    _ => "thermal supervisor snapshot unavailable",
                }
            }))
            .into_response()
        }
    }
}

async fn get_thermal_posture(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    let miner = state.state_rx.borrow().clone();
    let power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let curtailment_state = {
        let guard = state.curtailment.lock().await;
        guard.state()
    };
    let now_ms = unix_time_ms();
    let (thresholds, threshold_source, threshold_reason) = read_thermal_posture_thresholds();
    let safety_envelope = crate::mode_middleware::SafetyEnvelope::for_mode(mode);

    let valid_temps: Vec<(u8, f32)> = miner
        .chains
        .iter()
        .filter(|chain| chain.temp_c.is_finite() && chain.temp_c > 0.0)
        .map(|chain| (chain.id, chain.temp_c))
        .collect();
    let max_temp = valid_temps
        .iter()
        .copied()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let avg_temp_c = if valid_temps.is_empty() {
        None
    } else {
        Some(
            valid_temps
                .iter()
                .map(|(_, temp)| *temp as f64)
                .sum::<f64>()
                / valid_temps.len() as f64,
        )
    };
    let max_temp_c = max_temp.map(|(_, temp)| temp as f64);
    let hottest_chain_id = max_temp.map(|(chain_id, _)| chain_id);
    let fan_ = !miner.fans.per_fan.is_empty() || miner.fans.rpm > 0;
    let fan_tach_suspect = miner.fans.pwm > 0
        && miner.fans.rpm == 0
        && miner.chains.iter().any(|chain| chain.chips > 0);
    let thermal_related_limit = power.dispatcher_limits.iter().any(|limit| {
        limit.active_sources.iter().any(|source| {
            matches!(
                source.as_str(),
                "thermal" | "sensor_safety" | "fan_clamp" | "power_cap"
            )
        }) || matches!(
            limit.dominant_source.as_deref(),
            Some("thermal" | "sensor_safety" | "fan_clamp" | "power_cap")
        )
    });
    let (posture_status, posture_reason) = classify_thermal_posture(
        max_temp.map(|(_, temp)| temp),
        thermal_related_limit,
        fan_tach_suspect,
        thresholds,
    );
    let power_age_s = if power.timestamp_ms > 0 && now_ms >= power.timestamp_ms {
        Some((now_ms - power.timestamp_ms) / 1000)
    } else {
        None
    };
    let power_projection = (&power, &miner, &hardware);
    let watt_cap = power.watt_cap.clone();
    let power_cap_active = watt_cap.as_ref().map(|cap| cap.throttling).unwrap_or(false);
    let per_fan = miner.fans.per_fan.clone();
    let chain_readings: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .map(|chain| {
            serde_json::json!({
                "id": chain.id,
                "temp_c": if chain.temp_c.is_finite() && chain.temp_c > 0.0 {
                    serde_json::json!(chain.temp_c)
                } else {
                    serde_json::Value::Null
                },
                "status": chain.status,
                "source": "miner_state",
            })
        })
        .collect();

    Json(serde_json::json!({
        "schema": "dcentos.thermal.posture.v1",
        "status": posture_status,
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "telemetry_source": "watch_state",
        "source": "existing_status_power_config_state",
        "mode": mode,
        "generated_at_s": now_ms / 1000,
        "fetched_at_ms": now_ms,
        "thermal": {
            "available": !valid_temps.is_empty(),
            "reason": posture_reason,
            "avg_temp_c": avg_temp_c,
            "max_temp_c": max_temp_c,
            "hottest_chain_id": hottest_chain_id,
            "valid_chain_count": valid_temps.len(),
            "missing_chain_count": miner.chains.len().saturating_sub(valid_temps.len()),
            "chains": chain_readings,
            "thresholds": {
                "target_c": thresholds.target_temp_c,
                "hot_c": thresholds.hot_temp_c,
                "dangerous_c": thresholds.dangerous_temp_c,
                "hysteresis_c": thresholds.hysteresis_c,
                "source": threshold_source,
                "reason": threshold_reason,
            },
        },
        "fans": {
            "available": fan_ || miner.fans.pwm > 0,
            "pwm": miner.fans.pwm,
            "rpm": if miner.fans.rpm > 0 { serde_json::json!(miner.fans.rpm) } else { serde_json::Value::Null },
            "per_fan": per_fan,
            "rpm_": fan_,
            "tach_suspect": fan_tach_suspect,
            "min_pwm": thresholds.fan_min_pwm,
            "max_pwm": thresholds.fan_max_pwm,
            "range_source": threshold_source,
            "reason": if fan_ {
                "Fan feedback is present in miner state."
            } else {
                "No fan RPM feedback is currently available; PWM is still reported from miner state."
            },
        },
        "power": build_thermal_posture_power_section(&power_projection, &power, power_age_s),
        "curtailment": {
            "available": true,
            "state": curtailment_state,
            "source": "curtailment_state_read",
            "read_only": true,
            "reason": "Observed curtailment state only; no sleep or wake action was invoked.",
        },
        "hardware_support": {
            "fan_rpm_feedback": fan_,
            "power_source": power_projection.source.as_str(),
            "power_calibrated": power_projection.calibrated,
            "pmbus_measured": power_projection.source_detail == "pmbus_measured",
            "reason": "Support flags are inferred from existing telemetry fields, not from live hardware probing.",
        },
        "runtime_ownership": {
            "dispatcher_limits_visible": true,
            "thermal_related_limit": thermal_related_limit,
            "power_cap_active": power_cap_active,
            "reason": "Runtime ownership is derived from dispatcher limits already published by the power watch channel.",
        },
        "safety": {
            "mode": mode,
            "envelope": {
                "dangerous_temp_c": safety_envelope.dangerous_temp_c,
                "max_frequency_mhz": safety_envelope.max_frequency_mhz,
                "allow_overclock": safety_envelope.allow_overclock,
                "allow_raw_registers": safety_envelope.allow_raw_registers,
                "min_fan_pwm": safety_envelope.min_fan_pwm,
                "max_power_watts": safety_envelope.max_power_watts,
            },
            "thermal_blocker": posture_status == "hot" || posture_status == "critical" || posture_status == "limited" || posture_status == "sensor_limited",
            "reason": "Safety posture is observational only; enforcement remains in daemon thermal and power loops.",
        },
        "sources": [
            "state_rx",
            "mode_rx",
            "power_rx",
            "curtailment_state",
            "active_config_or_defaults",
            "mode_safety_envelope"
        ],
        "limitations": [
            "This endpoint is read-only and does not change fan PWM, voltage, frequency, PSU, curtailment, pool, watchdog, upgrade, or rollback state.",
            "Thermal state is classified from reported chain temperatures and configured/default thresholds; it is not proof that hardware enforcement was triggered.",
            "Power values are nullable until the live power estimator publishes a positive reading."
        ],
    }))
}

fn build_thermal_posture_power_section(
    projection: &PowerTelemetryProjection,
    power: &dcentrald_autotuner::LivePowerEstimate,
    power_age_s: Option<u64>,
) -> serde_json::Value {
    let live_available = projection.live_power_available;
    serde_json::json!({
        "available": live_available,
        "board_watts": if live_available && projection.board_watts > 0 {
            serde_json::json!(projection.board_watts)
        } else {
            serde_json::Value::Null
        },
        "wall_watts": if live_available && projection.wall_watts > 0 {
            serde_json::json!(projection.wall_watts)
        } else {
            serde_json::Value::Null
        },
        "efficiency_jth": if live_available && projection.efficiency_jth > 0.0 {
            serde_json::json!(projection.efficiency_jth)
        } else {
            serde_json::Value::Null
        },
        "btu_h": if live_available && projection.btu_h > 0.0 {
            serde_json::json!(projection.btu_h)
        } else {
            serde_json::Value::Null
        },
        "source": projection.source.as_str(),
        "source_detail": projection.source_detail,
        "live_power_available": live_available,
        "modeled": projection.modeled,
        "calibrated": projection.calibrated,
        "calibration_multiplier": projection.calibration_multiplier,
        "age_s": power_age_s,
        "watt_cap": power.watt_cap.clone(),
        "runtime_limits_visible": true,
        "dispatcher_limit_count": power.dispatcher_limits.len(),
        "runtime_limits": power.dispatcher_limits.clone(),
        "reason": projection.note,
        "note": projection.note,
    })
}

/// POST /api/fleet/discover -- Probe known LAN miners from local hints.
///
/// This is intentionally conservative: it uses a shared miners.toml when available,
/// then merges manual/browser hints from the dashboard request. It does not perform
/// blind subnet scans.
async fn post_fleet_discover(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FleetDiscoverRequest>,
) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let mac = std::fs::read_to_string("/sys/class/net/eth0/address")
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
        .trim()
        .to_string();
    let power = state.power_rx.borrow().clone();
    let reported_power_watts = fleet_discovery_reported_power_watts(&power, &miner, &hw);

    Json(build_local_fleet_discover_response(
        &miner,
        &hw,
        local_hostname(),
        eth0_ipv4(),
        mac,
        reported_power_watts,
        &body,
        unix_time_ms(),
    ))
    .into_response()
}

/// GET /api/fleet/pool-stats -- Local read-only fleet pool rollup.
async fn get_fleet_pool_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    Json(build_local_fleet_pool_stats_response(
        &miner,
        &hw,
        local_hostname(),
        eth0_ipv4(),
        unix_time_ms() / 1000,
    ))
    .into_response()
}

const STATS_PER_CHAIN_SHARE_ACCOUNTING_REASON: &str =
    "Accepted/rejected shares are tracked at miner pool-session scope; this firmware does not attribute shares to individual chains.";

fn build_stats_share_accounting_meta() -> serde_json::Value {
    serde_json::json!({
        "totals_tracked": true,
        "totals_scope": "miner_pool_session",
        "totals_source": "miner_state.accepted_rejected",
        "per_chain_tracked": false,
        "per_chain_source": "not_tracked_per_chain",
        "reason": STATS_PER_CHAIN_SHARE_ACCOUNTING_REASON,
    })
}

fn build_stats_chain_rows(miner: &crate::MinerState) -> Vec<serde_json::Value> {
    miner
        .chains
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "chips": c.chips,
                "frequency_mhz": c.frequency_mhz,
                "frequency_source": chain_frequency_source(c),
                "voltage_mv": c.voltage_mv,
                "voltage_v": c.voltage_mv as f64 / 1000.0,
                "temp_c": c.temp_c,
                "temp_source": c.temp_source,
                "hashrate_ghs": c.hashrate_ghs,
                "hashrate_ths": c.hashrate_ghs / 1000.0,
                "errors": c.errors,
                "status": c.status,
                "accepted": 0,
                "rejected": 0,
                "accepted_source": "not_tracked_per_chain",
                "rejected_source": "not_tracked_per_chain",
                "share_accounting": {
                    "tracked": false,
                    "scope": "miner_pool_session",
                    "source": "not_tracked_per_chain",
                    "reason": STATS_PER_CHAIN_SHARE_ACCOUNTING_REASON,
                },
                "hw_errors": c.errors,
            })
        })
        .collect()
}

/// GET /api/v1/capabilities -- Shared cross-family capability descriptor.
async fn get_capabilities(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    Json(build_antminer_capability_descriptor(&miner, &hw))
}

/// GET /api/stats -- Detailed per-chain, per-chip statistics.
///
/// Gated to Standard + Hacker modes. Returns per-chain detailed stats
/// including per-chip health data when available.
async fn get_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/stats", mode) {
        return resp.into_response();
    }

    let miner = state.state_rx.borrow().clone();

    let chains = build_stats_chain_rows(&miner);
    let share_accounting = build_stats_share_accounting_meta();

    // Read live power estimate from the work dispatcher's watch channel.
    // This is computed every 5s from ACTUAL per-chip frequencies and voltages,
    // reflecting autotuner changes, thermal throttling, and voltage drift.
    let power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let power_projection = (&power, &miner, &hardware);
    let targeting = build_power_targeting_state(mode, &power_projection);

    // Daily profitability estimate — read economics from config. P2-4 (§4.E):
    // the daemon `[home]` section is the SINGLE SOURCE OF TRUTH for the
    // electricity rate + currency; the dashboard surfaces these values instead
    // of its own localStorage guess.
    // P3-2: read the persisted config from the in-memory, post-write-fresh
    // cache instead of re-parsing dcentrald.toml from disk on every request.
    let stats_config_table = state.config_cache.snapshot();
    let stats_economics = home_economics_from_table(&stats_config_table);
    let stats_elec_rate = stats_economics.rate_usd_per_kwh;
    // RE-011: space-heater heat-reuse credit. `[home].heating_offset_fraction`
    // (0..1) is how much of the miner's heat actually offsets heating you'd
    // otherwise pay for — defaults to 0.0 (NO credit assumed) so we never
    // overstate ROI. The operator opts into the credit by setting it.
    let heating_offset_fraction = stats_config_table
        .get("home")
        .and_then(|v| v.as_table())
        .and_then(|h| h.get("heating_offset_fraction"))
        .and_then(|v| v.as_float())
        .unwrap_or(0.0);
    // RE-011: electricity cost, heat-reuse credit, and net daily cost use live
    // published wall power only. Static fallback watts are useful for display
    // estimates but must not become bare current-cost claims.
    let power_cost =
        (&power_projection, stats_elec_rate, heating_offset_fraction);
    // RE-011: best-effort live network difficulty (None if offline / not yet
    // fetched — never fabricated).
    let network_difficulty = cached_network_difficulty();

    Json(serde_json::json!({
        "hashrate_ghs": miner.hashrate_ghs,
        "hashrate_ths": miner.hashrate_ghs / 1000.0,
        "accepted": miner.accepted,
        "rejected": miner.rejected,
        "uptime_s": miner.uptime_s,
        "chains": chains,
        "share_accounting": share_accounting,
        "fans": {
            "pwm": miner.fans.pwm,
            "rpm": miner.fans.rpm,
        },
        "pool": {
            // SEC (W20 / parity #66): stratum URLs can embed inline credentials
            // (stratum+tcp://worker:pass@host). Strip them for the status
            // display surface (the daemon reconnects via the real config URL,
            // never this display copy). Mirrors get_pools, which already sanitizes.
            "url": dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
            "status": miner.pool.status,
            "difficulty": miner.pool.difficulty,
            "pool_target_difficulty": miner.pool.difficulty,
            "last_share_s": secs_since(miner.pool.last_share_at),
            "share_efficiency": miner.pool.share_efficiency,
            "protocol": miner.pool.protocol,
            "encrypted": miner.pool.encrypted,
            "encrypted_source": miner.pool.encrypted_source,
            "sv2_session": miner.pool.sv2_session,
            "sv2_session_source": miner.pool.sv2_session_source,
            "donating": miner.pool.donating,
            "donating_source": miner.pool.donating_source,
            "auto_fallback_active": miner.pool.auto_fallback_active,
            "auto_fallback_source": miner.pool.auto_fallback_source,
            "auto_retry_sv2_after_s": miner.pool.auto_retry_sv2_after_s,
            "auto_fallback_reason": miner.pool.auto_fallback_reason,
            "failover": miner.pool.failover.clone(),
            "failover_source": miner.pool.failover_source,
            "hashrate_split": miner.pool.hashrate_split.clone(),
            "hashrate_split_source": miner.pool.hashrate_split_source,
            "latency_ms": miner.pool.latency_ms,
            "latency_ms_source": miner.pool.latency_ms_source,
            "reject_reason_counts": miner.pool.reject_reason_counts,
            "reject_reason_counts_source": miner.pool.reject_reason_counts_source,
            "rolling_acceptance_pct_30min": miner.pool.rolling_acceptance_pct_30min,
            "rolling_acceptance_count_30min": miner.pool.rolling_acceptance_count_30min,
            "rolling_acceptance_source": miner.pool.rolling_acceptance_source,
        },
        "power": {
            "watts": power_projection.board_watts,
            "wall_watts": power_projection.wall_watts,
            "efficiency_jth": power_projection.efficiency_jth,
            "btu_h": power_projection.btu_h,
            "source": power_projection.source,
            "source_detail": power_projection.source_detail,
            "live_power_available": power_projection.live_power_available,
            "modeled": power_projection.modeled,
            "note": power_projection.note,
            "calibrated": power_projection.calibrated,
            "calibration_multiplier": power_projection.calibration_multiplier,
            "per_chain_watts": power.per_chain_watts,
            "runtime_limits": power.dispatcher_limits,
            "watt_cap": power.watt_cap,
            "targeting": targeting,
        },
        "profitability_summary": {
            "daily_electricity_cost_usd": format!("{:.2}", power_cost.daily_cost_usd),
            "daily_electricity_cost_power_watts": power_cost.wall_watts,
            "daily_electricity_cost_power_live_available": power_cost.live_power_available,
            "daily_electricity_cost_power_modeled": power_cost.modeled,
            "daily_electricity_cost_power_source_detail": power_cost.source_detail,
            "daily_electricity_cost_note": power_cost.note,
            "electricity_rate_kwh": stats_elec_rate,
            "currency": stats_economics.currency,
            // P2-4 (§4.E): false until the operator confirms a rate at setup —
            // the cost figures above are an uncalibrated default-rate estimate
            // until then, and the UI must say so.
            "electricity_rate_calibrated": stats_economics.rate_calibrated,
            // RE-011: space-heater heat-reuse credit + net cost after it.
            "heating_offset_fraction": heating_offset_fraction,
            "heat_reuse_credit_usd_per_day": format!("{:.2}", power_cost.heat_reuse_credit_usd_per_day),
            "net_daily_cost_after_heat_credit_usd": format!("{:.2}", power_cost.net_daily_cost_after_heat_credit),
            "heat_reuse_note": "Heat-reuse credit values the resistive electric heating this miner displaces. Set [home].heating_offset_fraction (0..1) to the share of heat that offsets heating you'd otherwise pay for. Defaults to 0 (no credit).",
            // RE-011: best-effort live network difficulty (null if offline /
            // not yet fetched — never fabricated).
            "network_difficulty": network_difficulty.as_ref().map(|d| serde_json::json!({
                "difficulty": if d.difficulty > 0.0 { serde_json::json!(d.difficulty) } else { serde_json::Value::Null },
                "difficulty_change_percent": d.difficulty_change_percent,
                "network_hashrate_ehs": d.network_hashrate_ehs,
                "fetched_at_ms": d.fetched_at_ms,
                "source": d.source,
            })),
        },
        "share_efficiency": miner.pool.share_efficiency,
        "tuning_status": "unavailable",
    }))
    .into_response()
}

/// GET /api/pools -- Pool configuration and status.
async fn get_pools(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let configured_pools = read_configured_pools();
    let failover_contract = build_pool_failover_contract(&miner, &configured_pools);

    let pools = if configured_pools.is_empty() {
        vec![serde_json::json!({
            "id": 0,
            "url": dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
            "worker": "",
            "status": miner.pool.status,
            "priority": 0,
            "difficulty": miner.pool.difficulty,
            "pool_target_difficulty": miner.pool.difficulty,
            "accepted": miner.accepted,
            "rejected": miner.rejected,
            "last_share_s": secs_since(miner.pool.last_share_at),
            "share_efficiency": miner.pool.share_efficiency,
            // HLA-9: last measured submit->response round-trip latency (ms).
            // 0 = not yet measured. VNish surfaces this as pools[].ping.
            "latency_ms": miner.pool.latency_ms,
            // Disambiguate "0 ms RTT" from "never measured" truthfully.
            "latency_measured": miner.pool.latency_ms_source.is_some(),
            // Superior diagnostic observability: session reject counts bucketed
            // by actionable cause (low_difficulty / stale / duplicate /
            // above_target / unauthorized / other) -- not just a total.
            "rejects_by_reason": serde_json::Value::Object(
                crate::REJECT_REASON_LABELS
                    .iter()
                    .zip(miner.pool.reject_reason_counts.iter())
                    .map(|(label, count)| (label.to_string(), serde_json::json!(count)))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
            ),
            "stratum_active": is_pool_mining_capable(&miner.pool.status),
            "stratum_connected": is_pool_connected(&miner.pool.status),
            "stratum_connecting": is_pool_connecting(&miner.pool.status),
            "stratum_mining_capable": is_pool_mining_capable(&miner.pool.status),
            "protocol": miner.pool.protocol,
            "sv2_url": serde_json::Value::Null,
            "encrypted": miner.pool.encrypted,
            "encrypted_source": miner.pool.encrypted_source,
            "sv2_session_source": miner.pool.sv2_session_source,
            "telemetry_source": "runtime_state_unconfigured",
            "health_limitations": [
                "last_share_s is accepted-share age, not mining.notify age",
                "no_notify_age_s is unavailable until stratum notify timestamps are tracked",
                "GET /api/pools is read-only and does not switch pools or trigger failover",
            ],
            "no_notify_age_s": serde_json::Value::Null,
            "failover_policy": "runtime_observed_read_only",
            "failover_active_pool_index": miner.pool.failover.active_pool_index,
            "failover_last_switch_reason": miner.pool.failover.last_switch_reason.clone(),
            "failover_switch_count": miner.pool.failover.switch_count,
            "failover_stale_jobs_flushed_on_switch": miner.pool.failover.stale_jobs_flushed_on_switch,
            "failover_source": miner.pool.failover_source,
            "pending_submit_correlations_cleared": miner.pool.failover.pending_submit_correlations_cleared,
            "shares_unresolved": miner.pool.failover.shares_unresolved,
            "pending_submit_dropped": miner.pool.failover.pending_submit_dropped,
            "auto_fallback_active": miner.pool.auto_fallback_active,
            "auto_fallback_source": miner.pool.auto_fallback_source,
            "auto_retry_sv2_after_s": miner.pool.auto_retry_sv2_after_s,
            "auto_fallback_reason": miner.pool.auto_fallback_reason,
            "latency_ms_source": miner.pool.latency_ms_source,
            "reject_reason_counts_source": miner.pool.reject_reason_counts_source,
            "rolling_acceptance_pct_30min": miner.pool.rolling_acceptance_pct_30min,
            "rolling_acceptance_count_30min": miner.pool.rolling_acceptance_count_30min,
            "rolling_acceptance_source": miner.pool.rolling_acceptance_source,
            "hashrate_split_bps": serde_json::Value::Null,
            "hashrate_split_pct": serde_json::Value::Null,
            "hashrate_split_active": false,
            "hashrate_split_route": miner.pool.hashrate_split.active_route,
            "hashrate_split_source": miner.pool.hashrate_split_source,
        })]
    } else {
        let active_index = if miner.pool.hashrate_split.enabled
            && miner.pool.hashrate_split.active
            && miner.pool.hashrate_split.active_pool_index < configured_pools.len()
        {
            Some(miner.pool.hashrate_split.active_pool_index)
        } else if miner.pool.failover.configured_pool_count > 0
            && miner.pool.failover.active_pool_index < configured_pools.len()
        {
            Some(miner.pool.failover.active_pool_index)
        } else {
            configured_pools
                .iter()
                .position(|pool| pool.url == miner.pool.url)
        };
        configured_pools
            .iter()
            .enumerate()
            .map(|(index, pool)| {
                let is_active = active_index == Some(index);
                serde_json::json!({
                    "id": index,
                    "url": dcentrald_stratum::pool_api::sanitize_pool_url(&pool.url),
                    // MASK: the worker is the operator's BTC payout address on V1
                    // solo — never emit it raw (wallet-mask rule).
                    "worker": pool_worker_display(&pool.worker),
                    "status": if is_active { miner.pool.status.clone() } else { "Configured".to_string() },
                    "priority": pool.priority,
                    "difficulty": if is_active { miner.pool.difficulty } else { 0.0 },
                    "pool_target_difficulty": if is_active { miner.pool.difficulty } else { 0.0 },
                    "accepted": if is_active { miner.accepted } else { 0 },
                    "rejected": if is_active { miner.rejected } else { 0 },
                    "last_share_s": if is_active { secs_since(miner.pool.last_share_at) } else { 0 },
                    "share_efficiency": if is_active { serde_json::json!(miner.pool.share_efficiency) } else { serde_json::Value::Null },
                    "latency_ms": if is_active { serde_json::json!(miner.pool.latency_ms) } else { serde_json::Value::Null },
                    // HLA-9 truthfulness: disambiguate "0 ms RTT" from "never
                    // measured" per pool. Only the active pool has a measured
                    // submit->response RTT in the daemon-mirrored PoolState;
                    // inactive pools have no measured latency until stratum
                    // selects them (mirroring StratumStats::per_pool_latency_ms
                    // into PoolState is the daemon-side follow-up).
                    "latency_measured": is_active && miner.pool.latency_ms_source.is_some(),
                    "latency_ms_source": if is_active { miner.pool.latency_ms_source.clone() } else { None },
                    "rejects_by_reason": if is_active {
                        serde_json::Value::Object(
                            crate::REJECT_REASON_LABELS
                                .iter()
                                .zip(miner.pool.reject_reason_counts.iter())
                                .map(|(label, count)| (label.to_string(), serde_json::json!(count)))
                                .collect::<serde_json::Map<String, serde_json::Value>>(),
                        )
                    } else {
                        serde_json::Value::Null
                    },
                    "reject_reason_counts_source": if is_active { miner.pool.reject_reason_counts_source.clone() } else { None },
                    "rolling_acceptance_pct_30min": if is_active { serde_json::json!(miner.pool.rolling_acceptance_pct_30min) } else { serde_json::Value::Null },
                    "rolling_acceptance_count_30min": if is_active { serde_json::json!(miner.pool.rolling_acceptance_count_30min) } else { serde_json::Value::Null },
                    "rolling_acceptance_source": if is_active { miner.pool.rolling_acceptance_source.clone() } else { None },
                    "stratum_active": is_active && is_pool_mining_capable(&miner.pool.status),
                    "stratum_connected": is_active && is_pool_connected(&miner.pool.status),
                    "stratum_connecting": is_active && is_pool_connecting(&miner.pool.status),
                    "stratum_mining_capable": is_active && is_pool_mining_capable(&miner.pool.status),
                    "protocol": pool.protocol.clone().unwrap_or_else(|| if is_active { miner.pool.protocol.clone() } else { "sv1".to_string() }),
                    "sv2_url": pool.sv2_url.clone(),
                    "encrypted": if is_active { serde_json::json!(miner.pool.encrypted) } else { serde_json::Value::Null },
                    "encrypted_source": if is_active { miner.pool.encrypted_source.clone() } else { None },
                    "sv2_session_source": if is_active { miner.pool.sv2_session_source.clone() } else { None },
                    "telemetry_source": if is_active { "runtime_state" } else { "configured_pool" },
                    "health_limitations": if is_active {
                        serde_json::json!([
                            "last_share_s is accepted-share age, not mining.notify age",
                            "no_notify_age_s is unavailable until stratum notify timestamps are tracked",
                            "GET /api/pools is read-only and does not switch pools or trigger failover",
                        ])
                    } else {
                        serde_json::json!([
                            "inactive configured pool; live counters are unavailable until stratum selects it",
                            "GET /api/pools is read-only and does not switch pools or trigger failover",
                        ])
                    },
                    "no_notify_age_s": serde_json::Value::Null,
                    "failover_policy": "runtime_observed_read_only",
                    "failover_active_pool_index": miner.pool.failover.active_pool_index,
                    "failover_last_switch_reason": if is_active { serde_json::json!(miner.pool.failover.last_switch_reason.clone()) } else { serde_json::Value::Null },
                    "failover_switch_count": miner.pool.failover.switch_count,
                    "failover_stale_jobs_flushed_on_switch": miner.pool.failover.stale_jobs_flushed_on_switch,
                    "failover_source": if is_active { miner.pool.failover_source.clone() } else { None },
                    "pending_submit_correlations_cleared": miner.pool.failover.pending_submit_correlations_cleared,
                    "shares_unresolved": if is_active { serde_json::json!(miner.pool.failover.shares_unresolved) } else { serde_json::Value::Null },
                    "pending_submit_dropped": if is_active { serde_json::json!(miner.pool.failover.pending_submit_dropped) } else { serde_json::Value::Null },
                    "auto_fallback_active": if is_active { serde_json::json!(miner.pool.auto_fallback_active) } else { serde_json::Value::Bool(false) },
                    "auto_fallback_source": if is_active { miner.pool.auto_fallback_source.clone() } else { None },
                    "auto_retry_sv2_after_s": if is_active { serde_json::json!(miner.pool.auto_retry_sv2_after_s) } else { serde_json::Value::Null },
                    "auto_fallback_reason": if is_active { serde_json::json!(miner.pool.auto_fallback_reason) } else { serde_json::Value::Null },
                    "hashrate_split_bps": pool.split_bps,
                    "hashrate_split_pct": pool.split_bps.map(|bps| f64::from(bps) / 100.0),
                    "hashrate_split_active": miner.pool.hashrate_split.enabled
                        && miner.pool.hashrate_split.active
                        && miner.pool.hashrate_split.active_pool_index == index,
                    "hashrate_split_route": if miner.pool.hashrate_split.enabled
                        && miner.pool.hashrate_split.active_pool_index == index {
                        miner.pool.hashrate_split.active_route.clone()
                    } else {
                        String::new()
                    },
                    "hashrate_split_source": if is_active { miner.pool.hashrate_split_source.clone() } else { None },
                })
            })
            .collect::<Vec<_>>()
    };

    // W5.5: surface the active donation route so the dashboard chip can
    // render "Donating to D-Central primary" vs "Donating via Braiins
    // Pool fallback" instead of a bare "DONATING" badge.
    // Pool URLs never include passwords, so this is safe to expose
    // unauthenticated. `route` is the human label the UI uses; the index
    // is kept for stable machine consumption.
    let donation_route_label = if !miner.pool.donating {
        "user_pool"
    } else if miner.pool.donation_pool_index == 0 {
        "donation_primary"
    } else {
        "donation_fallback"
    };
    let donation_obj = serde_json::json!({
        "active": miner.pool.donating,
        "route": donation_route_label,
        "active_url": dcentrald_stratum::pool_api::sanitize_pool_url(
            &miner.pool.donation_active_url
        ),
        // GROUP-B SW-08 follow-up: the donation worker is a wallet-shaped
        // string (D-Central's donation worker, e.g. `DungeonMaster`).
        // Mask it on this unauthenticated read-only surface for consistency
        // with the `/api/status` RE-013 donation block (which already
        // redacts via `redact_worker`) and the setup-wizard wallet masking.
        // Pool URLs are sanitized separately; worker names get the wallet
        // mask so a leaked screenshot/support bundle can't expose the raw
        // identity. Empty string when not in a donation window.
        "active_worker": dcentrald_common::wallet_mask::mask_wallet(
            &miner.pool.donation_active_worker
        ),
        "pool_index": miner.pool.donation_pool_index,
    });

    Json(serde_json::json!({
        "pools": pools,
        "failover": failover_contract,
        "hashrate_split": build_hashrate_split_contract(&miner, &configured_pools),
        "donation": donation_obj,
    }))
}

/// POST /api/pools -- Add or modify pool configuration.
///
/// Writes the pool config to the [pool] section of /data/dcentrald.toml.
/// The stratum client will pick up the new config on next reconnect or restart.
pub(crate) async fn post_pools(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PoolConfigRequest>,
) -> Response {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::PoolsRw, "/api/pools")
    {
        return response;
    }

    //  W1b — capture current active primary pool URL so a real
    // change can be audited. Failover-slot edits and worker-name-only
    // edits still emit the broader PoolConfigWrite event below.
    let old_primary_url = state.state_rx.borrow().pool.url.clone();
    let old_pool_audit_snapshot = read_pool_audit_snapshot();

    let (pool_requests, split_request) = body.into_parts();
    let (pools, hashrate_split) = match validate_and_write_pool_config(pool_requests, split_request)
    {
        Ok(parts) => parts,
        Err((status, message)) => {
            return api_error(
                status,
                if status == StatusCode::BAD_REQUEST {
                    dcentrald_api_types::api_error_codes::POOL_VALIDATION
                } else {
                    dcentrald_api_types::api_error_codes::POOL_CONFIG_WRITE_FAILED
                },
                message,
                (status == StatusCode::BAD_REQUEST).then_some(
                    "Check the pool URL format, worker name, and failover split settings.",
                ),
            );
        }
    };

    let config_path = get_writable_config_path();

    // `validate_and_write_pool_config` already validated + persisted the config
    // atomically (any I/O failure surfaced as the early `Err` return above), so
    // reaching here means the write succeeded. Emit the audit trail + response.
    tracing::info!(
        config_path,
        "Pool config saved — stratum client will reconnect on next cycle"
    );

    //  W1b — audit primary pool URL change. Skip on no-op
    // (worker-name-only edits keep `old_primary_url == pools[0].url`).
    let new_pool_audit_snapshot = pool_audit_snapshot_from_request(&pools, hashrate_split.as_ref());
    crate::push_audit_event(
        &state,
        "rest_dashboard",
        pool_config_write_audit_event(
            pools.len(),
            pool_config_changed_fields(&old_pool_audit_snapshot, &new_pool_audit_snapshot),
        ),
    );

    if pools[0].url != old_primary_url {
        let from_value = if old_primary_url.is_empty() {
            None
        } else {
            Some(old_primary_url.clone())
        };
        crate::push_audit_event(
            &state,
            "rest_dashboard",
            dcentrald_api_types::audit_log::AuditEvent::PoolSwitch {
                from: from_value,
                to: pools[0].url.clone(),
            },
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "message": "Pool configuration saved to disk with primary and failover pools. Existing mining enable state was preserved.",
            "pool": {
                "url": pools[0].url.clone(),
                "worker": pools[0].worker.clone(),
                "priority": pools[0].priority.unwrap_or(0),
                "protocol": pools[0].protocol.clone(),
                "sv2_url": pools[0].sv2_url.clone(),
                "split_bps": pools[0].split_bps,
            },
            "pools": pools.iter().map(|pool| serde_json::json!({
                "url": pool.url.clone(),
                "worker": pool.worker.clone(),
                "priority": pool.priority.unwrap_or(0),
                "protocol": pool.protocol.clone(),
                "sv2_url": pool.sv2_url.clone(),
                "split_bps": pool.split_bps,
            })).collect::<Vec<_>>(),
            "hashrate_split": hashrate_split.as_ref().map(|split| serde_json::json!({
                "enabled": true,
                "primary_bps": split.primary_bps,
                "secondary_bps": split.secondary_bps,
                "secondary_pct": f64::from(split.secondary_bps) / 100.0,
                "cycle_duration_s": split.cycle_duration_s,
            })),
            "config_path": config_path,
        })),
    )
    .into_response()
}

/// Validate + atomically persist a pool-config request to the `[pool]` section
/// of the writable config TOML. This is the shared core extracted from
/// `post_pools` so both the REST handler and the gRPC `grpc_bridge_set_pools`
/// bridge enforce the SAME validation (≤3 pools, non-empty primary URL, V1 pool
/// URL support, hashrate-split shape) and the SAME atomic read-modify-write —
/// there is no second, divergent pool-write path.
///
/// On success returns the normalized pools (priority-assigned, split_bps wired)
/// and the normalized hashrate split for the caller's audit/response. On failure
/// returns the HTTP status the REST handler would have used plus the message
/// (`400` for validation, `500` for the disk write).
fn validate_and_write_pool_config(
    pool_requests: Vec<PoolRequest>,
    split_request: Option<HashrateSplitRequest>,
) -> std::result::Result<(Vec<PoolRequest>, Option<NormalizedHashrateSplit>), (StatusCode, String)>
{
    let mut pools =
        normalize_pool_requests(pool_requests).map_err(|m| (StatusCode::BAD_REQUEST, m))?;

    let hashrate_split = normalize_hashrate_split_request(split_request, &pools)
        .map_err(|m| (StatusCode::BAD_REQUEST, m))?;

    if let Some(split) = &hashrate_split {
        pools[0].split_bps = Some(split.primary_bps);
        pools[1].split_bps = Some(split.secondary_bps);
    } else {
        for pool in &mut pools {
            pool.split_bps = None;
        }
    }

    for pool in &pools {
        validate_pool_url_support(&pool.url).map_err(|m| (StatusCode::BAD_REQUEST, m))?;
    }

    // W1.4: never log the full worker/wallet at INFO. The worker field is the
    // operator's wallet address in plaintext on Stratum V1 (masked below), and
    // the pool URL is run through sanitize_pool_url so a `user:pass@`-style
    // credential embedded in a malformed URL is stripped before it hits the log
    // (parity with the ~15 stratum client.rs pool-URL log sites).
    tracing::info!(
        count = pools.len(),
        primary_url = %dcentrald_stratum::pool_api::sanitize_pool_url(&pools[0].url),
        primary_worker = %dcentrald_common::wallet_mask::mask_wallet(&pools[0].worker),
        "Pool configuration request — writing to config"
    );

    let config_path = get_writable_config_path();

    // Read-modify-write the [pool] section in the TOML config.
    (|| -> std::result::Result<(), String> {
        // RELIAB-2b: serialize load→modify→write (lost-update guard). The guard
        // lives only inside this synchronous closure, so it drops before the
        // surrounding async fn reaches any `.await`.
        let _cfg_write_guard = crate::atomic_io::config_write_lock();
        let mut table = load_config_table_for_write()?;

        if let Some(parent) = std::path::Path::new(config_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        let pool = table
            .entry("pool".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut pool_table) = pool {
            pool_table.remove("failover1");
            pool_table.remove("failover2");
            pool_table.remove("routing_mode");
            pool_table.remove("split_cycle_duration_s");
            write_pool_to_table(pool_table, &pools[0]);

            if let Some(pool2) = pools.get(1) {
                let mut failover1 = toml::Table::new();
                write_pool_to_table(&mut failover1, pool2);
                pool_table.insert("failover1".into(), toml::Value::Table(failover1));
            }
            if let Some(pool3) = pools.get(2) {
                let mut failover2 = toml::Table::new();
                write_pool_to_table(&mut failover2, pool3);
                pool_table.insert("failover2".into(), toml::Value::Table(failover2));
            }
            if let Some(split) = &hashrate_split {
                pool_table.insert(
                    "routing_mode".into(),
                    toml::Value::String("weighted_split".to_string()),
                );
                pool_table.insert(
                    "split_cycle_duration_s".into(),
                    toml::Value::Integer(split.cycle_duration_s as i64),
                );
            }
        }

        let output = toml::to_string_pretty(&table)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
        Ok(())
    })()
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to save pool config");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save pool config: {}", e),
        )
    })?;

    Ok((pools, hashrate_split))
}

// ───────────────────────────────────────────────────────────────────────
// SW-02 gRPC WRITE-control bridge surface.
//
// These three `pub async fn grpc_bridge_*` functions are the SMALL, explicit
// public hooks the daemon's `DaemonGrpcWriteDelegate` calls to bridge the
// `set_pools` / `set_fan_mode` / `reboot` gRPC write RPCs to the SAME gated
// logic the REST handlers use. The raw axum handlers stay `pub(crate)` — only
// these narrow bridges are public, so the gRPC control plane reaches exactly
// the same validation + safety caps and nothing else:
//
//   * `grpc_bridge_set_pools`  → `validate_and_write_pool_config` (same ≤3-pool
//     / URL-validation / atomic-write path as `post_pools`).
//   * `grpc_bridge_set_fan`    → `compute_commanded_fan_pwm` (same per-mode
//     envelope incl. the load-bearing HOME PWM-30 hard cap as `post_fan`) then
//     the same `set_fan_pwm_via_hal` write. Returns the POST-clamp applied PWM.
//   * `grpc_bridge_reboot`     → `trigger_daemon_restart` (the same restart
//     action `post_action_restart` performs).
//
// Honest outcomes: each returns `Ok(..)` only on a real applied success and
// `Err(String)` on validation/cap rejection or hardware failure — never a
// silent ack. The whole surface is still gated by `DCENT_GRPC_WRITE_CONTROL`
// at the daemon (the delegate is only installed when the gate is ON), so
// gate-off behavior is byte-identical (no delegate ⇒ RPCs stay UNIMPLEMENTED).
// ───────────────────────────────────────────────────────────────────────

/// Result of a successful `grpc_bridge_set_pools` — the primary pool URL and the
/// count of configured pools, for the delegate's ack detail.
pub struct GrpcPoolBridgeOk {
    pub pool_count: usize,
    pub primary_url: String,
}

/// Pure conversion of the gRPC delegate's plain
/// `(url, worker, password, priority)` tuples into `PoolRequest`s. Split out so
/// it is host-testable independent of the (filesystem-writing) write core.
/// gRPC `SetPools` carries no protocol/sv2/split, so those are left `None`.
fn grpc_pools_to_requests(pools: Vec<(String, String, String, u32)>) -> Vec<PoolRequest> {
    pools
        .into_iter()
        .map(|(url, worker, password, priority)| PoolRequest {
            url,
            worker,
            password,
            priority: Some(priority.min(u8::MAX as u32) as u8),
            protocol: None,
            sv2_url: None,
            split_bps: None,
        })
        .collect()
}

/// gRPC bridge for `SetPools`. Converts the delegate's plain
/// `(url, worker, password, priority)` tuples into `PoolRequest`s and runs them
/// through the SAME `validate_and_write_pool_config` core as `POST /api/pools`
/// (≤3 pools, non-empty primary URL, V1 pool-URL support, atomic write).
///
/// Returns `Err` with the validation/IO message on rejection (the delegate maps
/// it to a gRPC `failed_precondition` — an honest error, never a silent ack).
/// gRPC `SetPools` does not carry hashrate-split, so split is always `None`
/// (matching a single/multiple pool request with no split — leaving any prior
/// split cleared exactly as the REST handler would for a no-split request).
pub async fn grpc_bridge_set_pools(
    state: &AppState,
    pools: Vec<(String, String, String, u32)>,
) -> std::result::Result<GrpcPoolBridgeOk, String> {
    // CE-052: fail-closed capability gate FIRST — before any validation or the
    // atomic TOML write. REST twin `post_pools` enforces `PoolsRw`; this bridge
    // did not, silently bypassing the Beta-tier + identity gate.
    bridge_runtime_capability_guard(state, RuntimeCapability::PoolsRw, "bridge:set_pools")?;
    let pool_requests = grpc_pools_to_requests(pools);

    // Same core as the REST handler. `None` split = no weighted split (and the
    // write clears any previously-configured split, exactly like the REST path
    // for a no-split request).
    let (normalized, _split) = validate_and_write_pool_config(pool_requests, None)
        // The bridge boundary is a flat String; drop the HTTP status (the
        // delegate doesn't carry one) but keep the human message verbatim.
        .map_err(|(_status, message)| message)?;

    Ok(GrpcPoolBridgeOk {
        pool_count: normalized.len(),
        primary_url: normalized[0].url.clone(),
    })
}

/// gRPC bridge for `SetFanMode`. Computes the actually-commanded PWM through the
/// SAME `compute_commanded_fan_pwm` envelope as `POST /api/fan` (so the
/// load-bearing HOME PWM-30 hard cap is enforced here exactly as on the REST
/// path), then writes it via the SAME `set_fan_pwm_via_hal`.
///
/// `current_mode` is the daemon's live `OperatingMode` (read from
/// `AppState::mode_rx`). The gRPC request only carries a target PWM, so it is
/// treated as a `"custom"` request: the per-mode minimum/maximum + the PWM-30
/// home cap apply. `allow_loud` is `false` (the gRPC surface offers no loud
/// override) so a Home unit can NEVER be driven above PWM 30 on this path.
///
/// Returns the POST-clamp applied PWM on success (the delegate surfaces it as
/// `applied_value`, so a client requesting 100 on a home unit sees 30), or an
/// honest `Err` on a below-minimum custom PWM or a HAL write failure.
pub fn grpc_bridge_set_fan(
    state: &AppState,
    requested_pwm: u32,
) -> std::result::Result<u8, String> {
    // CE-052: fail-closed capability gate FIRST — before the HAL fan write. REST
    // twin `post_fan` enforces `PowerControl`; this bridge only had the PWM-30
    // clamp, not the capability gate. (The PWM-30 home cap below is untouched.)
    bridge_runtime_capability_guard(state, RuntimeCapability::PowerControl, "bridge:set_fan")?;
    let current_mode = *state.mode_rx.borrow();
    let custom_pwm = Some(requested_pwm.min(u8::MAX as u32) as u8);
    // `allow_loud = false`: the gRPC bridge never opts above the universal
    // PWM-30 safety cap. On Home this is doubly-enforced (per-mode max is 30).
    let pwm = compute_commanded_fan_pwm(current_mode, "custom", custom_pwm, false)?;
    let (_uio, _variant, commanded_pwm, _pwm0, _pwm1, _max_rpm) = set_fan_pwm_via_hal(pwm)?;
    tracing::info!(
        requested_pwm,
        applied_pwm = pwm,
        commanded_pwm,
        ?current_mode,
        "Fan PWM command accepted via gRPC write bridge"
    );
    // Report the value we actually clamped to (the post-PWM-30 value on a home
    // unit), not the hardware shadow readback — the contract is "what we set".
    Ok(pwm)
}

/// gRPC bridge for `Reboot`. Triggers the SAME daemon-restart action as
/// `POST /api/action/restart` (write the restart flag + spawn the init.d
/// restart). Returns the human status string for the delegate's ack.
pub fn grpc_bridge_reboot(state: &AppState) -> std::result::Result<String, String> {
    // CE-052: fail-closed capability gate FIRST — before the restart flag is
    // written / init.d restart is spawned. REST twins enforce `Reboot`.
    bridge_runtime_capability_guard(state, RuntimeCapability::Reboot, "bridge:reboot")?;
    trigger_daemon_restart();
    Ok("Mining restart initiated — daemon will restart in ~2 seconds".to_string())
}

// ───────────────────────────────────────────────────────────────────────
// P2-7 (Omega): MQTT / Home-Assistant command sink.
//
// The MQTT publisher (`dcentrald_api::mqtt`) is publish-only by default. When
// `[mqtt]` is enabled the daemon wires this sink so Home Assistant can WRITE a
// few safe setpoints. It routes each command through the SAME clamped setters
// the REST control plane uses — it opens NO new unclamped path:
//   * fan PWM      → `grpc_bridge_set_fan` (`compute_commanded_fan_pwm` +
//                    `set_fan_pwm_via_hal`): the load-bearing HOME PWM-30 hard
//                    cap is enforced against the live `OperatingMode`,
//                    `allow_loud = false` — a remote command can NEVER raise
//                    fans above PWM 30.
//   * target watts → clamped to the published envelope, then dispatched to the
//                    SAME live autotuner `PowerTarget` command the REST
//                    increment/decrement endpoints use (the autotuner applies
//                    its own downstream voltage/PVT clamps).
//   * target temp  → clamped to the thermal envelope (bounded under the
//                    dangerous threshold so a remote setpoint can't disable
//                    cooling), then persisted to `[thermal].target_temp_c`.
// ───────────────────────────────────────────────────────────────────────

/// Concrete [`crate::mqtt::MqttCommandSink`] over the live [`AppState`].
pub struct AppStateMqttCommandSink {
    state: Arc<AppState>,
}

/// Build the MQTT/HA command sink the daemon hands to the MQTT publisher. The
/// sink routes every operator setpoint through the same clamped setters the REST
/// API uses (see the module note above). Returned as the trait object the
/// publisher consumes.
pub fn app_state_mqtt_command_sink(state: Arc<AppState>) -> Arc<dyn crate::mqtt::MqttCommandSink> {
    Arc::new(AppStateMqttCommandSink { state })
}

// CE-052: `impl crate::mqtt::MqttCommandSink for AppStateMqttCommandSink` lives
// in `rest/late.rs` (a child module of `rest`, so it reaches these private
// items via `super::*`). It was relocated there to keep this file under the
// 10000-line CI gate after the CE-052 bridge capability guards were added.

/// CE-052: the MQTT `set_target_temp_c` thermal clamp stays anchored in THIS
/// file (not the relocated impl in `rest/late.rs`) so the safety-clamp manifest
/// keeps classifying it as `thermal|dcentrald/dcentrald-api/src/rest.rs`. The
/// clamp itself is unchanged — bounded under the dangerous threshold so a remote
/// MQTT setpoint can never disable cooling.
pub(super) fn mqtt_clamp_target_temp_c(requested_temp_c: f64) -> u8 {
    let clamped = (requested_temp_c.round() as i64).clamp(
        crate::mqtt::CMD_TARGET_TEMP_MIN_C as i64,
        crate::mqtt::CMD_TARGET_TEMP_MAX_C as i64,
    ) as u8;
    clamped
}

/// POST /api/pools/test -- non-persistent connectivity check for a pool endpoint.
async fn post_pools_test(Json(body): Json<PoolRequest>) -> impl IntoResponse {
    if let Err(message) = validate_pool_url_support(&body.url) {
        return pool_validation_error(message);
    }

    let (host, port) = match parse_pool_host_port(&body.url) {
        Ok(parts) => parts,
        Err(message) => {
            return pool_validation_error(message);
        }
    };

    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_stream)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "reachable": true,
                "message": format!("Reached {}:{}", host, port),
            })),
        ),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "status": "error",
                "reachable": false,
                "message": format!("Connection failed: {}", e),
            })),
        ),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "status": "error",
                "reachable": false,
                "message": format!("Timed out reaching {}:{}", host, port),
            })),
        ),
    }
    .into_response()
}

/// GET /api/config -- Current configuration.
async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    let miner = state.state_rx.borrow().clone();

    Json(serde_json::json!({
        "mode": {
            "active": mode,
        },
        "firmware_version": miner.firmware_version,
        "api": {
            "cgminer_port": state.config.cgminer_port,
            "http_port": state.config.http_port,
            "http_bind": state.config.http_bind.clone(),
            "websocket_enabled": state.config.websocket_enabled,
            "websocket_tickets": state.config.websocket_tickets,
            "auth_enabled": crate::auth::is_password_set(),
        },
    }))
}

async fn get_shared_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let table = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());
    Json(shared_config_snapshot(&state, &table, &miner, &hw))
}

fn merge_shared_config_patch_table(
    mut table: toml::Table,
    body: SharedConfigPatch,
) -> std::result::Result<toml::Table, String> {
    if let Some(network) = body.network {
        if network.ssid.is_some() || network.wifi_password.is_some() {
            return Err(
                "network.ssid and network.wifiPassword are not supported on DCENT_OS".to_string(),
            );
        }
        let general = table
            .entry("general".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut general_table) = general {
            if let Some(hostname) = network.hostname {
                general_table.insert("hostname".into(), toml::Value::String(hostname));
            }
        }
    }

    if let Some(primary_pool) = body.primary_pool {
        let pool = table
            .entry("pool".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut pool_table) = pool {
            if let Some(url) = primary_pool.url {
                validate_pool_url_support(&url)?;
                pool_table.insert("url".into(), toml::Value::String(url));
            }
            if let Some(port) = primary_pool.port {
                let host = pool_table
                    .get("url")
                    .and_then(|value| value.as_str())
                    .and_then(|url| url.rsplit_once(':').map(|(host, _)| host.to_string()));
                if let Some(host) = host {
                    pool_table.insert(
                        "url".into(),
                        toml::Value::String(format!("{}:{}", host, port)),
                    );
                }
            }
            if let Some(worker) = primary_pool.worker {
                pool_table.insert("worker".into(), toml::Value::String(worker));
            }
            if let Some(password) = primary_pool.password {
                pool_table.insert("password".into(), toml::Value::String(password));
            }
            if let Some(protocol) = primary_pool.protocol {
                pool_table.insert("protocol".into(), toml::Value::String(protocol));
            }
        }
    }

    if body.fallback_pool.is_some() {
        return Err("fallbackPool is not supported on DCENT_OS yet".to_string());
    }

    if let Some(mining) = body.mining {
        let mining_table = table
            .entry("mining".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut mining_section) = mining_table {
            if let Some(enabled) = mining.enabled {
                mining_section.insert("enabled".into(), toml::Value::Boolean(enabled));
            }
            if let Some(frequency) = mining.frequency_mhz {
                mining_section.insert(
                    "frequency_mhz".into(),
                    toml::Value::Integer(frequency.round() as i64),
                );
            }
            if let Some(voltage_mv) = mining.voltage_mv {
                mining_section.insert("voltage_mv".into(), toml::Value::Integer(voltage_mv as i64));
            }
        }
    }

    if let Some(thermal) = body.thermal {
        if thermal.manual_fan_speed_pct.is_some() {
            return Err("thermal.manualFanSpeedPct is not supported on DCENT_OS".to_string());
        }
        let thermal_table = table
            .entry("thermal".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut thermal_section) = thermal_table {
            if let Some(target_temp_c) = thermal.target_temp_c {
                thermal_section.insert(
                    "target_temp_c".into(),
                    toml::Value::Integer(target_temp_c as i64),
                );
            }
        }
    }

    if let Some(auth_patch) = body.auth {
        let api_table = table
            .entry("api".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut api_section) = api_table {
            if let Some(metrics_require_auth) = auth_patch.metrics_require_auth {
                api_section.insert(
                    "metrics_require_auth".into(),
                    toml::Value::Boolean(metrics_require_auth),
                );
            }
        }
    }

    validate_mining_write(&table)?;
    validate_imported_config_table(&table)?;
    Ok(table)
}

async fn post_shared_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SharedConfigPatch>,
) -> Response {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/v1/config")
    {
        return response;
    }

    let config_path = get_writable_config_path();
    let write_result = (|| -> std::result::Result<toml::Table, ConfigPersistenceError> {
        // RELIAB-2b: serialize load→modify→write (lost-update guard). Scoped to
        // this synchronous closure so it drops before any `.await`.
        let _cfg_write_guard = crate::atomic_io::config_write_lock();
        let table = load_config_table_for_write().map_err(ConfigPersistenceError::bad_request)?;
        let table = merge_shared_config_patch_table(table, body)
            .map_err(ConfigPersistenceError::bad_request)?;

        let output = toml::to_string_pretty(&table).map_err(|e| {
            ConfigPersistenceError::bad_request(format!("Failed to serialize config: {}", e))
        })?;
        atomic_write(config_path, output)
            .map_err(|e| ConfigPersistenceError::from_io("Failed to write config", e))?;
        Ok(table)
    })();

    match write_result {
        Ok(table) => {
            let miner = state.state_rx.borrow().clone();
            let hw = state
                .hardware_info
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "ok",
                    "config_path": config_path,
                    "config": shared_config_snapshot(&state, &table, &miner, &hw),
                })),
            )
                .into_response()
        }
        Err(error) => error.into_response(),
    }
}

/// POST /api/config -- Apply configuration changes.
///
/// Accepts partial config updates as JSON. Merges with the current TOML
/// config on disk and writes back. A daemon restart is required for most
/// changes to take effect.
///
/// Supports any top-level TOML keys: mode, pool, mining, thermal, api, led, etc.
/// Allowed top-level config keys that can be set via POST /api/config.
/// SECURITY (2026-04-11): Whitelist prevents injection of arbitrary keys
/// (e.g., `general.root_password`, `watchdog.disabled`).
const CONFIG_ALLOWED_KEYS: &[&str] = &[
    "general",
    "pool",
    "mining",
    "power",
    "thermal",
    "donation",
    "mode",
    "led",
    "autotuner",
    "api",
    "webhook",
];

mod capabilities;
mod late;
use capabilities::*;
// CE-052: re-export the narrow bridge guards so the daemon crate can call
// `dcentrald_api::rest::bridge_guard_{asic_options,identify}` without a
// `dcent_schema` capability dependency.
pub use capabilities::{bridge_guard_asic_options, bridge_guard_identify};
use late::*;
pub(crate) use late::{
    compute_commanded_fan_pwm, dispatch_autotuner_mode_command, onboarding_device_ready,
    onboarding_password_opt_out_active, persist_autotuner_mode, post_action_sleep,
    post_action_wake, post_config, post_fan, post_led_locate, post_profiles,
};
pub use late::{LedConfigUpdateRequest, LocateRequest};
