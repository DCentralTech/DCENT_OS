//! CGMiner-compatible TCP API on port 4028.
//!
//! Implements the CGMiner/BFGMiner API protocol for compatibility with:
//! - pyasic (Python ASIC management library, 400+ miner models)
//! - hass-miner (Home Assistant miner integration)
//! - CGMiner-compatible monitoring tools
//!
//! Protocol: TCP socket, newline-terminated JSON.
//! Request:  `{"command":"summary"}` or `summary` (plain text shorthand)
//! Response: JSON object with STATUS array and command-specific data.
//!
//! Supported commands:
//!   summary       - Overall miner summary (hashrate, uptime, temps)
//!   stats         - Detailed per-chain statistics
//!   pools         - Pool configuration and status
//!   devs          - Per-device (chain) data
//!   version       - Firmware and API version
//!   coin          - Current coin being mined
//!   config        - CGMiner configuration summary
//!   switchpool|N  - Switch to pool N as primary
//!   enablepool|N  - Enable pool N
//!   disablepool|N - Disable pool N
//!   addpool|url,user,pass - Add a new pool (acts: rewrites the pool set)
//!   restart       - Restart mining daemon (acts: respawns, keeps fan management)
//!   quit          - REFUSED (would strand the board with no fan manager — use SSH)
//!
//! pyasic polls every 10 seconds. Responses must complete within 5 seconds.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dcentrald_api_types::cgminer_status_codes::CgminerStatusCode;
use dcentrald_api_types::prometheus_metrics::hw_error_percent;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::{AppState, Result};

/// CGMiner API version string.
pub const API_VERSION: &str = "3.7";

/// CGMiner version string (presented as dcentrald).
/// Uses the crate version from Cargo.toml at compile time.
pub const CGMINER_VERSION: &str = concat!("dcentrald/", env!("CARGO_PKG_VERSION"));

/// Honest firmware-identity marker emitted in the `version` response.
///
/// pyasic / asic-rs classify the firmware STACK from the CGMiner
/// version object. DCENT_OS reports the real hardware MODEL (via
/// `Type`, so fleet tools size their per-board loops correctly) but
/// the firmware identity must stay honest: this is DCENT_OS /
/// `dcentrald`, NOT Antminer/BraiinsOS/LuxOS/VNish. Never claim to BE
/// another firmware — only to run on real Antminer HARDWARE.
pub const DCENTOS_FIRMWARE_MARKER: &str = "DCENTOS";

/// Resolve the real hardware MODEL string (e.g. "Antminer S19 Pro")
/// from the detected `HardwareInfo`, for the pyasic/asic-rs `Type`
/// token. Mirrors `rest::fleet_model_label`'s precedence (canonical
/// `MinerProfile` name → hashboard type → chip-type fallback) so the
/// CGMiner surface and the REST fleet surface agree on the model.
///
/// This identifies the HARDWARE only. Firmware identity stays honest
/// as DCENT_OS via `DCENTOS_FIRMWARE_MARKER` + `CGMINER_VERSION`.
fn hardware_model_label(hw: &crate::HardwareInfo) -> String {
    // Canonical chip-id → model name (same table rest.rs uses).
    let chip_id = match hw.chip_type.trim().to_ascii_uppercase().as_str() {
        "BM1387" => Some(0x1387u16),
        "BM1397" => Some(0x1397),
        "BM1398" => Some(0x1398),
        "BM1362" => Some(0x1362),
        "BM1366" => Some(0x1366),
        "BM1368" => Some(0x1368),
        "BM1370" => Some(0x1370),
        _ => None,
    };
    if let Some(profile) = chip_id.and_then(dcentrald_asic::drivers::MinerProfile::for_chip) {
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
        return format!("Antminer ({})", hw.chip_type.trim());
    }
    "Antminer (unknown ASIC)".to_string()
}

/// Maximum request size in bytes.
pub const MAX_REQUEST_SIZE: usize = 4096;

/// Maximum concurrent CGMiner TCP connections. The protocol is unauthenticated
/// and optionally LAN-exposed, so slow clients must not create unbounded tasks.
pub const MAX_CGMINER_CONNECTIONS: usize = 32;

/// Per-read idle timeout while waiting for one newline/EOF-delimited command.
pub const CGMINER_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum CgMinerRequestReadError {
    Timeout,
    TooLarge,
    Io(std::io::Error),
}

async fn read_cgminer_request<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::result::Result<Option<String>, CgMinerRequestReadError> {
    read_cgminer_request_with_timeout(reader, CGMINER_REQUEST_READ_TIMEOUT).await
}

async fn read_cgminer_request_with_timeout<R: AsyncRead + Unpin>(
    reader: &mut R,
    idle_timeout: Duration,
) -> std::result::Result<Option<String>, CgMinerRequestReadError> {
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 512];

    loop {
        if buf.contains(&b'\n') {
            break;
        }
        if buf.len() >= MAX_REQUEST_SIZE {
            return Err(CgMinerRequestReadError::TooLarge);
        }

        let remaining = MAX_REQUEST_SIZE - buf.len();
        let read_len = remaining.min(chunk.len());
        let n = tokio::time::timeout(idle_timeout, reader.read(&mut chunk[..read_len]))
            .await
            .map_err(|_| CgMinerRequestReadError::Timeout)?
            .map_err(CgMinerRequestReadError::Io)?;

        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    if buf.is_empty() {
        return Ok(None);
    }

    let end = buf
        .iter()
        .position(|b| *b == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(buf.len());
    Ok(Some(String::from_utf8_lossy(&buf[..end]).to_string()))
}

/// CGMiner API status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusCode {
    /// Success.
    S,
    /// Informational.
    I,
    /// Warning.
    W,
    /// Error.
    E,
    /// Fatal error.
    F,
}

/// CGMiner API status entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CgMinerStatus {
    /// Status code. The CGMiner :4028 contract names this inner field all-caps
    /// `STATUS` (cgminer-3.12.0 `_STATUS`); PascalCase would emit "Status", which
    /// pyasic/hass-miner read as `data["STATUS"][0]["STATUS"]` would miss — making
    /// every legacy read verb parse as errored. Force the canonical key.
    #[serde(rename = "STATUS")]
    pub status: String,
    /// Timestamp.
    pub when: u64,
    /// Status code number.
    pub code: i32,
    /// Human-readable message.
    pub msg: String,
    /// Description (API version string).
    pub description: String,
}

impl CgMinerStatus {
    /// Create a success status.
    pub fn success(msg: impl Into<String>) -> Self {
        Self {
            status: "S".to_string(),
            when: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            code: 0,
            msg: msg.into(),
            description: CGMINER_VERSION.to_string(),
        }
    }

    /// Create an error status.
    pub fn error(code: i32, msg: impl Into<String>) -> Self {
        Self {
            status: "E".to_string(),
            when: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            code,
            msg: msg.into(),
            description: CGMINER_VERSION.to_string(),
        }
    }

    /// Create a success status using the canonical
    /// `dcentrald_api_types::cgminer_status_codes::CgminerStatusCode`
    /// catalog ( wiring). Replaces the old code=0 default with
    /// the code that matches what cgminer 3.12.0 emits for the same
    /// command (Summary=11, Pool=7, Version=22, etc.) so pyasic /
    /// hass-miner / Awesome Miner read canonical values.
    pub fn success_for(code: CgminerStatusCode, msg: impl Into<String>) -> Self {
        Self {
            status: code.severity().letter().to_string(),
            when: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            code: code.code() as i32,
            msg: msg.into(),
            description: CGMINER_VERSION.to_string(),
        }
    }

    /// Create an error status using the canonical CGMiner code catalog.
    ///  wiring — replaces ad-hoc integer literals at error sites.
    pub fn error_for(code: CgminerStatusCode, msg: impl Into<String>) -> Self {
        Self {
            status: code.severity().letter().to_string(),
            when: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            code: code.code() as i32,
            msg: msg.into(),
            description: CGMINER_VERSION.to_string(),
        }
    }
}

/// Parsed CGMiner API command.
#[derive(Debug, Clone)]
pub struct CgMinerCommand {
    /// Command name (lowercase).
    pub command: String,
    /// Optional parameter (e.g., pool number, or url,user,pass).
    pub parameter: Option<String>,
}

impl CgMinerCommand {
    /// Parse a raw request string into a command.
    ///
    /// Supports two formats:
    /// - JSON: `{"command":"summary","parameter":""}`
    /// - Plain text: `summary` or `switchpool|0`
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim().trim_end_matches('\0');

        // Try JSON format first
        if trimmed.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                let command = json.get("command")?.as_str()?.to_lowercase();
                let parameter = json
                    .get("parameter")
                    .and_then(|p| p.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                return Some(Self { command, parameter });
            }
        }

        // Plain text format: command or command|parameter
        if let Some((cmd, param)) = trimmed.split_once('|') {
            Some(Self {
                command: cmd.to_lowercase(),
                parameter: Some(param.to_string()),
            })
        } else {
            Some(Self {
                command: trimmed.to_lowercase(),
                parameter: None,
            })
        }
    }
}

/// CGMiner TCP API server.
///
/// Listens on port 4028 and handles one command per connection
/// (CGMiner protocol is connect-send-receive-disconnect).
///
/// SECURITY (2026-04-11): Binds to 127.0.0.1 by default to prevent
/// hashrate theft via unauthenticated `addpool`/`switchpool` commands.
/// Set `cgminer_bind_lan = true` in [api] config to expose on LAN
/// (required for pyasic/hass-miner remote monitoring).
pub struct CgMinerServer {
    /// Shared application state.
    state: Arc<AppState>,
    /// Listening port.
    port: u16,
    /// Bind to LAN (0.0.0.0) instead of localhost (127.0.0.1).
    bind_lan: bool,
}

impl CgMinerServer {
    /// Create a new CGMiner API server.
    pub fn new(state: Arc<AppState>, port: u16) -> Self {
        let bind_lan = state.config.cgminer_bind_lan;
        Self {
            state,
            port,
            bind_lan,
        }
    }

    /// Start the CGMiner TCP listener.
    ///
    /// Spawns a Tokio task that accepts connections and dispatches
    /// commands. Each connection handles exactly one command.
    pub async fn run(&self) -> Result<()> {
        // SECURITY (2026-04-11): Default to localhost-only. The CGMiner protocol
        // has NO authentication — any LAN client can send addpool/switchpool to
        // redirect hashrate. Bind to 0.0.0.0 only when explicitly opted in via
        // `cgminer_bind_lan = true` for pyasic/hass-miner compatibility.
        let bind_addr = if self.bind_lan {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        };
        let listener = TcpListener::bind(format!("{}:{}", bind_addr, self.port))
            .await
            .map_err(|e| {
                crate::ApiError::CgMiner(format!(
                    "Failed to bind {}:{}: {}",
                    bind_addr, self.port, e
                ))
            })?;

        tracing::info!(port = self.port, %bind_addr, "CGMiner API server listening");

        let connection_limit = Arc::new(tokio::sync::Semaphore::new(MAX_CGMINER_CONNECTIONS));
        loop {
            let (mut socket, addr) = listener
                .accept()
                .await
                .map_err(|e| crate::ApiError::CgMiner(format!("Accept error: {}", e)))?;

            let state = self.state.clone();
            let Ok(permit) = connection_limit.clone().try_acquire_owned() else {
                tracing::warn!(
                    %addr,
                    limit = MAX_CGMINER_CONNECTIONS,
                    "CGMiner API connection refused: concurrent connection cap reached"
                );
                let _ = socket.shutdown().await;
                continue;
            };

            tokio::spawn(async move {
                let _permit = permit;
                tracing::debug!(%addr, "CGMiner API connection");

                match read_cgminer_request(&mut socket).await {
                    Ok(Some(raw)) => {
                        if let Some(cmd) = CgMinerCommand::parse(&raw) {
                            // API-1: thread the accepted peer through so mutating
                            // verbs from a non-loopback peer can be refused unless
                            // `cgminer_lan_writes` is set. Reads stay open.
                            let response = handle_command_from_peer(&state, &cmd, addr).await;
                            let json = serde_json::to_string(&response).unwrap_or_default();
                            let _ = socket.write_all(json.as_bytes()).await;
                        }
                    }
                    Ok(None) => {}
                    Err(CgMinerRequestReadError::Timeout) => {
                        tracing::warn!(%addr, "CGMiner API connection timed out waiting for request");
                    }
                    Err(CgMinerRequestReadError::TooLarge) => {
                        tracing::warn!(
                            %addr,
                            max_bytes = MAX_REQUEST_SIZE,
                            "CGMiner API request exceeded size limit"
                        );
                    }
                    Err(CgMinerRequestReadError::Io(error)) => {
                        tracing::debug!(%addr, error = %error, "CGMiner API read failed");
                    }
                }
            });
        }
    }

    /// Handle a single CGMiner command and return a JSON response.
    pub async fn handle(&self, cmd: &CgMinerCommand) -> serde_json::Value {
        handle_command_arc(&self.state, cmd).await
    }
}

/// API-1: is `name` a state-MUTATING CGMiner/LuxOS verb (as opposed to a
/// pure read, a telemetry projection, or a session-lifecycle gateway)?
///
/// "Mutating" = anything that changes pool set, tuning (voltage/frequency/
/// autotuner/power target/profile), fan, curtailment, network, LED, PSU, temp
/// control, OR daemon lifecycle (`restart`). This is the set that must be
/// loopback-only unless `cgminer_lan_writes` is enabled. Session lifecycle
/// (`logon`/`logoff`/`session`/`kill`) is NOT a state mutation, so it is not in
/// this set — but `kill` is still LAN-gated separately (see
/// [`is_lan_restricted_verb`]) because it force-evicts ANY controller's session.
///
/// We reuse the modeled `cgminer_luxos::requires_session` contract (every
/// session-gated LuxOS verb is a mutation) and add the legacy `cgminer.rs`
/// mutators that are not session-gated LuxOS verbs (`restart`). `quit` is
/// already refused unconditionally, so it is moot here but included for
/// clarity/defense-in-depth.
pub fn is_mutating_verb(name: &str) -> bool {
    // Session-lifecycle verbs (logon/logoff/session/kill) require a session
    // token but only manage the caller's OWN session — they do not mutate
    // mining/hardware/pool state, so they must stay open from LAN exactly like
    // reads/telemetry (otherwise the documented monitoring opt-in breaks).
    if matches!(name, "logon" | "logoff" | "session" | "kill") {
        return false;
    }
    crate::cgminer_luxos::requires_session(name) || matches!(name, "restart" | "quit")
}

