// DCENT_axe Shared State
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0

use crate::config::DcentAxeConfig;
use dcent_schema::mcp::MINIMAL_PROFILE_ID;
pub use dcent_schema::swarm::{
    SwarmCoordinationStatus, SwarmDiscoveryInfo as DiscoveryInfo, SwarmNode, SwarmRole, SwarmSource,
};
use dcentaxe_hal::board::BoardConfig;
use dcentaxe_mining::stats::{MiningStats, PoolStatsSnapshot};
use dcentaxe_stratum::{SharedStratumStatus, StratumEventRecord, StratumStatus};
use esp_idf_svc::nvs::{EspNvs, NvsDefault};
use esp_idf_svc::wifi::EspWifi;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Thread-safe shared state for cross-thread access.
///
/// The HTTP server, mining dispatcher, and main heartbeat loop all need
/// access to mining stats, config, and hardware telemetry.
///
/// MAINAPI-8 / COMP-5 — CANONICAL LOCK ORDER (deadlock avoidance):
/// when a code path must hold more than one of these `Mutex`es at once, acquire
/// them in this FIXED order and release in reverse:
///   1. `stats`
///   2. `config`
///   3. `telemetry`
///   4. `autotuner`
///   5. `stratum_status` / `pool_stats`
///   6. `swarm`
///   7. `history`
///   8. `nvs`
///   9. `wifi`
/// `/api/system/info` acquires stats -> config -> telemetry -> autotuner -> swarm
/// in this order. Prefer snapshot-then-drop (lock, clone the needed fields into
/// owned locals, drop the guard) over holding a guard across a long serialization.
/// Never acquire an earlier-listed lock while holding a later-listed one — that is
/// the lock-ordering inversion this table exists to prevent.
#[derive(Clone)]
pub struct SharedState {
    pub stats: Arc<Mutex<MiningStats>>,
    pub config: Arc<Mutex<DcentAxeConfig>>,
    pub telemetry: Arc<Mutex<Telemetry>>,
    pub autotuner: Arc<Mutex<AutotunerState>>,
    pub swarm: Arc<Mutex<SwarmState>>,
    pub board_limits: BoardLimits,
    /// Per-pool statistics for hashrate splitting (empty if single pool).
    pub pool_stats: Arc<Mutex<Vec<PoolStatsSnapshot>>>,
    /// Live per-client stratum runtime status handles.
    pub stratum_status: Arc<Mutex<Vec<SharedStratumStatus>>>,
    /// Shared NVS handle — singleton, taken once at boot.
    /// API/MCP handlers lock this to persist config changes.
    pub nvs: Arc<Mutex<Option<EspNvs<NvsDefault>>>>,
    /// Live WiFi handle so API endpoints can query connection state and scan.
    pub wifi: Arc<Mutex<Option<Box<EspWifi<'static>>>>>,
    /// Rolling miner history for API/MCP/dashboard use.
    pub history: Arc<Mutex<MinerHistoryState>>,
    /// Factory self-test runner + last report.
    pub self_test: Arc<crate::self_test::SelfTestRunner>,
}

/// Hardware safety limits (immutable after init).
#[derive(Debug, Clone)]
pub struct BoardLimits {
    pub min_voltage_mv: u16,
    pub max_voltage_mv: u16,
    pub min_frequency: f32,
    pub max_frequency: f32,
}

/// Per-chip telemetry for multi-chip boards (GT, Hex).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChipData {
    pub temp_c: Option<f32>,
    pub status: ChipStatus,
    pub hw_errors: u32,
    pub shares: u32,
    pub hashrate_ghs: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub enum ChipStatus {
    Active,
    Idle,
    Error,
    Unknown,
}

impl ChipData {
    pub fn status_str(&self) -> &'static str {
        match self.status {
            ChipStatus::Active => "active",
            ChipStatus::Idle => "idle",
            ChipStatus::Error => "error",
            ChipStatus::Unknown => "unknown",
        }
    }
}

impl Default for ChipData {
    fn default() -> Self {
        Self {
            temp_c: None,
            status: ChipStatus::Unknown,
            hw_errors: 0,
            shares: 0,
            hashrate_ghs: None,
        }
    }
}

