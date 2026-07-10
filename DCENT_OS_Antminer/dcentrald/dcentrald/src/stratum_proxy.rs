//! Stratum TCP relay for the S19j Pro proxy path.
//!
//! A dumb bidirectional TCP relay. No share rewriting, no protocol mutation.
//! Byte-identical forwarding between bosminer (the local client) and an
//! upstream pool endpoint.
//!
//! # Why this exists
//!
//! On the S19j Pro am2 platform bosminer owns the FPGA / PIC / PSU state.
//! Every attempt we made to pause bosminer or force the UART relay from
//! outside ended up cutting PSU voltage (see
//!  and
//! ). The decision in
//!  was path γ: leave bosminer running
//! hardware, but redirect its pool traffic through dcentrald so we can swap
//! pools / observe traffic / inject future features without ever touching the
//! hardware path.
//!
//! On `a lab unit` bosminer does not speak only one protocol shape:
//!   - the user pool session starts as ordinary Stratum V1 JSON-RPC
//!   - later dev-fee sessions are 64-byte binary payloads that align with the
//!     stock `a830bcc3.bos.braiins.com:3336` Stratum2/Noise path
//!
//! We therefore inspect only the first inbound chunk so we can route JSON-RPC
//! sessions to the configured user pool and route non-JSON sessions to an
//! optional alternate upstream. After that one routing choice, traffic stays
//! completely transparent.
//!
//! # Design contract (from D1 review, `phase11a_D1_review.md`)
//!
//! 1. `tokio::io::copy_bidirectional` — NOT a custom `try_join!` copy pair.
//!    The custom pattern stalls when one side half-closes.
//! 2. `TCP_NODELAY` set on BOTH sockets immediately after accept / connect.
//!    Nagle adds up to ~200ms per share submit → stale shares. Mirrors
//!    `dcentrald-stratum/src/v1/connection.rs:91`.
//! 3. DNS / upstream connect failure → drop inbound immediately so bosminer
//!    sees a clean RST and enters its normal reconnect backoff. Never leave
//!    an inbound hanging without an upstream.
//! 4. One task per accepted connection. The accept loop never blocks.
//! 5. Clean shutdown via `CancellationToken`. SIGTERM must drain in <1s.
//! 6. Log connection lifecycle (open, route, bytes forwarded, close). The only
//!    protocol inspection allowed is first-chunk classification for routing;
//!    all forwarded bytes remain verbatim.
//!
//! # Invariants (enforced by review — keep this list in sync with the code)
//!
//! - NO imports from `dcentrald_hal::psu`, `::dspic`, `::i2c`, `::fpga_chain`,
//!   `::glitch_monitor`, `::serial_chain` (W13.B1: `uart_relay` was renamed
//!   into `glitch_monitor::BraiinsGlitchMonitor`).
//! - NO writes to `/dev/mem`, `/dev/i2c*`, `/dev/ttyS*`.
//! - NO shelling out to `bosminer api *` or any other system command.
//! - Forwarded bytes are VERBATIM — no mutation, no rewrite.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use dcentrald_api::{
    RuntimeBosminerHealth, RuntimeBosminerSummary, RuntimeHealthMode, RuntimeHealthSnapshot,
    RuntimeScrapeHealth,
};
use dcentrald_stratum::url_validator::{validate_sv2_pool_url, validate_v1_pool_url};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::DcentraldConfig;

const BOSMINER_CGMINER_URL: &str = "http://127.0.0.1:4028";
const BLOCKER_CGMINER_UNREACHABLE: u64 = 1 << 0;

/// Shared counters updated by the Stratum sniffer + bosminer health task.
///
/// Read by the dashboard through the runtime-health publisher. The fields are
/// independent atomics so we do not need a Mutex on the hot path.
#[derive(Debug, Default)]
pub struct ProxiedStats {
    /// Whether bosminer's CGMiner API at 127.0.0.1:4028 has answered recently.
    pub bosminer_alive: AtomicBool,
    /// Unix epoch seconds of the most recent successful bosminer summary poll.
    pub bosminer_last_seen_s: AtomicU64,
    /// Sniffed `mining.notify` count (jobs dispatched by upstream pool).
    pub jobs_seen: AtomicU64,
    /// Sniffed `mining.submit` count (shares submitted by bosminer).
    pub shares_submitted: AtomicU64,
    /// Most recent `accepted` count from `bosminer api summary`.
    pub bosminer_accepted: AtomicU64,
    /// Most recent `rejected` count from `bosminer api summary`.
    pub bosminer_rejected: AtomicU64,
    /// Unix epoch seconds of last sniffed `mining.submit`.
    pub last_share_at_s: AtomicU64,
    /// Most recent reported MHS (5s) from bosminer summary, scaled by 1000
    /// so we can store as u64 (i.e. value = MHS * 1000). Avoids floats in
    /// atomics. 0 = unknown.
    pub bosminer_mhs5s_milli: AtomicU64,
    /// Unix epoch seconds of the most recent summary poll attempt.
    pub bosminer_last_poll_s: AtomicU64,
    /// Whether the most recent CGMiner summary scrape reached bosminer.
    pub bosminer_cgminer_reachable: AtomicBool,
    /// Consecutive summary poll failures.
    pub bosminer_consecutive_failures: AtomicU64,
    /// Bitmask of known blockers detected by production-safe scrapes.
    pub bosminer_blocker_mask: AtomicU64,
}