/// API-1: verbs a NON-loopback peer may NOT issue unless `cgminer_lan_writes`
/// is set — the LAN gate's full restriction set. This is every state mutation
/// (`is_mutating_verb`) PLUS `kill`: `kill` force-evicts ANY LuxOS session
/// (including the local operator's), i.e. a remote denial-of-control, even
/// though it mutates no mining/hardware state. Reads/telemetry/`logon`/`logoff`/
/// `session` are NOT restricted (LAN monitoring + own-session lifecycle stay
/// open).
pub fn is_lan_restricted_verb(name: &str) -> bool {
    is_mutating_verb(name) || name == "kill"
}

/// API-1: may a peer issue a MUTATING verb?
///
/// `true` when the peer is loopback (local control is always allowed) OR when
/// `cgminer_lan_writes` is explicitly enabled. Pure helper so the
/// peer/loopback decision is unit-testable without a live socket. Reads are
/// never gated by this — only callers guarding mutations consult it.
pub fn lan_write_allowed(peer: &SocketAddr, lan_writes_enabled: bool) -> bool {
    peer.ip().is_loopback() || lan_writes_enabled
}

/// API-1: peer-aware entry. Refuses MUTATING verbs from a non-loopback peer
/// unless `cgminer_lan_writes` is set, then dispatches normally. Read-only and
/// telemetry verbs (summary/stats/pools/devs/version/metrics/…) are always
/// served, matching the documented LAN-monitoring opt-in. This is the entry
/// the TCP listener uses; `handle_command_arc` is the loopback-equivalent
/// convenience wrapper (and the existing test/in-process API).
pub async fn handle_command_from_peer(
    state: &Arc<AppState>,
    cmd: &CgMinerCommand,
    peer: SocketAddr,
) -> serde_json::Value {
    // Gate mutating/control verbs from a non-loopback peer (unless
    // cgminer_lan_writes). A `+`-batch MUST be checked over EVERY sub-command,
    // not the literal batch string: `summary+restart` would otherwise slip past
    // a whole-string check (the token "summary+restart" is no known verb), and
    // handle_command_arc's batch loop would then dispatch the mutating
    // `restart`. expand_batch() returns None for a non-batch command, so a
    // single verb is checked as-is.
    if !lan_write_allowed(&peer, state.config.cgminer_lan_writes) {
        let verbs = crate::cgminer_luxos::expand_batch(&cmd.command)
            .unwrap_or_else(|| vec![cmd.command.clone()]);
        if let Some(blocked) = verbs.iter().find(|v| is_lan_restricted_verb(v)) {
            return serde_json::json!({
                "STATUS": [CgMinerStatus::error_for(
                    CgminerStatusCode::AccessDenied,
                    format!(
                        "access denied: LAN writes disabled — '{}' is a mutating/control \
                         command and the peer is not loopback. Enable [api] \
                         cgminer_lan_writes=true on a trusted LAN, or issue control \
                         commands from the device itself.",
                        blocked
                    ),
                )],
                "id": 1
            });
        }
    }
    handle_command_arc(state, cmd).await
}

/// Top-level CGMiner dispatcher (Arc-aware).
///
/// Order of resolution:
/// 1. `+`-batched parameterless reads (LuxOS contract §2) — each
///    sub-command is dispatched independently and merged under one STATUS.
/// 2. The legacy 13-command read-only set (`summary`/`stats`/… unchanged).
/// 3. The LuxOS session model + mutating/telemetry contract
///    (`logon`/`voltageset`/`metrics`/…) — every mutating verb delegates
///    to the same gated `rest::` handler the dashboard calls. NO new
///    control/voltage/NAND path is introduced here.
///
/// SECURITY NOTE (API-1): this entry does NOT enforce the LAN-write gate — it
/// is the loopback-equivalent path used by in-process callers and tests. The
/// TCP listener calls [`handle_command_from_peer`], which refuses mutating
/// verbs from non-loopback peers unless `cgminer_lan_writes` is set.
pub async fn handle_command_arc(state: &Arc<AppState>, cmd: &CgMinerCommand) -> serde_json::Value {
    // 1. `+` batching — only parameterless commands may be batched.
    if let Some(parts) = crate::cgminer_luxos::expand_batch(&cmd.command) {
        if cmd.parameter.is_some() {
            return serde_json::json!({
                "STATUS": [CgMinerStatus::error_for(
                    CgminerStatusCode::InvalidCommand,
                    "Only parameterless commands can be batched with '+'",
                )],
                "id": 1
            });
        }
        // Batches are parameterless READS only (LuxOS contract §2). Reject any
        // batch that smuggles a mutating verb (e.g. `summary+restart`) so the
        // dispatch loop below can never reach a mutation regardless of caller —
        // defense-in-depth behind the peer gate in handle_command_from_peer.
        if let Some(mutating) = parts.iter().find(|p| is_mutating_verb(p)) {
            return serde_json::json!({
                "STATUS": [CgMinerStatus::error_for(
                    CgminerStatusCode::InvalidCommand,
                    format!(
                        "'{mutating}' is a mutating command and cannot be batched \
                         with '+' (batches are read-only)"
                    ),
                )],
                "id": 1
            });
        }
        let mut results = std::collections::HashMap::new();
        for part in parts {
            let sub = CgMinerCommand {
                command: part.clone(),
                parameter: None,
            };
            // Batched sub-commands are reads; recurse without re-batching.
            let v = dispatch_single(state, &sub).await;
            results.insert(part, v);
        }
        return crate::cgminer_luxos::merge_batch(results);
    }

    dispatch_single(state, cmd).await
}

/// Dispatch one (already batch-expanded) command.
async fn dispatch_single(state: &Arc<AppState>, cmd: &CgMinerCommand) -> serde_json::Value {
    // Pure read-only verbs keep their EXACT legacy behavior/shape so
    // existing pyasic/hass-miner consumers are byte-unaffected.
    match cmd.command.as_str() {
        "summary" | "stats" | "pools" | "devs" | "version" | "coin" | "config" | "restart"
        | "quit" | "devdetails" | "asccount" | "check" | "notify" => {
            handle_command(state.as_ref(), cmd).await
        }
        // Pool-mutation + every LuxOS session/mutating/telemetry verb goes
        // through the LuxOS layer, which session-gates mutations and
        // delegates to the same gated rest:: handlers the dashboard uses.
        other if crate::cgminer_luxos::is_luxos_command(other) => {
            crate::cgminer_luxos::handle_luxos_command(state, cmd).await
        }
        _ => {
            serde_json::json!({
                "STATUS": [CgMinerStatus::error_for(
                    CgminerStatusCode::InvalidCommand,
                    format!("Invalid command: {}", cmd.command),
                )],
                "id": 1
            })
        }
    }
}

/// Handle a CGMiner command and produce a JSON response.
async fn handle_command(state: &AppState, cmd: &CgMinerCommand) -> serde_json::Value {
    match cmd.command.as_str() {
        "summary" => handle_summary(state).await,
        "stats" => handle_stats(state).await,
        "pools" => handle_pools(state).await,
        "devs" => handle_devs(state).await,
        "version" => handle_version(state).await,
        "coin" => handle_coin(state).await,
        "config" => handle_config(state).await,
        "devdetails" => handle_devdetails(state).await,
        "asccount" => handle_asccount(state).await,
        "check" => handle_check(state, &cmd.parameter).await,
        "notify" => handle_notify(state).await,
        "switchpool" => handle_switchpool(state, &cmd.parameter).await,
        "enablepool" => handle_enablepool(state, &cmd.parameter).await,
        "disablepool" => handle_disablepool(state, &cmd.parameter).await,
        "addpool" => handle_addpool(state, &cmd.parameter).await,
        "restart" => handle_restart(state).await,
        "quit" => handle_quit(state).await,
        _ => {
            serde_json::json!({
                "STATUS": [CgMinerStatus::error_for(
                    CgminerStatusCode::InvalidCommand,
                    format!("Invalid command: {}", cmd.command),
                )],
                "id": 1
            })
        }
    }
}

/// Handle the `summary` command.
///
/// Returns overall miner summary compatible with pyasic/CGMiner format.
async fn handle_summary(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let hardware_errors: u64 = miner.chains.iter().map(|chain| chain.errors as u64).sum();
    // P1-5 (D-15): real cgminer `Best Share` from the daemon's recorded
    // achieved difficulties (max over the recent share-history window — see
    // best_achieved_share_difficulty). 0 = no proven share yet, not "broken".
    let best_share = best_achieved_share_difficulty(
        &state
            .recent_share_history
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default(),
    );

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Summary, "Summary")],
        "SUMMARY": [build_summary_object(
            miner.accepted,
            miner.rejected,
            hardware_errors,
            miner.uptime_s,
            miner.hashrate_ghs,
            miner.hashrate_5s_ghs,
            best_share,
        )],
        "id": 1
    })
}

/// SW-12: assemble the CGMiner `SUMMARY` object from already-projected scalar
/// telemetry, with no `AppState` / HAL coupling — the same pure-fn + host-test
/// pattern as [`build_bmminer_stats_object`]. This host-pins the whole summary
/// honesty contract:
///   - `Device Hardware%` is the REAL hardware-error percent via
///     [`hw_error_percent`] (single-sourced with the Prometheus
///     `dcentrald_hw_error_rate` gauge), NOT a fabricated `0.0`. Because it is
///     now a genuinely-supported, computed field, it is correctly ABSENT from
///     `_DCENTUnsupported`.
///   - `Utility` = accepted/(uptime_minutes), guarded against `uptime_s == 0`.
///   - `_DCENTUnsupported` lists exactly the placeholder/zero fields the daemon
///     does not yet source.
fn build_summary_object(
    accepted: u64,
    rejected: u64,
    hw_errors: u64,
    uptime_s: u64,
    hashrate_ghs: f64,
    hashrate_5s_ghs: f64,
    best_share: f64,
) -> serde_json::Value {
    serde_json::json!({
        "Elapsed": uptime_s,
        // TEL-3 (2026-06-20): of the five MHS windows, only "MHS 5s" has a
        // distinct (5-second) source. "MHS 1m"/"5m"/"15m" approximate the
        // labeled window with the lifetime average (`hashrate_ghs`) — this
        // matches CGMiner-compat consumer expectations (pyasic/Foreman) and
        // the value is a real average, not fabricated. Genuinely
        // time-windowed averages are available on the native surfaces: the
        // REST `/api/history` window helper and the Prometheus 15m/24h
        // gauges (which stay None until their ring fills, never faked).
        "MHS av": hashrate_ghs * 1000.0,
        "MHS 5s": hashrate_5s_ghs * 1000.0,
        "MHS 1m": hashrate_ghs * 1000.0,
        "MHS 5m": hashrate_ghs * 1000.0,
        "MHS 15m": hashrate_ghs * 1000.0,
        "Found Blocks": 0,
        "Getworks": 0,
        "Accepted": accepted,
        "Rejected": rejected,
        "Hardware Errors": hw_errors,
        "Utility": if uptime_s > 0 {
            accepted as f64 / (uptime_s as f64 / 60.0)
        } else { 0.0 },
        "Discarded": 0,
        "Stale": 0,
        "Get Failures": 0,
        "Local Work": 0,
        "Remote Failures": 0,
        "Network Blocks": 0,
        "Total MH": hashrate_ghs * 1000.0 * uptime_s as f64,
        "Work Utility": 0.0,
        "Difficulty Accepted": accepted as f64,
        "Difficulty Rejected": rejected as f64,
        "Difficulty Stale": 0.0,
        "Best Share": best_share,
        // Real hardware-error rate (0..100 percent), single-sourced with the
        // Prometheus `dcentrald_hw_error_rate` gauge via `hw_error_percent`.
        // 0.0 ONLY when there is no work yet OR the boards are genuinely
        // error-free — never a fabricated "perfect health" placeholder.
        "Device Hardware%": hw_error_percent(hw_errors, accepted, rejected),
        "Device Rejected%": if accepted + rejected > 0 {
            rejected as f64 / (accepted + rejected) as f64 * 100.0
        } else { 0.0 },
        "Pool Rejected%": 0.0,
        "Pool Stale%": 0.0,
        "Last getwork": 0,
        "_DCENTUnsupported": [
            "Found Blocks",
            "Getworks",
            "Discarded",
            "Stale",
            "Get Failures",
            "Local Work",
            "Remote Failures",
            "Network Blocks",
            "Work Utility",
            "Difficulty Stale",
            "Pool Rejected%",
            "Pool Stale%",
            "Last getwork"
        ],
        "_DCENTFieldSources": {
            "Hardware Errors": "sum(chains.errors)",
            "Accepted": "miner_state.accepted",
            "Rejected": "miner_state.rejected",
            "Device Hardware%": "hw_error_percent(hw_errors, accepted, rejected) — hw_errors / (accepted + rejected + hw_errors) * 100; single-sourced with Prometheus dcentrald_hw_error_rate",
            "Difficulty Accepted": "accepted_share_count_compat",
            "Difficulty Rejected": "rejected_share_count_compat",
            "Best Share": "max(recent_share_history[].achieved_difficulty) — best locally-proven achieved difficulty in the recent share window (0 = none yet, never the pool target)",
            "Total MH": "hashrate_ghs × 1000 × uptime_s — APPROXIMATION from instantaneous hashrate × elapsed, NOT true cumulative work (drifts after any ramp/tune/gap or restart)"
        }
    })
}

/// SW-12: minimal per-chain projection used by the pyasic-compat stats
/// builder. Mirrors `crate::ChainState`'s fields we surface (id / chips /
/// frequency / voltage / temp / hashrate / errors / status) so the
/// flattened BMMiner-stats object can be assembled in a host test WITHOUT
/// constructing a HAL-bound `AppState`.
#[derive(Debug, Clone)]
struct CgStatsChain {
    id: u8,
    chips: u8,
    frequency_mhz: u16,
    voltage_mv: u16,
    temp_c: f32,
    hashrate_ghs: f64,
    errors: u32,
    status: String,
}