/// Live hardware telemetry (updated by main heartbeat loop).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Telemetry {
    pub voltage_mv: f32,
    pub current_ma: f32,
    pub power_w: f32,
    pub input_voltage_mv: f32,
    pub chip_temp_c: f32,
    pub board_temp_c: f32,
    pub vreg_temp_c: f32,
    pub inlet_temp_c: f32,
    pub outlet_temp_c: f32,
    pub fan_speed_pct: u8,
    pub fan_rpm: u32,
    pub fan2_rpm: u32,
    pub uptime_secs: u64,
    pub free_heap: u32,
    pub wifi_rssi: i8,
    /// Device IP address from WiFi sta_netif
    pub device_ip: String,
    pub reset_reason: String,
    /// All-time best share difficulty (persisted to NVS across reboots)
    pub best_diff_ever: f64,
    /// Whether at least one temperature sensor is returning valid data
    pub sensors_ok: bool,
    /// Whether the stratum pool connection is active
    pub pool_connected: bool,
    /// Current mining difficulty set by the pool
    pub pool_difficulty: f64,
    /// Bitmask of unlocked achievements (persisted in NVS)
    pub achievements: u32,
    /// Number of unlocked achievements (count of set bits in `achievements`)
    pub achievement_count: u32,
    /// Lifetime accepted shares across all sessions (persisted in NVS)
    pub lifetime_shares: u32,
    /// Current evolution stage name (Egg, Hatchling, Miner, etc.)
    pub creature_stage: String,
    /// Current creature mood score 0–10 (0=sad, 10=ecstatic)
    pub creature_mood: u8,
    /// Whether mining is enabled (can be toggled via API without reboot for stop,
    /// reboot-required for restart)
    pub mining_enabled: bool,
    /// Per-chip telemetry for multi-chip boards (empty for single-chip boards).
    pub chip_data: Vec<ChipData>,
    /// Whether a panic coredump is currently stored in flash (see
    /// `/api/system/coredump`). Surfaced to dashboard + /api/system/info.
    pub coredump_present: bool,
    /// Whether the firmware booted into WDT safe mode because of repeated task
    /// watchdog resets. Mining is suppressed; dashboard + API stay up so the
    /// user can recover via `POST /api/system/clear-safe-mode`.
    pub safe_mode: bool,
    /// Task-watchdog reset counter within the current 300-second window.
    pub wdt_reset_count: u8,
    /// HALT-6 / XPSAFE-7: true when the board has NO fan-tach proof available
    /// (`tach_proof_required()==false`), so the FAN STALL kill is heuristic-only
    /// and the thermal ladder is the backstop. Surfaced so the dashboard/self-test
    /// does not present "fan healthy" as proven. Default true (conservative: no
    /// proof) until a tach-proof board clears it at boot.
    pub fan_proof_heuristic_only: bool,
    /// HALT-5: true on `emc_internal_temp` boards (Ultra 0.11/201-205 + Supra
    /// 400/401) where `chip_temp_c` is the EMC2101 INTERNAL die + a fixed offset
    /// used as a junction PROXY (board-ambient + offset), NOT a true junction-diode
    /// reading. Surfaced so the dashboard does not present it as true junction temp.
    /// The thermal cuts (105/95/90 C) and the offset are UNCHANGED — this is a
    /// label only, never a threshold change.
    pub chip_temp_is_ambient_proxy: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SwarmState {
    pub local: SwarmNode,
    pub peers: Vec<SwarmNode>,
    pub discovery: DiscoveryInfo,
    pub max_peers: usize,
    pub role: SwarmRole,
    pub cluster_id: Option<String>,
    pub queen_id: Option<String>,
    pub coordination: SwarmCoordinationStatus,
    pub observed_room_temp_c: Option<f32>,
    pub room_temp_source: Option<String>,
    pub room_temp_expires_epoch_s: Option<u64>,
    pub identify_until_epoch_s: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinerHistorySample {
    pub ts_unix_ms: u64,
    pub hashrate_ghs: f64,
    pub hashrate_15s_ghs: f64,
    pub hashrate_30s_ghs: f64,
    pub power_w: f32,
    pub temp_c: f32,
    pub local_accepted_shares: u64,
    pub local_rejected_shares: u64,
    pub submitted_shares: u64,
    pub pool_accepted_shares: u64,
    pub pool_rejected_shares: u64,
    pub response_time_ms: f64,
    pub failover_active: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinerHistoryState {
    pub samples: Vec<MinerHistorySample>,
    pub events: Vec<StratumEventRecord>,
}

pub fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            voltage_mv: 0.0,
            current_ma: 0.0,
            power_w: 0.0,
            input_voltage_mv: 0.0,
            chip_temp_c: 0.0,
            board_temp_c: 0.0,
            vreg_temp_c: 0.0,
            inlet_temp_c: 0.0,
            outlet_temp_c: 0.0,
            fan_speed_pct: 0,
            fan_rpm: 0,
            fan2_rpm: 0,
            uptime_secs: 0,
            free_heap: 0,
            wifi_rssi: 0,
            device_ip: String::new(),
            reset_reason: String::new(),
            best_diff_ever: 0.0,
            sensors_ok: false,
            pool_connected: false,
            pool_difficulty: 0.0,
            achievements: 0,
            achievement_count: 0,
            lifetime_shares: 0,
            creature_stage: "Egg".to_string(),
            creature_mood: 5,
            mining_enabled: true,
            chip_data: Vec::new(),
            coredump_present: false,
            safe_mode: false,
            wdt_reset_count: 0,
            fan_proof_heuristic_only: true,
            chip_temp_is_ambient_proxy: false,
        }
    }
}

