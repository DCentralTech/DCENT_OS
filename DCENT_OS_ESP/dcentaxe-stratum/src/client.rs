// DCENT_axe Stratum V1 TCP Client
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Synchronous Stratum V1 client using std::net::TcpStream.
// ESP-IDF supports std networking via lwIP -- no async runtime needed.
//
// Protocol: JSON-RPC 2.0 over TCP, newline-delimited.

use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

use log::{debug, error, info, warn};
use serde_json::Value;

use crate::mask::{mask_wallet, sanitize_pool_url};
use crate::types::*;

/// User agent string sent to pools.
const USER_AGENT: &str = concat!("DCENTaxe/", env!("CARGO_PKG_VERSION"));

/// TCP connection timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read timeout for the TCP socket (non-blocking poll interval).
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// Maximum reconnect backoff in seconds.
const MAX_BACKOFF_SECS: u64 = 30;

/// Initial reconnect backoff in seconds.
const INITIAL_BACKOFF_SECS: u64 = 1;

/// Hard cap on the `client.reconnect` `wait_seconds` a pool can request.
/// The reconnect wait is implemented as a blocking `thread::sleep` loop on the
/// stratum thread (no share submit, no failover, no shutdown check while it
/// runs), so an unbounded pool-supplied value (e.g. `client.reconnect[h,p,86400]`
/// = a 24h outage, or `u32::MAX` ≈ a century) would be a guaranteed mining
/// outage / DoS. Clamp at parse time so the bound is central + testable. Matches
/// the 300s `DEAD_CONNECTION_TIMEOUT` order of magnitude.
const MAX_RECONNECT_WAIT_SECS: u32 = 300;
const MAX_RECENT_EVENTS: usize = 64;
const MAX_REJECT_REASONS: usize = 8;
const PRIMARY_REPROBE_COOLDOWN_SECS: u64 = 30 * 60;
const PRIMARY_REPROBE_JOB_PROOF_TIMEOUT_SECS: u64 = 5;

/// Tightened handshake budget used ONLY by the cooldown-gated primary reprobe,
/// so the synchronous probe cannot stall the live fallback socket read path for
/// the full ~45s a default handshake could take. Net worst-case reprobe stall
/// drops to <12s (3s connect + 3s subscribe + 3s authorize + 5s job-wait).
const PRIMARY_REPROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PRIMARY_REPROBE_SUBSCRIBE_DEADLINE: Duration = Duration::from_secs(3);
const PRIMARY_REPROBE_AUTHORIZE_DEADLINE: Duration = Duration::from_secs(3);

/// Default (live-connection) handshake budget. Preserves the historic timeouts:
/// 10s connect, 15s subscribe wait, 10s authorize wait.
const DEFAULT_SUBSCRIBE_DEADLINE: Duration = Duration::from_secs(15);
const DEFAULT_AUTHORIZE_DEADLINE: Duration = Duration::from_secs(10);

/// Per-phase timeouts for a connect+subscribe+authorize handshake.
#[derive(Debug, Clone, Copy)]
struct HandshakeBudget {
    connect_timeout: Duration,
    subscribe_deadline: Duration,
    authorize_deadline: Duration,
}

impl HandshakeBudget {
    /// The default budget used on the live connection path (historic timeouts).
    const fn default_live() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            subscribe_deadline: DEFAULT_SUBSCRIBE_DEADLINE,
            authorize_deadline: DEFAULT_AUTHORIZE_DEADLINE,
        }
    }

    /// The tightened budget used only by the primary reprobe.
    const fn reprobe() -> Self {
        Self {
            connect_timeout: PRIMARY_REPROBE_CONNECT_TIMEOUT,
            subscribe_deadline: PRIMARY_REPROBE_SUBSCRIBE_DEADLINE,
            authorize_deadline: PRIMARY_REPROBE_AUTHORIZE_DEADLINE,
        }
    }
}

/// Maximum length (in bytes) of a single newline-delimited Stratum line before
/// the transport gives up on it and resyncs to the next true `\n` boundary.
///
/// Raised from the historic 4096 guard to accommodate legitimately large
/// fee-heavy `mining.notify` frames that carry many merkle_branches. This is a
/// local transport buffer guard only — no protocol/register constant changes.
const MAX_LINE_BYTES: usize = 16384;

/// Request ID allocation.
const ID_CONFIGURE: u64 = 1;
const ID_SUBSCRIBE: u64 = 2;
const ID_AUTHORIZE: u64 = 3;
const ID_SUGGEST_DIFF: u64 = 4;
const ID_EXTRANONCE_SUBSCRIBE: u64 = 5;
/// Share submission IDs start here and increment.
const ID_SUBMIT_BASE: u64 = 10;

/// How long a pending submit may wait for a pool response before it is pruned
/// and booked unresolved. Raised to be >= the 120s `mining.notify` job lifetime
/// so a slow-but-valid pool accept/reject is not evicted before it arrives.
const PENDING_SUBMIT_TTL_SECS: u64 = 120;

/// Cap on the recently-evicted submit ring. A late accept/reject for an
/// already-evicted submit id is reclassified from this ring instead of being
/// ignored as a non-submit response.
const MAX_EVICTED_SUBMITS: usize = 16;

#[derive(Debug, Clone)]
struct PendingSubmit {
    id: u64,
    job_id: String,
    difficulty: f64,
    submitted_at: std::time::Instant,
}

/// A pending submit that was evicted (pruned on timeout or dropped on the
/// 64-cap) before its pool response arrived. Kept briefly so a late response
/// can still be classified as accept/reject instead of silently dropped.
#[derive(Debug, Clone)]
struct EvictedSubmit {
    id: u64,
    job_id: String,
    difficulty: f64,
    evicted_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageLoopExit {
    PrimaryFailback,
}

/// The Stratum V1 client.
///
/// Manages pool connection, handshake, job dispatch, and share submission.
/// Designed to run in a dedicated thread on ESP32-S3.
pub struct StratumClient {
    /// Pool configuration.
    config: StratumConfig,

    /// Original primary pool configuration used for explicit failback reprobes.
    primary_config: StratumConfig,

    /// TCP stream to the pool (None when disconnected).
    /// Used for both reading and writing — no clone needed on ESP-IDF.
    stream: Option<TcpStream>,

    /// Session state (extranonce, difficulty, version mask, etc.)
    pub session: SessionState,

    /// Incrementing ID for share submissions.
    next_submit_id: u64,

    /// Channel to send events to the mining thread.
    event_tx: mpsc::Sender<StratumEvent>,

    /// Channel to receive share submissions from the mining thread.
    share_rx: mpsc::Receiver<MiningEvent>,

    /// Current reconnect backoff in seconds.
    backoff_secs: u64,

    /// Pending share submissions awaiting pool response. Capped at 64 entries.
    pending_submits: Vec<PendingSubmit>,

    /// Recently-evicted submits (timeout-pruned or 64-cap dropped) kept for a
    /// short window so a late pool accept/reject can still be classified and
    /// the unresolved counter corrected, instead of being silently ignored.
    recently_evicted: Vec<EvictedSubmit>,

    /// Persistent line buffer for recv_line() — survives timeouts so partial
    /// reads are not lost between calls.
    line_buffer: Vec<u8>,

    /// True while recv_line() is dropping the tail of an oversized line until
    /// the next `\n` boundary. Persists across READ_TIMEOUT-driven recv_line
    /// calls so a single overflowing line spanning many invocations is fully
    /// consumed (not just the prefix) before the stream is treated as realigned.
    discarding_oversized_line: bool,

    /// Statistics.
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_unresolved: u64,
    pub jobs_received: u64,

    /// Optional fallback pool config, tried after consecutive primary failures.
    pub fallback_config: Option<StratumConfig>,

    /// True while we are connected to the fallback pool (not the primary).
    failover_active: bool,

    /// Number of consecutive connection failures (reset on successful connect).
    consecutive_failures: u32,

    /// True once the current session has seen a structurally usable job.
    usable_job_seen: bool,

    /// Local monotonic time when fallback mode was entered.
    failover_entered_at: Option<std::time::Instant>,

    /// Local monotonic time of the most recent primary reprobe attempt.
    last_primary_reprobe_at: Option<std::time::Instant>,

    /// Optional live status handle for API/reporting layers.
    status: Option<SharedStratumStatus>,
}

impl StratumClient {
    /// Create a new Stratum client.
    pub fn new(
        config: StratumConfig,
        event_tx: mpsc::Sender<StratumEvent>,
        share_rx: mpsc::Receiver<MiningEvent>,
    ) -> Self {
        Self {
            primary_config: config.clone(),
            config,
            stream: None,
            session: SessionState::default(),
            next_submit_id: ID_SUBMIT_BASE,
            event_tx,
            share_rx,
            backoff_secs: INITIAL_BACKOFF_SECS,
            pending_submits: Vec::new(),
            recently_evicted: Vec::new(),
            shares_submitted: 0,
            shares_accepted: 0,
            shares_rejected: 0,
            shares_unresolved: 0,
            jobs_received: 0,
            line_buffer: Vec::with_capacity(256),
            discarding_oversized_line: false,
            fallback_config: None,
            failover_active: false,
            consecutive_failures: 0,
            usable_job_seen: false,
            failover_entered_at: None,
            last_primary_reprobe_at: None,
            status: None,
        }
    }

    pub fn set_status_handle(&mut self, status: SharedStratumStatus) {
        self.status = Some(status);
        self.sync_status(None);
    }

    fn drain_pending_share_queue(&mut self) {
        let mut dropped = 0_u32;
        while self.share_rx.try_recv().is_ok() {
            dropped += 1;
        }
        if dropped > 0 {
            // Book the loss so it is visible in status (sync_status publishes
            // shares_unresolved). Same-session resubmit is intentionally out of
            // scope here — extranonce1 routinely rotates on a fresh subscribe.
            self.shares_unresolved = self.shares_unresolved.saturating_add(dropped as u64);
            warn!(
                "Stratum: dropped {} stale queued share(s) before starting new session (counted unresolved)",
                dropped
            );
        }
    }

    fn current_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }

    fn with_status_mut<F>(&self, mutate: F)
    where
        F: FnOnce(&mut StratumStatus),
    {
        if let Some(status) = &self.status {
            if let Ok(mut status) = status.lock() {
                mutate(&mut status);
            }
        }
    }

    fn record_status_event(&self, kind: StratumEventKind, detail: impl Into<String>) {
        let detail = detail.into();
        self.with_status_mut(|status| {
            let ts_unix_ms = Self::current_unix_ms();
            status.update_primary_failback_from_event(kind, detail.clone(), ts_unix_ms);
            status.recent_events.push(StratumEventRecord {
                ts_unix_ms,
                kind,
                detail,
            });
            if status.recent_events.len() > MAX_RECENT_EVENTS {
                let excess = status.recent_events.len() - MAX_RECENT_EVENTS;
                status.recent_events.drain(0..excess);
            }
        });
    }

    fn record_reject_reason(&self, reason: &str) {
        let now = Self::current_unix_ms();
        self.with_status_mut(|status| {
            if let Some(entry) = status
                .reject_reason_counts
                .iter_mut()
                .find(|entry| entry.key == reason)
            {
                entry.count = entry.count.saturating_add(1);
                entry.last_seen_unix_ms = now;
            } else {
                status.reject_reason_counts.push(RejectReasonCount {
                    key: reason.to_string(),
                    count: 1,
                    last_seen_unix_ms: now,
                });
                if status.reject_reason_counts.len() > MAX_REJECT_REASONS {
                    status.reject_reason_counts.remove(0);
                }
            }
        });
    }

    fn sync_status(&self, reject_reason: Option<&str>) {
        let oldest_pending_submit_age_ms = self
            .pending_submits
            .iter()
            .map(|submit| submit.submitted_at.elapsed().as_millis() as u64)
            .max()
            .unwrap_or(0);
        self.with_status_mut(|status| {
            status.connected = self.stream.is_some() && self.session.authorized;
            status.authorized = self.session.authorized;
            status.extranonce_subscribe_requested = self.session.extranonce_subscribe_requested;
            status.extranonce_subscribe_accepted = self.session.extranonce_subscribe_accepted;
            status.failover_active = self.failover_active;
            status.active_url = self.config.url.clone();
            status.active_port = self.config.port;
            status.shares_submitted = self.shares_submitted;
            status.shares_accepted = self.shares_accepted;
            status.shares_rejected = self.shares_rejected;
            status.shares_pending = self.pending_submits.len() as u32;
            status.shares_unresolved = self.shares_unresolved;
            status.oldest_pending_submit_age_ms = oldest_pending_submit_age_ms;
            status.jobs_received = self.jobs_received;
            status.difficulty = self.session.difficulty;
            status.consecutive_failures = self.consecutive_failures;
            status.backoff_secs = self.backoff_secs;
            if let Some(reason) = reject_reason {
                status.last_reject_reason = reason.to_string();
            }
        });
    }

    fn mark_unresolved_pending(&mut self, count: usize, reason: &str) {
        if count == 0 {
            return;
        }
        self.shares_unresolved = self.shares_unresolved.saturating_add(count as u64);
        warn!(
            "Stratum: marked {} pending share(s) unresolved: {}",
            count, reason
        );
    }