/// P1-5 (D-14): project the real per-fan RPM list from the live `FanState`
/// for the bmminer `fan_num` / `fan{N}` keys pyasic / Foreman read.
///
/// pyasic sizes its fan loop from `fan_num` and reads `fan1..fan{fan_num}`
/// RPM. Before this projection the cgminer surface hardcoded `fan_num = 0`, so
/// every fleet dashboard saw DCENT_OS as a fanless / fan-failed miner even
/// though `/api/status` reports live fan RPM. We mirror EXACTLY what the
/// daemon already monitors: the per-fan readings when present (one entry per
/// monitored fan header, RPM reported honestly — including 0 for a dead/unspun
/// fan), falling back to the legacy single primary-tach RPM when the per-fan
/// vector is empty. Reporting-only: no fan/thermal control effect.
fn project_fan_rpms(fans: &crate::FanState) -> Vec<u32> {
    if !fans.per_fan.is_empty() {
        fans.per_fan.iter().map(|reading| reading.rpm).collect()
    } else if fans.rpm > 0 {
        // Legacy single-tach path (no per-fan breakdown available).
        vec![fans.rpm]
    } else {
        Vec::new()
    }
}

/// P1-5 (D-15): best (max) locally-proven achieved share difficulty across the
/// recent share-history window — the honest cgminer `Best Share` value.
///
/// cgminer's `Best Share` is the single best difficulty a miner has actually
/// proven, NOT a sum of pool-target credit and NOT the pool's current target.
/// DCENT_OS already records each share's locally-computed achieved difficulty
/// in `recent_share_history` (proven from the exact accepted header/hash); the
/// cgminer shim discarded it and reported 0, making fleet tools believe the
/// miner had never found a share. We take the max over accepted/lucky events
/// that carry a finite, positive locally-proven difficulty. Returns 0.0 when
/// no such share exists yet (cgminer convention: 0 = none).
///
/// Provenance note: this is bounded to the `recent_share_history` window (the
/// same source `/api/mining/work/posture` uses), so it is "best in the recent
/// share window", reported honestly in `_DCENTFieldSources`. It NEVER
/// fabricates a value from the pool target, and rejected shares never count.
fn best_achieved_share_difficulty(events: &[crate::RecentShareEvent]) -> f64 {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.result.to_ascii_lowercase().as_str(),
                "accepted" | "lucky"
            )
        })
        .filter_map(|event| event.difficulty)
        .filter(|difficulty| difficulty.is_finite() && *difficulty > 0.0)
        .fold(0.0_f64, f64::max)
}

/// SW-12: build the pyasic-canonical, flattened BMMiner-shape `stats[0]`
/// object.
///
/// pyasic's Antminer (BMMiner/CGMiner) data extractors read per-board
/// telemetry from a SINGLE flattened STATS object using the historical
/// `bmminer` key family — NOT from per-chain STATS rows. Specifically pyasic
/// looks for `chain_rate{N}` (per-board GH/s), `chain_acn{N}` (responding
/// chips), `temp{N}` (PCB temp), `temp2_{N}` / `temp_chip{N}` (chip temp),
/// `freq_avg`, and `total_rate`. Without these keys pyasic reports `0`
/// boards / `None` temps for DCENT_OS even though our per-chain rows carry
/// the data, so fleet tools mis-parse the miner.
///
/// This object is **strictly additive** — the existing per-chain STATS rows
/// (with their `_DCENTFieldSources` provenance) are preserved and appended
/// after it. Boards are 1-indexed to match the bmminer convention pyasic
/// parses. Reporting-only: no mining / power / thermal effect.
///
/// `fan_rpms` is the projected live per-fan RPM list (see [])
/// surfaced as the bmminer `fan_num` + 1-indexed `fan{N}` keys.
fn build_bmminer_stats_object(
    chains: &[CgStatsChain],
    uptime_s: u64,
    fan_rpms: &[u32],
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("STATS".into(), serde_json::json!(0));
    obj.insert("ID".into(), serde_json::json!("BMMiner0"));
    obj.insert("Type".into(), serde_json::json!("DCENTos"));
    obj.insert("Elapsed".into(), serde_json::json!(uptime_s));

    let board_count = chains.len();
    obj.insert("miner_count".into(), serde_json::json!(board_count));
    // P1-5 (D-14): bmminer fan-count + per-fan RPM keys pyasic / Foreman read
    // to size their fan loop. Projected from the daemon's live per-fan
    // telemetry (the same `fans` surfaced by `/api/status`). Before this,
    // `fan_num` was hardcoded 0, so fleet tools flagged DCENT_OS as fanless /
    // fan-failed even with fans spinning. 1-indexed `fan{N}` matches the
    // bmminer convention pyasic parses.
    obj.insert("fan_num".into(), serde_json::json!(fan_rpms.len()));
    for (idx, &rpm) in fan_rpms.iter().enumerate() {
        obj.insert(format!("fan{}", idx + 1), serde_json::json!(rpm));
    }
    obj.insert("temp_num".into(), serde_json::json!(board_count));

    let total_rate_ghs: f64 = chains.iter().map(|c| c.hashrate_ghs).sum();
    // `GHS 5s` / `GHS av` are the bmminer summary-rate keys; `total_rate` is
    // the pyasic-preferred per-summary key (GH/s).
    obj.insert(
        "GHS 5s".into(),
        serde_json::json!(format!("{:.2}", total_rate_ghs)),
    );
    obj.insert("GHS av".into(), serde_json::json!(total_rate_ghs));
    obj.insert("total_rate".into(), serde_json::json!(total_rate_ghs));

    // freq_avg = mean of per-board commanded frequency (0 when no boards).
    let freq_avg = if board_count > 0 {
        chains.iter().map(|c| c.frequency_mhz as f64).sum::<f64>() / board_count as f64
    } else {
        0.0
    };
    obj.insert("frequency".into(), serde_json::json!(freq_avg));
    obj.insert("freq_avg".into(), serde_json::json!(freq_avg));

    let mut temp_max: f32 = 0.0;
    for (idx, chain) in chains.iter().enumerate() {
        // 1-indexed boards per the bmminer convention pyasic parses.
        let n = idx + 1;
        // Per-board responding-chip count + hashrate (GH/s).
        obj.insert(format!("chain_acn{}", n), serde_json::json!(chain.chips));
        obj.insert(
            format!("chain_rate{}", n),
            serde_json::json!(format!("{:.2}", chain.hashrate_ghs)),
        );
        // Per-board frequency + voltage (V) — pyasic FrequencyData / VoltageData.
        obj.insert(format!("freq{}", n), serde_json::json!(chain.frequency_mhz));
        obj.insert(
            format!("chain_voltage{}", n),
            serde_json::json!(chain.voltage_mv as f64 / 1000.0),
        );
        // Per-board CRC/HW errors + chain-alive map character.
        obj.insert(format!("chain_hw{}", n), serde_json::json!(chain.errors));
        obj.insert(
            format!("chain_xtime{}", n),
            serde_json::json!(if chain.chips > 0 { "o" } else { "x" }),
        );
        // PCB temp (`temp{N}`) + chip temp (`temp2_{N}` / `temp_chip{N}`).
        // DCENT_OS reports a single per-chain temperature; surface it under
        // both key families so pyasic's PCB-vs-chip readers both resolve.
        obj.insert(format!("temp{}", n), serde_json::json!(chain.temp_c));
        obj.insert(format!("temp2_{}", n), serde_json::json!(chain.temp_c));
        obj.insert(format!("temp_chip{}", n), serde_json::json!(chain.temp_c));
        if chain.temp_c > temp_max {
            temp_max = chain.temp_c;
        }
    }
    obj.insert("temp_max".into(), serde_json::json!(temp_max));

    obj.insert(
        "_DCENTFieldSources".into(),
        serde_json::json!({
            "chain_acn{N}": "chains[N-1].chips",
            "chain_rate{N}": "chains[N-1].hashrate_ghs",
            "freq{N}": "chains[N-1].frequency_mhz",
            "chain_voltage{N}": "chains[N-1].voltage_mv",
            "temp{N}/temp2_{N}/temp_chip{N}": "chains[N-1].temp_c (single per-chain sensor)",
            "freq_avg": "mean(chains.frequency_mhz)",
            "total_rate": "sum(chains.hashrate_ghs) GH/s",
            "fan_num/fan{N}": "miner_state.fans.per_fan[].rpm (live per-fan telemetry; legacy single primary-tach RPM fallback)"
        }),
    );
    obj.insert(
        "_DCENTNote".into(),
        serde_json::json!(
            "Flattened bmminer-shape board telemetry for pyasic. DCENT_OS reports one \
             temperature per board (no separate PCB/chip sensors), so temp{N}/temp2_{N}/\
             temp_chip{N} carry the same value."
        ),
    );

    serde_json::Value::Object(obj)
}

/// Handle the `stats` command.
///
/// Returns detailed per-chain statistics compatible with pyasic. The first
/// STATS object is the pyasic-canonical flattened bmminer-shape board
/// telemetry (SW-12); the per-chain rows follow it for tools that prefer the
/// per-row view.
async fn handle_stats(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();

    let projected: Vec<CgStatsChain> = miner
        .chains
        .iter()
        .map(|chain| CgStatsChain {
            id: chain.id,
            chips: chain.chips,
            frequency_mhz: chain.frequency_mhz,
            voltage_mv: chain.voltage_mv,
            temp_c: chain.temp_c,
            hashrate_ghs: chain.hashrate_ghs,
            errors: chain.errors,
            status: chain.status.clone(),
        })
        .collect();

    // SW-12: flattened bmminer-shape object FIRST (pyasic reads board
    // telemetry from this single object via the `chain_*{N}` / `temp*{N}`
    // key family), then the legacy per-chain rows.
    // P1-5 (D-14): project live per-fan RPM into the bmminer `fan_num`/`fan{N}`.
    let fan_rpms = project_fan_rpms(&miner.fans);
    let mut stats = vec![build_bmminer_stats_object(
        &projected,
        miner.uptime_s,
        &fan_rpms,
    )];
    for chain in &projected {
        stats.push(build_cg_chain_stats_row(chain, miner.uptime_s));
    }

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::MineStats, "Stats")],
        "STATS": stats,
        "id": 1
    })
}

/// Build a single legacy per-chain STATS row (the per-row view some fleet tools
/// prefer over the flattened bmminer object).
///
/// API-3 ( truthfulness): DCENT_OS does NOT track accepted/rejected shares
/// per chain — share accounting is pool-session-level (one active Stratum session
/// for the whole miner, see `MinerState.accepted`/`rejected`). The per-chain
/// `Accepted`/`Rejected` counters are STRUCTURALLY UNAVAILABLE, not measured-zero.
/// They stay 0 for cgminer schema shape but are flagged in `_DCENTUnsupported` +
/// `_DCENTFieldSources` so fleet tools don't read a fabricated 0 as a real
/// per-chain accept/reject count.
fn build_cg_chain_stats_row(chain: &CgStatsChain, uptime_s: u64) -> serde_json::Value {
    serde_json::json!({
        "STATS": chain.id,
        "ID": format!("CHAIN{}", chain.id),
        "Elapsed": uptime_s,
        "MHS av": chain.hashrate_ghs * 1000.0,
        "MHS 5s": chain.hashrate_ghs * 1000.0,
        "Accepted": 0,
        "Rejected": 0,
        "Hardware Errors": chain.errors,
        "chain_acn": chain.chips,
        "frequency": chain.frequency_mhz,
        "temp": chain.temp_c,
        "temp_chip": chain.temp_c,
        "voltage": chain.voltage_mv as f64 / 1000.0,
        "status": chain.status,
        "_DCENTUnsupported": ["Accepted", "Rejected"],
        "_DCENTFieldSources": {
            "Accepted": "not tracked per chain (share accounting is pool-session-level: MinerState.accepted) — 0 is a schema placeholder, not a measured count",
            "Rejected": "not tracked per chain (share accounting is pool-session-level: MinerState.rejected) — 0 is a schema placeholder, not a measured count",
            "Hardware Errors": "chain.errors",
            "chain_acn": "chain.chips",
            "frequency": "chain.frequency_mhz",
            "temp": "chain.temp_c",
            "voltage": "chain.voltage_mv"
        }
    })
}

/// A single configured pool entry read from the daemon's TOML config.
///
/// Mirrors `rest::ConfiguredPoolInfo` (private to rest.rs) so the CGMiner
/// `pools` surface can emit the FULL configured pool set — fleet tools
/// (pyasic / Foreman / Awesome Miner) expect every configured pool with a
/// per-pool Priority/Status, not just the active one.
#[derive(Debug, Clone)]
struct CgConfiguredPool {
    url: String,
    worker: String,
    priority: u8,
}

/// Read configured pools (primary + failover1 + failover2) from the active
/// daemon config. Same `[pool]` / `[pool.failover1]` / `[pool.failover2]`
/// layout `rest::read_configured_pools` parses. Returns an empty vec on any
/// read/parse miss (the caller then falls back to live-state-only).
fn cg_read_configured_pools() -> Vec<CgConfiguredPool> {
    let Ok(contents) = std::fs::read_to_string(crate::rest::get_config_path()) else {
        return Vec::new();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&contents) else {
        return Vec::new();
    };
    let Some(pool) = table.get("pool").and_then(|v| v.as_table()) else {
        return Vec::new();
    };

    fn one(t: &toml::Table, priority: u8) -> Option<CgConfiguredPool> {
        let url = t.get("url")?.as_str()?.trim().to_string();
        if url.is_empty() {
            return None;
        }
        Some(CgConfiguredPool {
            url,
            worker: t
                .get("worker")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            priority,
        })
    }

    let mut pools = Vec::new();
    if let Some(primary) = one(pool, 0) {
        pools.push(primary);
    }
    if let Some(f1) = pool
        .get("failover1")
        .and_then(|v| v.as_table())
        .and_then(|t| one(t, 1))
    {
        pools.push(f1);
    }
    if let Some(f2) = pool
        .get("failover2")
        .and_then(|v| v.as_table())
        .and_then(|t| one(t, 2))
    {
        pools.push(f2);
    }
    pools
}