/// Serde default for `AutotunerState::phase` (data-model-fields §7.4(b)): legacy
/// NVS-persisted autotuner states that predate the `phase` field load as "idle".
fn default_autotuner_phase() -> String {
    "idle".into()
}

/// Autotuner state (readable by API, writable by autotuner).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutotunerState {
    pub enabled: bool,
    pub mode: AutotuneMode,
    pub target_value: f32,
    pub current_frequency: f32,
    pub current_voltage_mv: u16,
    pub best_efficiency: f32,
    pub last_good_frequency: f32,
    pub last_good_voltage_mv: u16,
    pub last_good_jth: f32,
    pub last_good_error_rate: f32,
    pub silicon_grade: String,
    pub status: String,
    /// data-model-fields §7.4(b): stable autotuner-stage token, surfaced on the
    /// wire as `dcentaxe.autotuner.phase`. The human `status` string is freeform
    /// (e.g. "fine-tuning", "power limit", "wattage descent") and too lossy to
    /// cleanly derive the shared phase-ribbon rung from, so the autotuner engine
    /// writes its private `Phase` enum's `as_str()` here at every status update.
    /// Vocabulary: warmup|profiling|wattage_descent|optimizing|maintaining|idle.
    /// DEFAULT "idle" + `#[serde(default)]` ⇒ legacy NVS-persisted states load
    /// cleanly. NOT an AxeOS-compat top-level key — lives under `dcentaxe.*` only.
    #[serde(default = "default_autotuner_phase")]
    pub phase: String,
    /// XPAUTO-2 (cross-pollinated from DCENT_OS DpsWalker HealthBackoff):
    /// opt-in chip-health-aware backoff. When `true`, the autotuner retreats
    /// freq/voltage to the last-known-good point after a sustained rise in the
    /// HW-error rate (debounced over N consecutive ticks), taking precedence
    /// over the existing power/temp/hashrate-drift checks.
    ///
    /// DEFAULT-OFF (`#[serde(default)]` ⇒ legacy configs load `false`): a
    /// field-proven board's converged-point behavior changes ONLY when the
    /// operator opts in — exactly like DCENT_OS's `DCENT_BM139X_OPEN_CORE`
    /// gating. The backoff only ever retreats toward an already-proven point;
    /// it can never raise freq/voltage or lower a safety limit.
    #[serde(default)]
    pub health_backoff_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AutotuneMode {
    /// Maximize hashrate regardless of power
    MaxHashrate,
    /// Target a specific power consumption in watts
    TargetWatts,
    /// Minimize J/TH (best efficiency)
    BestEfficiency,
    /// Maximize hashrate while staying under temp target
    TargetTemp,
}

impl AutotuneMode {
    pub fn from_api_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "max_hashrate" => Some(Self::MaxHashrate),
            "target_watts" => Some(Self::TargetWatts),
            "best_efficiency" => Some(Self::BestEfficiency),
            "target_temp" => Some(Self::TargetTemp),
            _ => None,
        }
    }

    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::MaxHashrate => "max_hashrate",
            Self::TargetWatts => "target_watts",
            Self::BestEfficiency => "best_efficiency",
            Self::TargetTemp => "target_temp",
        }
    }
}