    fn clear_pending_submits_unresolved(&mut self, reason: &str) {
        let count = self.pending_submits.len();
        self.pending_submits.clear();
        self.mark_unresolved_pending(count, reason);
    }

    /// Push an evicted submit onto the recently-evicted ring so a late pool
    /// response can still be classified. Prunes entries older than the pending
    /// TTL and bounds the ring to MAX_EVICTED_SUBMITS.
    fn push_evicted(&mut self, submit: &PendingSubmit, now: std::time::Instant) {
        self.recently_evicted.retain(|entry| {
            now.duration_since(entry.evicted_at).as_secs() < PENDING_SUBMIT_TTL_SECS
        });
        self.recently_evicted.push(EvictedSubmit {
            id: submit.id,
            job_id: submit.job_id.clone(),
            difficulty: submit.difficulty,
            evicted_at: now,
        });
        while self.recently_evicted.len() > MAX_EVICTED_SUBMITS {
            self.recently_evicted.remove(0);
        }
    }

    fn prune_stale_pending_submits(&mut self, now: std::time::Instant) -> usize {
        let mut expired = 0usize;
        let mut remaining = Vec::with_capacity(self.pending_submits.len());
        for submit in std::mem::take(&mut self.pending_submits) {
            if now.duration_since(submit.submitted_at).as_secs() < PENDING_SUBMIT_TTL_SECS {
                remaining.push(submit);
            } else {
                self.push_evicted(&submit, now);
                expired += 1;
            }
        }
        self.pending_submits = remaining;
        self.mark_unresolved_pending(expired, "submit response timeout");
        expired
    }

    fn enforce_pending_submit_limit(&mut self) -> usize {
        if self.pending_submits.len() < 64 {
            return 0;
        }
        warn!("Stratum: pending_submits full (64), dropping oldest");
        let dropped = self.pending_submits.remove(0);
        self.push_evicted(&dropped, std::time::Instant::now());
        self.mark_unresolved_pending(1, "pending queue full");
        1
    }

    /// Configure a fallback pool to switch to after 3 consecutive primary failures.
    /// Performs a quick reachability check (DNS + TCP connect, 5s timeout).
    pub fn set_fallback(&mut self, config: StratumConfig) {
        match Self::check_pool_reachable(&config.url, config.port) {
            Ok(()) => info!(
                "Stratum: fallback pool {}:{} reachable",
                sanitize_pool_url(&config.url),
                config.port
            ),
            Err(e) => warn!(
                "Stratum: fallback pool {}:{} not reachable: {}",
                sanitize_pool_url(&config.url),
                config.port,
                e
            ),
        }
        self.fallback_config = Some(config);
    }

    /// Quick reachability test: DNS resolve + TCP connect with 5s timeout.
    fn check_pool_reachable(url: &str, port: u16) -> Result<(), String> {
        use std::net::{TcpStream, ToSocketAddrs};
        let host = endpoint_host_from_url(url);
        let addr = format!("{}:{}", host, port);
        let socket_addr = addr
            .to_socket_addrs()
            .map_err(|e| format!("DNS failed: {}", e))?
            .next()
            .ok_or_else(|| "DNS returned no addresses".to_string())?;
        TcpStream::connect_timeout(&socket_addr, std::time::Duration::from_secs(5))
            .map_err(|e| format!("TCP connect failed: {}", e))?;
        Ok(())
    }

    /// Main client loop. Connects, handshakes, then processes messages forever.
    ///
    /// This function runs in a dedicated thread. It never returns under normal
    /// operation. On disconnect, it reconnects with exponential backoff.
    pub fn run(&mut self) {
        loop {
            // Pool failover: after 3 consecutive failures, try fallback pool
            if self.consecutive_failures >= 3
                && self.fallback_config.is_some()
                && !self.failover_active
            {
                let fb = self.fallback_config.as_ref().unwrap();
                // Quick reachability check before switching
                if Self::check_pool_reachable(&fb.url, fb.port).is_err() {
                    warn!(
                        "Stratum: fallback pool {}:{} unreachable — staying on primary",
                        sanitize_pool_url(&fb.url),
                        fb.port
                    );
                    self.record_status_event(
                        StratumEventKind::FailoverSkipped,
                        format!(
                            "fallback unreachable {}:{}",
                            sanitize_pool_url(&fb.url),
                            fb.port
                        ),
                    );
                } else {
                    info!(
                        "Stratum: primary pool failed {} times, switching to fallback {}:{}",
                        self.consecutive_failures,
                        sanitize_pool_url(&fb.url),
                        fb.port
                    );
                    self.config = fb.clone();
                    self.failover_active = true;
                    self.consecutive_failures = 0;
                    self.failover_entered_at = Some(std::time::Instant::now());
                    self.last_primary_reprobe_at = None;
                    self.record_status_event(
                        StratumEventKind::FailoverEntered,
                        format!(
                            "switched to fallback {}:{}",
                            sanitize_pool_url(&self.config.url),
                            self.config.port
                        ),
                    );
                }
            }

            match self.connect_and_handshake() {
                Ok(()) => {
                    self.consecutive_failures = 0;
                    self.with_status_mut(|status| {
                        status.last_connect_cause = "handshake_complete".to_string();
                        status.last_connect_unix_ms = Self::current_unix_ms();
                    });
                    self.record_status_event(
                        StratumEventKind::Connect,
                        format!(
                            "connected to {}:{}",
                            sanitize_pool_url(&self.config.url),
                            self.config.port
                        ),
                    );
                    if self.failover_active {
                        info!(
                            "Stratum: connected to FALLBACK pool {}:{}; primary failback reprobe is cooldown-gated",
                            sanitize_pool_url(&self.config.url),
                            self.config.port
                        );
                    } else {
                        info!(
                            "Stratum: connected and authorized to {}:{}",
                            sanitize_pool_url(&self.config.url),
                            self.config.port
                        );
                    }
                    self.backoff_secs = INITIAL_BACKOFF_SECS;

                    // Send session data so the mining dispatcher can create a WorkBuilder.
                    // Without this, the dispatcher never knows extranonce1/extranonce2_size
                    // and generate_next_work() returns None forever.
                    self.drain_pending_share_queue();
                    let _ = self.event_tx.send(StratumEvent::ExtranonceChanged {
                        extranonce1: self.session.extranonce1.clone(),
                        extranonce2_size: self.session.extranonce2_size,
                    });

                    // Send initial difficulty if pool set it during handshake
                    if self.session.difficulty > 0.0 {
                        let _ = self
                            .event_tx
                            .send(StratumEvent::DifficultyChanged(self.session.difficulty));
                    }

                    // Send version mask if negotiated
                    if self.session.version_mask != 0 {
                        let _ = self
                            .event_tx
                            .send(StratumEvent::VersionMaskChanged(self.session.version_mask));
                    }

                    let _ = self.event_tx.send(StratumEvent::Reconnected);
                    self.sync_status(None);
                }
                Err(e) => {
                    error!("Stratum: connection failed: {}", e);
                    self.consecutive_failures += 1;
                    self.with_status_mut(|status| {
                        status.last_disconnect_cause = e.clone();
                        status.last_disconnect_unix_ms = Self::current_unix_ms();
                    });
                    self.record_status_event(StratumEventKind::Disconnect, e.clone());
                    self.sync_status(None);
                    self.do_backoff();
                    continue;
                }
            }

            let mut immediate_reconnect = false;
            match self.message_loop() {
                Ok(MessageLoopExit::PrimaryFailback) => {
                    info!("Stratum: reconnecting immediately for primary failback");
                    immediate_reconnect = true;
                }
                Err(e) => {
                    error!("Stratum: connection lost: {}", e);
                    self.consecutive_failures += 1;
                    self.with_status_mut(|status| {
                        status.last_disconnect_cause = e.clone();
                        status.last_disconnect_unix_ms = Self::current_unix_ms();
                    });
                    self.record_status_event(StratumEventKind::Disconnect, e.clone());
                    let _ = self.event_tx.send(StratumEvent::Disconnected);
                    self.sync_status(None);
                }
            }

            self.stream = None;
            self.session.authorized = false;
            self.session.extranonce_subscribe_requested = false;
            self.session.extranonce_subscribe_accepted = false;
            if immediate_reconnect {
                self.clear_pending_submits_unresolved("primary failback");
            } else {
                self.clear_pending_submits_unresolved("connection reset");
            }
            self.line_buffer.clear();
            self.discarding_oversized_line = false;
            self.sync_status(None);

            if immediate_reconnect {
                self.backoff_secs = INITIAL_BACKOFF_SECS;
                continue;
            }

            self.do_backoff();
        }
    }

    /// Sleep for the current backoff period, then double it (up to max).
    fn do_backoff(&mut self) {
        warn!("Stratum: reconnecting in {} seconds", self.backoff_secs);
        self.with_status_mut(|status| {
            status.last_reconnect_cause = format!("backoff {}s", self.backoff_secs);
        });
        self.record_status_event(
            StratumEventKind::ReconnectBackoff,
            format!("waiting {}s", self.backoff_secs),
        );
        // Sleep in 1-second chunks to avoid triggering the ESP-IDF task watchdog (30s)
        for _ in 0..self.backoff_secs {
            std::thread::sleep(Duration::from_secs(1));
        }
        self.backoff_secs = (self.backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }

    fn primary_reprobe_cooldown() -> Duration {
        Duration::from_secs(PRIMARY_REPROBE_COOLDOWN_SECS)
    }

    fn primary_reprobe_due(&self, now: std::time::Instant, fallback_connected: bool) -> bool {
        if !self.failover_active
            || !fallback_connected
            || !self.session.authorized
            || !self.pending_submits.is_empty()
        {
            return false;
        }

        if self.config.url == self.primary_config.url
            && self.config.port == self.primary_config.port
        {
            return false;
        }

        let Some(failover_entered_at) = self.failover_entered_at else {
            return false;
        };
        let Some(failover_age) = now.checked_duration_since(failover_entered_at) else {
            return false;
        };
        if failover_age < Self::primary_reprobe_cooldown() {
            return false;
        }

        if let Some(last_reprobe_at) = self.last_primary_reprobe_at {
            let Some(reprobe_age) = now.checked_duration_since(last_reprobe_at) else {
                return false;
            };
            if reprobe_age < Self::primary_reprobe_cooldown() {
                return false;
            }
        }

        true
    }

    fn maybe_enter_primary_failback(&mut self) -> bool {
        let now = std::time::Instant::now();
        if !self.primary_reprobe_due(now, self.stream.is_some()) {
            return false;
        }

        self.begin_primary_reprobe(now);
        let result = self.probe_primary_ready();
        self.finish_primary_reprobe(result)
    }

    fn begin_primary_reprobe(&mut self, now: std::time::Instant) {
        self.last_primary_reprobe_at = Some(now);
        info!(
            "Stratum: primary reprobe started for {}:{}",
            sanitize_pool_url(&self.primary_config.url),
            self.primary_config.port
        );
        self.with_status_mut(|status| {
            status.last_reconnect_cause = "primary reprobe pending".to_string();
        });
        self.record_status_event(
            StratumEventKind::PrimaryReprobeStarted,
            format!(
                "probing primary {}:{}",
                sanitize_pool_url(&self.primary_config.url),
                self.primary_config.port
            ),
        );
        self.sync_status(None);
    }

    fn finish_primary_reprobe(&mut self, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => {
                info!(
                    "Stratum: primary reprobe ready for {}:{}",
                    sanitize_pool_url(&self.primary_config.url),
                    self.primary_config.port
                );
                self.record_status_event(
                    StratumEventKind::PrimaryReprobeReady,
                    format!(
                        "primary {}:{} authorized with job proof",
                        sanitize_pool_url(&self.primary_config.url),
                        self.primary_config.port
                    ),
                );

                if !self.pending_submits.is_empty() {
                    warn!("Stratum: primary failback delayed; pending fallback submits remain");
                    self.record_status_event(
                        StratumEventKind::PrimaryReprobeFailed,
                        "pending fallback submits remain; failback delayed",
                    );
                    self.sync_status(None);
                    return false;
                }

                self.config = self.primary_config.clone();
                self.failover_active = false;
                self.consecutive_failures = 0;
                self.failover_entered_at = None;
                self.with_status_mut(|status| {
                    status.last_reconnect_cause = "primary failback entered".to_string();
                });
                self.record_status_event(
                    StratumEventKind::PrimaryFailbackEntered,
                    format!(
                        "switching back to primary {}:{}",
                        sanitize_pool_url(&self.config.url),
                        self.config.port
                    ),
                );
                true
            }
            Err(e) => {
                warn!(
                    "Stratum: primary reprobe failed for {}:{}: {}",
                    sanitize_pool_url(&self.primary_config.url),
                    self.primary_config.port,
                    e
                );
                self.with_status_mut(|status| {
                    status.last_reconnect_cause = "primary reprobe failed".to_string();
                });
                self.record_status_event(
                    StratumEventKind::PrimaryReprobeFailed,
                    format!(
                        "primary {}:{} failed: {}",
                        sanitize_pool_url(&self.primary_config.url),
                        self.primary_config.port,
                        e
                    ),
                );
                self.sync_status(None);
                false
            }
        }
    }