impl ProxiedStats {
    /// Construct a new zeroed stats structure.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn blocker_names(&self) -> Vec<String> {
        let mask = self.bosminer_blocker_mask.load(Ordering::Relaxed);
        let mut blockers = Vec::new();
        if mask & BLOCKER_CGMINER_UNREACHABLE != 0 {
            blockers.push("cgminer_unreachable".to_string());
        }
        blockers
    }
}

const FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(2);
const FIRST_CHUNK_PREVIEW_BYTES: usize = 192;
const INITIAL_TRAFFIC_WINDOW: Duration = Duration::from_secs(5);
const INITIAL_TRAFFIC_MAX_EVENTS: usize = 6;

#[derive(Clone, Debug)]
struct ParsedUpstream {
    label: &'static str,
    host: String,
    port: u16,
}

impl ParsedUpstream {
    fn parse(label: &'static str, url: &str) -> Result<Self> {
        let (host, port) = parse_stratum_url(url)?;
        Ok(Self { label, host, port })
    }

    fn peer_label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionRoute {
    PrimaryJson,
    Binary,
}

impl SessionRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryJson => "primary-json",
            Self::Binary => "binary",
        }
    }
}

/// Run the Stratum V1 TCP relay until `shutdown` fires.
///
/// Binds to `config.stratum_proxy.listen_addr`, resolves / connects to
/// `config.stratum_proxy.upstream_url` on each accepted session, and forwards
/// bytes both directions until either side closes or shutdown is requested.
///
/// `stats` is an optional shared counter set updated by the JSON-RPC sniffer
/// during the initial-traffic window. When `None`, the relay still works but
/// the dashboard sees zeros for jobs / shares.
///
/// Returns `Err` only if the listener bind fails — bind conflicts are fatal
/// so the deploy script can roll back cleanly (see D1 review note #6).
pub async fn run(
    config: DcentraldConfig,
    shutdown: CancellationToken,
    stats: Option<Arc<ProxiedStats>>,
) -> Result<()> {
    let proxy_cfg = config
        .stratum_proxy
        .as_ref()
        .context("[stratum_proxy] section required when --stratum-proxy is set")?;
    let listen_addr: SocketAddr = proxy_cfg.listen_addr.parse().with_context(|| {
        format!(
            "invalid stratum_proxy.listen_addr: {}",
            proxy_cfg.listen_addr
        )
    })?;
    let primary_upstream = ParsedUpstream::parse("primary", &proxy_cfg.upstream_url)?;
    let binary_upstream = proxy_cfg
        .binary_upstream_url
        .as_deref()
        .map(|url| ParsedUpstream::parse("binary", url))
        .transpose()?;

    info!(
        listen = %listen_addr,
        primary_upstream = %primary_upstream.peer_label(),
        binary_upstream = %binary_upstream.as_ref().map(|u| u.peer_label()).unwrap_or_else(|| "disabled".to_string()),
        "=== DCENT_OS Stratum proxy starting ==="
    );

    // Bind-fail is FATAL — do NOT retry in a loop. If another process holds
    // :3333 the deploy script needs a non-zero exit to roll back.
    let listener = TcpListener::bind(&listen_addr)
        .await
        .with_context(|| format!("bind {}", listen_addr))?;

    info!(listen = %listen_addr, "relay listening — awaiting bosminer connections");

    let mut session_id: u64 = 0;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("shutdown requested — stopping accept loop");
                break;
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((inbound, peer)) => {
                        session_id = session_id.wrapping_add(1);
                        let sid = session_id;
                        let primary_upstream = primary_upstream.clone();
                        let binary_upstream = binary_upstream.clone();
                        let session_shutdown = shutdown.child_token();
                        let session_stats = stats.clone();
                        tokio::spawn(async move {
                            match handle_session(
                                sid,
                                inbound,
                                peer,
                                primary_upstream,
                                binary_upstream,
                                session_shutdown,
                                session_stats,
                            )
                            .await
                            {
                                Ok(()) => {
                                    info!(session = sid, %peer, "session ended cleanly");
                                }
                                Err(e) => {
                                    warn!(session = sid, %peer, error = %e, "session ended with error");
                                }
                            }
                        });
                    }
                    Err(e) => {
                        // listener.accept errors are almost always transient
                        // (fd exhaustion, EMFILE, etc). Back off briefly and
                        // keep serving.
                        error!(error = %e, "listener.accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    info!("stratum relay stopped");
    Ok(())
}

/// Handle one bosminer session: connect upstream, bridge bytes, log close.
async fn handle_session(
    session_id: u64,
    mut inbound: TcpStream,
    peer: SocketAddr,
    primary_upstream: ParsedUpstream,
    binary_upstream: Option<ParsedUpstream>,
    shutdown: CancellationToken,
    stats: Option<Arc<ProxiedStats>>,
) -> Result<()> {
    info!(session = session_id, %peer, "bosminer connected");

    // TCP_NODELAY on inbound immediately — Nagle's algorithm can add up to
    // ~200ms per share-submit line, which directly inflates stale shares.
    // Mirrors dcentrald-stratum/src/v1/connection.rs:91.
    if let Err(e) = inbound.set_nodelay(true) {
        warn!(session = session_id, error = %e, "inbound set_nodelay failed (continuing)");
    }

    let first_chunk = read_first_chunk(session_id, &mut inbound).await?;
    let mut route = classify_first_chunk(&first_chunk);
    let selected_upstream = match (route, binary_upstream) {
        (SessionRoute::PrimaryJson, _) => primary_upstream,
        (SessionRoute::Binary, Some(upstream)) => upstream,
        (SessionRoute::Binary, None) => {
            warn!(
                session = session_id,
                bytes = first_chunk.len(),
                "binary first chunk detected but no binary_upstream_url configured; falling back to primary upstream"
            );
            route = SessionRoute::PrimaryJson;
            primary_upstream
        }
    };

    info!(
        session = session_id,
        route = route.as_str(),
        upstream_kind = selected_upstream.label,
        upstream = %selected_upstream.peer_label(),
        preview_bytes = first_chunk.len(),
        has_newline = first_chunk.contains(&b'\n'),
        "proxy route selected"
    );

    let mut upstream = connect_upstream(session_id, &selected_upstream).await?;

    if let Err(e) = upstream.set_nodelay(true) {
        warn!(session = session_id, error = %e, "upstream set_nodelay failed (continuing)");
    }

    let upstream_peer = upstream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| selected_upstream.peer_label());
    info!(
        session = session_id,
        route = route.as_str(),
        upstream = %upstream_peer,
        "upstream connected — bridging bytes"
    );

    relay_initial_traffic(
        session_id,
        &mut inbound,
        &mut upstream,
        first_chunk,
        route,
        stats.clone(),
    )
    .await?;

    // tokio::io::copy_bidirectional handles both half-close directions
    // correctly and returns (up_bytes, down_bytes). A custom try_join! copy
    // pair would stall if one side half-closes without closing write.
    let bridge = tokio::io::copy_bidirectional(&mut inbound, &mut upstream);

    tokio::select! {
        r = bridge => {
            match r {
                Ok((up_bytes, down_bytes)) => {
                    info!(
                        session = session_id,
                        up_bytes,
                        down_bytes,
                        "bridge closed by peer"
                    );
                }
                Err(e) => {
                    warn!(
                        session = session_id,
                        error = %e,
                        "bridge ended with I/O error"
                    );
                    return Err(anyhow::Error::from(e).context("copy_bidirectional"));
                }
            }
        }
        _ = shutdown.cancelled() => {
            info!(session = session_id, "shutdown — dropping bridge; kernel will RST both sockets");
        }
    }

    Ok(())
}

fn chunk_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(FIRST_CHUNK_PREVIEW_BYTES)
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

fn chunk_ascii(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes.iter().take(FIRST_CHUNK_PREVIEW_BYTES) {
        match b {
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7E => out.push(b as char),
            _ => out.push('.'),
        }
    }
    out
}

fn looks_like_json_rpc_line(chunk: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(chunk) else {
        return false;
    };
    let Some(newline) = text.find('\n') else {
        return false;
    };
    let line = &text[..newline];
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    msg.get("method").and_then(Value::as_str).is_some()
}

fn classify_first_chunk(chunk: &[u8]) -> SessionRoute {
    if chunk.is_empty() {
        return SessionRoute::PrimaryJson;
    }
    if looks_like_json_rpc_line(chunk) {
        SessionRoute::PrimaryJson
    } else {
        SessionRoute::Binary
    }
}

fn log_chunk(session_id: u64, direction: &str, bytes: &[u8], event_idx: usize) {
    info!(
        session = session_id,
        event_idx,
        direction,
        bytes = bytes.len(),
        has_newline = bytes.contains(&b'\n'),
        ascii = %chunk_ascii(bytes),
        hex = %chunk_hex(bytes),
        "chunk"
    );
}

async fn relay_initial_traffic(
    session_id: u64,
    inbound: &mut TcpStream,
    upstream: &mut TcpStream,
    initial_inbound_chunk: Vec<u8>,
    route: SessionRoute,
    stats: Option<Arc<ProxiedStats>>,
) -> Result<()> {
    let mut inbound_buf = [0u8; 1024];
    let mut upstream_buf = [0u8; 1024];

    let deadline = tokio::time::Instant::now() + INITIAL_TRAFFIC_WINDOW;
    let mut event_idx = 0usize;
    let mut saw_any = false;

    if !initial_inbound_chunk.is_empty() {
        saw_any = true;
        log_chunk(
            session_id,
            "bosminer->proxy",
            &initial_inbound_chunk,
            event_idx,
        );
        if matches!(route, SessionRoute::PrimaryJson) {
            sniff_jsonrpc_chunk(
                session_id,
                "bosminer->proxy",
                &initial_inbound_chunk,
                &stats,
            );
        }
        upstream
            .write_all(&initial_inbound_chunk)
            .await
            .context("forward initial inbound chunk")?;
        event_idx += 1;
    }

    while event_idx < INITIAL_TRAFFIC_MAX_EVENTS {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let timeout = deadline - now;

        tokio::select! {
            biased;
            inbound_res = tokio::time::timeout(timeout, inbound.read(&mut inbound_buf)) => {
                match inbound_res {
                    Ok(Ok(0)) => {
                        warn!(session = session_id, event_idx, "inbound closed during initial traffic window");
                        break;
                    }
                    Ok(Ok(n)) => {
                        saw_any = true;
                        let chunk = &inbound_buf[..n];
                        log_chunk(session_id, "bosminer->proxy", chunk, event_idx);
                        if matches!(route, SessionRoute::PrimaryJson) {
                            sniff_jsonrpc_chunk(session_id, "bosminer->proxy", chunk, &stats);
                        }
                        upstream.write_all(chunk).await.context("forward inbound chunk")?;
                        event_idx += 1;
                    }
                    Ok(Err(e)) => {
                        return Err(anyhow::Error::from(e).context("read inbound chunk during initial traffic"));
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
            upstream_res = tokio::time::timeout(timeout, upstream.read(&mut upstream_buf)) => {
                match upstream_res {
                    Ok(Ok(0)) => {
                        warn!(session = session_id, event_idx, "upstream closed during initial traffic window");
                        break;
                    }
                    Ok(Ok(n)) => {
                        saw_any = true;
                        let chunk = &upstream_buf[..n];
                        log_chunk(session_id, "proxy->bosminer", chunk, event_idx);
                        if matches!(route, SessionRoute::PrimaryJson) {
                            sniff_jsonrpc_chunk(session_id, "proxy->bosminer", chunk, &stats);
                        }
                        inbound.write_all(chunk).await.context("forward upstream chunk")?;
                        event_idx += 1;
                    }
                    Ok(Err(e)) => {
                        return Err(anyhow::Error::from(e).context("read upstream chunk during initial traffic"));
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        }
    }

    if !saw_any {
        warn!(
            session = session_id,
            timeout_ms = INITIAL_TRAFFIC_WINDOW.as_millis() as u64,
            "no initial traffic observed before relay handoff"
        );
    }

    Ok(())
}

async fn read_first_chunk(session_id: u64, inbound: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(FIRST_CHUNK_TIMEOUT, inbound.read(&mut buf)).await {
        Ok(Ok(0)) => anyhow::bail!("inbound closed before first chunk"),
        Ok(Ok(n)) => Ok(buf[..n].to_vec()),
        Ok(Err(e)) => Err(anyhow::Error::from(e).context("read first inbound chunk")),
        Err(_) => {
            warn!(
                session = session_id,
                timeout_ms = FIRST_CHUNK_TIMEOUT.as_millis() as u64,
                "no first chunk before timeout; defaulting to primary upstream"
            );
            Ok(Vec::new())
        }
    }
}

async fn connect_upstream(session_id: u64, upstream: &ParsedUpstream) -> Result<TcpStream> {
    match tokio::time::timeout(
        Duration::from_secs(5),
        TcpStream::connect((upstream.host.as_str(), upstream.port)),
    )
    .await
    {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => {
            warn!(
                session = session_id,
                upstream_kind = upstream.label,
                upstream_host = %upstream.host,
                upstream_port = upstream.port,
                error = %e,
                "upstream connect failed — closing inbound so bosminer retries"
            );
            Err(anyhow::Error::from(e).context("upstream connect"))
        }
        Err(_) => {
            warn!(
                session = session_id,
                upstream_kind = upstream.label,
                upstream_host = %upstream.host,
                upstream_port = upstream.port,
                "upstream connect timeout (5s) — closing inbound so bosminer retries"
            );
            anyhow::bail!("upstream connect timeout after 5s")
        }
    }
}

/// Parse a Stratum pool URL into (host, port).
///
/// Accepts:
///   - `stratum+tcp://host:port`
///   - `stratum2+tcp://host:port`
///
/// Current scope note: the native `dcentrald-stratum` V1 client supports TLS,
/// but this byte-forwarding proxy rejects TLS because it does not terminate it.
fn parse_stratum_url(url: &str) -> Result<(String, u16)> {
    let trimmed = url.trim();
    if trimmed.starts_with("stratum+ssl://") || trimmed.starts_with("stratum+tls://") {
        anyhow::bail!(
            "stratum_proxy upstream TLS URLs are unsupported; use the native Stratum V1 client for stratum+tls:// or stratum+ssl:// pools"
        );
    }

    if let Ok(valid) = validate_v1_pool_url(url) {
        return Ok((valid.host, valid.port));
    }
    if let Ok(valid) = validate_sv2_pool_url(url) {
        return Ok((valid.host, valid.port));
    }

    anyhow::bail!(
        "stratum_proxy upstream URL must be stratum+tcp://host:port or stratum2+tcp://host:port (got {})",
        url
    )
}

fn now_epoch_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Inspect a forwarded chunk for newline-delimited JSON-RPC messages and
/// update `stats` in place. Bytes are NOT mutated — this is read-only.
///
/// Counts:
/// - `mining.notify` (upstream→bosminer): increments `jobs_seen`
/// - `mining.submit` (bosminer→upstream): increments `shares_submitted`
///   and stamps `last_share_at_s` with current epoch seconds.
///
/// Tolerates partial-line buffering by silently ignoring lines that don't
/// parse — the goal is best-effort observability, not protocol re-parsing.
fn sniff_jsonrpc_chunk(
    session_id: u64,
    direction: &str,
    chunk: &[u8],
    stats: &Option<Arc<ProxiedStats>>,
) {
    let Some(s) = stats.as_ref() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(chunk) else {
        return;
    };
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue;
        };
        match method {
            "mining.notify" if direction == "proxy->bosminer" => {
                s.jobs_seen.fetch_add(1, Ordering::Relaxed);
                debug!(session = session_id, "sniffed mining.notify");
            }
            "mining.submit" if direction == "bosminer->proxy" => {
                s.shares_submitted.fetch_add(1, Ordering::Relaxed);
                let now = now_epoch_s();
                s.last_share_at_s.store(now, Ordering::Relaxed);
                debug!(session = session_id, "sniffed mining.submit");
            }
            _ => {}
        }
    }
}

/// Spawn a background task that polls bosminer's CGMiner API at
/// `127.0.0.1:4028` every 10 seconds and updates `stats.bosminer_alive`
/// after 3 consecutive failures. Successful polls extract `Accepted`,
/// `Rejected`, and `MHS 5s` from the `summary` response.
///
/// The task respects `shutdown` and exits cleanly when cancelled.
///
/// Safe to call even when bosminer is not running — failures are logged at
/// `debug!` level so the operator's log isn't spammed.
pub fn spawn_bosminer_health_task(
    stats: Arc<ProxiedStats>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        // First tick fires immediately — skip it so we don't race deploy.
        interval.tick().await;
        let mut consecutive_failures: u32 = 0;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("bosminer health task: shutdown");
                    break;
                }
                _ = interval.tick() => {
                    let poll_s = now_epoch_s();
                    stats.bosminer_last_poll_s.store(poll_s, Ordering::Relaxed);
                    match poll_bosminer_summary().await {
                        Ok(summary) => {
                            consecutive_failures = 0;
                            stats.bosminer_alive.store(true, Ordering::Relaxed);
                            stats.bosminer_cgminer_reachable.store(true, Ordering::Relaxed);
                            stats.bosminer_consecutive_failures.store(0, Ordering::Relaxed);
                            stats.bosminer_blocker_mask.store(0, Ordering::Relaxed);
                            let now = now_epoch_s();
                            stats.bosminer_last_seen_s.store(now, Ordering::Relaxed);
                            stats.bosminer_accepted.store(summary.accepted, Ordering::Relaxed);
                            stats.bosminer_rejected.store(summary.rejected, Ordering::Relaxed);
                            // Store MHS 5s as fixed-point milli-MHS.
                            let mhs_milli = (summary.mhs_5s * 1000.0) as u64;
                            stats.bosminer_mhs5s_milli.store(mhs_milli, Ordering::Relaxed);
                            debug!(
                                accepted = summary.accepted,
                                rejected = summary.rejected,
                                mhs_5s = summary.mhs_5s,
                                "bosminer summary"
                            );
                        }
                        Err(e) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            stats.bosminer_cgminer_reachable.store(false, Ordering::Relaxed);
                            stats
                                .bosminer_consecutive_failures
                                .store(consecutive_failures as u64, Ordering::Relaxed);
                            debug!(consecutive_failures, error = %e, "bosminer summary poll failed");
                            if consecutive_failures >= 3 {
                                stats
                                    .bosminer_blocker_mask
                                    .fetch_or(BLOCKER_CGMINER_UNREACHABLE, Ordering::Relaxed);
                                if stats.bosminer_alive.swap(false, Ordering::Relaxed) {
                                    warn!(
                                        consecutive_failures,
                                        "bosminer CGMiner API at 127.0.0.1:4028 unreachable — marking bosminer_alive=false"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Publish a dashboard-friendly snapshot derived from `ProxiedStats`.
///
/// This is the bridge between the proxy-only daemon path and
/// `/api/system/health`. It performs production-safe `/proc` reads for
/// bosminer PID discovery and never shells out or touches hardware devices.
pub fn spawn_proxy_health_publisher(
    stats: Arc<ProxiedStats>,
    tx: watch::Sender<RuntimeHealthSnapshot>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.tick().await;
        let mut pid_history: Vec<u32> = Vec::new();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("proxy health publisher: shutdown");
                    break;
                }
                _ = interval.tick() => {
                    let pid = find_bosminer_pid().await;
                    if let Some(pid) = pid {
                        if pid_history.last().copied() != Some(pid) {
                            pid_history.push(pid);
                            if pid_history.len() > 8 {
                                let overflow = pid_history.len() - 8;
                                pid_history.drain(..overflow);
                            }
                        }
                    }

                    let last_seen_s = stats.bosminer_last_seen_s.load(Ordering::Relaxed);
                    let last_poll_s = stats.bosminer_last_poll_s.load(Ordering::Relaxed);
                    let snapshot = RuntimeHealthSnapshot {
                        mode: RuntimeHealthMode::Proxy,
                        bosminer: RuntimeBosminerHealth {
                            alive: stats.bosminer_alive.load(Ordering::Relaxed),
                            pid,
                            pid_history: pid_history.clone(),
                            last_seen_ms: last_seen_s.saturating_mul(1000),
                            blockers: stats.blocker_names(),
                            last_summary: RuntimeBosminerSummary {
                                accepted: stats.bosminer_accepted.load(Ordering::Relaxed),
                                rejected: stats.bosminer_rejected.load(Ordering::Relaxed),
                                mhs_5s: stats.bosminer_mhs5s_milli.load(Ordering::Relaxed)
                                    as f64
                                    / 1000.0,
                            },
                        },
                        scrape: RuntimeScrapeHealth {
                            cgminer_url: Some(BOSMINER_CGMINER_URL.to_string()),
                            cgminer_reachable: Some(
                                stats.bosminer_cgminer_reachable.load(Ordering::Relaxed),
                            ),
                            last_poll_ms: if last_poll_s == 0 {
                                None
                            } else {
                                Some(last_poll_s.saturating_mul(1000))
                            },
                            consecutive_failures: stats
                                .bosminer_consecutive_failures
                                .load(Ordering::Relaxed),
                        },
                    };

                    if tx.send(snapshot).is_err() {
                        debug!("proxy health publisher has no runtime-health receivers");
                    }
                }
            }
        }
    })
}

async fn find_bosminer_pid() -> Option<u32> {
    let mut entries = tokio::fs::read_dir("/proc").await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if proc_pid_is_bosminer(pid).await {
            return Some(pid);
        }
    }
    None
}

async fn proc_pid_is_bosminer(pid: u32) -> bool {
    let comm_path = format!("/proc/{pid}/comm");
    if let Ok(comm) = tokio::fs::read_to_string(comm_path).await {
        if comm.trim().contains("bosminer") {
            return true;
        }
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    let Ok(cmdline) = tokio::fs::read(cmdline_path).await else {
        return false;
    };
    cmdline
        .windows(b"bosminer".len())
        .any(|window| window == b"bosminer")
}

#[derive(Debug, Default)]
struct BosminerSummary {
    accepted: u64,
    rejected: u64,
    mhs_5s: f64,
}

/// Poll bosminer's CGMiner API for a summary. Returns parsed accepted /
/// rejected / MHS5s values.
///
/// CGMiner protocol is line-oriented JSON over plain TCP: send
/// `{"command":"summary"}\n`, read until newline, parse as JSON. We use a
/// 2-second connect+read timeout so a stalled bosminer doesn't pile up tasks.
async fn poll_bosminer_summary() -> Result<BosminerSummary> {
    let total_timeout = Duration::from_secs(2);
    let connect = TcpStream::connect("127.0.0.1:4028");
    let mut stream = tokio::time::timeout(total_timeout, connect)
        .await
        .context("bosminer summary connect timeout")?
        .context("bosminer summary connect failed")?;

    stream
        .write_all(b"{\"command\":\"summary\"}\n")
        .await
        .context("bosminer summary write failed")?;

    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    let read_loop = async {
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            // CGMiner replies are single-line JSON terminated by \0 or
            // \n. Stop when we see either.
            if buf.contains(&0u8) || buf.contains(&b'\n') {
                break;
            }
            if buf.len() >= 16 * 1024 {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::time::timeout(total_timeout, read_loop)
        .await
        .context("bosminer summary read timeout")?
        .context("bosminer summary read failed")?;

    // Trim trailing NULs / whitespace so serde_json doesn't choke.
    while matches!(
        buf.last(),
        Some(0u8) | Some(b'\n') | Some(b'\r') | Some(b' ')
    ) {
        buf.pop();
    }
    let value: Value =
        serde_json::from_slice(&buf).context("bosminer summary JSON parse failed")?;
    let summary_obj = value
        .get("SUMMARY")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .context("bosminer summary missing SUMMARY[0]")?;
    let accepted = summary_obj
        .get("Accepted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let rejected = summary_obj
        .get("Rejected")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mhs_5s = summary_obj
        .get("MHS 5s")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Ok(BosminerSummary {
        accepted,
        rejected,
        mhs_5s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stratum_tcp_scheme() {
        let (h, p) = parse_stratum_url("stratum+tcp://btc.global.luxor.tech:700").unwrap();
        assert_eq!(h, "btc.global.luxor.tech");
        assert_eq!(p, 700);
    }

    #[test]
    fn parses_stratum2_tcp_scheme() {
        let (h, p) = parse_stratum_url("stratum2+tcp://a830bcc3.bos.braiins.com:3336").unwrap();
        assert_eq!(h, "a830bcc3.bos.braiins.com");
        assert_eq!(p, 3336);
    }

    #[test]
    fn rejects_tcp_scheme_shortcut() {
        assert!(parse_stratum_url("tcp://pool.example.com:3333").is_err());
    }

    #[test]
    fn rejects_bare_host_port_shortcut() {
        assert!(parse_stratum_url("127.0.0.1:3333").is_err());
    }

    #[test]
    fn rejects_outer_whitespace() {
        assert!(parse_stratum_url(" stratum+tcp://pool.example.com:3333").is_err());
    }

    #[test]
    fn rejects_ssl_scheme() {
        assert!(parse_stratum_url("stratum+ssl://pool.example.com:443").is_err());
    }

    #[test]
    fn rejects_missing_port() {
        assert!(parse_stratum_url("stratum+tcp://pool.example.com").is_err());
    }

    #[test]
    fn rejects_invalid_port() {
        assert!(parse_stratum_url("stratum+tcp://pool.example.com:notaport").is_err());
    }

    #[test]
    fn rejects_empty_host() {
        assert!(parse_stratum_url("stratum+tcp://:3333").is_err());
    }

    #[test]
    fn classifies_json_first_chunk_as_primary() {
        let chunk = br#"{"id":1,"method":"mining.configure","params":[[],{}]}
"#;
        assert_eq!(classify_first_chunk(chunk), SessionRoute::PrimaryJson);
    }

    #[test]
    fn classifies_binary_first_chunk_as_binary() {
        let chunk = [0x9f; 64];
        assert_eq!(classify_first_chunk(&chunk), SessionRoute::Binary);
    }

    #[tokio::test]
    async fn binary_proxy_session_routes_and_preserves_bytes() {
        let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary_addr = primary_listener.local_addr().unwrap();
        let primary_seen = Arc::new(AtomicBool::new(false));
        let primary_seen_task = {
            let primary_seen = primary_seen.clone();
            tokio::spawn(async move {
                if tokio::time::timeout(Duration::from_millis(300), primary_listener.accept())
                    .await
                    .is_ok()
                {
                    primary_seen.store(true, Ordering::Relaxed);
                }
            })
        };

        let binary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let binary_addr = binary_listener.local_addr().unwrap();
        let binary_payload = vec![
            0x00, 0xff, 0x9f, 0x10, 0x0a, 0x7b, 0x22, 0x6e, 0x6f, 0x74, 0x5f, 0x6a, 0x73, 0x6f,
            0x6e, 0x22,
        ];
        let binary_reply = vec![0x80, 0x00, 0x13, 0x37, 0xff, 0x00, 0x42];
        let expected_payload = binary_payload.clone();
        let expected_reply = binary_reply.clone();
        let binary_task = tokio::spawn(async move {
            let (mut socket, _) = binary_listener.accept().await.unwrap();
            let mut got = vec![0u8; expected_payload.len()];
            socket.read_exact(&mut got).await.unwrap();
            socket.write_all(&expected_reply).await.unwrap();
            socket.shutdown().await.unwrap();
            got
        });

        let inbound_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_addr = inbound_listener.local_addr().unwrap();
        let (client_res, accept_res) =
            tokio::join!(TcpStream::connect(inbound_addr), inbound_listener.accept());
        let mut client = client_res.unwrap();
        let (inbound, peer) = accept_res.unwrap();

        let shutdown = CancellationToken::new();
        let session_task = tokio::spawn(handle_session(
            42,
            inbound,
            peer,
            ParsedUpstream {
                label: "primary",
                host: primary_addr.ip().to_string(),
                port: primary_addr.port(),
            },
            Some(ParsedUpstream {
                label: "binary",
                host: binary_addr.ip().to_string(),
                port: binary_addr.port(),
            }),
            shutdown,
            None,
        ));

        client.write_all(&binary_payload).await.unwrap();
        let mut reply = vec![0u8; binary_reply.len()];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, binary_reply);
        drop(client);

        let forwarded = binary_task.await.unwrap();
        assert_eq!(forwarded, binary_payload);

        let session = tokio::time::timeout(Duration::from_secs(2), session_task)
            .await
            .expect("proxy session should finish after client closes")
            .expect("proxy task join should succeed");
        session.expect("proxy session should close cleanly");

        primary_seen_task.await.unwrap();
        assert!(
            !primary_seen.load(Ordering::Relaxed),
            "binary first chunk must not connect to primary JSON upstream"
        );
    }

    #[tokio::test]
    async fn json_proxy_session_routes_primary_preserves_bytes_and_sniffs_stats() {
        let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary_addr = primary_listener.local_addr().unwrap();
        let client_submit =
            br#"{"id":4,"method":"mining.submit","params":["worker","job","ex2","ntime","nonce"]}
"#
            .to_vec();
        let upstream_notify =
            br#"{"id":null,"method":"mining.notify","params":["job","prev","cb1","cb2",[],"ver","bits","time",true]}
"#
            .to_vec();
        let expected_submit = client_submit.clone();
        let expected_notify = upstream_notify.clone();
        let primary_task = tokio::spawn(async move {
            let (mut socket, _) = primary_listener.accept().await.unwrap();
            let mut got = vec![0u8; expected_submit.len()];
            socket.read_exact(&mut got).await.unwrap();
            socket.write_all(&expected_notify).await.unwrap();
            socket.shutdown().await.unwrap();
            got
        });

        let binary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let binary_addr = binary_listener.local_addr().unwrap();
        let binary_seen = Arc::new(AtomicBool::new(false));
        let binary_seen_task = {
            let binary_seen = binary_seen.clone();
            tokio::spawn(async move {
                if tokio::time::timeout(Duration::from_millis(300), binary_listener.accept())
                    .await
                    .is_ok()
                {
                    binary_seen.store(true, Ordering::Relaxed);
                }
            })
        };

        let inbound_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_addr = inbound_listener.local_addr().unwrap();
        let (client_res, accept_res) =
            tokio::join!(TcpStream::connect(inbound_addr), inbound_listener.accept());
        let mut client = client_res.unwrap();
        let (inbound, peer) = accept_res.unwrap();
        let stats = ProxiedStats::new();

        let session_task = tokio::spawn(handle_session(
            43,
            inbound,
            peer,
            ParsedUpstream {
                label: "primary",
                host: primary_addr.ip().to_string(),
                port: primary_addr.port(),
            },
            Some(ParsedUpstream {
                label: "binary",
                host: binary_addr.ip().to_string(),
                port: binary_addr.port(),
            }),
            CancellationToken::new(),
            Some(stats.clone()),
        ));

        client.write_all(&client_submit).await.unwrap();
        let mut reply = vec![0u8; upstream_notify.len()];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, upstream_notify);
        drop(client);

        let forwarded = primary_task.await.unwrap();
        assert_eq!(forwarded, client_submit);

        let session = tokio::time::timeout(Duration::from_secs(2), session_task)
            .await
            .expect("proxy session should finish after client closes")
            .expect("proxy task join should succeed");
        session.expect("proxy session should close cleanly");

        binary_seen_task.await.unwrap();
        assert!(
            !binary_seen.load(Ordering::Relaxed),
            "JSON first chunk must not connect to binary upstream"
        );
        assert_eq!(stats.shares_submitted.load(Ordering::Relaxed), 1);
        assert_eq!(stats.jobs_seen.load(Ordering::Relaxed), 1);
        assert!(stats.last_share_at_s.load(Ordering::Relaxed) > 0);
    }
}