impl Default for AutotunerState {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: AutotuneMode::BestEfficiency,
            target_value: 0.0,
            current_frequency: 0.0,
            current_voltage_mv: 0,
            best_efficiency: 0.0,
            last_good_frequency: 0.0,
            last_good_voltage_mv: 0,
            last_good_jth: 0.0,
            last_good_error_rate: 0.0,
            silicon_grade: "unknown".into(),
            status: "idle".into(),
            phase: "idle".into(),
            health_backoff_enabled: false,
        }
    }
}

impl SharedState {
    pub fn new(
        config: DcentAxeConfig,
        board_config: &BoardConfig,
        nvs_handle: EspNvs<NvsDefault>,
    ) -> Self {
        let default_hostname = if config.hostname.is_empty() {
            format!("dcentaxe-{}", board_config.device_model)
        } else {
            config.hostname.clone()
        };
        let swarm = SwarmState {
            local: SwarmNode {
                id: format!(
                    "{}-{}",
                    board_config.device_model, board_config.board_version
                ),
                hostname: default_hostname.clone(),
                display_name: board_config.model.name().to_string(),
                ip: String::new(),
                board_model: board_config.device_model.clone(),
                board_version: board_config.board_version.clone(),
                board_target: board_config.model.board_target().to_string(),
                asic_model: board_config.asic_model.clone(),
                firmware_version: env!("CARGO_PKG_VERSION").to_string(),
                mining_enabled: true,
                pool_connected: false,
                hashrate_ghs: 0.0,
                last_seen_unix_ms: unix_time_ms(),
                source: SwarmSource::SelfReported,
            },
            peers: Vec::new(),
            discovery: DiscoveryInfo {
                mdns_enabled: false,
                mdns_hostname: Some(format!("{}.local", default_hostname)),
                discovery_hint: "mDNS not linked yet; use reported hostname/IP metadata"
                    .to_string(),
                api_url: None,
                mcp_url: None,
                mcp_transport: Some("http-jsonrpc".to_string()),
                mcp_profile: Some(MINIMAL_PROFILE_ID.to_string()),
            },
            max_peers: 8,
            role: SwarmRole::Standalone,
            cluster_id: None,
            queen_id: None,
            coordination: SwarmCoordinationStatus {
                report_endpoint: Some("/api/swarm/report".to_string()),
                ..SwarmCoordinationStatus::default()
            },
            observed_room_temp_c: None,
            room_temp_source: None,
            room_temp_expires_epoch_s: None,
            identify_until_epoch_s: 0,
        };
        let qualified_ceiling = config.qualify_operating_point(
            config.power_limits().max_frequency,
            config.power_limits().max_voltage_mv,
            crate::config::ControlSurface::BootRestore,
        );
        let board_limits = BoardLimits {
            min_voltage_mv: board_config.min_voltage_mv,
            max_voltage_mv: qualified_ceiling.voltage_mv,
            min_frequency: 50.0,
            max_frequency: qualified_ceiling.frequency_mhz,
        };
        Self {
            stats: Arc::new(Mutex::new(MiningStats::new())),
            config: Arc::new(Mutex::new(config)),
            telemetry: Arc::new(Mutex::new(Telemetry::default())),
            autotuner: Arc::new(Mutex::new(AutotunerState::default())),
            swarm: Arc::new(Mutex::new(swarm)),
            board_limits,
            pool_stats: Arc::new(Mutex::new(Vec::new())),
            stratum_status: Arc::new(Mutex::new(Vec::new())),
            nvs: Arc::new(Mutex::new(Some(nvs_handle))),
            wifi: Arc::new(Mutex::new(None)),
            history: Arc::new(Mutex::new(MinerHistoryState::default())),
            self_test: Arc::new(crate::self_test::SelfTestRunner::default()),
        }
    }
}

pub fn stratum_status_snapshots(state: &SharedState) -> Vec<StratumStatus> {
    stratum_status_snapshots_with_recent_event_limit(state, usize::MAX)
}