    fn probe_primary_ready(&self) -> Result<(), String> {
        let (event_tx, _event_rx) = mpsc::channel();
        let (_share_tx, share_rx) = mpsc::channel();
        let mut probe = StratumClient::new(self.primary_config.clone(), event_tx, share_rx);
        // Use the tightened reprobe budget so a slow/unreachable primary cannot
        // stall the live fallback socket read path for tens of seconds.
        probe.connect_and_handshake_with_budget(HandshakeBudget::reprobe())?;
        probe.wait_for_reprobe_job(Duration::from_secs(PRIMARY_REPROBE_JOB_PROOF_TIMEOUT_SECS))
    }

    fn wait_for_reprobe_job(&mut self, timeout: Duration) -> Result<(), String> {
        if self.usable_job_seen {
            return Ok(());
        }

        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match self.recv_line() {
                Ok(Some(line)) => {
                    let msg = parse_message(&line);
                    match msg {
                        StratumMessage::Notify(job) => {
                            if Self::job_has_reprobe_proof(&job) {
                                self.flush_pending_job_context();
                                self.usable_job_seen = true;
                                self.jobs_received += 1;
                                return Ok(());
                            }
                            return Err(format!("primary returned unusable job {}", job.job_id));
                        }
                        StratumMessage::SetDifficulty(diff) => {
                            self.session.pending_difficulty = Some(diff);
                        }
                        StratumMessage::SetExtranonce {
                            extranonce1,
                            extranonce2_size,
                        } => {
                            self.stage_extranonce(extranonce1, extranonce2_size, "reprobe");
                        }
                        StratumMessage::SetVersionMask(mask) => {
                            const SAFE_VERSION_MASK: u32 = 0x1fffe000;
                            self.session.version_mask = mask & SAFE_VERSION_MASK;
                        }
                        StratumMessage::Ping(id) => {
                            self.send_pong(id)?;
                        }
                        StratumMessage::GetVersion(id) => {
                            self.send_version(id)?;
                        }
                        StratumMessage::Reconnect { host, port, .. } => {
                            return Err(format!(
                                "primary requested reconnect to {}:{} during reprobe",
                                host, port
                            ));
                        }
                        StratumMessage::Response { id, result, error } => {
                            if id == ID_CONFIGURE {
                                self.handle_configure_response(result, error)?;
                            } else if id == ID_EXTRANONCE_SUBSCRIBE {
                                self.handle_extranonce_subscribe_response(result, error)?;
                            }
                        }
                        StratumMessage::ShowMessage(msg) => {
                            debug!("Stratum: primary reprobe message: {}", msg);
                        }
                        StratumMessage::Unknown(raw) => {
                            debug!("Stratum: primary reprobe ignored message: {}", raw);
                        }
                    }
                }
                Ok(None) => continue,
                Err(e) => return Err(format!("recv waiting for primary job proof: {}", e)),
            }
        }

        Err("timeout waiting for primary job proof".into())
    }

    fn job_has_reprobe_proof(job: &StratumJob) -> bool {
        !job.job_id.is_empty()
            && Self::is_hex_len(&job.prev_hash, 64)
            && Self::is_hex_len(&job.version, 8)
            && Self::is_hex_len(&job.nbits, 8)
            && Self::is_hex_len(&job.ntime, 8)
            && !job.coinbase1.is_empty()
    }

    fn is_hex_len(value: &str, len: usize) -> bool {
        value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    // -----------------------------------------------------------------------
    // Connection + Handshake
    // -----------------------------------------------------------------------

    /// Establish TCP connection and perform the full Stratum V1 handshake on
    /// the live connection path, using the default (historic) timeout budget.
    fn connect_and_handshake(&mut self) -> Result<(), String> {
        self.connect_and_handshake_with_budget(HandshakeBudget::default_live())
    }

    /// Establish TCP connection and perform the full Stratum V1 handshake with
    /// an explicit per-phase timeout budget. The reprobe path passes a tightened
    /// budget so it cannot stall the live fallback socket for tens of seconds.
    fn connect_and_handshake_with_budget(&mut self, budget: HandshakeBudget) -> Result<(), String> {
        self.session.pending_difficulty = None;
        self.session.pending_extranonce = None;
        self.session.extranonce_subscribe_requested = false;
        self.session.extranonce_subscribe_accepted = false;
        self.usable_job_seen = false;

        let host = self.config.endpoint_host();
        let addr = format!("{}:{}", host, self.config.port);
        info!("Stratum: connecting to {}", addr);
        self.with_status_mut(|status| {
            status.last_connect_cause = format!("connecting to {}", addr);
        });

        // Resolve hostname to IP (SocketAddr::parse only accepts IPs, not hostnames)
        use std::net::ToSocketAddrs;
        let socket_addr = addr
            .to_socket_addrs()
            .map_err(|e| format!("DNS resolve failed for {}: {}", addr, e))?
            .next()
            .ok_or_else(|| format!("DNS returned no addresses for {}", addr))?;

        info!("Stratum: resolved to {}", socket_addr);

        let stream = TcpStream::connect_timeout(&socket_addr, budget.connect_timeout)
            .map_err(|e| format!("TCP connect failed: {}", e))?;

        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|e| format!("set read timeout: {}", e))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set write timeout: {}", e))?;
        stream.set_nodelay(true).ok();

        // Enable TCP keepalive to detect dead connections at the OS level.
        // KEEPIDLE=60s, KEEPINTVL=10s, KEEPCNT=3 → dead peer detected in ~90s.
        configure_tcp_keepalive(&stream);

        self.stream = Some(stream);

        // mining.configure (version rolling, optional)
        if self.config.version_rolling {
            self.send_configure()?;
        }

        // mining.subscribe
        self.send_subscribe()?;

        // Wait for subscribe response. mining.configure is opportunistic: some
        // pools ignore it entirely, and late responses are still useful.
        let deadline = std::time::Instant::now() + budget.subscribe_deadline;
        let mut got_subscribe = false;

        while std::time::Instant::now() < deadline && !got_subscribe {
            match self.recv_line() {
                Ok(Some(line)) => {
                    let msg = parse_message(&line);
                    match msg {
                        StratumMessage::Response { id, result, error } => {
                            if id == ID_SUBSCRIBE {
                                self.handle_subscribe_response(result, error)?;
                                got_subscribe = true;
                            } else if id == ID_CONFIGURE {
                                self.handle_configure_response(result, error)?;
                            }
                        }
                        StratumMessage::SetDifficulty(diff) => {
                            self.session.pending_difficulty = Some(diff);
                            info!(
                                "Stratum: initial difficulty staged at {} for next job",
                                diff
                            );
                        }
                        StratumMessage::SetExtranonce {
                            extranonce1,
                            extranonce2_size,
                        } => {
                            self.stage_extranonce(extranonce1, extranonce2_size, "initial");
                        }
                        StratumMessage::SetVersionMask(mask) => {
                            const SAFE_VERSION_MASK: u32 = 0x1fffe000;
                            let safe_mask = mask & SAFE_VERSION_MASK;
                            self.session.version_mask = safe_mask;
                            info!("Stratum: version mask set to 0x{:08x}", safe_mask);
                            let _ = self
                                .event_tx
                                .send(StratumEvent::VersionMaskChanged(safe_mask));
                        }
                        _ => {
                            debug!("Stratum: ignoring message during handshake: {:?}", msg);
                        }
                    }
                }
                Ok(None) => continue,
                Err(e) => return Err(format!("recv during handshake: {}", e)),
            }
        }

        if !got_subscribe {
            return Err("timeout waiting for subscribe response".into());
        }

        info!(
            "Stratum: subscribed -- extranonce1={}, extranonce2_size={}",
            self.session.extranonce1, self.session.extranonce2_size
        );

        // mining.authorize
        self.send_authorize()?;

        let deadline = std::time::Instant::now() + budget.authorize_deadline;
        while std::time::Instant::now() < deadline {
            match self.recv_line() {
                Ok(Some(line)) => {
                    let msg = parse_message(&line);
                    match msg {
                        StratumMessage::Response { id, result, error } => {
                            if id == ID_AUTHORIZE {
                                self.handle_authorize_response(result, error)?;
                                let _ = self.send_extranonce_subscribe();
                                if self.config.suggest_difficulty > 0 {
                                    let _ = self.send_suggest_difficulty();
                                }
                                self.flush_pending_job_context();
                                return Ok(());
                            } else if id == ID_CONFIGURE {
                                self.handle_configure_response(result, error)?;
                            }
                        }
                        StratumMessage::SetDifficulty(diff) => {
                            self.session.pending_difficulty = Some(diff);
                        }
                        StratumMessage::SetExtranonce {
                            extranonce1,
                            extranonce2_size,
                        } => {
                            self.stage_extranonce(extranonce1, extranonce2_size, "authorize-stage");
                        }
                        StratumMessage::Notify(job) => {
                            // Pool may send first job before auth response
                            self.flush_pending_job_context();
                            if Self::job_has_reprobe_proof(&job) {
                                self.usable_job_seen = true;
                            }
                            self.jobs_received += 1;
                            self.sync_status(None);
                            let _ = self.event_tx.send(StratumEvent::NewJob(job));
                        }
                        _ => {}
                    }
                }
                Ok(None) => continue,
                Err(e) => return Err(format!("recv during authorize: {}", e)),
            }
        }

        Err("timeout waiting for authorize response".into())
    }

    // -----------------------------------------------------------------------
    // Send Methods
    // -----------------------------------------------------------------------

    fn send_json(&mut self, json: &str) -> Result<(), String> {
        if let Some(ref mut stream) = self.stream {
            debug!("Stratum TX: {}", json.trim());
            stream
                .write_all(json.as_bytes())
                .map_err(|e| format!("write failed: {}", e))?;
            stream.flush().map_err(|e| format!("flush failed: {}", e))?;
            Ok(())
        } else {
            Err("not connected".into())
        }
    }

    fn send_configure(&mut self) -> Result<(), String> {
        let req = serde_json::json!({
            "id": ID_CONFIGURE,
            "method": "mining.configure",
            "params": [
                ["version-rolling", "subscribe-extranonce"],
                {
                    "version-rolling.mask": "1fffe000",
                    "version-rolling.min-bit-count": 2
                }
            ]
        });
        let mut s = req.to_string();
        s.push('\n');
        self.send_json(&s)
    }

    fn send_subscribe(&mut self) -> Result<(), String> {
        // Best-effort session resume: if we hold a subscription_id from a prior
        // successful subscribe, pass it as the optional 2nd mining.subscribe
        // param so resumable pools may preserve extranonce1. Pools that ignore
        // the 2nd param behave identically. This does NOT make dropped shares
        // re-submittable on its own (that half is deferred per STRATUM-1).
        let params = build_subscribe_params(USER_AGENT, &self.session.subscription_id);
        let req = serde_json::json!({
            "id": ID_SUBSCRIBE,
            "method": "mining.subscribe",
            "params": params
        });
        let mut s = req.to_string();
        s.push('\n');
        self.send_json(&s)
    }

    fn send_authorize(&mut self) -> Result<(), String> {
        let req = serde_json::json!({
            "id": ID_AUTHORIZE,
            "method": "mining.authorize",
            "params": [self.config.worker_name, self.config.password]
        });
        let mut s = req.to_string();
        s.push('\n');
        self.send_json(&s)
    }

    fn send_suggest_difficulty(&mut self) -> Result<(), String> {
        let req = serde_json::json!({
            "id": ID_SUGGEST_DIFF,
            "method": "mining.suggest_difficulty",
            "params": [self.config.suggest_difficulty]
        });
        let mut s = req.to_string();
        s.push('\n');
        self.send_json(&s)
    }

    fn send_extranonce_subscribe(&mut self) -> Result<(), String> {
        let req = serde_json::json!({
            "id": ID_EXTRANONCE_SUBSCRIBE,
            "method": "mining.extranonce.subscribe",
            "params": []
        });
        let mut s = req.to_string();
        s.push('\n');
        self.send_json(&s)?;
        self.session.extranonce_subscribe_requested = true;
        self.session.extranonce_subscribe_accepted = false;
        self.sync_status(None);
        Ok(())
    }

    fn handle_extranonce_subscribe_response(
        &mut self,
        result: Option<Value>,
        error: Option<Value>,
    ) -> Result<(), String> {
        self.session.extranonce_subscribe_requested = true;
        if let Some(err) = error {
            if !err.is_null() {
                self.session.extranonce_subscribe_accepted = false;
                self.sync_status(None);
                warn!("Stratum: extranonce.subscribe rejected: {}", err);
                return Ok(());
            }
        }

        let accepted = result
            .as_ref()
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        self.session.extranonce_subscribe_accepted = accepted;
        self.sync_status(None);
        if accepted {
            debug!("Stratum: extranonce.subscribe accepted");
        } else {
            warn!("Stratum: extranonce.subscribe returned false");
        }
        Ok(())
    }

    /// Send mining.submit for a share.
    pub fn submit_share(&mut self, share: &ShareSubmission) -> Result<(), String> {
        let id = self.next_submit_id;
        self.next_submit_id += 1;

        let mut params = vec![
            Value::String(self.config.worker_name.clone()),
            Value::String(share.job_id.clone()),
            Value::String(share.extranonce2.clone()),
            Value::String(share.ntime.clone()),
            Value::String(share.nonce.clone()),
        ];

        if let Some(ref vb) = share.version_bits {
            params.push(Value::String(vb.clone()));
        }

        let req = serde_json::json!({
            "id": id,
            "method": "mining.submit",
            "params": params
        });
        let mut s = req.to_string();
        s.push('\n');

        // Prune stale pending submits (>60s old) and cap at 64
        let now = std::time::Instant::now();
        self.prune_stale_pending_submits(now);
        self.enforce_pending_submit_limit();

        info!(
            "Stratum: submitting share -- job={}, nonce={}, en2={}, ntime={}",
            share.job_id, share.nonce, share.extranonce2, share.ntime
        );

        self.send_json(&s)?;

        self.pending_submits.push(PendingSubmit {
            id,
            job_id: share.job_id.clone(),
            difficulty: share.difficulty,
            submitted_at: now,
        });
        self.shares_submitted += 1;
        self.with_status_mut(|status| {
            let now_ms = Self::current_unix_ms();
            status.last_share_submit_unix_ms = now_ms;
            status.last_share_difficulty = share.difficulty;
        });
        self.record_status_event(
            StratumEventKind::ShareSubmitted,
            format!("job={} diff={:.1}", share.job_id, share.difficulty),
        );
        self.sync_status(None);
        Ok(())
    }

    fn send_pong(&mut self, id: u64) -> Result<(), String> {
        let mut s = build_pong_payload(id);
        s.push('\n');
        self.send_json(&s)
    }
}