/// Build a single POOL object for the CGMiner `pools` array.
///
/// `is_active` decides which pool carries the live share/difficulty stats
/// from `MinerState` (the runtime only tracks ONE active session). Inactive
/// configured pools are reported as standby with zeroed counters but a
/// truthful URL/Priority/User so fleet tools see the full failover set.
fn build_cg_pool_entry(
    index: usize,
    url: &str,
    worker: &str,
    priority: u8,
    status: &str,
    is_active: bool,
    miner: &crate::MinerState,
    best_share: f64,
) -> serde_json::Value {
    // SEC (W20 / parity #66): strip inline stratum credentials from EVERY
    // cgminer pool entry — active, fallback, AND configured/failover (the
    // multi-pool path). The cgminer API is LAN-exposed when
    // cgminer_bind_lan=true. Centralized here so all call sites are covered;
    // idempotent (a clean URL is returned unchanged).
    let url = dcentrald_stratum::pool_api::sanitize_pool_url(url);
    let url = url.as_str();
    // TEL-4 ( masking): the cgminer `User` field is the pool worker, which
    // for V1 solo is the operator's full BTC payout address. The Prometheus
    // exporter already masks this identical value; the cgminer API is LAN-exposed
    // when cgminer_bind_lan=true, so it MUST mask too. `mask_wallet` is a no-op on
    // short non-wallet workers (e.g. "rig.1").
    let masked_worker = dcentrald_common::wallet_mask::mask_wallet(worker);
    let worker = masked_worker.as_str();
    let (accepted, rejected, last_share_at, difficulty) = if is_active {
        (
            miner.accepted,
            miner.rejected,
            miner.pool.last_share_at,
            miner.pool.difficulty,
        )
    } else {
        (0, 0, 0, 0.0)
    };
    let stratum_active = is_active && is_pool_alive(status);

    // P1-5 (D-15): the active session carries the real best achieved difficulty;
    // standby pools have no achieved-difficulty of their own (the runtime tracks
    // ONE active session), so they honestly report 0 and keep "Best Share" in
    // their unsupported list.
    let pool_best_share = if is_active { best_share } else { 0.0 };
    let mut unsupported = vec![
        "Getworks",
        "Works",
        "Discarded",
        "Stale",
        "Get Failures",
        "Remote Failures",
        "Diff1 Shares",
        "Difficulty Stale",
        "Has GBT",
        "Last Share Difficulty",
        "Pool Rejected%",
        "Pool Stale%",
    ];
    if !is_active {
        unsupported.push("Best Share");
    }

    serde_json::json!({
        "POOL": index,
        "URL": url,
        "Status": status,
        "Priority": priority,
        "Quota": 1,
        "Long Poll": "N",
        "Getworks": 0,
        "Accepted": accepted,
        "Rejected": rejected,
        "Works": 0,
        "Discarded": 0,
        "Stale": 0,
        "Get Failures": 0,
        "Remote Failures": 0,
        "User": worker,
        "Last Share Time": last_share_at,
        "Diff1 Shares": 0,
        "Proxy Type": "",
        "Proxy": "",
        "Difficulty Accepted": accepted as f64,
        "Difficulty Rejected": rejected as f64,
        "Difficulty Stale": 0.0,
        "Last Share Difficulty": 0.0,
        "Work Difficulty": difficulty,
        "Has Stratum": true,
        "Stratum Active": stratum_active,
        "Stratum URL": url,
        "Stratum Difficulty": difficulty,
        "Has GBT": false,
        "Best Share": pool_best_share,
        "Pool Rejected%": 0.0,
        "Pool Stale%": 0.0,
        "_DCENTUnsupported": unsupported,
        "_DCENTFieldSources": {
            "Accepted": if is_active { "miner_state.accepted (active pool only)" } else { "0 (standby pool — runtime tracks one active session)" },
            "Rejected": if is_active { "miner_state.rejected (active pool only)" } else { "0 (standby pool)" },
            "User": "config[pool*].worker (masked via wallet_mask::mask_wallet — TEL-4)",
            "Priority": "config[pool*] slot (0=primary,1=failover1,2=failover2)",
            "Status": "active=miner_state.pool.status; standby=\"Standby\"",
            "Work Difficulty": "miner_state.pool.difficulty_current_pool_target (active only)",
            "Best Share": if is_active { "max(recent_share_history[].achieved_difficulty) — active session best (0 = none yet)" } else { "0 (standby pool — no achieved difficulty of its own)" }
        }
    })
}

/// CGMiner pool-status convention: "Alive" = pool responding.
fn is_pool_alive(status: &str) -> bool {
    status.eq_ignore_ascii_case("alive")
}

/// Handle the `pools` command.
///
/// Returns the FULL configured pool set (primary + failover1 + failover2),
/// one POOL entry each with per-pool Priority/Status — fleet tools (pyasic /
/// Foreman / Awesome Miner) read the whole POOLS list to render failover.
/// Before this fix only the single active pool was emitted, so multi-pool
/// failover was invisible on the cgminer-compat surface.
///
/// The active pool (matched by URL against the live `MinerState`, falling
/// back to the failover/split `active_pool_index`) carries the live
/// share/difficulty stats; standby pools report truthful URL/Priority/User
/// with zeroed counters. If the config can't be read, we fall back to the
/// single live-state pool (prior behavior) so the surface never returns empty.
async fn handle_pools(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let configured = cg_read_configured_pools();
    // P1-5 (D-15): best achieved difficulty for the active pool's `Best Share`.
    let best_share = best_achieved_share_difficulty(
        &state
            .recent_share_history
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default(),
    );

    let pools = if configured.is_empty() {
        // Fallback: emit the single live-state active pool (prior behavior).
        // (URL credential-stripping is centralized inside build_cg_pool_entry.)
        vec![build_cg_pool_entry(
            0,
            &miner.pool.url,
            &miner.pool.worker,
            0,
            &miner.pool.status,
            true,
            &miner,
            best_share,
        )]
    } else {
        // Determine the active index. Prefer a URL match against the live
        // session (most reliable); fall back to the failover/split index.
        let active_url = miner.pool.url.trim();
        let active_index = configured
            .iter()
            .position(|p| !active_url.is_empty() && p.url.trim() == active_url)
            .or_else(|| {
                let split = &miner.pool.hashrate_split;
                if split.enabled && split.active_pool_index < configured.len() {
                    Some(split.active_pool_index)
                } else {
                    let fo = &miner.pool.failover;
                    (fo.active_pool_index < configured.len()).then_some(fo.active_pool_index)
                }
            });

        configured
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let is_active = active_index == Some(i);
                // Active pool uses the live status; standby pools are "Standby".
                let status = if is_active {
                    miner.pool.status.clone()
                } else {
                    "Standby".to_string()
                };
                build_cg_pool_entry(
                    i, &p.url, &p.worker, p.priority, &status, is_active, &miner, best_share,
                )
            })
            .collect::<Vec<_>>()
    };

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Pool, "Pools")],
        "POOLS": pools,
        "id": 1
    })
}

/// Handle the `devs` command.
///
/// Returns per-device (per-chain) data compatible with pyasic.
async fn handle_devs(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();

    let mut devs = vec![];
    for (i, chain) in miner.chains.iter().enumerate() {
        // Derive cgminer device Status/Enabled from real chain health instead
        // of hardcoding "Alive"/"Y". Before this, EVERY chain reported
        // `Status: "Alive"` + `Enabled: "Y"` unconditionally — so a chain that
        // failed enumeration (0 responding chips, marked dead by the daemon)
        // still told pyasic / fleet tooling it was healthy, masking a dead
        // hashboard from operators watching the cgminer-compat surface. cgminer
        // convention: "Alive" = device responding, "Dead" = no response. We use
        // `chain.chips > 0` (number of responding chips) as the truthful
        // discriminator. Reporting-only — no mining/power/thermal change.
        let chain_alive = chain.chips > 0;
        devs.push(serde_json::json!({
            "ASC": i,
            "Name": format!("chain{}", chain.id),
            "ID": i,
            "Enabled": if chain_alive { "Y" } else { "N" },
            "Status": if chain_alive { "Alive" } else { "Dead" },
            "Temperature": chain.temp_c,
            "MHS av": chain.hashrate_ghs * 1000.0,
            "MHS 5s": chain.hashrate_ghs * 1000.0,
            "MHS 1m": chain.hashrate_ghs * 1000.0,
            "MHS 5m": chain.hashrate_ghs * 1000.0,
            "MHS 15m": chain.hashrate_ghs * 1000.0,
            "Accepted": 0,
            "Rejected": 0,
            "Hardware Errors": chain.errors,
            "Utility": 0.0,
            "Last Share Pool": 0,
            // P1-5 (D-15): project the pool-level last accepted-share time onto
            // alive devices (chips > 0). The daemon tracks ONE pool session, not
            // per-device share attribution, so a dead device honestly reports 0.
            // Before this, every device reported 0 → fleet tools read "this
            // hashboard never found a share" on a fully working miner.
            "Last Share Time": if chain_alive { miner.pool.last_share_at } else { 0 },
            "Total MH": chain.hashrate_ghs * 1000.0 * miner.uptime_s as f64,
            "Diff1 Work": 0,
            "Difficulty Accepted": 0.0,
            "Difficulty Rejected": 0.0,
            "Last Share Difficulty": 0.0,
            "Last Valid Work": 0,
            "Device Hardware%": 0.0,
            "Device Rejected%": 0.0,
            "Device Elapsed": miner.uptime_s,
            "chain_acn": chain.chips,
            "frequency": chain.frequency_mhz,
            "voltage": chain.voltage_mv as f64 / 1000.0,
            "_DCENTUnsupported": [
                "Accepted",
                "Rejected",
                "Utility",
                "Last Share Pool",
                "Diff1 Work",
                "Difficulty Accepted",
                "Difficulty Rejected",
                "Last Share Difficulty",
                "Last Valid Work",
                "Device Hardware%",
                "Device Rejected%"
            ],
            "_DCENTFieldSources": {
                "Hardware Errors": "chain.errors",
                "Temperature": "chain.temp_c",
                "chain_acn": "chain.chips",
                "frequency": "chain.frequency_mhz",
                "voltage": "chain.voltage_mv (commanded chain voltage, mV→V)",
                "Last Share Time": "miner_state.pool.last_share_at for alive chains (pool-level last accepted-share epoch; daemon tracks one pool session, not per-device attribution)"
            }
        }));
    }

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Devs, "Devs")],
        "DEVS": devs,
        "id": 1
    })
}

/// Handle the `version` command.
///
/// Returns firmware and API version information.
///
/// pyasic/asic-rs classify a miner from this object. Before this fix it
/// carried ONLY `CGMiner`/`API`, with no `Type` token — so pyasic's
/// CGMiner backend couldn't map DCENT_OS to a hardware model and fleet
/// tools saw an unclassifiable miner. We now add:
///   - `Type` — the real hardware MODEL (e.g. "Antminer S19 Pro"),
///     resolved from the detected ASIC via the canonical `MinerProfile`
///     table, so pyasic/asic-rs size their per-board telemetry loops
///     correctly against the HARDWARE.
///   - `Miner` / `BMMiner` — the bmminer-family version tokens pyasic
///     reads as the miner-software identity (kept honest as
///     `dcentrald/<ver>`).
///   - `Description` / `DCENTOS` — an explicit, honest firmware marker so
///     consumers see this is DCENT_OS, NOT Antminer/BraiinsOS/LuxOS/VNish
///     firmware. We report the real hardware model but never impersonate
///     another firmware stack.
async fn handle_version(state: &AppState) -> serde_json::Value {
    let model = state
        .hardware_info
        .lock()
        .ok()
        .map(|hw| hardware_model_label(&hw))
        .unwrap_or_else(|| "Antminer (unknown ASIC)".to_string());
    // `hw` above is a MutexGuard<HardwareInfo>; `&hw` deref-coerces to
    // `&HardwareInfo` for the helper arg, and the guard is dropped at the
    // end of the `.map()` closure so the lock is not held across the json! .

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Version, "Version")],
        "VERSION": [{
            "CGMiner": CGMINER_VERSION,
            "API": API_VERSION,
            // Real hardware MODEL for pyasic/asic-rs classification.
            "Type": model,
            // bmminer-family identity tokens pyasic reads — kept honest as
            // dcentrald, NOT a competitor firmware string.
            "Miner": CGMINER_VERSION,
            "BMMiner": CGMINER_VERSION,
            // Explicit, honest firmware-stack marker.
            "Firmware": DCENTOS_FIRMWARE_MARKER,
            "DCENTOS": env!("CARGO_PKG_VERSION"),
            "Description": concat!("DCENT_OS dcentrald/", env!("CARGO_PKG_VERSION"), " (open-source firmware on Antminer hardware)"),
            "_DCENTNote": "Type is the real hardware model for fleet classification; \
                           firmware identity is honestly DCENT_OS (dcentrald), not Antminer/BraiinsOS/LuxOS/VNish.",
        }],
        "id": 1
    })
}

/// Whether `cmd` is a CGMiner verb this daemon recognizes. Drives the
/// `check` capability probe (and only that) — the union of the read/control
/// verbs handled here plus every LuxOS-layer verb.
fn is_known_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "summary"
            | "stats"
            | "pools"
            | "devs"
            | "version"
            | "coin"
            | "config"
            | "switchpool"
            | "enablepool"
            | "disablepool"
            | "addpool"
            | "restart"
            | "quit"
            | "devdetails"
            | "asccount"
            | "check"
            | "notify"
    ) || crate::cgminer_luxos::is_luxos_command(cmd)
}