pub fn stratum_status_snapshots_with_recent_event_limit(
    state: &SharedState,
    recent_event_limit: usize,
) -> Vec<StratumStatus> {
    state
        .stratum_status
        .lock()
        .map(|statuses| {
            statuses
                .iter()
                .filter_map(|status| {
                    status.lock().ok().map(|snapshot| {
                        snapshot.snapshot_with_recent_event_limit(recent_event_limit)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StratumMetricsSnapshot {
    pub pool_index: u8,
    pub shares_pending: u32,
    pub shares_unresolved: u64,
    pub oldest_pending_submit_age_ms: u64,
}

pub fn stratum_metric_snapshots(state: &SharedState) -> Vec<StratumMetricsSnapshot> {
    state
        .stratum_status
        .lock()
        .map(|statuses| {
            statuses
                .iter()
                .filter_map(|status| {
                    status.lock().ok().map(|snapshot| StratumMetricsSnapshot {
                        pool_index: snapshot.pool_index,
                        shares_pending: snapshot.shares_pending,
                        shares_unresolved: snapshot.shares_unresolved,
                        oldest_pending_submit_age_ms: snapshot.oldest_pending_submit_age_ms,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Pool worker / URL read-surface masking (B-ESP-10) ───────────────────────
//
// MIRRORS the Antminer load-bearing rule
// : the pool
// `worker` is the operator's FULL BTC payout address on V1 solo, and a pool URL
// can embed `user:pass@` credentials. EVERY read surface that emits these must
// route through these helpers. The shapes match the canonical Antminer helpers
// EXACTLY so axe and Antminer redact identically:
//   - [`mask_wallet`] ≡ `dcentrald_common::wallet_mask::mask_wallet`
//     → `<first6>…<last4>` (U+2026 ellipsis); passthrough for inputs < 12 bytes.
//   - [`sanitize_pool_url`] ≡ `dcentrald_stratum::pool_api::sanitize_pool_url`
//     → strips the `user:pass@` authority, keeps `scheme://host[:port][/path]`.
//
// READ-ONLY: never call these on the config WRITE/edit path (the operator must
// be able to store a real worker/url). The `is_*_echo` helpers below make the
// read masking round-trip-safe (the dashboard form re-POSTs what it rendered).

/// Mask a pool worker (operator BTC payout address) for read surfaces.
/// `<first6>…<last4>` for inputs ≥ 12 bytes; shorter strings pass through.
pub fn mask_wallet(addr: &str) -> String {
    let bytes = addr.as_bytes();
    if bytes.len() < 12 {
        return addr.to_string();
    }
    // Wallet addresses are ASCII (bech32 / base58 / hex), so byte-slicing 6 from
    // the front and 4 from the back is safe; fall back to a char-based slice for
    // any non-ASCII input to avoid panicking on a UTF-8 boundary.
    if addr.is_ascii() {
        let prefix = &addr[..6];
        let suffix = &addr[addr.len() - 4..];
        return format!("{prefix}\u{2026}{suffix}");
    }
    let chars: Vec<char> = addr.chars().collect();
    if chars.len() < 12 {
        return addr.to_string();
    }
    let prefix: String = chars.iter().take(6).collect();
    let suffix: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{prefix}\u{2026}{suffix}")
}

/// Strip the `user:pass@` authority from a pool URL for read surfaces, keeping
/// `scheme://host[:port][/path]`. A URL without `://` passes through trimmed.
pub fn sanitize_pool_url(url: &str) -> String {
    let trimmed = url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(parts) => parts,
        None => return trimmed.to_string(),
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(authority_end);
    let authority = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    format!("{scheme}://{authority}{suffix}")
}

/// True when `incoming` is the read-masked echo of the stored `current` worker.
/// The config WRITE path uses this to KEEP the stored full BTC payout address
/// instead of clobbering it with its own mask when the dashboard form re-POSTs
/// the value it last rendered. A genuinely new (non-masked) worker still applies
/// — this is NOT masking on write.
pub fn is_masked_worker_echo(incoming: &str, current: &str) -> bool {
    !current.is_empty() && incoming == mask_wallet(current)
}

/// True when `incoming` is the read-sanitized echo of the stored `current` URL.
/// Same round-trip guard as [`is_masked_worker_echo`], for credential-bearing
/// pool URLs (so a re-saved sanitized URL does not strip stored `user:pass@`).
pub fn is_sanitized_url_echo(incoming: &str, current: &str) -> bool {
    !current.is_empty() && incoming == sanitize_pool_url(current)
}