/// Build the mining.pong JSON-RPC RESPONSE payload for a given request id.
///
/// Pong is the response to a `mining.ping` REQUEST, so it must use the
/// JSON-RPC response shape `{"id":..., "result": null, "error": null}` —
/// NOT a notification (`{"id": ..., "method": "pong", "params": []}`).
/// Strict pools (NiceHash, public-pool) drop the socket when a request id
/// gets answered with a notification frame.
pub(crate) fn build_pong_payload(id: u64) -> String {
    let resp = serde_json::json!({
        "id": id,
        "result": null,
        "error": null
    });
    resp.to_string()
}

/// Build the `params` array for a `mining.subscribe` request.
///
/// On the very first connect there is no session token, so only the user-agent
/// is sent (`[USER_AGENT]`). On a reconnect where a non-empty `subscription_id`
/// from the prior session is held, the token is appended as the optional 2nd
/// param (`[USER_AGENT, subscription_id]`) so resumable pools may preserve
/// extranonce1. Pools that don't support resume simply ignore the 2nd param.
pub(crate) fn build_subscribe_params(user_agent: &str, subscription_id: &str) -> Vec<Value> {
    let mut params = vec![Value::String(user_agent.to_string())];
    if !subscription_id.is_empty() {
        params.push(Value::String(subscription_id.to_string()));
    }
    params
}

impl StratumClient {
    fn send_version(&mut self, id: u64) -> Result<(), String> {
        let resp = serde_json::json!({
            "id": id,
            "result": USER_AGENT,
            "error": null
        });
        let mut s = resp.to_string();
        s.push('\n');
        self.send_json(&s)
    }

    // -----------------------------------------------------------------------
    // Receive + Parse
    // -----------------------------------------------------------------------