/// Handle the `devdetails` command (cgminer MSG_DEVDETAILS / code 69).
///
/// Per-device inventory pyasic / asic-rs read to classify hardware (driver,
/// model, device name). One entry per hashboard/chain. Read-only.
async fn handle_devdetails(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let (model, chip) = state
        .hardware_info
        .lock()
        .ok()
        .map(|hw| (hardware_model_label(&hw), hw.chip_type.trim().to_string()))
        .unwrap_or_else(|| ("Antminer (unknown ASIC)".to_string(), String::new()));
    let device_name = if chip.is_empty() {
        "BitMain".to_string()
    } else {
        chip
    };

    let details: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .enumerate()
        .map(|(i, chain)| {
            serde_json::json!({
                "DEVDETAILS": i,
                "Name": device_name,
                "ID": i,
                "Driver": "bitmain",
                "Kernel": "",
                "Model": model,
                "Device Path": format!("chain{}", chain.id),
            })
        })
        .collect();

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::DevDetails, "Device Details")],
        "DEVDETAILS": details,
        // Mirror the version command's honesty note (W9 convention): the
        // Driver/Model/Name fields describe the REAL Bitmain hardware for
        // fleet classification — the firmware identity is DCENT_OS, not a
        // competitor stack.
        "_DCENTNote": "Driver/Model/Name describe the real Bitmain hardware for fleet \
                       classification; firmware identity is DCENT_OS (dcentrald) — see the \
                       version command — not bmminer/BraiinsOS/LuxOS/VNish.",
        "id": 1
    })
}

/// Handle the `asccount` command (cgminer MSG_NUMASC / code 104). Number of
/// ASIC devices (hashboards/chains) — fleet tools use it to size the device
/// loop. Read-only.
async fn handle_asccount(state: &AppState) -> serde_json::Value {
    let count = state.state_rx.borrow().chains.len();
    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::NumAsc, "ASC count")],
        "ASCS": [{ "Count": count }],
        "id": 1
    })
}

/// Handle the `check|<command>` command (cgminer MSG_CHECK / code 72, or
/// MSG_MISCHK / code 71 when the sub-command is missing). Capability probe:
/// fleet tools call it to detect whether a verb is supported before relying
/// on it. `Exists` = recognized verb; `Access` mirrors it — DCENT serves all
/// recognized read verbs and session-gates mutations at execution time rather
/// than hiding them from the probe. Read-only.
async fn handle_check(_state: &AppState, param: &Option<String>) -> serde_json::Value {
    let Some(sub) = param.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return serde_json::json!({
            "STATUS": [CgMinerStatus::error_for(
                CgminerStatusCode::MissingCheck,
                "Missing check command",
            )],
            "id": 1
        });
    };
    let exists = is_known_command(sub);
    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Check, "Check command")],
        "CHECK": [{
            "Exists": if exists { "Y" } else { "N" },
            "Access": if exists { "Y" } else { "N" },
        }],
        "id": 1
    })
}

/// Handle the `notify` command (cgminer MSG_NOTIFY / code 60). Per-device
/// health/notification list. DCENT tracks chain liveness + comms errors; the
/// untracked cgminer health counters are reported 0 and listed in
/// `_DCENTUnsupported` so they are never presented as real telemetry.
/// Read-only.
async fn handle_notify(state: &AppState) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let notes: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .enumerate()
        .map(|(i, chain)| {
            let alive = chain.chips > 0;
            serde_json::json!({
                "NOTIFY": i,
                "Name": format!("chain{}", chain.id),
                "ID": i,
                // Well/not-well timestamps are not tracked — report 0 (cgminer
                // "unknown") rather than fabricate a time.
                "Last Well": 0,
                "Last Not Well": 0,
                "Reason Not Well": if alive { "None" } else { "Dead" },
                "*Thread Fail Init": 0,
                "*Thread Zero Hash": 0,
                "*Thread Fail Queue": 0,
                "*Dev Sick Idle 60s": 0,
                "*Dev Dead Idle 600s": 0,
                "*Dev Nostart": 0,
                "*Dev Over Heat": 0,
                "*Dev Thermal Cutoff": 0,
                "*Dev Comms Error": chain.errors,
                "*Dev Throttle": 0,
                "_DCENTUnsupported": [
                    "Last Well", "Last Not Well",
                    "*Thread Fail Init", "*Thread Zero Hash", "*Thread Fail Queue",
                    "*Dev Sick Idle 60s", "*Dev Dead Idle 600s", "*Dev Nostart",
                    "*Dev Over Heat", "*Dev Thermal Cutoff", "*Dev Throttle"
                ],
                "_DCENTFieldSources": {
                    "Reason Not Well": "chain.chips > 0 ? None : Dead",
                    "*Dev Comms Error": "chain.errors"
                }
            })
        })
        .collect();

    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Notify, "Notify")],
        "NOTIFY": notes,
        "id": 1
    })
}

/// Handle the `coin` command.
async fn handle_coin(_state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::MineCoin, "Coin")],
        "COIN": [{
            "Hash Method": "sha256d",
            "Current Block Time": 0.0,
            "Current Block Hash": "",
            "LP": true,
            "Network Difficulty": 0.0,
        }],
        "id": 1
    })
}

/// Handle the `config` command.
async fn handle_config(state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::MineConfig, "Config")],
        "CONFIG": [{
            "ASC Count": state.state_rx.borrow().chains.len(),
            "PGA Count": 0,
            "Pool Count": 1,
            "Strategy": "Failover",
            "Log Interval": 5,
            "Device Code": "",
            "OS": "DCENTos",
            "Hotplug": "None",
        }],
        "id": 1
    })
}

/// Handle the `switchpool` command.
///
/// ROUTING/CAPABILITY NOTE: the live dispatch path never reaches this arm —
/// `dispatch_single` routes `switchpool` (an `is_luxos_command`) to
/// `cgminer_luxos::handle_luxos_command`, which returns the same honest
/// rejection. DCENT_OS has NO per-index runtime pool-switch primitive: pool
/// selection is failover-FSM- / priority-driven, and the set is changed by
/// rewriting it via `addpool`/`POST /api/pools` (read-only switch surface by
/// design). A genuine `switchpool` action would require new runtime
/// pool-selection plumbing in the Stratum client — FLAGGED, out of scope for
/// `cgminer.rs`. We return an honest, actionable error rather than a fake ack.
async fn handle_switchpool(_state: &AppState, param: &Option<String>) -> serde_json::Value {
    let _pool_num = param
        .as_ref()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);

    serde_json::json!({
        "STATUS": [CgMinerStatus::error_for(
            CgminerStatusCode::MissingId,
            "switchpool: per-index runtime pool switch is not supported on DCENT_OS \
             (failover is priority/FSM-driven). Reorder pools via `addpool` (rewrites \
             the set, primary first) or the REST /api/pools surface.",
        )],
        "id": 1
    })
}

/// Handle the `enablepool` command. See `handle_switchpool` routing note —
/// per-index pool enable/disable is not a DCENT_OS runtime primitive.
async fn handle_enablepool(_state: &AppState, _param: &Option<String>) -> serde_json::Value {
    serde_json::json!({
        "STATUS": [CgMinerStatus::error_for(
            CgminerStatusCode::MissingId,
            "enablepool: per-index pool enable is not supported on DCENT_OS — manage the \
             pool set via `addpool` (rewrites the set) or the REST /api/pools surface.",
        )],
        "id": 1
    })
}

/// Handle the `disablepool` command. See `handle_switchpool` routing note.
async fn handle_disablepool(_state: &AppState, _param: &Option<String>) -> serde_json::Value {
    serde_json::json!({
        "STATUS": [CgMinerStatus::error_for(
            CgminerStatusCode::MissingId,
            "disablepool: per-index pool disable is not supported on DCENT_OS — manage the \
             pool set via `addpool` (rewrites the set) or the REST /api/pools surface.",
        )],
        "id": 1
    })
}

/// Handle the `addpool` command.
///
/// NOTE on routing: the LIVE dispatch path never reaches this handler —
/// `dispatch_single` routes `addpool` (an `is_luxos_command`) to
/// `cgminer_luxos::handle_luxos_command`, whose `delegate_addpool` ALREADY
/// acts (it calls the same gated `rest::post_pools` writer the dashboard
/// uses). This handler is the legacy `handle_command` arm; it is wired to
/// the SAME action core for correctness/parity so it can never diverge into
/// a fake success if a future caller reaches it directly.
async fn handle_addpool(state: &AppState, param: &Option<String>) -> serde_json::Value {
    let Some(params) = param else {
        return serde_json::json!({
            "STATUS": [CgMinerStatus::error_for(
                CgminerStatusCode::InvalidJson,
                "Missing parameters (url,user,pass)",
            )],
            "id": 1
        });
    };
    let parts: Vec<&str> = params.splitn(3, ',').collect();
    if parts.len() < 3 {
        return serde_json::json!({
            "STATUS": [CgMinerStatus::error_for(
                CgminerStatusCode::InvalidJson,
                "Missing parameters (url,user,pass)",
            )],
            "id": 1
        });
    }
    // Reuse the SAME gated pool-config core the gRPC SetPools bridge uses
    // (`grpc_bridge_set_pools` → `validate_and_write_pool_config`: ≤3 pools,
    // URL validation, atomic write) so this is a real action with the same
    // safety as REST — never a fake ack. `priority 0` = primary.
    let pools = vec![(
        parts[0].trim().to_string(),
        parts[1].trim().to_string(),
        parts[2].trim().to_string(),
        0u32,
    )];
    match crate::rest::grpc_bridge_set_pools(state, pools).await {
        Ok(ok) => serde_json::json!({
            "STATUS": [CgMinerStatus::success_for(
                CgminerStatusCode::Pool,
                format!("Pool added ({} configured); reconnect on next cycle", ok.pool_count),
            )],
            "id": 1
        }),
        Err(message) => serde_json::json!({
            "STATUS": [CgMinerStatus::error_for(CgminerStatusCode::MissingValue, message)],
            "id": 1
        }),
    }
}

/// Handle the `restart` command.
///
/// Routes through the same capability and persistent-session policy as REST
/// and gRPC. Until typed hardware-disposition receipts exist, an authorized
/// request is explicitly refused without signalling the live hardware owner.
async fn handle_restart(state: &AppState) -> serde_json::Value {
    match crate::rest::grpc_bridge_reboot(state) {
        Ok(message) => serde_json::json!({
            "STATUS": [CgMinerStatus::success_for(CgminerStatusCode::Version, message)],
            "id": 1
        }),
        Err(message) => serde_json::json!({
            "STATUS": [CgMinerStatus::error_for(CgminerStatusCode::MissingValue, message)],
            "id": 1
        }),
    }
}

/// Handle the `quit` command.
///
/// `quit` in the CGMiner protocol terminates the mining process with NO
/// respawn. That is deliberately REFUSED on DCENT_OS: `dcentrald` owns fan
/// control (esp. on AM2/XIL, where fan PWM is held only while a process owns
/// the UIO mmap — killing the daemon with no replacement reverts the board to
/// FULL-SPEED fans).
/// An unauthenticated TCP `quit` must never strand a home unit with blasting
/// fans. Operators who really want to stop the daemon use SSH; remote
/// lifecycle uses `restart` (which respawns and keeps fan management).
async fn handle_quit(_state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "STATUS": [CgMinerStatus::error_for(
            CgminerStatusCode::AccessDenied,
            "quit is refused on DCENT_OS: terminating the daemon with no respawn would \
             leave the board with no fan manager (AM2 reverts to full-speed fans). Use \
             `restart` for remote lifecycle, or SSH `kill -15 $(pidof dcentrald)` to stop.",
        )],
        "id": 1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn cgminer_check_advertises_exactly_the_dispatched_command_set() {
        // Fleet-compatibility pin. pyasic / asic-rs / hass-miner use `check <cmd>`
        // for capability detection, and `check` answers from is_known_command. That
        // list is maintained SEPARATELY from dispatch_single's actual routing, so a
        // future edit that adds/removes a command in one place but not the other
        // would make `check` lie — advertising a verb the daemon rejects with
        // InvalidCommand, or hiding one it supports. Pin both directions.
        //
        // Read verbs routed by dispatch_single's first arm to handle_command:
        const DISPATCHED_READ_VERBS: &[&str] = &[
            "summary",
            "stats",
            "pools",
            "devs",
            "version",
            "coin",
            "config",
            "restart",
            "quit",
            "devdetails",
            "asccount",
            "check",
            "notify",
        ];
        for cmd in DISPATCHED_READ_VERBS {
            assert!(
                is_known_command(cmd),
                "check must advertise dispatched verb '{cmd}'"
            );
        }
        // Pool-mutation verbs are dispatched via the LuxOS layer, NOT the first arm;
        // check must advertise them AND they must actually route there (else the
        // first arm rejects them as InvalidCommand while check says Exists:Y).
        for cmd in ["switchpool", "enablepool", "disablepool", "addpool"] {
            assert!(
                is_known_command(cmd),
                "check must advertise mutation verb '{cmd}'"
            );
            assert!(
                crate::cgminer_luxos::is_luxos_command(cmd),
                "mutation verb '{cmd}' must route via the LuxOS dispatch arm"
            );
        }
        // Unknown commands must NOT be advertised (check returns Exists:N). Commands
        // reach is_known_command already lowercased by CgMinerCommand::parse, so an
        // uppercase form is correctly not advertised as-is.
        for cmd in ["", "bogus", "summaryx", "not_a_command", "SUMMARY"] {
            assert!(
                !is_known_command(cmd),
                "check must not advertise unknown '{cmd}'"
            );
        }
    }

    #[tokio::test]
    async fn cgminer_request_reader_accepts_split_newline_command() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            client.write_all(br#"{"command":"sum"#).await.unwrap();
            client.write_all(br#"mary"}"#).await.unwrap();
            client.write_all(b"\n").await.unwrap();
        });

        let raw = read_cgminer_request_with_timeout(&mut server, Duration::from_secs(1))
            .await
            .expect("read should succeed")
            .expect("request should be present");
        writer.await.unwrap();

        let cmd = CgMinerCommand::parse(&raw).expect("split command should parse");
        assert_eq!(cmd.command, "summary");
    }

    #[tokio::test]
    async fn cgminer_request_reader_times_out_idle_client() {
        let (_client, mut server) = tokio::io::duplex(64);

        let err = read_cgminer_request_with_timeout(&mut server, Duration::from_millis(10))
            .await
            .expect_err("idle client must time out");

        assert!(matches!(err, CgMinerRequestReadError::Timeout));
    }

    #[tokio::test]
    async fn cgminer_request_reader_rejects_oversized_unterminated_command() {
        let (mut client, mut server) = tokio::io::duplex(MAX_REQUEST_SIZE + 128);
        let writer = tokio::spawn(async move {
            client
                .write_all(&vec![b'a'; MAX_REQUEST_SIZE + 1])
                .await
                .unwrap();
        });

        let err = read_cgminer_request_with_timeout(&mut server, Duration::from_secs(1))
            .await
            .expect_err("oversized request must fail");
        writer.await.unwrap();

        assert!(matches!(err, CgMinerRequestReadError::TooLarge));
    }

    #[test]
    fn cgminer_connection_cap_is_finite_for_lan_exposure() {
        assert!(MAX_CGMINER_CONNECTIONS > 0);
        assert!(MAX_CGMINER_CONNECTIONS <= 64);
    }

    #[test]
    fn cgminer_command_parse_never_panics_on_arbitrary_input() {
        // Fuzz the untrusted CGMiner-API command parser (priority 1). The :4028
        // listener parses whatever a client sends — and when cgminer_bind_lan is
        // enabled (required for pyasic/hass-miner), that client can be any LAN peer.
        // A panic there is a remote DoS. The parser is written defensively; this
        // pins it. Deterministic LCG (reproducible; harness forbids RNG). Families:
        // raw random Latin-1 bytes, well-formed + wrong-typed JSON envelopes,
        // truncated JSON, and legacy pipe format with 0..N pipes. Only assertion:
        // parse() always RETURNS (Some/None), never panics.
        let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        let cmd_toks = [
            "\"summary\"",
            "\"switchpool\"",
            "\"\"",
            "123",
            "null",
            "true",
            "summary",
        ];
        let param_toks = [
            "\"\"",
            "\"0\"",
            "\"a,b,c\"",
            "null",
            "999999999999",
            "\"x\"",
        ];
        for _ in 0..6000u32 {
            let s = match next() % 4 {
                0 => {
                    let len = (next() % 128) as usize;
                    let mut s = String::with_capacity(len);
                    for _ in 0..len {
                        s.push(char::from((next() % 0x100) as u8));
                    }
                    s
                }
                1 => format!(
                    "{{\"command\":{},\"parameter\":{}}}",
                    cmd_toks[(next() as usize) % cmd_toks.len()],
                    param_toks[(next() as usize) % param_toks.len()],
                ),
                2 => format!("{{\"command\":\"{}", "x".repeat((next() % 40) as usize)),
                _ => {
                    let n = (next() % 6) as usize;
                    let mut s = String::from("cmd");
                    for _ in 0..n {
                        s.push('|');
                        s.push_str(&next().to_string());
                    }
                    s
                }
            };
            let _ = CgMinerCommand::parse(&s); // MUST NOT panic on any input
        }
        for s in [
            "",
            "   ",
            "\0\0",
            "{",
            "}",
            "|",
            "||",
            "\u{0}{",
            "{\"command\":123}",
        ] {
            let _ = CgMinerCommand::parse(s);
        }
    }

    fn chain(
        id: u8,
        chips: u8,
        freq: u16,
        mv: u16,
        temp: f32,
        ghs: f64,
        errs: u32,
    ) -> CgStatsChain {
        CgStatsChain {
            id,
            chips,
            frequency_mhz: freq,
            voltage_mv: mv,
            temp_c: temp,
            hashrate_ghs: ghs,
            errors: errs,
            status: "Alive".to_string(),
        }
    }

    fn command(name: &str) -> CgMinerCommand {
        CgMinerCommand {
            command: name.to_string(),
            parameter: None,
        }
    }

    fn assert_success_status(value: &serde_json::Value, msg: &str) {
        assert_eq!(value["id"], serde_json::json!(1));
        assert_eq!(value["STATUS"][0]["STATUS"], serde_json::json!("S"));
        assert_eq!(value["STATUS"][0]["Msg"], serde_json::json!(msg));
        assert_eq!(
            value["STATUS"][0]["Description"],
            serde_json::json!(CGMINER_VERSION)
        );
    }

    fn golden_app_state() -> Arc<AppState> {
        let mut miner = crate::MinerState::empty(dcentrald_api_types::OperatingMode::Standard);
        miner.hashrate_ghs = 104_250.5;
        miner.hashrate_5s_ghs = 103_900.0;
        miner.accepted = 40;
        miner.rejected = 1;
        miner.uptime_s = 7_800;
        miner.chains = vec![
            crate::ChainState {
                id: 6,
                chips: 114,
                frequency_mhz: 525,
                voltage_mv: 13_700,
                temp_c: 62.5,
                temp_source: None,
                hashrate_ghs: 21_000.0,
                errors: 3,
                status: "Alive".to_string(),
            },
            crate::ChainState {
                id: 7,
                chips: 110,
                frequency_mhz: 530,
                voltage_mv: 13_750,
                temp_c: 64.0,
                temp_source: None,
                hashrate_ghs: 20_500.0,
                errors: 7,
                status: "Alive".to_string(),
            },
        ];
        miner.fans = crate::FanState {
            pwm: 30,
            rpm: 2940,
            per_fan: vec![
                crate::PerFanReading {
                    id: 0,
                    rpm: 2940,
                    pwm_percent: 30,
                },
                crate::PerFanReading {
                    id: 1,
                    rpm: 3720,
                    pwm_percent: 30,
                },
            ],
        };
        miner.pool.url = "stratum+tcp://pool.example:3333".to_string();
        miner.pool.worker = "rig.1".to_string();
        miner.pool.status = "Alive".to_string();
        miner.pool.difficulty = 512.0;
        miner.pool.last_share_at = 1_700_000_000;

        let (_state_tx, state_rx) = tokio::sync::watch::channel(miner);
        crate::build_minimal_app_state(crate::MinimalAppStateInputs {
            api_config: crate::ApiConfig::default(),
            pool_url: "stratum+tcp://pool.example:3333".to_string(),
            pool_protocol: "sv1".to_string(),
            mode: dcentrald_api_types::OperatingMode::Standard,
            firmware_version: "test".to_string(),
            fan_pwm: 30,
            network_block: crate::NetworkBlockConfig::default(),
            profile_path: "test-profiles".to_string(),
            control_board_label: "Zynq am2-s19jpro".to_string(),
            chip_type_label: "BM1362".to_string(),
            external_state_rx: Some(state_rx),
        })
    }

    fn cgminer_contract_fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/cgminer/dcentos_cgminer_golden.json"
        )))
        .expect("shared CGMiner fixture must parse")
    }

    fn assert_fixture_status(actual: &serde_json::Value, expected: &serde_json::Value) {
        let actual_status = &actual["STATUS"][0];
        let expected_status = &expected["STATUS"][0];
        for key in ["STATUS", "Code", "Msg"] {
            assert_eq!(
                actual_status[key], expected_status[key],
                "CGMiner STATUS field {key} drifted"
            );
        }
    }

    fn assert_fixture_fields(actual: &serde_json::Value, expected: &serde_json::Value) {
        let expected_obj = expected.as_object().expect("fixture row object");
        for (key, expected_value) in expected_obj {
            let actual_value = &actual[key];
            match (actual_value.as_f64(), expected_value.as_f64()) {
                (Some(actual_num), Some(expected_num)) => assert!(
                    (actual_num - expected_num).abs() < 1e-9,
                    "CGMiner field {key} drifted: actual={actual_num} expected={expected_num}"
                ),
                _ => assert_eq!(actual_value, expected_value, "CGMiner field {key} drifted"),
            }
        }
    }

    #[tokio::test]
    async fn cgminer_golden_response_corpus_core_read_verbs() {
        let state = golden_app_state();
        state
            .recent_share_history
            .lock()
            .expect("recent share lock")
            .extend([
                recent_share("accepted", Some(1_024.0)),
                recent_share("lucky", Some(60_933.43)),
            ]);

        let summary = handle_command_arc(&state, &command("summary")).await;
        assert_success_status(&summary, "Summary");
        let summary_obj = &summary["SUMMARY"][0];
        assert_eq!(summary_obj["Elapsed"], serde_json::json!(7_800));
        assert_eq!(summary_obj["Accepted"], serde_json::json!(40));
        assert_eq!(summary_obj["Rejected"], serde_json::json!(1));
        assert_eq!(summary_obj["Hardware Errors"], serde_json::json!(10));
        assert_eq!(summary_obj["MHS av"], serde_json::json!(104_250_500.0));
        assert_eq!(summary_obj["MHS 5s"], serde_json::json!(103_900_000.0));
        assert_eq!(summary_obj["Best Share"], serde_json::json!(60_933.43));

        let stats = handle_command_arc(&state, &command("stats")).await;
        assert_success_status(&stats, "Stats");
        assert_eq!(stats["STATS"].as_array().expect("stats array").len(), 3);
        let bmminer = &stats["STATS"][0];
        assert_eq!(bmminer["miner_count"], serde_json::json!(2));
        assert_eq!(bmminer["fan_num"], serde_json::json!(2));
        assert_eq!(bmminer["fan1"], serde_json::json!(2940));
        assert_eq!(bmminer["chain_acn1"], serde_json::json!(114));
        assert_eq!(bmminer["chain_rate1"], serde_json::json!("21000.00"));
        assert_eq!(bmminer["temp_chip2"], serde_json::json!(64.0));
        assert_eq!(bmminer["freq_avg"], serde_json::json!(527.5));
        assert_eq!(bmminer["total_rate"], serde_json::json!(41_500.0));
        let chain_row = &stats["STATS"][1];
        assert_eq!(chain_row["ID"], serde_json::json!("CHAIN6"));
        assert_eq!(chain_row["chain_acn"], serde_json::json!(114));
        assert_eq!(chain_row["Hardware Errors"], serde_json::json!(3));

        let devs = handle_command_arc(&state, &command("devs")).await;
        assert_success_status(&devs, "Devs");
        assert_eq!(devs["DEVS"].as_array().expect("devs array").len(), 2);
        assert_eq!(devs["DEVS"][0]["ASC"], serde_json::json!(0));
        assert_eq!(devs["DEVS"][0]["Name"], serde_json::json!("chain6"));
        assert_eq!(devs["DEVS"][0]["Enabled"], serde_json::json!("Y"));
        assert_eq!(devs["DEVS"][0]["Status"], serde_json::json!("Alive"));
        assert_eq!(
            devs["DEVS"][0]["Last Share Time"],
            serde_json::json!(1_700_000_000u64)
        );

        let version = handle_command_arc(&state, &command("version")).await;
        assert_success_status(&version, "Version");
        assert_eq!(
            version["VERSION"][0]["CGMiner"],
            serde_json::json!(CGMINER_VERSION)
        );
        assert_eq!(version["VERSION"][0]["API"], serde_json::json!(API_VERSION));
        assert_eq!(
            version["VERSION"][0]["Miner"],
            serde_json::json!(CGMINER_VERSION)
        );
        assert_eq!(
            version["VERSION"][0]["Firmware"],
            serde_json::json!("DCENTOS")
        );
        assert!(
            version["VERSION"][0]["Type"]
                .as_str()
                .is_some_and(|model| model.starts_with("Antminer")),
            "version Type must be a real Antminer hardware model: {version}"
        );

        let config = handle_command_arc(&state, &command("config")).await;
        assert_success_status(&config, "Config");
        assert_eq!(config["CONFIG"][0]["ASC Count"], serde_json::json!(2));
        assert_eq!(config["CONFIG"][0]["Pool Count"], serde_json::json!(1));
        assert_eq!(
            config["CONFIG"][0]["Strategy"],
            serde_json::json!("Failover")
        );
        assert_eq!(config["CONFIG"][0]["OS"], serde_json::json!("DCENTos"));
    }

    #[tokio::test]
    async fn cgminer_shared_toolbox_contract_fixture_matches_dispatcher() {
        let fixture = cgminer_contract_fixture();
        let state = golden_app_state();
        state
            .recent_share_history
            .lock()
            .expect("recent share lock")
            .extend([
                recent_share("accepted", Some(1_024.0)),
                recent_share("lucky", Some(60_933.43)),
            ]);

        let summary = handle_command_arc(&state, &command("summary")).await;
        assert_fixture_status(&summary, &fixture["summary"]);
        assert_fixture_fields(&summary["SUMMARY"][0], &fixture["summary"]["SUMMARY"][0]);

        let devs = handle_command_arc(&state, &command("devs")).await;
        assert_fixture_status(&devs, &fixture["devs"]);
        let expected_devs = fixture["devs"]["DEVS"]
            .as_array()
            .expect("fixture devs array");
        let actual_devs = devs["DEVS"].as_array().expect("dispatcher devs array");
        assert_eq!(actual_devs.len(), expected_devs.len());
        for (actual, expected) in actual_devs.iter().zip(expected_devs) {
            assert_fixture_fields(actual, expected);
        }

        let version = handle_command_arc(&state, &command("version")).await;
        assert_fixture_status(&version, &fixture["version"]);
        let actual_version = &version["VERSION"][0];
        let expected_version = &fixture["version"]["VERSION"][0];
        assert_eq!(actual_version["API"], expected_version["API"]);
        assert_eq!(actual_version["Firmware"], expected_version["Firmware"]);
        for key in ["CGMiner", "Miner", "BMMiner"] {
            assert!(
                actual_version[key]
                    .as_str()
                    .is_some_and(|value| value.starts_with("dcentrald/")),
                "CGMiner VERSION field {key} must remain an honest dcentrald marker"
            );
        }
        assert!(
            actual_version["Type"]
                .as_str()
                .is_some_and(|value| value.starts_with(expected_version["Type"].as_str().unwrap())),
            "CGMiner VERSION Type must remain an Antminer hardware model"
        );
        assert!(
            actual_version["Description"]
                .as_str()
                .is_some_and(|value| value.contains("DCENT_OS dcentrald/")),
            "CGMiner VERSION Description must keep the DCENT_OS identity"
        );
    }

    #[test]
    fn cgminer_golden_response_corpus_pools_entry() {
        let miner = miner_state("stratum+tcp://pool.example:3333", "Alive", 42);
        let entry = build_cg_pool_entry(
            0,
            "stratum+tcp://user:secret@pool.example.com:3333",
            "rig.1",
            0,
            "Alive",
            true,
            &miner,
            60_933.43,
        );

        assert_eq!(entry["POOL"], serde_json::json!(0));
        assert_eq!(
            entry["URL"],
            serde_json::json!("stratum+tcp://pool.example.com:3333")
        );
        assert_eq!(
            entry["Stratum URL"],
            serde_json::json!("stratum+tcp://pool.example.com:3333")
        );
        assert_eq!(entry["Status"], serde_json::json!("Alive"));
        assert_eq!(entry["Priority"], serde_json::json!(0));
        assert_eq!(entry["Accepted"], serde_json::json!(42));
        assert_eq!(entry["User"], serde_json::json!("rig.1"));
        assert_eq!(entry["Work Difficulty"], serde_json::json!(512.0));
        assert_eq!(entry["Best Share"], serde_json::json!(60_933.43));
        assert_eq!(entry["Stratum Active"], serde_json::json!(true));

        let serialized = serde_json::to_string(&entry).expect("serialize pool entry");
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("user:secret@"));
    }

    // SW-12: the flattened bmminer-shape object carries the per-board
    // `chain_*{N}` / `temp*{N}` / `freq_avg` / `total_rate` keys pyasic reads.
    #[test]
    fn bmminer_stats_object_has_pyasic_per_board_keys() {
        let chains = vec![
            chain(6, 114, 525, 13_700, 62.5, 21_000.0, 3),
            chain(7, 110, 530, 13_750, 64.0, 20_500.0, 7),
        ];
        let obj = build_bmminer_stats_object(&chains, 1234, &[]);

        assert_eq!(obj["miner_count"], serde_json::json!(2));
        assert_eq!(obj["temp_num"], serde_json::json!(2));
        // No fan data passed → fan_num is honestly 0 (no phantom fans).
        assert_eq!(obj["fan_num"], serde_json::json!(0));
        // Per-board responding-chip counts (1-indexed bmminer convention).
        assert_eq!(obj["chain_acn1"], serde_json::json!(114));
        assert_eq!(obj["chain_acn2"], serde_json::json!(110));
        // Per-board hashrate (GH/s, 2dp string like bmminer).
        assert_eq!(obj["chain_rate1"], serde_json::json!("21000.00"));
        // Per-board temps surfaced under all three pyasic key families.
        assert_eq!(obj["temp1"], serde_json::json!(62.5));
        assert_eq!(obj["temp2_1"], serde_json::json!(62.5));
        assert_eq!(obj["temp_chip1"], serde_json::json!(62.5));
        // Per-board freq + voltage(V). Voltage via tolerance (13700/1000 may
        // differ from the literal 13.7 by an ULP).
        assert_eq!(obj["freq1"], serde_json::json!(525));
        let v = obj["chain_voltage1"].as_f64().expect("voltage is a number");
        assert!((v - 13.7).abs() < 1e-9, "got {v}");
        // temp_max = hottest board; freq_avg = mean.
        assert_eq!(obj["temp_max"], serde_json::json!(64.0));
        assert_eq!(obj["freq_avg"], serde_json::json!(527.5));
        // total_rate = sum of per-board GH/s.
        assert_eq!(obj["total_rate"], serde_json::json!(41_500.0));
        // chain-alive map char ('o' = responding).
        assert_eq!(obj["chain_xtime1"], serde_json::json!("o"));
    }

    // A dead board (0 responding chips) must report 'x' in the alive-map and
    // contribute 0 to total_rate without panicking.
    #[test]
    fn bmminer_stats_object_marks_dead_board() {
        let chains = vec![chain(6, 0, 0, 0, 0.0, 0.0, 0)];
        let obj = build_bmminer_stats_object(&chains, 1, &[]);
        assert_eq!(obj["chain_acn1"], serde_json::json!(0));
        assert_eq!(obj["chain_xtime1"], serde_json::json!("x"));
        assert_eq!(obj["total_rate"], serde_json::json!(0.0));
    }

    // Zero boards (pre-enumeration) must not divide-by-zero on freq_avg.
    #[test]
    fn bmminer_stats_object_handles_empty_chains() {
        let obj = build_bmminer_stats_object(&[], 0, &[]);
        assert_eq!(obj["miner_count"], serde_json::json!(0));
        assert_eq!(obj["freq_avg"], serde_json::json!(0.0));
        assert_eq!(obj["temp_max"], serde_json::json!(0.0));
        assert_eq!(obj["total_rate"], serde_json::json!(0.0));
        // No per-board keys leak when there are no boards.
        assert!(obj.get("chain_acn1").is_none());
    }

    // ── SW-12: CGMiner SUMMARY honesty contract (build_summary_object) ──

    // Device Hardware% is the REAL hardware-error rate (NOT a fabricated 0.0)
    // and tracks hw_errors; Utility = accepted/(uptime/60); and the now-honest
    // Device Hardware% is correctly absent from `_DCENTUnsupported`.
    #[test]
    fn summary_object_device_hardware_pct_is_real_and_not_unsupported() {
        // 10 hw errors out of (40 accepted + 1 rejected + 10 errors) = 51
        // work units => 10/51*100 ≈ 19.6078 %. Matches the Prometheus gauge
        // (0.196078) × 100, by construction (shared `hw_error_percent`).
        let obj = build_summary_object(40, 1, 10, 7_800, 104_250.5, 103_900.0, 60_933.43);

        let dhw = obj["Device Hardware%"]
            .as_f64()
            .expect("Device Hardware% is a number");
        assert!((dhw - 19.607_843).abs() < 1e-5, "got {dhw}");
        // It must NOT be the old fabricated 0.0 once real errors exist.
        assert_ne!(dhw, 0.0);

        // Now that it's computed for real, it is a SUPPORTED field and must
        // not appear in the unsupported list (the core honesty fix).
        let unsupported = obj["_DCENTUnsupported"]
            .as_array()
            .expect("_DCENTUnsupported is an array");
        assert!(
            !unsupported.iter().any(|v| v == "Device Hardware%"),
            "Device Hardware% is computed for real and must not be flagged unsupported"
        );

        // Pin the rest of the honest projection.
        assert_eq!(obj["Accepted"], serde_json::json!(40));
        assert_eq!(obj["Rejected"], serde_json::json!(1));
        assert_eq!(obj["Hardware Errors"], serde_json::json!(10));
        assert_eq!(obj["Best Share"], serde_json::json!(60_933.43));
        // Utility = 40 accepted / (7800s / 60) = 40 / 130 = 0.307692…
        let util = obj["Utility"].as_f64().expect("Utility is a number");
        assert!((util - 0.307_692).abs() < 1e-5, "got {util}");
        // Device Rejected% = 1 / (40+1) * 100 ≈ 2.4390 %.
        let drej = obj["Device Rejected%"]
            .as_f64()
            .expect("Device Rejected% is a number");
        assert!((drej - 2.439_024).abs() < 1e-5, "got {drej}");
    }

    // A healthy miner (real accepted work, zero errors) reports an HONEST 0.0%
    // — indistinguishable from a measured 0 because it IS one.
    #[test]
    fn summary_object_healthy_miner_reports_honest_zero_hardware_pct() {
        let obj = build_summary_object(100, 0, 0, 600, 95_000.0, 95_500.0, 0.0);
        assert_eq!(obj["Device Hardware%"], serde_json::json!(0.0));
        // Still not listed as unsupported — it's a real, computed value.
        let unsupported = obj["_DCENTUnsupported"].as_array().unwrap();
        assert!(!unsupported.iter().any(|v| v == "Device Hardware%"));
    }

    // Pre-mining (no work, uptime 0) must not divide-by-zero anywhere.
    #[test]
    fn summary_object_no_work_zero_uptime_no_divide_by_zero() {
        let obj = build_summary_object(0, 0, 0, 0, 0.0, 0.0, 0.0);
        assert_eq!(obj["Device Hardware%"], serde_json::json!(0.0));
        assert_eq!(obj["Device Rejected%"], serde_json::json!(0.0));
        assert_eq!(obj["Utility"], serde_json::json!(0.0));
        assert_eq!(obj["Total MH"], serde_json::json!(0.0));
        assert_eq!(obj["Elapsed"], serde_json::json!(0));
    }

    // `_DCENTUnsupported` lists exactly the genuinely-unsupported placeholder
    // keys — and Device Hardware% / Accepted / Rejected / Hardware Errors /
    // Utility / Best Share (all sourced) are NOT among them.
    #[test]
    fn summary_object_unsupported_list_is_exactly_the_placeholder_keys() {
        let obj = build_summary_object(40, 1, 10, 7_800, 104_250.5, 103_900.0, 1_024.0);
        let unsupported: Vec<&str> = obj["_DCENTUnsupported"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        let expected = [
            "Found Blocks",
            "Getworks",
            "Discarded",
            "Stale",
            "Get Failures",
            "Local Work",
            "Remote Failures",
            "Network Blocks",
            "Work Utility",
            "Difficulty Stale",
            "Pool Rejected%",
            "Pool Stale%",
            "Last getwork",
        ];
        assert_eq!(unsupported, expected);

        // Genuinely-sourced fields are never flagged unsupported.
        for sourced in [
            "Device Hardware%",
            "Accepted",
            "Rejected",
            "Hardware Errors",
            "Utility",
            "Best Share",
        ] {
            assert!(
                !unsupported.contains(&sourced),
                "{sourced} is sourced and must not be unsupported"
            );
        }
    }

    // ── P1-5 (D-14/D-15): fan_num + Best Share projected from real data ──

    #[test]
    fn fan_rpms_project_from_per_fan_then_legacy_then_empty() {
        use crate::{FanState, PerFanReading};
        // Per-fan telemetry present → one entry per monitored fan, in order.
        let two = FanState {
            pwm: 30,
            rpm: 2940,
            per_fan: vec![
                PerFanReading {
                    id: 0,
                    rpm: 2940,
                    pwm_percent: 30,
                },
                PerFanReading {
                    id: 1,
                    rpm: 3720,
                    pwm_percent: 30,
                },
            ],
        };
        assert_eq!(project_fan_rpms(&two), vec![2940, 3720]);
        // No per-fan breakdown but a legacy primary tach → a single fan.
        let legacy = FanState {
            pwm: 20,
            rpm: 3660,
            per_fan: vec![],
        };
        assert_eq!(project_fan_rpms(&legacy), vec![3660]);
        // No fan data at all → empty (honest "no fans known"), never a phantom.
        let none = FanState {
            pwm: 0,
            rpm: 0,
            per_fan: vec![],
        };
        assert!(project_fan_rpms(&none).is_empty());
    }

    // Required pin: fan_num and Best Share are NON-ZERO when the underlying
    // data is present (the exact bug P1-5 fixes — both were hardcoded 0).
    #[test]
    fn fan_num_and_best_share_are_nonzero_when_data_present() {
        // fan_num / fan{N} projected from the live per-fan RPM list.
        let chains = vec![chain(6, 114, 525, 13_700, 62.5, 21_000.0, 3)];
        let obj = build_bmminer_stats_object(&chains, 1234, &[2940, 3720]);
        assert_eq!(obj["fan_num"], serde_json::json!(2));
        assert_eq!(obj["fan1"], serde_json::json!(2940));
        assert_eq!(obj["fan2"], serde_json::json!(3720));
        assert_ne!(obj["fan_num"], serde_json::json!(0));

        // Best Share = max achieved difficulty across the recent share window.
        let events = vec![
            recent_share("accepted", Some(1_024.0)),
            recent_share("lucky", Some(60_933.43)),
            recent_share("accepted", Some(8_192.0)),
        ];
        let best = best_achieved_share_difficulty(&events);
        assert!((best - 60_933.43).abs() < 1e-6, "got {best}");
        assert_ne!(best, 0.0);
    }

    fn recent_share(result: &str, difficulty: Option<f64>) -> crate::RecentShareEvent {
        crate::RecentShareEvent {
            result: result.to_string(),
            difficulty,
            ..Default::default()
        }
    }

    // Best Share is a MAX of locally-proven achieved difficulty — never a sum,
    // never the pool target, and rejected/unproven shares never count.
    #[test]
    fn best_share_is_max_achieved_not_sum_and_excludes_rejected() {
        let events = vec![
            recent_share("accepted", Some(1_024.0)),
            recent_share("lucky", Some(60_933.43)),
            recent_share("accepted", Some(8_192.0)),
            // A rejected share never counts toward Best Share…
            recent_share("rejected", Some(999_999.0)),
            // …nor does an accepted share with no locally-proven difficulty.
            recent_share("accepted", None),
        ];
        let best = best_achieved_share_difficulty(&events);
        assert!((best - 60_933.43).abs() < 1e-6, "got {best}");

        // No accepted share with a proven difficulty yet → 0 (cgminer "none"),
        // honestly, instead of fabricating a value.
        assert_eq!(best_achieved_share_difficulty(&[]), 0.0);
        assert_eq!(
            best_achieved_share_difficulty(&[recent_share("accepted", None)]),
            0.0
        );
        // Non-finite/zero/negative diffs are ignored, never propagated.
        assert_eq!(
            best_achieved_share_difficulty(&[
                recent_share("accepted", Some(f64::NAN)),
                recent_share("accepted", Some(0.0)),
                recent_share("accepted", Some(-5.0)),
            ]),
            0.0
        );
    }

    // ── W18: cgminer `check` capability-probe verb recognition ──

    #[test]
    fn check_probe_recognizes_known_and_rejects_unknown() {
        // Core read/control verbs + the W18 additions are recognized.
        for c in [
            "summary",
            "stats",
            "pools",
            "devs",
            "version",
            "config",
            "coin",
            "switchpool",
            "addpool",
            "restart",
            "quit",
            "devdetails",
            "asccount",
            "check",
            "notify",
        ] {
            assert!(super::is_known_command(c), "check should recognize {c:?}");
        }
        // Unknown / empty / wrong-case verbs are not recognized (cgminer
        // verbs are exact lowercase).
        for c in ["", "bogus", "frobnicate", "SUMMARY", "DevDetails"] {
            assert!(
                !super::is_known_command(c),
                "check must NOT recognize {c:?}"
            );
        }
    }

    // ── Item 1: version fingerprint — real hardware MODEL, honest firmware ──

    fn hw(chip_type: &str) -> crate::HardwareInfo {
        crate::HardwareInfo {
            chip_type: chip_type.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn hardware_model_resolves_real_antminer_model_from_chip() {
        // The canonical MinerProfile table maps the detected ASIC to a real
        // hardware model string for pyasic/asic-rs `Type` classification.
        // (Exact names come from dcentrald_asic::drivers::MINER_PROFILES.)
        let s9 = hardware_model_label(&hw("BM1387"));
        assert!(
            s9.starts_with("Antminer"),
            "BM1387 should map to an Antminer model, got {s9:?}"
        );
        let s19pro = hardware_model_label(&hw("BM1398"));
        assert!(s19pro.starts_with("Antminer"), "got {s19pro:?}");
        // Case-insensitive chip-type matching.
        assert_eq!(
            hardware_model_label(&hw("bm1387")),
            hardware_model_label(&hw("BM1387"))
        );
    }

    #[test]
    fn hardware_model_falls_back_without_claiming_competitor_firmware() {
        // Unknown chip → honest fallback, never an empty/garbage Type.
        let unknown = hardware_model_label(&hw("BM9999"));
        assert!(unknown.contains("BM9999"), "got {unknown:?}");
        let empty = hardware_model_label(&hw(""));
        assert_eq!(empty, "Antminer (unknown ASIC)");
        // hb_type is used when chip-type doesn't resolve a profile.
        let mut h = hw("");
        h.hb_type = Some("BHB42831".to_string());
        assert_eq!(hardware_model_label(&h), "BHB42831");
    }

    #[test]
    fn firmware_identity_marker_is_honest_dcentos_not_competitor() {
        // The firmware-stack identity must never impersonate Antminer/
        // BraiinsOS/LuxOS/VNish. The version object reports the real HARDWARE
        // model via Type, but the firmware markers stay DCENT_OS.
        assert_eq!(DCENTOS_FIRMWARE_MARKER, "DCENTOS");
        assert!(CGMINER_VERSION.starts_with("dcentrald/"));
        for forbidden in ["bmminer/", "BraiinsOS", "LuxOS", "VNish", "Antminer/"] {
            assert!(
                !CGMINER_VERSION.contains(forbidden),
                "firmware version must not impersonate {forbidden}"
            );
        }
    }

    // ── Item 3: multi-pool POOLS array ──

    fn miner_state(active_url: &str, status: &str, accepted: u64) -> crate::MinerState {
        serde_json::from_value(serde_json::json!({
            "hashrate_ghs": 0.0,
            "hashrate_5s_ghs": 0.0,
            "accepted": accepted,
            "rejected": 0u64,
            "chains": [],
            "fans": { "pwm": 0u8, "rpm": 0u32 },
            "pool": {
                "url": active_url,
                "worker": "bc1qworker",
                "status": status,
                "difficulty": 512.0,
                "last_share_at": 1700u64
            },
            "uptime_s": 100u64,
            "firmware_version": "0.5.0",
            "mode": "standard"
        }))
        .expect("MinerState fixture must deserialize")
    }

    #[test]
    fn pool_alive_is_case_insensitive() {
        assert!(is_pool_alive("Alive"));
        assert!(is_pool_alive("alive"));
        assert!(!is_pool_alive("Dead"));
        assert!(!is_pool_alive("Standby"));
    }

    #[test]
    fn active_pool_entry_carries_live_stats() {
        let miner = miner_state("stratum+tcp://pool.example:3333", "Alive", 42);
        let entry = build_cg_pool_entry(
            0,
            "stratum+tcp://pool.example:3333",
            "bc1qworker",
            0,
            "Alive",
            true,
            &miner,
            60_933.43,
        );
        assert_eq!(entry["POOL"], serde_json::json!(0));
        assert_eq!(entry["Priority"], serde_json::json!(0));
        assert_eq!(entry["Accepted"], serde_json::json!(42));
        assert_eq!(entry["Status"], serde_json::json!("Alive"));
        assert_eq!(entry["Stratum Active"], serde_json::json!(true));
        assert_eq!(entry["User"], serde_json::json!("bc1qworker"));
        // P1-5: the active pool carries the real best achieved difficulty…
        let best = entry["Best Share"]
            .as_f64()
            .expect("Best Share is a number");
        assert!((best - 60_933.43).abs() < 1e-6, "got {best}");
        // …and "Best Share" is no longer flagged unsupported for the active pool.
        let unsupported = entry["_DCENTUnsupported"]
            .as_array()
            .expect("unsupported list");
        assert!(
            !unsupported.iter().any(|v| v.as_str() == Some("Best Share")),
            "active pool must not flag Best Share unsupported"
        );
    }

    // TEL-4 ( masking) NEGATIVE regression: the cgminer `User` field is the
    // pool worker, which for V1 solo is the operator's full BTC payout address.
    // The Prometheus exporter masks this identical value, so the LAN-exposed
    // cgminer API MUST too. A full bech32 worker must NOT survive raw on the wire.
    #[test]
    fn cg_pool_entry_user_masks_full_wallet_worker() {
        let full_worker = "";
        let miner = miner_state("stratum+tcp://pool.example:3333", "Alive", 7);
        let entry = build_cg_pool_entry(
            0,
            "stratum+tcp://pool.example:3333",
            full_worker,
            0,
            "Alive",
            true,
            &miner,
            0.0,
        );
        let user = entry["User"].as_str().expect("User is a string");
        // The masked form is emitted, not the raw address.
        assert_ne!(
            user, full_worker,
            "raw wallet worker leaked in cgminer User"
        );
        assert_eq!(
            user,
            dcentrald_common::wallet_mask::mask_wallet(full_worker)
        );
        // NEGATIVE: no raw bech32 body survives anywhere in the serialized entry.
        let serialized = serde_json::to_string(&entry).expect("serialize pool entry");
        assert!(
            !serialized.contains("dzgmtjex6jlsv2fwhe4se4jxje6"),
            "cgminer pool entry leaked raw bech32 body: {serialized}"
        );
        assert!(
            !serialized.contains(full_worker),
            "cgminer pool entry leaked full wallet: {serialized}"
        );
    }

    // TEL-4: inline credentials in a pool URL must also never survive on the
    // cgminer `URL`/`Stratum URL` fields (sanitize_pool_url parity).
    #[test]
    fn cg_pool_entry_url_strips_inline_credentials() {
        let miner = miner_state("stratum+tcp://pool.example:3333", "Alive", 1);
        let entry = build_cg_pool_entry(
            0,
            "stratum+tcp://user:secret@pool.example.com:3333",
            "rig.1",
            0,
            "Alive",
            true,
            &miner,
            0.0,
        );
        let serialized = serde_json::to_string(&entry).expect("serialize pool entry");
        assert!(
            !serialized.contains("secret") && !serialized.contains(":secret@"),
            "cgminer pool entry leaked inline pool credentials: {serialized}"
        );
    }

    // API-3 ( truthfulness): per-chain Accepted/Rejected are structurally
    // unavailable (share accounting is pool-session-level). They must stay 0 for
    // schema shape BUT carry an honest unsupported/provenance marker so a fleet
    // tool never reads the fabricated 0 as a real per-chain count.
    #[test]
    fn cg_chain_stats_row_marks_accept_reject_unsupported() {
        let chain = CgStatsChain {
            id: 1,
            chips: 126,
            frequency_mhz: 500,
            voltage_mv: 13_700,
            temp_c: 49.3,
            hashrate_ghs: 1050.0,
            errors: 3,
            status: "mining".to_string(),
        };
        let row = build_cg_chain_stats_row(&chain, 1234);
        assert_eq!(row["Accepted"], serde_json::json!(0));
        assert_eq!(row["Rejected"], serde_json::json!(0));
        let unsupported = row["_DCENTUnsupported"]
            .as_array()
            .expect("_DCENTUnsupported list");
        assert!(
            unsupported.iter().any(|v| v.as_str() == Some("Accepted")),
            "Accepted must be flagged unsupported"
        );
        assert!(
            unsupported.iter().any(|v| v.as_str() == Some("Rejected")),
            "Rejected must be flagged unsupported"
        );
        // The provenance marker must say "not tracked", not present 0 as real.
        let sources = &row["_DCENTFieldSources"];
        assert!(
            sources["Accepted"]
                .as_str()
                .is_some_and(|s| s.contains("not tracked")),
            "Accepted provenance must disclose it is not tracked per chain"
        );
        assert!(
            sources["Rejected"]
                .as_str()
                .is_some_and(|s| s.contains("not tracked")),
            "Rejected provenance must disclose it is not tracked per chain"
        );
        // Real per-chain telemetry is still preserved.
        assert_eq!(row["Hardware Errors"], serde_json::json!(3));
        assert_eq!(row["chain_acn"], serde_json::json!(126));
    }

    #[test]
    fn standby_pool_entry_zeroes_stats_but_keeps_identity() {
        let miner = miner_state("stratum+tcp://primary:3333", "Alive", 42);
        // A failover pool that is NOT the active session.
        let entry = build_cg_pool_entry(
            1,
            "stratum+tcp://backup:3333",
            "bc1qbackup",
            1,
            "Standby",
            false,
            &miner,
            60_933.43,
        );
        assert_eq!(entry["POOL"], serde_json::json!(1));
        assert_eq!(entry["Priority"], serde_json::json!(1));
        // Runtime tracks ONE active session → standby pools report 0 shares,
        // not the active pool's counters (which would be a lie).
        assert_eq!(entry["Accepted"], serde_json::json!(0));
        assert_eq!(entry["Rejected"], serde_json::json!(0));
        assert_eq!(entry["Stratum Active"], serde_json::json!(false));
        // A standby pool has no achieved difficulty of its own → 0, and keeps
        // "Best Share" flagged unsupported (it is not this pool's metric).
        assert_eq!(entry["Best Share"], serde_json::json!(0.0));
        let unsupported = entry["_DCENTUnsupported"]
            .as_array()
            .expect("unsupported list");
        assert!(unsupported.iter().any(|v| v.as_str() == Some("Best Share")));
        // …but the configured identity (URL/Priority/User) is truthful.
        assert_eq!(entry["URL"], serde_json::json!("stratum+tcp://backup:3333"));
        assert_eq!(entry["User"], serde_json::json!("bc1qbackup"));
    }

    #[test]
    fn configured_pool_reader_tolerates_missing_config() {
        // No config file at the test path → empty (caller falls back to
        // single live-state pool), never a panic.
        let pools = cg_read_configured_pools();
        // We can't assert the contents (depends on the host's config path),
        // but the call must not panic and must return a Vec.
        let _ = pools.len();
    }

    // ── API-1: LAN-write gate (pure helpers) ────────────────────────────

    #[test]
    fn mutating_verb_classification_is_precise() {
        // State-mutating verbs (pools / tuning / fan / curtail / lifecycle).
        for v in [
            "voltageset",
            "frequencyset",
            "fanset",
            "curtail",
            "addpool",
            "switchpool",
            "enablepool",
            "disablepool",
            "removepool",
            "profileset",
            "profilenew",
            "profilerem",
            "autotunerset",
            "atmset",
            "powertargetset",
            "tempctrlset",
            "netset",
            "ledset",
            "psuset",
            "immersionswitch",
            "restart",
            "quit",
        ] {
            assert!(is_mutating_verb(v), "{v} must be classified mutating");
        }
        // Pure reads, telemetry, and session-lifecycle gateways are NOT
        // mutating — they must stay open from LAN when bind_lan is set.
        for v in [
            "summary",
            "stats",
            "pools",
            "devs",
            "version",
            "coin",
            "config",
            "devdetails",
            "asccount",
            "check",
            "notify",
            "metrics",
            "events",
            "systemaudit",
            "limits",
            "healthchipget",
            "logon",
            "logoff",
            "session",
            "kill",
        ] {
            assert!(!is_mutating_verb(v), "{v} must NOT be classified mutating");
        }
    }

    #[test]
    fn lan_write_allowed_loopback_always_others_only_when_enabled() {
        let loopback_v4: SocketAddr = "127.0.0.1:55000".parse().unwrap();
        let loopback_v6: SocketAddr = "[::1]:55000".parse().unwrap();
        let lan: SocketAddr = "203.0.113.50:55000".parse().unwrap();

        // Loopback is always allowed, regardless of the flag.
        assert!(lan_write_allowed(&loopback_v4, false));
        assert!(lan_write_allowed(&loopback_v6, false));
        assert!(lan_write_allowed(&loopback_v4, true));

        // Non-loopback peers are refused by default (fail-closed)…
        assert!(!lan_write_allowed(&lan, false));
        // …and allowed only when cgminer_lan_writes is explicitly enabled.
        assert!(lan_write_allowed(&lan, true));
    }

    #[test]
    fn lan_restricted_verb_covers_mutations_and_kill_but_not_reads() {
        // Every state mutation + `kill` (session eviction) is LAN-restricted.
        for v in [
            "voltageset",
            "frequencyset",
            "fanset",
            "curtail",
            "addpool",
            "switchpool",
            "enablepool",
            "disablepool",
            "removepool",
            "profileset",
            "restart",
            "quit",
            "kill",
            // High-impact hardware/curtailment mutations: a non-loopback peer must
            // NOT be able to reach these. psuset/voltageset/frequencyset/fanset can
            // damage hardware; `curtail` is a remote denial-of-mining (its `sleep`/
            // `wake` PARAMETER is not itself a top-level verb); immersionswitch
            // flips the cooling regime; tempctrlset moves the thermal ladder. All
            // are Session-auth catalog verbs — pin that they stay LAN-restricted so
            // a catalog auth-tier downgrade of any of them fails HERE.
            "psuset",
            "voltageset",
            "frequencyset",
            "fanset",
            "tempctrlset",
            "curtail",
            "immersionswitch",
        ] {
            assert!(is_lan_restricted_verb(v), "{v} must be LAN-restricted");
        }
        // Reads, telemetry, and own-session lifecycle stay open from LAN.
        for v in [
            "summary", "stats", "pools", "devs", "version", "config", "metrics", "events", "logon",
            "logoff", "session",
        ] {
            assert!(!is_lan_restricted_verb(v), "{v} must NOT be LAN-restricted");
        }
    }
}