    /// Try to read one newline-delimited JSON line from the socket.
    /// Reads byte-by-byte until newline (no BufReader — avoids try_clone on ESP-IDF).
    /// Uses `self.line_buffer` to persist partial reads across timeout boundaries.
    fn recv_line(&mut self) -> Result<Option<String>, String> {
        if let Some(ref mut stream) = self.stream {
            use std::io::Read;
            let mut byte = [0u8; 1];

            loop {
                match stream.read(&mut byte) {
                    Ok(0) => {
                        self.line_buffer.clear();
                        self.discarding_oversized_line = false;
                        return Err("connection closed by pool".into());
                    }
                    Ok(1) => {
                        // Resync mode: an earlier line overflowed MAX_LINE_BYTES.
                        // Drop every byte (without growing the buffer) until we
                        // reach a true `\n` boundary, then declare the stream
                        // realigned for the next call.
                        if self.discarding_oversized_line {
                            if byte[0] == b'\n' {
                                self.discarding_oversized_line = false;
                                self.line_buffer.clear();
                                return Ok(None);
                            }
                            continue;
                        }
                        if byte[0] == b'\n' {
                            // Complete line received — drain the buffer
                            let trimmed = String::from_utf8_lossy(&self.line_buffer)
                                .trim()
                                .to_string();
                            self.line_buffer.clear();
                            if trimmed.is_empty() {
                                return Ok(None);
                            } else {
                                debug!("Stratum RX: {}", trimmed);
                                return Ok(Some(trimmed));
                            }
                        }
                        self.line_buffer.push(byte[0]);
                        if self.line_buffer.len() > MAX_LINE_BYTES {
                            // Overflow: do NOT return a truncated fragment (that
                            // would desync the stream and corrupt the following
                            // valid message). Instead switch to discard-to-newline
                            // resync so the next message lands on a true boundary.
                            warn!(
                                "Stratum: line exceeds {} bytes; discarding to next newline to resync",
                                MAX_LINE_BYTES
                            );
                            self.line_buffer.clear();
                            self.discarding_oversized_line = true;
                            continue;
                        }
                    }
                    Ok(_) => {}
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        // Timeout — partial data (and any in-progress discard
                        // resync) stays in self for the next call.
                        return Ok(None);
                    }
                    Err(e) => {
                        self.line_buffer.clear();
                        self.discarding_oversized_line = false;
                        return Err(format!("read error: {}", e));
                    }
                }
            }
        } else {
            Err("not connected".into())
        }
    }

    // -----------------------------------------------------------------------
    // Response Handlers
    // -----------------------------------------------------------------------

    /// Handle the mining.subscribe response.
    ///
    /// Format: {"id":2,"result":[[["mining.set_difficulty","sub"],["mining.notify","sub"]],"extranonce1",extranonce2_size],"error":null}
    fn handle_subscribe_response(
        &mut self,
        result: Option<Value>,
        error: Option<Value>,
    ) -> Result<(), String> {
        if let Some(err) = error {
            if !err.is_null() {
                return Err(format!("subscribe rejected: {}", err));
            }
        }

        let result = result.ok_or("subscribe: no result")?;
        let arr = result.as_array().ok_or("subscribe: result not array")?;

        if arr.len() < 3 {
            return Err(format!(
                "subscribe: result has {} elements, need 3",
                arr.len()
            ));
        }

        // arr[0] = subscription details
        if let Some(subs) = arr[0].as_array() {
            for sub in subs {
                if let Some(sub_arr) = sub.as_array() {
                    if sub_arr.len() >= 2 {
                        if let Some(method) = sub_arr[0].as_str() {
                            if method == "mining.notify" {
                                self.session.subscription_id =
                                    sub_arr[1].as_str().unwrap_or("").to_string();
                            }
                        }
                    }
                }
            }
        }

        self.session.extranonce1 = arr[1]
            .as_str()
            .ok_or("subscribe: extranonce1 not string")?
            .to_string();

        let extranonce2_size_u64 = arr[2]
            .as_u64()
            .ok_or("subscribe: extranonce2_size not integer")?;
        let extranonce2_size = usize::try_from(extranonce2_size_u64)
            .map_err(|_| "subscribe: extranonce2_size does not fit usize")?;

        if !is_valid_extranonce2_size(extranonce2_size) {
            return Err(format!(
                "subscribe: invalid extranonce2_size: {} (valid range 1..={})",
                extranonce2_size, MAX_EXTRANONCE2_SIZE
            ));
        }
        self.session.extranonce2_size = extranonce2_size;

        Ok(())
    }

    /// Handle the mining.configure response (version rolling).
    fn handle_configure_response(
        &mut self,
        result: Option<Value>,
        _error: Option<Value>,
    ) -> Result<(), String> {
        if let Some(result) = result {
            if let Some(mask_str) = result.get("version-rolling.mask").and_then(|v| v.as_str()) {
                match u32::from_str_radix(mask_str, 16) {
                    Ok(mut mask) => {
                        // Validate version mask is within safe BIP 310 range
                        const SAFE_VERSION_MASK: u32 = 0x1fffe000;
                        // Minimum version-rolling bits we requested in
                        // send_configure (version-rolling.min-bit-count). Kept
                        // here so the request and the enforcement stay in sync.
                        const MIN_VERSION_ROLLING_BITS: u32 = 2;
                        if mask & !SAFE_VERSION_MASK != 0 {
                            warn!("Stratum: version mask 0x{:08x} extends beyond safe BIP 310 range, clamping to 0x{:08x}",
                                mask, mask & SAFE_VERSION_MASK);
                            mask = mask & SAFE_VERSION_MASK;
                        }
                        // Enforce the min-bit-count we requested instead of
                        // silently accepting any granted mask. Clamp first, then
                        // count bits (count after clamp).
                        let granted_bits = mask.count_ones();
                        self.with_status_mut(|status| {
                            status.negotiated_version_bits = granted_bits;
                        });
                        if granted_bits == 0 {
                            warn!(
                                "Stratum: pool declined version rolling (mask 0x{:08x}, 0 bits); ASICBoost disabled",
                                mask
                            );
                            self.session.version_mask = 0;
                            // Emit 0 so the WorkBuilder runs the single-midstate
                            // path (work.rs handles version_mask==0 safely).
                            let _ = self.event_tx.send(StratumEvent::VersionMaskChanged(0));
                        } else {
                            if granted_bits < MIN_VERSION_ROLLING_BITS {
                                warn!(
                                    "Stratum: pool granted only {} version-rolling bit(s), below requested min {}",
                                    granted_bits, MIN_VERSION_ROLLING_BITS
                                );
                            }
                            self.session.version_mask = mask;
                            info!("Stratum: version rolling enabled, mask=0x{:08x}", mask);
                            let _ = self.event_tx.send(StratumEvent::VersionMaskChanged(mask));
                        }
                    }
                    Err(e) => {
                        warn!("Stratum: invalid version mask '{}': {}", mask_str, e);
                    }
                }
            } else if result.get("version-rolling").and_then(|v| v.as_bool()) == Some(true) {
                self.session.version_mask = 0x1fffe000;
                info!("Stratum: version rolling accepted (default mask 0x1fffe000)");
                let _ = self
                    .event_tx
                    .send(StratumEvent::VersionMaskChanged(0x1fffe000));
            } else {
                info!("Stratum: version rolling not supported by pool");
            }
        }
        Ok(())
    }

    /// Handle the mining.authorize response.
    fn handle_authorize_response(
        &mut self,
        result: Option<Value>,
        error: Option<Value>,
    ) -> Result<(), String> {
        if let Some(err) = error {
            if !err.is_null() {
                return Err(format!("authorize rejected: {}", err));
            }
        }

        let authorized = result.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);

        if !authorized {
            return Err(format!("authorize failed: result={:?}", result));
        }

        self.session.authorized = true;
        info!(
            "Stratum: authorized as '{}'",
            mask_wallet(&self.config.worker_name)
        );
        self.sync_status(None);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Main Message Loop
    // -----------------------------------------------------------------------

    /// Process incoming pool messages and outgoing share submissions.
    fn message_loop(&mut self) -> Result<MessageLoopExit, String> {
        // Dead connection detection: if no message arrives for 5 minutes,
        // assume the connection is dead and trigger a reconnect.
        const DEAD_CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);
        let mut last_message_time = std::time::Instant::now();
        let mut last_pending_status_sync = std::time::Instant::now();

        loop {
            // Drain pending share submissions (non-blocking)
            while let Ok(event) = self.share_rx.try_recv() {
                match event {
                    MiningEvent::SubmitShare(share) => {
                        if let Err(e) = self.submit_share(&share) {
                            error!("Stratum: failed to submit share: {}", e);
                            return Err(e);
                        }
                    }
                }
            }
            if !self.pending_submits.is_empty()
                && last_pending_status_sync.elapsed() >= Duration::from_secs(1)
            {
                self.prune_stale_pending_submits(std::time::Instant::now());
                self.sync_status(None);
                last_pending_status_sync = std::time::Instant::now();
            }

            if self.maybe_enter_primary_failback() {
                return Ok(MessageLoopExit::PrimaryFailback);
            }

            match self.recv_line() {
                Ok(Some(line)) => {
                    last_message_time = std::time::Instant::now();
                    self.handle_message(&line)?;
                }
                Ok(None) => {
                    // Timeout — check for dead connection
                    if last_message_time.elapsed() > DEAD_CONNECTION_TIMEOUT {
                        return Err(format!(
                            "dead connection: no message from pool for {} seconds",
                            last_message_time.elapsed().as_secs()
                        ));
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn flush_pending_job_context(&mut self) {
        if let Some(diff) = self.session.pending_difficulty.take() {
            self.session.difficulty = diff;
            info!("Stratum: applying pending difficulty {} to next job", diff);
            let _ = self.event_tx.send(StratumEvent::DifficultyChanged(diff));
        }

        if let Some((extranonce1, extranonce2_size)) = self.session.pending_extranonce.take() {
            if !is_valid_extranonce2_size(extranonce2_size) {
                warn!(
                    "Stratum: dropping invalid pending extranonce2_size={} (valid range 1..={})",
                    extranonce2_size, MAX_EXTRANONCE2_SIZE
                );
                return;
            }
            self.session.extranonce1 = extranonce1.clone();
            self.session.extranonce2_size = extranonce2_size;
            info!(
                "Stratum: applying pending extranonce -- en1={}, en2_size={}",
                extranonce1, extranonce2_size
            );
            let _ = self.event_tx.send(StratumEvent::ExtranonceChanged {
                extranonce1,
                extranonce2_size,
            });
        }
    }

    /// Handle a single parsed message from the pool.
    fn handle_message(&mut self, line: &str) -> Result<(), String> {
        let msg = parse_message(line);

        match msg {
            StratumMessage::Notify(job) => {
                self.flush_pending_job_context();
                if Self::job_has_reprobe_proof(&job) {
                    self.usable_job_seen = true;
                }
                self.jobs_received += 1;
                self.sync_status(None);
                info!(
                    "Stratum: new job #{} (clean={})",
                    job.job_id, job.clean_jobs
                );
                let _ = self.event_tx.send(StratumEvent::NewJob(job));
            }
            StratumMessage::SetDifficulty(diff) => {
                self.session.pending_difficulty = Some(diff);
                self.sync_status(None);
                self.record_status_event(
                    StratumEventKind::DifficultyChanged,
                    format!("difficulty {}", diff),
                );
                info!(
                    "Stratum: difficulty changed to {} (queued for next job)",
                    diff
                );
            }
            StratumMessage::SetVersionMask(mask) => {
                const SAFE_VERSION_MASK: u32 = 0x1fffe000;
                let safe_mask = mask & SAFE_VERSION_MASK;
                if safe_mask != mask {
                    warn!(
                        "Stratum: runtime version mask 0x{:08x} extends beyond safe range, clamping to 0x{:08x}",
                        mask, safe_mask
                    );
                }
                self.session.version_mask = safe_mask;
                info!("Stratum: version mask updated to 0x{:08x}", safe_mask);
                let _ = self
                    .event_tx
                    .send(StratumEvent::VersionMaskChanged(safe_mask));
            }
            StratumMessage::SetExtranonce {
                extranonce1,
                extranonce2_size,
            } => {
                self.stage_extranonce(extranonce1, extranonce2_size, "runtime");
            }
            StratumMessage::Ping(id) => {
                debug!("Stratum: ping, sending pong");
                self.send_pong(id)?;
            }
            StratumMessage::GetVersion(id) => {
                debug!("Stratum: version request");
                self.send_version(id)?;
            }
            StratumMessage::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                // Honor the pool's redirect: update config so the reconnect
                // loop connects to the new host/port instead of the original.
                let target_host = if host.is_empty() {
                    self.config.url.clone()
                } else {
                    host.clone()
                };
                let target_port = if port == 0 { self.config.port } else { port };
                let changed = target_host != self.config.url || target_port != self.config.port;
                self.config.url = target_host;
                self.config.port = target_port;

                if changed {
                    info!(
                        "Stratum: pool requests reconnect to {}:{}",
                        sanitize_pool_url(&self.config.url),
                        self.config.port
                    );
                    self.with_status_mut(|status| {
                        status.last_reconnect_cause = format!(
                            "pool redirect {}:{}",
                            sanitize_pool_url(&self.config.url),
                            self.config.port
                        );
                    });
                    self.record_status_event(
                        StratumEventKind::ReconnectRequested,
                        format!(
                            "redirect to {}:{} in {}s",
                            sanitize_pool_url(&self.config.url),
                            self.config.port,
                            wait_seconds
                        ),
                    );
                } else {
                    info!("Stratum: pool requests reconnect (same server)");
                    self.with_status_mut(|status| {
                        status.last_reconnect_cause = "pool requested reconnect".to_string();
                    });
                    self.record_status_event(
                        StratumEventKind::ReconnectRequested,
                        format!("same server in {}s", wait_seconds),
                    );
                }
                // Sleep before reconnecting if the pool requests a delay
                if wait_seconds > 0 {
                    info!("Stratum: waiting {} seconds before reconnect", wait_seconds);
                    for _ in 0..wait_seconds {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                return Err(format!(
                    "pool requested reconnect to {}:{}",
                    sanitize_pool_url(&self.config.url),
                    self.config.port
                ));
            }
            StratumMessage::ShowMessage(msg) => {
                info!("Stratum: pool message: {}", msg);
                self.record_status_event(StratumEventKind::PoolMessage, msg);
            }
            StratumMessage::Response { id, result, error } => {
                if id == ID_CONFIGURE {
                    self.handle_configure_response(result, error)?;
                } else if id == ID_EXTRANONCE_SUBSCRIBE {
                    self.handle_extranonce_subscribe_response(result, error)?;
                } else if self.pending_submits.iter().any(|submit| submit.id == id) {
                    if self.handle_submit_response(id, result, error) {
                        return Err(
                            "pool rejected submit as unauthorized/not subscribed; reconnecting"
                                .into(),
                        );
                    }
                } else if id >= ID_SUBMIT_BASE
                    && self.recently_evicted.iter().any(|entry| entry.id == id)
                {
                    // A late accept/reject for a submit we already evicted and
                    // booked unresolved — reclassify instead of dropping it.
                    self.reclassify_evicted_response(id, result, error);
                } else {
                    debug!("Stratum: ignoring non-submit response id={}", id);
                }
            }
            StratumMessage::Unknown(raw) => {
                debug!("Stratum: unknown message: {}", raw);
            }
        }

        Ok(())
    }

    fn stage_extranonce(&mut self, extranonce1: String, extranonce2_size: usize, phase: &str) {
        if !is_valid_extranonce2_size(extranonce2_size) {
            warn!(
                "Stratum: ignored {} extranonce update with invalid extranonce2_size={} (valid range 1..={})",
                phase, extranonce2_size, MAX_EXTRANONCE2_SIZE
            );
            return;
        }

        self.session.pending_extranonce = Some((extranonce1.clone(), extranonce2_size));
        info!(
            "Stratum: {} extranonce staged -- en1={}, en2_size={}",
            phase, extranonce1, extranonce2_size
        );
    }

    /// Handle a response to a mining.submit request.
    ///
    /// Returns true when the reject means the current Stratum session is no
    /// longer authorized and the client must reconnect/re-authorize.
    fn handle_submit_response(
        &mut self,
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    ) -> bool {
        let pending = self
            .pending_submits
            .iter()
            .find(|submit| submit.id == id)
            .cloned();
        let job_id = pending.as_ref().map(|submit| submit.job_id.clone());
        let response_ms = pending
            .as_ref()
            .map(|submit| submit.submitted_at.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let share_difficulty = pending
            .as_ref()
            .map(|submit| submit.difficulty)
            .unwrap_or(0.0);

        self.pending_submits.retain(|submit| submit.id != id);

        let job_id_str = job_id.unwrap_or_else(|| format!("id={}", id));

        if let Some(ref err) = error {
            if !err.is_null() {
                self.shares_rejected += 1;
                let (err_code, err_msg) = Self::submit_error_code_and_message(err);
                let auth_fatal = Self::is_submit_auth_fatal(err_code, &err_msg);
                warn!(
                    "Stratum: share REJECTED for job {} -- {}",
                    job_id_str, err_msg
                );
                let now_ms = Self::current_unix_ms();
                self.with_status_mut(|status| {
                    status.last_share_response_ms = response_ms;
                    status.last_share_response_unix_ms = now_ms;
                    status.last_share_rejected_unix_ms = now_ms;
                    status.difficulty_rejected += share_difficulty;
                });
                self.record_reject_reason(&err_msg);
                self.record_status_event(
                    StratumEventKind::ShareRejected,
                    format!("job={} reason={}", job_id_str, err_msg),
                );
                if auth_fatal {
                    warn!("Stratum: submit reject is session-fatal; reconnecting to re-authorize");
                    self.with_status_mut(|status| {
                        status.last_reconnect_cause =
                            "submit rejected as unauthorized/not subscribed".to_string();
                    });
                    self.record_status_event(
                        StratumEventKind::ReconnectRequested,
                        "submit rejected as unauthorized/not subscribed",
                    );
                }
                self.sync_status(Some(&err_msg));
                return auth_fatal;
            }
        }

        let accepted = result.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);
        if accepted {
            self.shares_accepted += 1;
            info!(
                "Stratum: share ACCEPTED for job {} ({}/{})",
                job_id_str, self.shares_accepted, self.shares_submitted
            );
            let now_ms = Self::current_unix_ms();
            self.with_status_mut(|status| {
                status.last_share_response_ms = response_ms;
                status.last_share_response_unix_ms = now_ms;
                status.last_share_accepted_unix_ms = now_ms;
                status.last_share_time = now_ms / 1000;
                status.last_reject_reason.clear();
                status.difficulty_accepted += share_difficulty;
            });
            self.record_status_event(
                StratumEventKind::ShareAccepted,
                format!("job={} diff={:.1}", job_id_str, share_difficulty),
            );
            self.sync_status(None);
            false
        } else {
            self.shares_rejected += 1;
            // Surface any available payload as the reject reason instead of the
            // opaque constant, so reject_reason_counts is diagnosable in the
            // field (e.g. a systematic "low difficulty share" regression).
            let reason = Self::result_false_reject_reason(result.as_ref());
            // A rare result==false "unauthorized" must still trigger reconnect,
            // exactly like the error-array path does.
            let auth_fatal = Self::is_submit_auth_fatal(None, &reason);
            warn!(
                "Stratum: share REJECTED for job {} -- {}",
                job_id_str, reason
            );
            let now_ms = Self::current_unix_ms();
            self.with_status_mut(|status| {
                status.last_share_response_ms = response_ms;
                status.last_share_response_unix_ms = now_ms;
                status.last_share_rejected_unix_ms = now_ms;
                status.difficulty_rejected += share_difficulty;
            });
            self.record_reject_reason(&reason);
            self.record_status_event(
                StratumEventKind::ShareRejected,
                format!("job={} reason={}", job_id_str, reason),
            );
            if auth_fatal {
                warn!(
                    "Stratum: result=false reject is session-fatal; reconnecting to re-authorize"
                );
                self.with_status_mut(|status| {
                    status.last_reconnect_cause =
                        "submit rejected as unauthorized/not subscribed".to_string();
                });
                self.record_status_event(
                    StratumEventKind::ReconnectRequested,
                    "submit rejected as unauthorized/not subscribed",
                );
            }
            self.sync_status(Some(&reason));
            auth_fatal
        }
    }

    /// Derive a human-readable reject reason for a `result==false` (no error
    /// object) submit response. Surfaces a string payload directly, or the JSON
    /// of a structured payload, falling back to the historic opaque constant.
    fn result_false_reject_reason(result: Option<&Value>) -> String {
        match result {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Array(_)) | Some(Value::Object(_)) => result
                .map(|v| v.to_string())
                .unwrap_or_else(|| "pool rejected share".to_string()),
            _ => "pool rejected share".to_string(),
        }
    }

    /// Reclassify a late pool response for a submit that was already evicted
    /// (timeout-pruned or 64-cap dropped) and therefore booked unresolved.
    ///
    /// Moves the share from `shares_unresolved` to either `shares_accepted` or
    /// `shares_rejected` and records the matching status event. Does NOT run
    /// any reconnect/re-auth logic — a 2-minute-late "unauthorized" is stale,
    /// so we only correct the counter. Returns true if the id matched the ring.
    fn reclassify_evicted_response(
        &mut self,
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    ) -> bool {
        let Some(pos) = self
            .recently_evicted
            .iter()
            .position(|entry| entry.id == id)
        else {
            return false;
        };
        let evicted = self.recently_evicted.remove(pos);
        let job_id_str = evicted.job_id;
        let share_difficulty = evicted.difficulty;

        // Decrement unresolved (saturating) — this share was previously booked
        // unresolved on eviction and is now being resolved.
        self.shares_unresolved = self.shares_unresolved.saturating_sub(1);

        let rejected_by_error = error.as_ref().map_or(false, |err| !err.is_null());
        let accepted =
            !rejected_by_error && result.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);

        if accepted {
            self.shares_accepted += 1;
            info!(
                "Stratum: late ACCEPT for evicted submit job {} (id={})",
                job_id_str, id
            );
            let now_ms = Self::current_unix_ms();
            self.with_status_mut(|status| {
                status.last_share_response_unix_ms = now_ms;
                status.last_share_accepted_unix_ms = now_ms;
                status.last_share_time = now_ms / 1000;
                status.difficulty_accepted += share_difficulty;
            });
            self.record_status_event(
                StratumEventKind::ShareAccepted,
                format!(
                    "job={} diff={:.1} (late/evicted)",
                    job_id_str, share_difficulty
                ),
            );
            self.sync_status(None);
        } else {
            self.shares_rejected += 1;
            let reason = match &error {
                Some(err) if !err.is_null() => Self::submit_error_code_and_message(err).1,
                _ => "pool rejected share".to_string(),
            };
            warn!(
                "Stratum: late REJECT for evicted submit job {} (id={}) -- {}",
                job_id_str, id, reason
            );
            let now_ms = Self::current_unix_ms();
            self.with_status_mut(|status| {
                status.last_share_response_unix_ms = now_ms;
                status.last_share_rejected_unix_ms = now_ms;
                status.difficulty_rejected += share_difficulty;
            });
            self.record_reject_reason(&reason);
            self.record_status_event(
                StratumEventKind::ShareRejected,
                format!("job={} reason={} (late/evicted)", job_id_str, reason),
            );
            self.sync_status(Some(&reason));
        }
        true
    }

    fn submit_error_code_and_message(error: &Value) -> (Option<i64>, String) {
        if let Some(arr) = error.as_array() {
            let code = arr.first().and_then(|v| v.as_i64());
            let message = arr
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            (code, message)
        } else {
            (None, error.to_string())
        }
    }

    fn is_submit_auth_fatal(code: Option<i64>, message: &str) -> bool {
        let msg = message.to_ascii_lowercase();
        code == Some(24)
            || msg.contains("unauthoriz")
            || msg.contains("not authenticated")
            || msg.contains("not subscribed")
    }
}

// ---------------------------------------------------------------------------
// TCP Keepalive
// ---------------------------------------------------------------------------

/// Configure TCP keepalive on a connected socket.
///
/// Uses POSIX setsockopt (supported on ESP-IDF via lwIP):
/// - SO_KEEPALIVE = 1 (enable)
/// - TCP_KEEPIDLE = 60s (time before first probe)
/// - TCP_KEEPINTVL = 10s (interval between probes)
/// - TCP_KEEPCNT = 3 (probes before declaring dead)
///
/// Total dead-peer detection time: 60 + 10*3 = 90 seconds.
fn configure_tcp_keepalive(stream: &TcpStream) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        unsafe {
            let enable: libc::c_int = 1;
            let idle: libc::c_int = 60;
            let interval: libc::c_int = 10;
            let count: libc::c_int = 3;

            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_KEEPALIVE,
                &enable as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPIDLE,
                &idle as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPINTVL,
                &interval as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPCNT,
                &count as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        debug!("Stratum: TCP keepalive enabled (idle=60s, interval=10s, count=3)");
    }
    #[cfg(not(unix))]
    {
        let _ = stream;
        debug!("Stratum: TCP keepalive not available on this platform");
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC Parsing
// ---------------------------------------------------------------------------

/// Parse a JSON line from the pool into a typed StratumMessage.
pub fn parse_message(line: &str) -> StratumMessage {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            warn!("Stratum: JSON parse error: {} -- line: {}", e, line);
            return StratumMessage::Unknown(line.to_string());
        }
    };

    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
        let params = v.get("params").cloned().unwrap_or(Value::Array(vec![]));
        let id = v.get("id").and_then(|i| i.as_u64());

        match method {
            "mining.notify" => parse_notify(params),
            "mining.set_difficulty" => parse_set_difficulty(params),
            "mining.set_version_mask" => parse_set_version_mask(params),
            "mining.set_extranonce" => parse_set_extranonce(params),
            "mining.ping" => StratumMessage::Ping(id.unwrap_or(0)),
            "client.reconnect" => parse_reconnect(params),
            "client.get_version" => StratumMessage::GetVersion(id.unwrap_or(0)),
            "client.show_message" => {
                let msg = params
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                StratumMessage::ShowMessage(msg)
            }
            _ => {
                debug!("Stratum: unknown method '{}'", method);
                StratumMessage::Unknown(line.to_string())
            }
        }
    } else if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
        StratumMessage::Response {
            id,
            result: v.get("result").cloned(),
            error: v.get("error").cloned(),
        }
    } else {
        StratumMessage::Unknown(line.to_string())
    }
}

/// Extract block height from coinbase1 hex string (BIP34).
///
/// Coinbase transaction format (hex):
///   version(8) + txin_count(2) + prevout_hash(64) + prevout_idx(8) + scriptSig_len(2+) + scriptSig...
/// The scriptSig starts with a BIP34 push: push_len(2 hex) + height_LE(push_len*2 hex)
/// For current Bitcoin (height < 16M), push_len is 0x03 (3 bytes).
///
/// Typical coinbase1 offset: 4+1+32+4 = 41 bytes = 82 hex chars, then scriptSig length, then BIP34.
fn extract_block_height(coinbase1_hex: &str) -> u32 {
    // The BIP34 height push is at byte offset 42 in the raw coinbase tx
    // = hex offset 84. But scriptSig has a varint length prefix first.
    // Typical: offset 84 = scriptSig_len, offset 86 = push_len (0x03), offset 88 = height LE
    if coinbase1_hex.len() < 92 {
        return 0;
    }
    // Try to find the BIP34 push: look for 0x03 at the expected position
    let script_start = 84; // after version(8) + vin_count(2) + prevout(64) + idx(8) + scriptSig_len(2)
                           // The varint for scriptSig length might be 1 or 3 bytes. Try common positions.
    for offset in &[script_start + 2, script_start, script_start + 4] {
        let o = *offset;
        if o + 8 > coinbase1_hex.len() {
            continue;
        }
        if let Ok(push_len) = u8::from_str_radix(&coinbase1_hex[o..o + 2], 16) {
            if push_len == 3 && o + 8 <= coinbase1_hex.len() {
                // 3-byte LE block height
                if let (Ok(b0), Ok(b1), Ok(b2)) = (
                    u8::from_str_radix(&coinbase1_hex[o + 2..o + 4], 16),
                    u8::from_str_radix(&coinbase1_hex[o + 4..o + 6], 16),
                    u8::from_str_radix(&coinbase1_hex[o + 6..o + 8], 16),
                ) {
                    let height = b0 as u32 | (b1 as u32) << 8 | (b2 as u32) << 16;
                    const MIN_BLOCK_HEIGHT: u32 = 800_000;
                    const MAX_BLOCK_HEIGHT: u32 = 5_000_000; // ~80 years at current rate
                    if height >= MIN_BLOCK_HEIGHT && height <= MAX_BLOCK_HEIGHT {
                        return height;
                    }
                }
            } else if push_len == 4 && o + 10 <= coinbase1_hex.len() {
                // 4-byte LE block height (for heights > 16M, future-proof)
                if let (Ok(b0), Ok(b1), Ok(b2), Ok(b3)) = (
                    u8::from_str_radix(&coinbase1_hex[o + 2..o + 4], 16),
                    u8::from_str_radix(&coinbase1_hex[o + 4..o + 6], 16),
                    u8::from_str_radix(&coinbase1_hex[o + 6..o + 8], 16),
                    u8::from_str_radix(&coinbase1_hex[o + 8..o + 10], 16),
                ) {
                    let height =
                        b0 as u32 | (b1 as u32) << 8 | (b2 as u32) << 16 | (b3 as u32) << 24;
                    if height > 800000 {
                        return height;
                    }
                }
            }
        }
    }
    0 // couldn't extract
}

fn parse_notify(params: Value) -> StratumMessage {
    let arr = match params.as_array() {
        Some(a) if a.len() >= 9 => a,
        _ => {
            warn!("Stratum: mining.notify with insufficient params");
            return StratumMessage::Unknown(format!("bad notify: {}", params));
        }
    };

    let coinbase1 = arr[2].as_str().unwrap_or("").to_string();
    let block_height = extract_block_height(&coinbase1);

    StratumMessage::Notify(StratumJob {
        job_id: arr[0].as_str().unwrap_or("").to_string(),
        prev_hash: arr[1].as_str().unwrap_or("").to_string(),
        coinbase1,
        coinbase2: arr[3].as_str().unwrap_or("").to_string(),
        merkle_branches: arr[4]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        version: arr[5].as_str().unwrap_or("").to_string(),
        nbits: arr[6].as_str().unwrap_or("").to_string(),
        ntime: arr[7].as_str().unwrap_or("").to_string(),
        clean_jobs: arr[8].as_bool().unwrap_or(false),
        block_height,
    })
}

fn parse_set_difficulty(params: Value) -> StratumMessage {
    let diff = params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    StratumMessage::SetDifficulty(diff)
}

fn parse_set_version_mask(params: Value) -> StratumMessage {
    let mask_str = params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("00000000");
    let mask = u32::from_str_radix(mask_str, 16).unwrap_or(0);
    StratumMessage::SetVersionMask(mask)
}

fn parse_set_extranonce(params: Value) -> StratumMessage {
    let arr = params.as_array().cloned().unwrap_or_default();
    StratumMessage::SetExtranonce {
        extranonce1: arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        extranonce2_size: parse_extranonce2_size_param(arr.get(1)),
    }
}

fn parse_extranonce2_size_param(value: Option<&Value>) -> usize {
    match value.and_then(|v| v.as_u64()) {
        Some(raw) => usize::try_from(raw).unwrap_or(0),
        None => DEFAULT_EXTRANONCE2_SIZE,
    }
}

fn parse_reconnect(params: Value) -> StratumMessage {
    let arr = params.as_array().cloned().unwrap_or_default();
    let raw_wait = arr.get(2).and_then(|v| v.as_u64()).unwrap_or(0);
    StratumMessage::Reconnect {
        host: arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        port: arr.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as u16,
        // Clamp at parse time: a pool must never be able to pin the stratum
        // thread in the blocking reconnect-wait sleep loop for longer than
        // MAX_RECONNECT_WAIT_SECS. Missing/absent wait stays its default (0).
        wait_seconds: clamp_reconnect_wait(raw_wait),
    }
}

/// Clamp a pool-supplied `client.reconnect` wait to `MAX_RECONNECT_WAIT_SECS`.
/// Pure helper so the cap is unit-testable in isolation.
fn clamp_reconnect_wait(raw: u64) -> u32 {
    raw.min(MAX_RECONNECT_WAIT_SECS as u64) as u32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_notify() {
        let json = r#"{"id":null,"method":"mining.notify","params":["bf","4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6a0000000000000000000000000","01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020e1304","0d2f5374726174756d506f6f6c2f",["b1e3f140fa3d8f1c5e6abc3c0d3a7e96e051e3e3a6f9a0d3c4b2a1e0f9d8c7b6"],"20000000","170b3ce9","65a7e340",true]}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::Notify(job) => {
                assert_eq!(job.job_id, "bf");
                assert!(job.clean_jobs);
                assert_eq!(job.version, "20000000");
                assert_eq!(job.merkle_branches.len(), 1);
            }
            other => panic!("Expected Notify, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_difficulty() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[16384]}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::SetDifficulty(d) => assert_eq!(d, 16384.0),
            other => panic!("Expected SetDifficulty, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{"id":3,"error":null,"result":true}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::Response { id, result, .. } => {
                assert_eq!(id, 3);
                assert_eq!(result, Some(Value::Bool(true)));
            }
            other => panic!("Expected Response, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_version_mask() {
        let json = r#"{"id":null,"method":"mining.set_version_mask","params":["1fffe000"]}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::SetVersionMask(mask) => assert_eq!(mask, 0x1fffe000),
            other => panic!("Expected SetVersionMask, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_ping() {
        let json = r#"{"id":42,"method":"mining.ping","params":[]}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::Ping(id) => assert_eq!(id, 42),
            other => panic!("Expected Ping, got {:?}", other),
        }
    }

    #[test]
    fn test_clamp_reconnect_wait() {
        // A pool-requested 24h outage clamps to the cap.
        assert_eq!(clamp_reconnect_wait(86_400), MAX_RECONNECT_WAIT_SECS);
        // u32::MAX (≈ a century) clamps to the cap.
        assert_eq!(
            clamp_reconnect_wait(u32::MAX as u64),
            MAX_RECONNECT_WAIT_SECS
        );
        // u64::MAX clamps to the cap too.
        assert_eq!(clamp_reconnect_wait(u64::MAX), MAX_RECONNECT_WAIT_SECS);
        // A small in-range value passes through unchanged.
        assert_eq!(clamp_reconnect_wait(5), 5);
        // Exactly the cap is unchanged.
        assert_eq!(
            clamp_reconnect_wait(MAX_RECONNECT_WAIT_SECS as u64),
            MAX_RECONNECT_WAIT_SECS
        );
        // Zero (default) stays zero.
        assert_eq!(clamp_reconnect_wait(0), 0);
    }

    #[test]
    fn test_parse_reconnect_clamps_wait() {
        // wait=86400 clamps to the cap.
        let msg = parse_reconnect(serde_json::json!(["pool.example", 3333, 86_400]));
        match msg {
            StratumMessage::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                assert_eq!(host, "pool.example");
                assert_eq!(port, 3333);
                assert_eq!(wait_seconds, MAX_RECONNECT_WAIT_SECS);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }

        // wait=u32::MAX clamps to the cap.
        let msg = parse_reconnect(serde_json::json!(["pool.example", 3333, u32::MAX]));
        match msg {
            StratumMessage::Reconnect { wait_seconds, .. } => {
                assert_eq!(wait_seconds, MAX_RECONNECT_WAIT_SECS);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }

        // wait=5 passes through unchanged.
        let msg = parse_reconnect(serde_json::json!(["pool.example", 3333, 5]));
        match msg {
            StratumMessage::Reconnect { wait_seconds, .. } => {
                assert_eq!(wait_seconds, 5);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }

        // Absent wait param stays the current default (0).
        let msg = parse_reconnect(serde_json::json!(["pool.example", 3333]));
        match msg {
            StratumMessage::Reconnect { wait_seconds, .. } => {
                assert_eq!(wait_seconds, 0);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_extranonce() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef", 4]}"#;
        let msg = parse_message(json);
        match msg {
            StratumMessage::SetExtranonce {
                extranonce1,
                extranonce2_size,
            } => {
                assert_eq!(extranonce1, "deadbeef");
                assert_eq!(extranonce2_size, 4);
            }
            other => panic!("Expected SetExtranonce, got {:?}", other),
        }
    }

    #[test]
    fn test_subscribe_rejects_oversized_extranonce2() {
        let mut client = test_client();
        let result = serde_json::json!([[], "deadbeef", MAX_EXTRANONCE2_SIZE + 1]);

        let err = client
            .handle_subscribe_response(Some(result), None)
            .expect_err("oversized extranonce2_size must reject subscribe");

        assert!(err.contains("invalid extranonce2_size"));
    }

    #[test]
    fn test_runtime_set_extranonce_oversize_is_ignored() {
        let mut client = test_client();
        let json = format!(
            r#"{{"id":null,"method":"mining.set_extranonce","params":["deadbeef", {}]}}"#,
            MAX_EXTRANONCE2_SIZE + 1
        );

        client.handle_message(&json).unwrap();

        assert!(client.session.pending_extranonce.is_none());
    }

    fn test_client() -> StratumClient {
        let (event_tx, _event_rx) = mpsc::channel();
        let (_share_tx, share_rx) = mpsc::channel();
        StratumClient::new(StratumConfig::default(), event_tx, share_rx)
    }

    /// Build a client while retaining the share sender so a test can enqueue
    /// MiningEvents the way the mining thread would. Returns the sender too;
    /// dropping it would close the channel and stop try_recv from yielding.
    fn test_client_with_share_tx() -> (StratumClient, mpsc::Sender<MiningEvent>) {
        let (event_tx, _event_rx) = mpsc::channel();
        let (share_tx, share_rx) = mpsc::channel();
        (
            StratumClient::new(StratumConfig::default(), event_tx, share_rx),
            share_tx,
        )
    }

    fn submission(job_id: &str) -> ShareSubmission {
        ShareSubmission {
            job_id: job_id.to_string(),
            extranonce2: "00000000".to_string(),
            ntime: "65a7e340".to_string(),
            nonce: "00000000".to_string(),
            version: 0x20000000,
            version_bits: None,
            difficulty: 1.0,
        }
    }

    fn fallback_policy_client() -> StratumClient {
        let mut client = test_client();
        client.primary_config = StratumConfig {
            url: "primary.example".into(),
            port: 3333,
            worker_name: "worker".into(),
            password: "x".into(),
            suggest_difficulty: 0,
            version_rolling: true,
        };
        client.config = StratumConfig {
            url: "fallback.example".into(),
            port: 4444,
            worker_name: "worker".into(),
            password: "x".into(),
            suggest_difficulty: 0,
            version_rolling: true,
        };
        client.failover_active = true;
        client.session.authorized = true;
        client
    }

    fn old_enough_failover(now: std::time::Instant) -> std::time::Instant {
        now.checked_sub(StratumClient::primary_reprobe_cooldown() + Duration::from_secs(1))
            .unwrap()
    }

    fn reprobe_test_now() -> std::time::Instant {
        std::time::Instant::now()
            + StratumClient::primary_reprobe_cooldown()
            + Duration::from_secs(2)
    }

    #[test]
    fn primary_reprobe_waits_for_cooldown_and_fallback_authorization() {
        let now = reprobe_test_now();
        let mut client = fallback_policy_client();
        client.failover_entered_at = Some(now);

        assert!(!client.primary_reprobe_due(now, true));

        client.failover_entered_at = Some(old_enough_failover(now));
        client.session.authorized = false;
        assert!(!client.primary_reprobe_due(now, true));

        client.session.authorized = true;
        assert!(!client.primary_reprobe_due(now, false));
        assert!(client.primary_reprobe_due(now, true));
    }

    #[test]
    fn primary_reprobe_hysteresis_blocks_retry_and_pending_submits() {
        let now = reprobe_test_now();
        let mut client = fallback_policy_client();
        client.failover_entered_at = Some(old_enough_failover(now));
        client.last_primary_reprobe_at = Some(now);

        assert!(!client.primary_reprobe_due(now, true));

        client.last_primary_reprobe_at = Some(old_enough_failover(now));
        client.pending_submits.push(PendingSubmit {
            id: 99,
            job_id: "pending".into(),
            difficulty: 1.0,
            submitted_at: now,
        });

        assert!(!client.primary_reprobe_due(now, true));
    }

    #[test]
    fn failed_primary_reprobe_keeps_fallback_active_and_records_event() {
        let now = std::time::Instant::now();
        let mut client = fallback_policy_client();
        let status = new_shared_stratum_status(&client.primary_config, 0);
        client.set_status_handle(status.clone());

        client.begin_primary_reprobe(now);
        assert!(!client.finish_primary_reprobe(Err("tcp timeout".into())));

        assert!(client.failover_active);
        assert_eq!(client.config.url, "fallback.example");
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.last_reconnect_cause, "primary reprobe failed");
        assert!(snapshot
            .recent_events
            .iter()
            .any(|event| event.kind == StratumEventKind::PrimaryReprobeStarted));
        assert!(snapshot
            .recent_events
            .iter()
            .any(|event| event.kind == StratumEventKind::PrimaryReprobeFailed));
    }

    #[test]
    fn successful_primary_reprobe_enters_primary_failback_without_pending_submits() {
        let now = std::time::Instant::now();
        let mut client = fallback_policy_client();
        let status = new_shared_stratum_status(&client.primary_config, 0);
        client.set_status_handle(status.clone());

        client.begin_primary_reprobe(now);
        assert!(client.finish_primary_reprobe(Ok(())));

        assert!(!client.failover_active);
        assert_eq!(client.config.url, "primary.example");
        assert_eq!(client.consecutive_failures, 0);
        assert!(client.failover_entered_at.is_none());
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.last_reconnect_cause, "primary failback entered");
        assert!(snapshot
            .recent_events
            .iter()
            .any(|event| event.kind == StratumEventKind::PrimaryReprobeReady));
        assert!(snapshot
            .recent_events
            .iter()
            .any(|event| event.kind == StratumEventKind::PrimaryFailbackEntered));
    }

    #[test]
    fn primary_reprobe_job_proof_requires_structural_job_fields() {
        let mut job = StratumJob {
            job_id: "job-1".into(),
            prev_hash: "00".repeat(32),
            coinbase1: "01000000".into(),
            coinbase2: String::new(),
            merkle_branches: vec![],
            version: "20000000".into(),
            nbits: "170b3ce9".into(),
            block_height: 0,
            ntime: "65a7e340".into(),
            clean_jobs: true,
        };

        assert!(StratumClient::job_has_reprobe_proof(&job));

        job.prev_hash = "not-hex".into();
        assert!(!StratumClient::job_has_reprobe_proof(&job));
    }

    #[test]
    fn test_pending_submit_timeout_counts_unresolved() {
        let mut client = test_client();
        let now = std::time::Instant::now();
        // STRATUM-8: prune window raised to 120s (>= job lifetime). Use 121s to
        // exercise expiry; the 61s case now SURVIVES (see prune_window_is_120s).
        client.pending_submits.push(PendingSubmit {
            id: 10,
            job_id: "old".into(),
            difficulty: 1.0,
            submitted_at: now.checked_sub(Duration::from_secs(121)).unwrap(),
        });
        client.pending_submits.push(PendingSubmit {
            id: 11,
            job_id: "fresh".into(),
            difficulty: 2.0,
            submitted_at: now,
        });

        assert_eq!(client.prune_stale_pending_submits(now), 1);
        assert_eq!(client.pending_submits.len(), 1);
        assert_eq!(client.pending_submits[0].id, 11);
        assert_eq!(client.shares_unresolved, 1);
    }

    #[test]
    fn test_pending_submit_queue_cap_counts_unresolved() {
        let mut client = test_client();
        let now = std::time::Instant::now();
        for id in 0..64 {
            client.pending_submits.push(PendingSubmit {
                id,
                job_id: format!("job-{id}"),
                difficulty: 1.0,
                submitted_at: now,
            });
        }

        assert_eq!(client.enforce_pending_submit_limit(), 1);
        assert_eq!(client.pending_submits.len(), 63);
        assert_eq!(client.pending_submits[0].id, 1);
        assert_eq!(client.shares_unresolved, 1);
    }

    #[test]
    fn test_sync_status_reports_exact_pending_queue() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.shares_submitted = 7;
        client.shares_accepted = 3;
        client.shares_rejected = 1;
        client.shares_unresolved = 2;
        client.pending_submits.push(PendingSubmit {
            id: 12,
            job_id: "pending".into(),
            difficulty: 8.0,
            submitted_at: std::time::Instant::now(),
        });

        client.sync_status(None);

        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.shares_submitted, 7);
        assert_eq!(snapshot.shares_accepted, 3);
        assert_eq!(snapshot.shares_rejected, 1);
        assert_eq!(snapshot.shares_pending, 1);
        assert_eq!(snapshot.shares_unresolved, 2);
    }

    #[test]
    fn test_pending_submit_disconnect_counts_unresolved() {
        let mut client = test_client();
        let now = std::time::Instant::now();
        client.pending_submits.push(PendingSubmit {
            id: 12,
            job_id: "a".into(),
            difficulty: 1.0,
            submitted_at: now,
        });
        client.pending_submits.push(PendingSubmit {
            id: 13,
            job_id: "b".into(),
            difficulty: 1.0,
            submitted_at: now,
        });

        client.clear_pending_submits_unresolved("connection reset");

        assert!(client.pending_submits.is_empty());
        assert_eq!(client.shares_unresolved, 2);
    }

    #[test]
    fn test_submit_response_clears_pending_without_unresolved() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.shares_submitted = 1;
        client.pending_submits.push(PendingSubmit {
            id: 14,
            job_id: "job".into(),
            difficulty: 32.0,
            submitted_at: std::time::Instant::now(),
        });

        client.handle_submit_response(14, Some(Value::Bool(true)), None);

        assert!(client.pending_submits.is_empty());
        assert_eq!(client.shares_accepted, 1);
        assert_eq!(client.shares_unresolved, 0);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.shares_pending, 0);
        assert_eq!(snapshot.shares_unresolved, 0);
        assert!(snapshot.last_share_accepted_unix_ms > 0);
        assert_eq!(
            snapshot.last_share_time,
            snapshot.last_share_accepted_unix_ms / 1000
        );
        assert_eq!(snapshot.last_share_rejected_unix_ms, 0);
    }

    #[test]
    fn test_submit_auth_fatal_reject_requests_reconnect() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.shares_submitted = 1;
        client.pending_submits.push(PendingSubmit {
            id: 15,
            job_id: "job-auth".into(),
            difficulty: 64.0,
            submitted_at: std::time::Instant::now(),
        });

        let reconnect = client.handle_submit_response(
            15,
            None,
            Some(serde_json::json!([24, "Unauthorized worker", null])),
        );

        assert!(reconnect);
        assert!(client.pending_submits.is_empty());
        assert_eq!(client.shares_rejected, 1);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(
            snapshot.last_reconnect_cause,
            "submit rejected as unauthorized/not subscribed"
        );
        assert_eq!(snapshot.last_reject_reason, "Unauthorized worker");
        assert!(snapshot.last_share_rejected_unix_ms > 0);
        assert_eq!(snapshot.last_share_accepted_unix_ms, 0);
    }

    #[test]
    fn send_pong_writes_jsonrpc_response_shape() {
        // mining.pong must answer mining.ping as a JSON-RPC RESPONSE,
        // not a notification. Strict pools (NiceHash, public-pool) drop the
        // socket when a request id is answered with a `method`/`params` frame.
        let payload = build_pong_payload(42);
        let parsed: Value = serde_json::from_str(&payload).expect("pong is valid JSON");
        let obj = parsed.as_object().expect("pong is an object");

        // Must have id + result + error, must NOT have method or params.
        assert_eq!(obj.get("id").and_then(|v| v.as_u64()), Some(42));
        assert!(obj.contains_key("result"), "must contain `result` key");
        assert!(
            obj.get("result").map_or(false, |v| v.is_null()),
            "result must be null"
        );
        assert!(obj.contains_key("error"), "must contain `error` key");
        assert!(
            obj.get("error").map_or(false, |v| v.is_null()),
            "error must be null"
        );
        assert!(
            !obj.contains_key("method"),
            "MUST NOT have `method` field — that's a notification"
        );
        assert!(
            !obj.contains_key("params"),
            "MUST NOT have `params` field — that's a notification"
        );
    }

    // -----------------------------------------------------------------------
    // STRATUM-1: drained queued shares are counted unresolved
    // -----------------------------------------------------------------------
    #[test]
    fn test_drain_pending_share_queue_counts_unresolved() {
        let (mut client, share_tx) = test_client_with_share_tx();
        for i in 0..3 {
            share_tx
                .send(MiningEvent::SubmitShare(submission(&format!("job-{i}"))))
                .unwrap();
        }

        client.drain_pending_share_queue();

        assert_eq!(client.shares_unresolved, 3);
        // Queue is drained: a subsequent recv must find nothing.
        assert!(client.share_rx.try_recv().is_err());
    }

    #[test]
    fn test_drain_pending_share_queue_empty_no_change() {
        let (mut client, _share_tx) = test_client_with_share_tx();
        client.drain_pending_share_queue();
        assert_eq!(client.shares_unresolved, 0);
    }

    // -----------------------------------------------------------------------
    // STRATUM-3: oversized line discard-to-newline resync helper
    // -----------------------------------------------------------------------
    #[test]
    fn test_recv_line_resync_drops_oversized_tail_then_realigns() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            // > MAX_LINE_BYTES of non-newline garbage, a newline, then a valid
            // message. The garbage must be discarded and the valid message
            // returned intact (stream realigned on a true boundary).
            let garbage = vec![b'x'; MAX_LINE_BYTES + 512];
            sock.write_all(&garbage).unwrap();
            sock.write_all(b"\n").unwrap();
            sock.write_all(b"{\"id\":3,\"result\":true,\"error\":null}\n")
                .unwrap();
            sock.flush().unwrap();
            // Keep the socket open until the client has read.
            std::thread::sleep(Duration::from_millis(300));
        });

        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        let mut client = test_client();
        client.stream = Some(stream);

        // Pump recv_line until we either get the intact valid line or time out.
        // The load-bearing contract is that the oversized line's tail is fully
        // discarded and the FOLLOWING valid message is returned intact (stream
        // realigned). The discard may complete within a single recv_line call
        // when bytes arrive fast, so the flag is not asserted directly.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got_valid = None;
        while std::time::Instant::now() < deadline {
            match client.recv_line() {
                Ok(Some(line)) => {
                    got_valid = Some(line);
                    break;
                }
                Ok(None) => continue,
                Err(e) => panic!("unexpected recv error: {e}"),
            }
        }

        server.join().unwrap();
        let line = got_valid.expect("the valid message after the oversized line must be returned");
        // The returned line must be the clean valid message — NOT a corrupted
        // fragment containing leftover garbage from the oversized line.
        assert_eq!(line, "{\"id\":3,\"result\":true,\"error\":null}");
        assert!(!line.contains('x'), "no oversized-line garbage may leak in");
        // After realigning, the discard flag must be cleared.
        assert!(!client.discarding_oversized_line);
    }

    #[test]
    fn test_recv_line_large_under_cap_line_returned_intact() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // A line larger than the OLD 4096 cap but under MAX_LINE_BYTES must be
        // returned intact with no spurious discard.
        let payload = format!("{{\"id\":7,\"big\":\"{}\"}}", "a".repeat(8000));
        let payload_for_server = payload.clone();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            sock.write_all(payload_for_server.as_bytes()).unwrap();
            sock.write_all(b"\n").unwrap();
            sock.flush().unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });

        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        let mut client = test_client();
        client.stream = Some(stream);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while std::time::Instant::now() < deadline {
            match client.recv_line() {
                Ok(Some(line)) => {
                    got = Some(line);
                    break;
                }
                Ok(None) => continue,
                Err(e) => panic!("unexpected recv error: {e}"),
            }
        }
        server.join().unwrap();
        assert_eq!(got.expect("large-but-under-cap line returned"), payload);
        assert!(!client.discarding_oversized_line);
    }

    // -----------------------------------------------------------------------
    // STRATUM-5: reprobe handshake budget is tight
    // -----------------------------------------------------------------------
    #[test]
    fn test_reprobe_handshake_budget_is_tight() {
        let budget = HandshakeBudget::reprobe();
        assert!(budget.connect_timeout <= Duration::from_secs(3));
        assert!(budget.subscribe_deadline <= Duration::from_secs(3));
        assert!(budget.authorize_deadline <= Duration::from_secs(3));
        // And the default live budget keeps the historic timeouts.
        let live = HandshakeBudget::default_live();
        assert_eq!(live.connect_timeout, CONNECT_TIMEOUT);
        assert_eq!(live.subscribe_deadline, Duration::from_secs(15));
        assert_eq!(live.authorize_deadline, Duration::from_secs(10));
    }

    #[test]
    fn test_probe_primary_ready_fails_fast_on_refused_port() {
        let mut client = test_client();
        // Point primary at a refused port (closed) — the reprobe must fail fast,
        // well under the old ~35s, thanks to the 3s connect budget.
        client.primary_config = StratumConfig {
            url: "127.0.0.1".into(),
            port: 1,
            worker_name: "worker".into(),
            password: "x".into(),
            suggest_difficulty: 0,
            version_rolling: true,
        };

        let start = std::time::Instant::now();
        let result = client.probe_primary_ready();
        let elapsed = start.elapsed();

        assert!(result.is_err(), "refused port must yield Err");
        assert!(
            elapsed < Duration::from_secs(6),
            "reprobe should fail fast (<6s), took {:?}",
            elapsed
        );
    }

    // -----------------------------------------------------------------------
    // STRATUM-6: version-rolling min-bit-count negotiation
    // -----------------------------------------------------------------------
    #[test]
    fn test_configure_zero_bit_mask_disables_rolling() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.session.version_mask = 0x1fffe000;

        let result = serde_json::json!({ "version-rolling.mask": "00000000" });
        client
            .handle_configure_response(Some(result), None)
            .unwrap();

        assert_eq!(client.session.version_mask, 0);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.negotiated_version_bits, 0);
    }

    #[test]
    fn test_configure_single_bit_mask_below_min_still_applied() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());

        let result = serde_json::json!({ "version-rolling.mask": "00002000" });
        client
            .handle_configure_response(Some(result), None)
            .unwrap();

        // 1-bit mask is usable, just degraded — still applied.
        assert_eq!(client.session.version_mask, 0x2000);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.negotiated_version_bits, 1);
    }

    #[test]
    fn test_configure_full_mask_unchanged() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());

        let result = serde_json::json!({ "version-rolling.mask": "1fffe000" });
        client
            .handle_configure_response(Some(result), None)
            .unwrap();

        assert_eq!(client.session.version_mask, 0x1fffe000);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.negotiated_version_bits, 0x1fffe000u32.count_ones());
    }

    // -----------------------------------------------------------------------
    // STRATUM-7: build_subscribe_params
    // -----------------------------------------------------------------------
    #[test]
    fn test_build_subscribe_params_first_connect() {
        let params = build_subscribe_params(USER_AGENT, "");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], Value::String(USER_AGENT.to_string()));
    }

    #[test]
    fn test_build_subscribe_params_resume_with_token() {
        let params = build_subscribe_params(USER_AGENT, "abc123");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], Value::String(USER_AGENT.to_string()));
        assert_eq!(params[1], Value::String("abc123".to_string()));
    }

    // -----------------------------------------------------------------------
    // STRATUM-8: prune window raised to 120s + evicted-ring reclassify
    // -----------------------------------------------------------------------
    #[test]
    fn test_pending_submit_prune_window_is_120s() {
        let mut client = test_client();
        let now = std::time::Instant::now();
        // 61s old: previously expired at the 60s window, must now SURVIVE.
        client.pending_submits.push(PendingSubmit {
            id: 10,
            job_id: "survivor".into(),
            difficulty: 1.0,
            submitted_at: now.checked_sub(Duration::from_secs(61)).unwrap(),
        });
        // 121s old: beyond the 120s window, must expire.
        client.pending_submits.push(PendingSubmit {
            id: 11,
            job_id: "expired".into(),
            difficulty: 2.0,
            submitted_at: now.checked_sub(Duration::from_secs(121)).unwrap(),
        });

        assert_eq!(client.prune_stale_pending_submits(now), 1);
        assert_eq!(client.pending_submits.len(), 1);
        assert_eq!(client.pending_submits[0].id, 10);
        assert_eq!(client.shares_unresolved, 1);
        // The expired submit is in the recently-evicted ring.
        assert!(client.recently_evicted.iter().any(|e| e.id == 11));
    }

    #[test]
    fn test_evicted_ring_reclassifies_late_accept() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        let now = std::time::Instant::now();
        // Fill to the 64-cap so enforce_pending_submit_limit evicts the oldest.
        // Real submit ids start at ID_SUBMIT_BASE and increment; the oldest
        // evicted will be ID_SUBMIT_BASE itself.
        for offset in 0..64u64 {
            let id = ID_SUBMIT_BASE + offset;
            client.pending_submits.push(PendingSubmit {
                id,
                job_id: format!("job-{id}"),
                difficulty: 4.0,
                submitted_at: now,
            });
        }
        assert_eq!(client.enforce_pending_submit_limit(), 1);
        assert_eq!(client.shares_unresolved, 1);
        assert!(client
            .recently_evicted
            .iter()
            .any(|e| e.id == ID_SUBMIT_BASE));

        // A late accept arrives for the evicted id via handle_message.
        client
            .handle_message(&format!(
                r#"{{"id":{},"result":true,"error":null}}"#,
                ID_SUBMIT_BASE
            ))
            .unwrap();

        assert_eq!(client.shares_unresolved, 0);
        assert_eq!(client.shares_accepted, 1);
        assert!(!client
            .recently_evicted
            .iter()
            .any(|e| e.id == ID_SUBMIT_BASE));
    }

    #[test]
    fn test_evicted_ring_reclassifies_late_reject() {
        let mut client = test_client();
        let now = std::time::Instant::now();
        client.pending_submits.push(PendingSubmit {
            id: 30,
            job_id: "lossy".into(),
            difficulty: 8.0,
            submitted_at: now.checked_sub(Duration::from_secs(121)).unwrap(),
        });
        assert_eq!(client.prune_stale_pending_submits(now), 1);
        assert_eq!(client.shares_unresolved, 1);

        client
            .handle_message(r#"{"id":30,"result":null,"error":[23,"Low difficulty share",null]}"#)
            .unwrap();

        assert_eq!(client.shares_unresolved, 0);
        assert_eq!(client.shares_rejected, 1);
        assert!(!client.recently_evicted.iter().any(|e| e.id == 30));
    }

    #[test]
    fn test_unknown_non_submit_id_still_ignored() {
        let mut client = test_client();
        // An id below ID_SUBMIT_BASE that is not configure/extranonce and not in
        // any ring must move no counters (hits the debug-ignore else).
        client
            .handle_message(r#"{"id":6,"result":true,"error":null}"#)
            .unwrap();
        assert_eq!(client.shares_accepted, 0);
        assert_eq!(client.shares_rejected, 0);
        assert_eq!(client.shares_unresolved, 0);
    }

    // -----------------------------------------------------------------------
    // STRATUM-9: result==false reject surfaces reason + runs auth-fatal check
    // -----------------------------------------------------------------------
    #[test]
    fn test_result_false_reject_surfaces_string_reason() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.pending_submits.push(PendingSubmit {
            id: 40,
            job_id: "j".into(),
            difficulty: 1.0,
            submitted_at: std::time::Instant::now(),
        });

        let reconnect = client.handle_submit_response(
            40,
            Some(Value::String("low difficulty share".into())),
            None,
        );

        assert!(!reconnect);
        assert_eq!(client.shares_rejected, 1);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.last_reject_reason, "low difficulty share");
    }

    #[test]
    fn test_result_false_unauthorized_string_requests_reconnect() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.pending_submits.push(PendingSubmit {
            id: 41,
            job_id: "j".into(),
            difficulty: 1.0,
            submitted_at: std::time::Instant::now(),
        });

        let reconnect = client.handle_submit_response(
            41,
            Some(Value::String("unauthorized worker".into())),
            None,
        );

        assert!(reconnect);
        assert_eq!(client.shares_rejected, 1);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(
            snapshot.last_reconnect_cause,
            "submit rejected as unauthorized/not subscribed"
        );
    }

    #[test]
    fn test_result_false_no_payload_falls_back_to_constant() {
        let mut client = test_client();
        let status = new_shared_stratum_status(&StratumConfig::default(), 0);
        client.set_status_handle(status.clone());
        client.pending_submits.push(PendingSubmit {
            id: 42,
            job_id: "j".into(),
            difficulty: 1.0,
            submitted_at: std::time::Instant::now(),
        });

        let reconnect = client.handle_submit_response(42, Some(Value::Bool(false)), None);

        assert!(!reconnect);
        assert_eq!(client.shares_rejected, 1);
        let snapshot = status.lock().unwrap().clone();
        assert_eq!(snapshot.last_reject_reason, "pool rejected share");
    }
}
