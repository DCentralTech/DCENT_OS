//! Authentication middleware for dcentrald API.
//!
//! Requires a password on first boot. Password is stored as an argon2id hash
//! in `/data/dcent/auth.json`. Physical reset: hold reset button 15s to
//! delete the auth file and reset password.
//!
//! Unauthenticated endpoints:
//!   - GET  /api/auth/status   — returns whether password is set
//!   - POST /api/auth/setup    — set initial password (only when no password exists)
//!   - POST /api/auth/session  — create a revocable dashboard session
//!   - GET  /                 — dashboard
//!   - GET  /static/*         — static assets
//!   - GET  /api/setup/status — setup wizard
//!   - GET  /api/safety/warnings — safety warnings
//!
//! All other endpoints require a Bearer session token. WebSocket `/ws` accepts
//! the same bearer via `?token=` for browser compatibility, or a one-time
//! `?ticket=` when `[api].websocket_tickets = true`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

use crate::atomic_io::atomic_write;

/// Path where the auth credentials are persisted.
const AUTH_FILE: &str = "/data/dcent/auth.json";
const CORRUPT_AUTH_PASSWORD_HASH_SENTINEL: &str = "dcent-auth-corrupt-sessions-revoked";

/// Rate limit: max setup attempts per IP within the window.
const SETUP_RATE_LIMIT_MAX: u8 = 3;
/// Rate limit window in seconds.
const SETUP_RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Default bearer-session lifetime (30 days). This is the ABSOLUTE TTL — a
/// session can never live longer than this regardless of activity.
const SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Maximum persisted active bearer sessions. Expired/revoked sessions are
/// pruned at auth mutation points, and issuing the next session evicts the
/// oldest active record when this cap would otherwise be exceeded.
const MAX_AUTH_SESSIONS: usize = 32;

/// Default session idle timeout (8 hours). A session that goes unused for
/// longer than this is treated as expired even though its absolute 30-day TTL
/// has not elapsed. Overridable at startup via `init_auth_config`. 8 h is long
/// enough that an actively-used dashboard tab never gets logged out mid-session
/// (the dashboard polls/streams continuously), but short enough that an idle
/// bearer left in a closed laptop / forgotten curl session does not stay live
/// for a month.
const DEFAULT_SESSION_IDLE_TIMEOUT_SECS: u64 = 8 * 60 * 60;
const WS_TICKET_TTL_SECS: u64 = 30;
const MAX_WS_TICKETS: usize = 128;

/// Login (password→session) brute-force limiter (SEC, GROUP-C HIGH).
///
/// The setup endpoint has always been rate-limited, but the LOGIN endpoint
/// (`POST /api/auth/session`, which exchanges the owner password for a bearer)
/// had NO rate limit — an on-LAN attacker could grind the owner password at
/// line rate. This limiter is fail-closed with exponential backoff:
///
/// - Up to `LOGIN_RATE_LIMIT_SOFT_MAX` failed attempts per IP are allowed
///   inside the rolling window before the IP is locked out.
/// - Each lockout doubles the backoff (`base << min(lockouts-1, cap)`), capped
///   at `LOGIN_LOCKOUT_MAX_SECS`, so a persistent grinder is quickly throttled
///   to near-zero throughput while a fat-fingered operator recovers in seconds.
/// - A SUCCESSFUL login clears the IP's failure record (legitimate users are
///   never penalized for an eventual success).
///
/// Only auth.rs owns the policy; `rest.rs::post_auth_session` must (a) call
/// `check_login_rate_limit(ip)` BEFORE verifying the password and (b) call
/// `record_login_failure(ip)` on a bad password / `record_login_success(ip)`
/// on a good one. See the GROUP-C wiring note flagged for rest.rs.
const LOGIN_RATE_LIMIT_SOFT_MAX: u32 = 5;
/// Rolling window for counting login failures, in seconds.
const LOGIN_RATE_LIMIT_WINDOW_SECS: u64 = 300;
/// Base lockout duration once the soft cap is hit (doubles per lockout).
const LOGIN_LOCKOUT_BASE_SECS: u64 = 15;
/// Hard ceiling on the exponential lockout backoff.
const LOGIN_LOCKOUT_MAX_SECS: u64 = 15 * 60;

/// Per-IP rate limiter for /api/auth/setup to prevent brute-force password guessing.
static SETUP_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, (u8, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-IP login-attempt state for the brute-force limiter.
#[derive(Debug, Clone)]
struct LoginAttemptState {
    /// Failed attempts inside the current rolling window.
    failures: u32,
    /// Start of the current rolling failure window.
    window_start: Instant,
    /// Number of times this IP has been locked out (drives exponential backoff).
    lockouts: u32,
    /// If `Some`, the IP is locked out until this instant.
    locked_until: Option<Instant>,
}

/// Per-IP login brute-force limiter map. Separate from the setup limiter so the
/// two policies (and their windows) never interfere.
static LOGIN_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, LoginAttemptState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Configurable session idle timeout, in seconds. `0` disables the idle check
/// (absolute TTL still applies). Set once at startup via `init_auth_config`.
static SESSION_IDLE_TIMEOUT_SECS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DEFAULT_SESSION_IDLE_TIMEOUT_SECS);

/// In-memory "last activity" tracker for the idle-timeout check, keyed by the
/// session token hash (never the raw token). Kept in memory rather than
/// persisted so the hot per-request auth path never fsyncs `auth.json`; a
/// daemon restart simply grants every still-valid session a fresh idle window,
/// which can never extend a session past its persisted absolute TTL. Entries
/// for revoked/expired sessions are pruned lazily on access.
static SESSION_LAST_SEEN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static WEBSOCKET_TICKETS_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
struct WsTicketRecord {
    session_token_hash: String,
    expires_at_s: u64,
}

static WS_TICKETS: LazyLock<Mutex<HashMap<String, WsTicketRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static AUTH_CORRUPT_QUARANTINE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Check if an IP has exceeded the setup rate limit.
/// Returns Ok(()) if allowed, Err(Response) with 429 if rate-limited.
pub fn check_setup_rate_limit(ip: IpAddr) -> std::result::Result<(), Response<Body>> {
    let mut map = SETUP_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();

    if let Some((count, window_start)) = map.get_mut(&ip) {
        if now.duration_since(*window_start).as_secs() >= SETUP_RATE_LIMIT_WINDOW_SECS {
            // Window expired — reset
            *count = 1;
            *window_start = now;
            return Ok(());
        }
        if *count >= SETUP_RATE_LIMIT_MAX {
            let body = serde_json::json!({
                "error": "Too many requests",
                "detail": format!(
                    "Rate limited: max {} setup attempts per {} seconds",
                    SETUP_RATE_LIMIT_MAX, SETUP_RATE_LIMIT_WINDOW_SECS
                ),
            });
            return Err(Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("Content-Type", "application/json")
                .header("Retry-After", "60")
                .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
                .unwrap());
        }
        *count += 1;
    } else {
        map.insert(ip, (1, now));
    }
    Ok(())
}

/// Build the 429 lockout response with a `Retry-After` header.
fn login_locked_response(retry_after_secs: u64) -> Response<Body> {
    let body = serde_json::json!({
        "error": "Too many login attempts",
        "detail": format!(
            "Login temporarily locked due to repeated failures. Retry in {} seconds.",
            retry_after_secs
        ),
    });
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "application/json")
        .header("Retry-After", retry_after_secs.to_string())
        .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
        .unwrap()
}

/// Check whether `ip` may attempt a login right now.
///
/// Returns `Ok(())` if allowed, or `Err(429)` with a `Retry-After` header if
/// the IP is currently locked out. Call this BEFORE verifying the password in
/// `rest.rs::post_auth_session`; pair it with `record_login_failure` /
/// `record_login_success` on the verification result.
pub fn check_login_rate_limit(ip: IpAddr) -> std::result::Result<(), Response<Body>> {
    check_login_rate_limit_at(ip, Instant::now())
}

/// Internal seam for `check_login_rate_limit` parameterized on "now" so the
/// test suite can drive the window/backoff deterministically.
pub(crate) fn check_login_rate_limit_at(
    ip: IpAddr,
    now: Instant,
) -> std::result::Result<(), Response<Body>> {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = map.get_mut(&ip) {
        if let Some(locked_until) = state.locked_until {
            if now < locked_until {
                let retry_after = locked_until.saturating_duration_since(now).as_secs().max(1);
                return Err(login_locked_response(retry_after));
            }
            // Lockout expired — clear it and start a fresh failure window so the
            // IP gets a clean set of soft attempts before the next (longer)
            // lockout.
            state.locked_until = None;
            state.failures = 0;
            state.window_start = now;
        }
    }
    Ok(())
}

/// Record a FAILED login attempt for `ip` and, if the soft cap is exceeded
/// inside the window, escalate to an exponentially-backed-off lockout.
pub fn record_login_failure(ip: IpAddr) {
    record_login_failure_at(ip, Instant::now());
}

/// Internal seam for `record_login_failure` parameterized on "now".
pub(crate) fn record_login_failure_at(ip: IpAddr, now: Instant) {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let state = map.entry(ip).or_insert_with(|| LoginAttemptState {
        failures: 0,
        window_start: now,
        lockouts: 0,
        locked_until: None,
    });

    // Roll the window if it has fully elapsed and the IP is not actively locked.
    if state.locked_until.is_none()
        && now.duration_since(state.window_start).as_secs() >= LOGIN_RATE_LIMIT_WINDOW_SECS
    {
        state.failures = 0;
        state.window_start = now;
    }

    state.failures = state.failures.saturating_add(1);

    if state.failures >= LOGIN_RATE_LIMIT_SOFT_MAX {
        // Escalate to a lockout with exponential backoff:
        //   base * 2^min(lockouts, shift_cap), capped at LOGIN_LOCKOUT_MAX_SECS.
        state.lockouts = state.lockouts.saturating_add(1);
        let shift = (state.lockouts - 1).min(20); // guard against overflow
        let backoff = LOGIN_LOCKOUT_BASE_SECS
            .saturating_mul(1u64 << shift)
            .min(LOGIN_LOCKOUT_MAX_SECS);
        state.locked_until = Some(now + std::time::Duration::from_secs(backoff));
        // Reset the soft counter; the next window starts after the lockout.
        state.failures = 0;
        tracing::warn!(
            target: "auth_ratelimit",
            ip = %ip,
            lockouts = state.lockouts,
            backoff_secs = backoff,
            "login brute-force lockout engaged",
        );
    }
}

/// Record a SUCCESSFUL login for `ip`, clearing any accumulated failure state
/// so a legitimate user is never penalized for an eventual success.
pub fn record_login_success(ip: IpAddr) {
    let mut map = LOGIN_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(&ip);
}

/// Whether /metrics requires authentication (set at startup from ApiConfig).
/// Default: true so production images fail closed.
static METRICS_REQUIRE_AUTH: AtomicBool = AtomicBool::new(true);
const DASHBOARD_PROXY_HEADER: &str = "x-dcentos-dashboard-proxy";

/// Legacy static dashboard-proxy header value. Forgeable by any LAN client
/// (the dashboard `server.py` has no auth of its own), so it is ONLY trusted
/// on a DEV/LAB image. On a release image the daemon requires the per-boot
/// nonce (`DASHBOARD_PROXY_NONCE_FILE`) and rejects this value. See SEC-W24-1.
const DASHBOARD_PROXY_LEGACY_VALUE: &str = "1";

/// Per-boot trusted-loopback nonce file (SEC-W24-1). Minted by the
/// `S80dashboard` init script into tmpfs (`/run`, 0600 root-only) for explicit
/// same-host helpers that must bypass Bearer auth. The LAN-facing dashboard
/// `server.py` does not stamp this header; it forwards Bearer auth instead. A
/// LAN attacker cannot read the root-only file and cannot guess the 256-bit
/// secret, so a forged static header can never authenticate on a release image.
const DASHBOARD_PROXY_NONCE_FILE: &str = "/run/dcentos/proxy_nonce";

/// Baked marker that distinguishes a PRODUCTION/RELEASE image from a DEV/LAB
/// image. Stamped into the rootfs ONLY for release builds
/// (`DCENT_RELEASE_IMAGE=1` at Buildroot time → `scripts/lib/release_image
/// _provision.sh`). On a dev/lab image this file is absent and the
/// "freedom-first" passwordless opt-out keeps working byte-identically.
const RELEASE_IMAGE_MARKER: &str = "/etc/dcentos/release-image";

/// Cached release-image posture. `0` = unknown/not-yet-probed, `1` = release
/// image (marker present), `2` = dev/lab image (marker absent). Probed once
/// from the marker file on first call to `is_release_image()` and cached so
/// the hot auth path does not stat the filesystem on every request.
static RELEASE_IMAGE_STATE: AtomicU8 = AtomicU8::new(0);

/// Whether this firmware was built as a PRODUCTION/RELEASE image.
///
/// PRODUCTION images (`DCENT_RELEASE_IMAGE=1` Buildroot flag, marker file
/// `/etc/dcentos/release-image`) MUST require a password for the dashboard/API
/// — the freedom-first password opt-out (`/api/setup/skip-password`) and the
/// safety opt-out (`/api/setup/skip-safety`) are DISABLED. DEV/LAB images (no
/// marker) keep the freedom-first behavior byte-identically.
///
/// The posture is cached on first probe so the per-request auth path stays
/// allocation/syscall-free after warm-up.
pub fn is_release_image() -> bool {
    is_release_image_at(std::path::Path::new(RELEASE_IMAGE_MARKER))
}

/// Internal: probe the release-image posture at a caller-supplied marker path.
/// Used by `is_release_image()` in production and by the test suite to
/// exercise both postures against scratch paths without touching `/etc`.
///
/// The cache is keyed to the production marker path: when `path` is the real
/// `RELEASE_IMAGE_MARKER` we read/populate the process-wide cache; for any
/// other (test) path we always re-probe so tests are deterministic and never
/// poison the production cache.
pub(crate) fn is_release_image_at(path: &std::path::Path) -> bool {
    let is_production_path = path == std::path::Path::new(RELEASE_IMAGE_MARKER);
    if is_production_path {
        match RELEASE_IMAGE_STATE.load(Ordering::Relaxed) {
            1 => return true,
            2 => return false,
            _ => {}
        }
    }
    let present = path.exists();
    if is_production_path {
        RELEASE_IMAGE_STATE.store(if present { 1 } else { 2 }, Ordering::Relaxed);
    }
    present
}

/// Initialize auth module configuration. Call once at API server startup.
pub fn init_auth_config(metrics_require_auth: bool, websocket_tickets_enabled: bool) {
    METRICS_REQUIRE_AUTH.store(metrics_require_auth, Ordering::Relaxed);
    WEBSOCKET_TICKETS_ENABLED.store(websocket_tickets_enabled, Ordering::Relaxed);
}

/// Override the default session idle timeout (seconds). `0` disables the idle
/// check (the absolute 30-day TTL still applies). Call once at startup if the
/// operator configures a non-default idle window; if never called the
/// `DEFAULT_SESSION_IDLE_TIMEOUT_SECS` value is used.
pub fn set_session_idle_timeout_secs(secs: u64) {
    SESSION_IDLE_TIMEOUT_SECS.store(secs, Ordering::Relaxed);
}

/// Current configured session idle timeout in seconds (`0` = disabled).
pub fn session_idle_timeout_secs() -> u64 {
    SESSION_IDLE_TIMEOUT_SECS.load(Ordering::Relaxed)
}

/// Authorization scope for a bearer session.
///
/// `Admin` is the historical, full-access role: it can read everything AND use
/// mutating (POST/PUT/PATCH/DELETE) endpoints. `ReadOnly` is a new scoped role
/// for monitoring callers (dashboards, fleet pollers, Prometheus scrapers that
/// need authenticated reads): it can hit GET endpoints but every mutating
/// request is rejected with 403 by the auth middleware.
///
/// The serde representation is a lowercase string (`"admin"` / `"read_only"`).
/// An on-disk session written before this field existed deserializes to the
/// `#[default]` `Admin` variant, so existing sessions keep full access and
/// nothing regresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    /// Full read + write access (the historical behavior).
    #[default]
    Admin,
    /// Read-only: GET allowed, all mutating methods rejected.
    ReadOnly,
}

impl SessionRole {
    /// Whether this role is permitted to perform mutating (write) requests.
    pub fn can_write(self) -> bool {
        matches!(self, SessionRole::Admin)
    }
}

/// Stored authentication data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthSession {
    /// Opaque session identifier used for revocation.
    pub id: String,
    /// SHA-256 of the issued bearer token.
    pub token_hash: String,
    /// Creation timestamp (epoch-seconds string for low dependency overhead).
    pub created_at: String,
    /// Session label (dashboard, CLI, etc.).
    pub label: String,
    /// Optional expiration timestamp.
    pub expires_at: Option<String>,
    /// Timestamp when the session was revoked.
    pub revoked_at: Option<String>,
    /// Authorization scope. Absent in pre-role on-disk sessions → defaults to
    /// `Admin` (full access) so existing sessions are never silently demoted.
    pub role: SessionRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthData {
    /// File format version.
    pub version: u8,
    /// Argon2id hash of the admin password (PHC format).
    pub password_hash: String,
    /// Legacy single-token field kept only for migration from v1 auth files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,
    /// Active and revoked bearer sessions.
    pub sessions: Vec<AuthSession>,
}

#[derive(Debug, Clone)]
pub struct IssuedSession {
    pub id: String,
    pub token: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuedWsTicket {
    pub ticket: String,
    pub expires_in_s: u64,
}

/// Check if a password has been configured.
pub fn is_password_set() -> bool {
    is_password_set_at(
        std::path::Path::new(AUTH_FILE),
        std::path::Path::new(RELEASE_IMAGE_MARKER),
    )
}

pub(crate) fn is_password_set_at(
    auth_path: &std::path::Path,
    release_marker: &std::path::Path,
) -> bool {
    auth_path.exists()
        || (is_release_image_at(release_marker) && corrupt_auth_quarantine_exists(auth_path))
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_epoch_string() -> String {
    now_epoch_secs().to_string()
}

fn hash_session_token(token: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn session_is_active(session: &AuthSession) -> bool {
    if session.revoked_at.is_some() {
        return false;
    }

    session
        .expires_at
        .as_deref()
        .map(|expires_at| {
            expires_at
                .parse::<u64>()
                .map(|expires_at_s| expires_at_s > now_epoch_secs())
                .unwrap_or(false)
        })
        .unwrap_or(true)
}

fn session_created_at_secs(session: &AuthSession) -> u64 {
    session.created_at.parse::<u64>().unwrap_or(0)
}

fn prune_inactive_sessions(auth: &mut AuthData) -> usize {
    let before = auth.sessions.len();
    auth.sessions.retain(|session| {
        let keep = session_is_active(session);
        if !keep {
            forget_session_last_seen(&session.token_hash);
        }
        keep
    });
    before.saturating_sub(auth.sessions.len())
}

fn compact_sessions_before_issue(auth: &mut AuthData) {
    let pruned = prune_inactive_sessions(auth);
    let mut evicted = 0usize;

    while auth.sessions.len() >= MAX_AUTH_SESSIONS {
        let Some(oldest_index) = auth
            .sessions
            .iter()
            .enumerate()
            .min_by_key(|(_, session)| session_created_at_secs(session))
            .map(|(index, _)| index)
        else {
            break;
        };
        let removed = auth.sessions.remove(oldest_index);
        forget_session_last_seen(&removed.token_hash);
        evicted += 1;
    }

    if pruned != 0 || evicted != 0 {
        tracing::info!(
            target: "auth_session_store",
            pruned_inactive = pruned,
            evicted_oldest = evicted,
            active_sessions = auth.sessions.len(),
            max_sessions = MAX_AUTH_SESSIONS,
            "compacted persisted auth sessions",
        );
    }
}

/// Idle-timeout check + touch for a token-authenticated request.
///
/// Returns `true` if the session has NOT gone idle (and records the current
/// instant as its last-seen time), `false` if it has been idle longer than the
/// configured timeout (in which case the in-memory tracking entry is pruned so
/// the map cannot grow unbounded with dead sessions).
///
/// The timeout is read live so a startup override applies. A timeout of `0`
/// disables the idle check entirely (always `true`, but the last-seen is still
/// recorded so flipping the timeout on at runtime behaves sanely).
fn session_idle_ok_and_touch(token_hash: &str) -> bool {
    session_idle_ok_and_touch_at(token_hash, Instant::now())
}

/// Internal seam for `session_idle_ok_and_touch` parameterized on "now" so the
/// test suite can drive the idle window deterministically.
pub(crate) fn session_idle_ok_and_touch_at(token_hash: &str, now: Instant) -> bool {
    let timeout = session_idle_timeout_secs();
    let mut map = SESSION_LAST_SEEN.lock().unwrap_or_else(|e| e.into_inner());

    if timeout != 0 {
        if let Some(&last_seen) = map.get(token_hash) {
            if now.duration_since(last_seen).as_secs() >= timeout {
                // Gone idle — drop the tracking entry and reject.
                map.remove(token_hash);
                return false;
            }
        }
    }
    // First sight this process-lifetime, or still inside the idle window —
    // record activity and allow.
    map.insert(token_hash.to_string(), now);
    true
}

/// Clear the in-memory idle-tracking entry for a session token hash. Called on
/// explicit revocation so a revoked session's slot is reclaimed immediately.
fn forget_session_last_seen(token_hash: &str) {
    let mut map = SESSION_LAST_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(token_hash);
}

fn session_matches_token(session: &AuthSession, token: &str) -> bool {
    if !session_is_active(session) {
        return false;
    }
    let token_hash = hash_session_token(token);
    session.token_hash == token_hash && session_idle_ok_and_touch(&token_hash)
}

/// Load auth data from disk.
pub fn load_auth() -> Option<AuthData> {
    load_auth_at(
        std::path::Path::new(AUTH_FILE),
        std::path::Path::new(RELEASE_IMAGE_MARKER),
    )
}

pub(crate) fn load_auth_at(
    path: &std::path::Path,
    release_marker: &std::path::Path,
) -> Option<AuthData> {
    load_auth_with_release_posture(path, is_release_image_at(release_marker))
}

fn load_auth_with_release_posture(path: &std::path::Path, release_image: bool) -> Option<AuthData> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if release_image && corrupt_auth_quarantine_exists(path) {
                tracing::error!(
                    target: "auth_persistence",
                    path = %path.display(),
                    "auth.json is absent but a corrupt quarantine exists; treating release image as configured with sessions revoked",
                );
                return Some(corrupt_auth_sentinel());
            }
            return None;
        }
        Err(err) => {
            tracing::error!(
                target: "auth_persistence",
                path = %path.display(),
                error = %err,
                "could not read auth.json",
            );
            return None;
        }
    };
    let data = match String::from_utf8(bytes) {
        Ok(data) => data,
        Err(err) => return handle_corrupt_auth(path, release_image, err.to_string()),
    };
    let mut auth: AuthData = match serde_json::from_str(&data) {
        Ok(auth) => auth,
        Err(err) => return handle_corrupt_auth(path, release_image, err.to_string()),
    };
    let mut dirty = false;

    if auth.version < 2 {
        auth.version = 2;
        dirty = true;
    }

    if let Some(legacy_token) = auth.api_token.take() {
        if !legacy_token.trim().is_empty() {
            auth.sessions.push(AuthSession {
                id: generate_token(),
                token_hash: hash_session_token(&legacy_token),
                created_at: now_epoch_string(),
                label: "legacy-migrated".to_string(),
                expires_at: None,
                revoked_at: None,
                // A migrated legacy single-token session keeps full access —
                // it predates roles and was the sole owner token.
                role: SessionRole::Admin,
            });
        }
        dirty = true;
    }

    if dirty {
        let _ = save_auth_at(path, &auth);
    }

    Some(auth)
}

fn corrupt_auth_sentinel() -> AuthData {
    AuthData {
        version: 2,
        password_hash: CORRUPT_AUTH_PASSWORD_HASH_SENTINEL.to_string(),
        api_token: None,
        sessions: Vec::new(),
    }
}

fn handle_corrupt_auth(
    path: &std::path::Path,
    release_image: bool,
    reason: String,
) -> Option<AuthData> {
    let quarantine_path = quarantine_corrupt_auth(path);
    tracing::error!(
        target: "auth_persistence",
        path = %path.display(),
        quarantine_path = quarantine_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<failed>".to_string()),
        reason = %reason,
        release_image,
        "auth.json is corrupt; quarantined and revoked in-memory sessions",
    );
    if release_image {
        Some(corrupt_auth_sentinel())
    } else {
        None
    }
}

fn corrupt_auth_quarantine_exists(path: &std::path::Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let prefix = format!("{file_name}.corrupt.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with(&prefix))
            .unwrap_or(false)
    })
}

fn quarantine_corrupt_auth(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let parent = path.parent()?;
    let file_name = path.file_name()?.to_str()?;
    let epoch = now_epoch_secs();
    let mut target = parent.join(format!("{file_name}.corrupt.{epoch}"));
    if target.exists() {
        let seq = AUTH_CORRUPT_QUARANTINE_SEQ.fetch_add(1, Ordering::Relaxed);
        target = parent.join(format!(
            "{file_name}.corrupt.{epoch}.{}.{}",
            std::process::id(),
            seq
        ));
    }
    match std::fs::rename(path, &target) {
        Ok(()) => Some(target),
        Err(err) => {
            tracing::error!(
                target: "auth_persistence",
                path = %path.display(),
                target = %target.display(),
                error = %err,
                "failed to quarantine corrupt auth.json",
            );
            None
        }
    }
}

/// Save auth data to disk with hardened permissions (W1.5).
///
/// Guarantees:
///   - parent dir is `0o700` (root:root only)
///   - auth.json file is `0o600` (root:root only)
///
/// SECURITY (W1.5, 2026-05-07): the auth.json file holds an argon2id password
/// hash and active bearer-session hashes. World/group readability lets any
/// local UID (including non-root processes that should not have miner control)
/// dump the hashes for offline cracking and harvest live session tokens. This
/// fn now refuses to leave the file with permissions wider than `0o600` after
/// write. Failures to tighten perms are logged but do not block the save —
/// fail-soft so password rotation never bricks an in-the-field unit.
pub fn save_auth(auth: &AuthData) -> std::io::Result<()> {
    save_auth_at(std::path::Path::new(AUTH_FILE), auth)
}

/// Internal: save auth data to a caller-supplied path. Used by `save_auth()`
/// in production and by the test suite to exercise the harden-perms path
/// against a tempdir without touching `/data/dcent/`.
pub(crate) fn save_auth_at(path: &std::path::Path, auth: &AuthData) -> std::io::Result<()> {
    // Ensure directory exists with tight perms.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // Tighten parent dir to 0o700. Non-fatal: a fail-soft warning so we
        // do not strand operators on a unit where /data/dcent existed with
        // wider perms before this code shipped.
        if let Err(err) = set_mode(parent, 0o700) {
            tracing::warn!(
                target: "auth_perms",
                path = %parent.display(),
                error = %err,
                "could not tighten auth parent dir to 0o700",
            );
        }
    }
    let json = serde_json::to_string_pretty(auth).map_err(std::io::Error::other)?;
    atomic_write(path, json.as_bytes())?;
    if let Err(err) = set_mode(path, 0o600) {
        tracing::warn!(
            target: "auth_perms",
            path = %path.display(),
            error = %err,
            "could not tighten auth file to 0o600",
        );
    }
    Ok(())
}

/// Set Unix file mode. Wraps `std::fs::set_permissions` so callers don't have
/// to repeat the `PermissionsExt` import at every site.
#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    // Windows host build (test/dev only — production targets are all Unix).
    // Treat as success so the test suite can run on dev hosts; the real
    // hardening happens on the embedded Linux target.
    Ok(())
}

/// Verify and (when possible) auto-correct the on-disk auth file/dir perms.
///
/// SECURITY (W1.5, 2026-05-07): called once on daemon startup, before any
/// socket binds. If the file is wider than 0o600 or the parent dir is wider
/// than 0o700, this fn auto-corrects rather than failing closed — fail-closed
/// would brick first-boot units that pre-existed before this hardening
/// shipped, and that is a worse failure mode than a transient wide-perm
/// window. Owner != uid 0 is logged at ERROR level (it should never happen
/// on a production boot where dcentrald runs as root) but is also non-fatal,
/// because lab hosts running cargo test under non-root must not crash here.
///
/// Returns Ok in all non-IO error paths. Only IO errors that prevent reading
/// the metadata at all surface as Err — and even those become a startup
/// warning, not a panic, because losing access to /data/dcent/auth.json on
/// startup means the unit can still serve the dashboard's "set password"
/// flow.
pub fn verify_auth_file_perms() -> std::io::Result<()> {
    verify_auth_file_perms_at(std::path::Path::new(AUTH_FILE))
}

/// Internal: verify perms at a caller-supplied path. Same contract as
/// `verify_auth_file_perms()` — used by the test suite.
pub(crate) fn verify_auth_file_perms_at(path: &std::path::Path) -> std::io::Result<()> {
    // Parent dir: must be 0o700 (or tighter).
    if let Some(parent) = path.parent() {
        if parent.exists() {
            check_and_tighten(parent, 0o700, "auth parent dir");
        }
    }
    // Auth file: must be 0o600 (or tighter). If it does not exist yet (no
    // password configured), nothing to do — save_auth() will create it with
    // the right perms when the operator sets a password.
    if path.exists() {
        check_and_tighten(path, 0o600, "auth file");
        check_owner_is_root(path);
    }
    Ok(())
}

/// Inspect `path`'s mode and tighten it to `wanted_mode` when it is wider.
/// Logs a warning and bumps the perms-correction tracing event so operators
/// can see drift via the diagnostic dashboard.
#[cfg(unix)]
fn check_and_tighten(path: &std::path::Path, wanted_mode: u32, label: &str) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(
                target: "auth_perms",
                path = %path.display(),
                error = %err,
                "could not stat {} for perm check",
                label,
            );
            return;
        }
    };
    let actual = metadata.permissions().mode() & 0o777;
    if actual & !wanted_mode != 0 {
        // Wider than wanted — auto-correct.
        tracing::warn!(
            target: "auth_perms",
            path = %path.display(),
            actual_mode = format!("0o{:o}", actual),
            wanted_mode = format!("0o{:o}", wanted_mode),
            "{} perms wider than expected — tightening",
            label,
        );
        if let Err(err) = set_mode(path, wanted_mode) {
            tracing::error!(
                target: "auth_perms",
                path = %path.display(),
                error = %err,
                "could not auto-tighten {} perms",
                label,
            );
        }
    }
}

#[cfg(not(unix))]
fn check_and_tighten(_path: &std::path::Path, _wanted_mode: u32, _label: &str) {
    // No-op on non-Unix dev hosts.
}

/// Log an ERROR if the auth file is not owned by root (uid 0). Non-fatal so
/// `cargo test` under a non-root user does not panic.
#[cfg(unix)]
fn check_owner_is_root(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    let uid = metadata.uid();
    if uid != 0 {
        tracing::error!(
            target: "auth_perms",
            path = %path.display(),
            uid = uid,
            "auth file not owned by root (uid 0) — possible privilege drift",
        );
    }
}

#[cfg(not(unix))]
fn check_owner_is_root(_path: &std::path::Path) {}

pub fn active_session_count() -> usize {
    load_auth()
        .map(|auth| {
            auth.sessions
                .iter()
                .filter(|session| session_is_active(session))
                .count()
        })
        .unwrap_or(0)
}

pub fn has_active_sessions() -> bool {
    active_session_count() > 0
}

/// Issue a full-access (`Admin`) bearer session. Backward-compatible signature
/// — existing callers keep producing admin sessions exactly as before.
pub fn issue_session(auth: &mut AuthData, label: Option<&str>) -> IssuedSession {
    issue_session_with_role(auth, label, SessionRole::Admin)
}

/// Issue a bearer session with an explicit authorization scope.
///
/// `SessionRole::ReadOnly` mints a monitoring token that can GET but cannot use
/// any mutating endpoint (the auth middleware enforces this). Callers that want
/// the historical full-access token use `issue_session` (or pass
/// `SessionRole::Admin`).
pub fn issue_session_with_role(
    auth: &mut AuthData,
    label: Option<&str>,
    role: SessionRole,
) -> IssuedSession {
    compact_sessions_before_issue(auth);

    let token = generate_token();
    let now_s = now_epoch_secs();
    let expires_at = Some((now_s + SESSION_TTL_SECS).to_string());
    let session = AuthSession {
        id: generate_token(),
        token_hash: hash_session_token(&token),
        created_at: now_s.to_string(),
        label: label.unwrap_or("dashboard").to_string(),
        expires_at: expires_at.clone(),
        revoked_at: None,
        role,
    };
    // Seed the idle tracker so the freshly-issued token starts its idle window
    // now (rather than only on its first authenticated request).
    let _ = session_idle_ok_and_touch(&session.token_hash);
    let id = session.id.clone();
    auth.version = 2;
    auth.api_token = None;
    auth.sessions.push(session);
    IssuedSession {
        id,
        token,
        expires_at,
    }
}

pub fn revoke_session(auth: &mut AuthData, session_id: &str) -> bool {
    let revoked = if let Some(session) = auth
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id && session_is_active(session))
    {
        session.revoked_at = Some(now_epoch_string());
        true
    } else {
        false
    };

    if revoked {
        prune_inactive_sessions(auth);
    }

    revoked
}

/// Resolve the authorization role for a valid bearer token, if any. Returns
/// `None` when no password is configured or the token does not match an active,
/// non-idle session.
fn role_for_token(token: &str) -> Option<SessionRole> {
    let auth_data = load_auth()?;
    session_for_token(&auth_data, token).map(|session| session.role)
}

fn session_for_token(auth: &AuthData, token: &str) -> Option<AuthSession> {
    auth.sessions
        .iter()
        .find(|session| session_matches_token(session, token))
        .cloned()
}

fn session_for_token_hash(auth: &AuthData, token_hash: &str) -> Option<AuthSession> {
    auth.sessions
        .iter()
        .find(|session| session.token_hash == token_hash && session_is_active(session))
        .cloned()
}

fn websocket_tickets_enabled() -> bool {
    WEBSOCKET_TICKETS_ENABLED.load(Ordering::Relaxed)
}

fn prune_ws_tickets_locked(tickets: &mut HashMap<String, WsTicketRecord>, now_s: u64) {
    tickets.retain(|_, ticket| ticket.expires_at_s > now_s);
    if tickets.len() <= MAX_WS_TICKETS {
        return;
    }

    let mut by_expiry: Vec<(String, u64)> = tickets
        .iter()
        .map(|(ticket_hash, record)| (ticket_hash.clone(), record.expires_at_s))
        .collect();
    by_expiry.sort_by_key(|(_, expires_at_s)| *expires_at_s);
    let drop_count = tickets.len().saturating_sub(MAX_WS_TICKETS);
    for (ticket_hash, _) in by_expiry.into_iter().take(drop_count) {
        tickets.remove(&ticket_hash);
    }
}

fn issue_ws_ticket_for_session_at(session: &AuthSession, now_s: u64) -> IssuedWsTicket {
    let ticket = generate_token();
    let ticket_hash = hash_session_token(&ticket);
    let expires_at_s = now_s.saturating_add(WS_TICKET_TTL_SECS);
    let mut tickets = WS_TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    prune_ws_tickets_locked(&mut tickets, now_s);
    tickets.insert(
        ticket_hash,
        WsTicketRecord {
            session_token_hash: session.token_hash.clone(),
            expires_at_s,
        },
    );
    IssuedWsTicket {
        ticket,
        expires_in_s: WS_TICKET_TTL_SECS,
    }
}

pub fn issue_ws_ticket(
    authorization: Option<&str>,
) -> std::result::Result<IssuedWsTicket, Response<Body>> {
    if !websocket_tickets_enabled() {
        let body = serde_json::json!({
            "error": "WebSocket tickets disabled",
            "detail": "Set [api].websocket_tickets = true to enable one-time WebSocket tickets",
        });
        return Err(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
            .unwrap());
    }

    let auth_data = load_auth().ok_or_else(|| unauthorized_response("Password not configured"))?;
    let header =
        authorization.ok_or_else(|| unauthorized_response("No Authorization header provided"))?;
    let Some(token) = bearer_token_from_header(header) else {
        return Err(unauthorized_response("Bearer session required"));
    };
    let Some(session) = session_for_token(&auth_data, token) else {
        return Err(unauthorized_response("Invalid bearer token"));
    };

    Ok(issue_ws_ticket_for_session_at(&session, now_epoch_secs()))
}

fn redeem_ws_ticket_with_auth_at(ticket: &str, auth: &AuthData, now_s: u64) -> bool {
    if !websocket_tickets_enabled() {
        return false;
    }
    let ticket_hash = hash_session_token(ticket);
    let record = {
        let mut tickets = WS_TICKETS.lock().unwrap_or_else(|e| e.into_inner());
        prune_ws_tickets_locked(&mut tickets, now_s);
        tickets.remove(&ticket_hash)
    };
    let Some(record) = record else {
        return false;
    };
    if record.expires_at_s <= now_s {
        return false;
    }
    session_for_token_hash(auth, &record.session_token_hash).is_some()
}

fn redeem_ws_ticket(ticket: &str) -> bool {
    let Some(auth) = load_auth() else {
        return false;
    };
    redeem_ws_ticket_with_auth_at(ticket, &auth, now_epoch_secs())
}

/// Check if a request path is exempt from authentication.
///
/// These endpoints must be accessible without auth so the user can:
/// - Check if a password has been set (GET /api/auth/status)
/// - Set the initial password (POST /api/auth/setup)
///
/// NOTE: /metrics exemption is conditional on `metrics_require_auth` config flag.
/// When the flag is true, /metrics requires auth like any other endpoint.
/// The conditional check is done in `is_metrics_exempt()` separately.
pub fn is_auth_exempt(path: &str) -> bool {
    path == "/api/auth/status"
        || path == "/api/auth/setup"
        || path == "/api/auth/session"
        || path == "/api/system/update/metadata"
        || path == "/api/setup/status"
        || path == "/api/safety/warnings"
        // W9.5: donation pool public-info disclosure. Read-only,
        // intentionally public — pool URL + payout address + explorer
        // link. No auth required so operators can verify the firmware's
        // donation claims even before completing the setup wizard.
        || path == "/api/donation/info"
        || path == "/"
        || path.starts_with("/static/")
    // BUG FIX (2026-04-11): /ws removed from exempt list. WebSocket streams
    // full live telemetry (pool URL, temps, hashrate). Now requires auth token
    // via query param ?token= (browsers can't set Authorization on WS upgrade).
    // /metrics: conditionally exempt — see is_metrics_exempt().
}

/// Check if /metrics should be auth-exempt based on config.
/// Returns true if the path is /metrics AND metrics_require_auth is false.
pub fn is_metrics_exempt(path: &str, metrics_require_auth: bool) -> bool {
    path == "/metrics" && !metrics_require_auth
}

fn is_loopback_request(request: &Request<Body>) -> bool {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().is_loopback())
        .unwrap_or(false)
}

/// Extract the dashboard-proxy header value, if present and UTF-8.
fn dashboard_proxy_header_value(request: &Request<Body>) -> Option<String> {
    request
        .headers()
        .get(DASHBOARD_PROXY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|s| s.to_string())
}

/// Legacy bool predicate: does the request carry the static `"1"` proxy
/// header? Retained for the DEV-posture regression tests; the actual trust
/// decision now goes through `is_proxy_header_trusted_for_image()` so the
/// release image can require the per-boot nonce instead.
fn is_dashboard_proxy_request(request: &Request<Body>) -> bool {
    dashboard_proxy_header_value(request)
        .map(|value| value == DASHBOARD_PROXY_LEGACY_VALUE)
        .unwrap_or(false)
}

/// Cached per-boot dashboard-proxy nonce. `None` until first probed; the
/// `Mutex<Option<Option<String>>>` outer layer tracks "probed yet?", the inner
/// `Option<String>` is the nonce value (`None` = file absent/empty). Probed
/// once from `DASHBOARD_PROXY_NONCE_FILE` and cached so the hot auth path does
/// not stat tmpfs on every request.
static DASHBOARD_PROXY_NONCE: LazyLock<Mutex<Option<Option<String>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Read (and cache) the per-boot dashboard-proxy nonce from the production
/// path. Returns the trimmed nonce, or `None` if the file is absent/empty.
fn dashboard_proxy_nonce() -> Option<String> {
    let mut guard = DASHBOARD_PROXY_NONCE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(read_proxy_nonce_at(std::path::Path::new(
            DASHBOARD_PROXY_NONCE_FILE,
        )));
    }
    guard.clone().flatten()
}

/// Internal: read a nonce from a caller-supplied path (no caching). Used by
/// `dashboard_proxy_nonce()` in production and by the test suite against
/// scratch paths.
pub(crate) fn read_proxy_nonce_at(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Constant-time string comparison (length-independent leak avoided by mixing
/// the length difference into the accumulator). Used for the proxy-nonce
/// match so a release-image attacker cannot time-oracle the secret.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let ba = a.get(i).copied().unwrap_or(0);
        let bb = b.get(i).copied().unwrap_or(0);
        diff |= ba ^ bb;
    }
    diff == 0
}

/// CE-114: a trusted release-image proxy nonce must carry strong entropy — the
/// 64-hex (256-bit) value minted from `/dev/urandom` by S80dashboard's
/// `generate_proxy_nonce` (`od -An -tx1 -N32 /dev/urandom | tr -d ' \n'`). The
/// weak `date +%s%N`$$/uptime fallback (fired only when `/dev/urandom` + `od`
/// are both unavailable) is a variable-length decimal string that a LAN client
/// could brute; if such a low-entropy nonce ever lands in the proxy-nonce file
/// (older rootfs, misprovision, tamper), the release path must NOT trust it and
/// must fall through to bearer auth. This mirrors the init-script's own
/// `^[0-9a-f]{64}$` strong-entropy definition exactly.
fn is_strong_proxy_nonce(nonce: &str) -> bool {
    nonce.len() == 64
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Decide whether a request's dashboard-proxy header value should be trusted,
/// parameterized on the image posture + the configured per-boot nonce so the
/// test suite can prove BOTH postures deterministically.
///
/// SEC-W24-1 contract:
/// - **Release image:** trust ONLY a constant-time match against the per-boot
///   nonce, AND (CE-114) only when that nonce carries strong entropy (64-hex).
///   The forgeable static `"1"` is rejected; a low-entropy nonce is rejected. If
///   no nonce is provisioned (init script didn't run / older rootfs), NOTHING is
///   trusted via the header — the request falls through to bearer auth
///   (fail-closed).
/// - **DEV/LAB image:** byte-identical to today — accept the legacy `"1"` AND
///   (additively) the nonce if one happens to be present. Nothing dev-facing
///   breaks (no entropy requirement on dev).
fn is_proxy_header_trusted_for_image(
    header_value: Option<&str>,
    release_image: bool,
    nonce: Option<&str>,
) -> bool {
    let value = match header_value {
        Some(v) => v,
        None => return false,
    };
    let nonce_match = nonce.map(|n| constant_time_eq(value, n)).unwrap_or(false);
    if release_image {
        // PROD: the per-boot nonce is the ONLY accepted credential, and it must
        // be a strong (256-bit / 64-hex) secret — a weak nonce is never trusted
        // on a release image (CE-114 fail-closed). The strong-entropy check runs
        // on the SERVER-side provisioned nonce (constant per boot), so it adds no
        // attacker-controlled timing signal beyond the constant-time match.
        nonce_match && nonce.map(is_strong_proxy_nonce).unwrap_or(false)
    } else {
        // DEV/LAB: legacy static value still accepted (byte-identical to
        // today); nonce also accepted if present.
        nonce_match || value == DASHBOARD_PROXY_LEGACY_VALUE
    }
}

fn is_direct_loopback_request(request: &Request<Body>) -> bool {
    is_loopback_request(request) && dashboard_proxy_header_value(request).is_none()
}

/// Returns true ONLY for requests that are BOTH loopback-originated AND carry
/// the trusted-proxy header. This is the SAFE bypass condition.
///
/// SECURITY (wave 8, 2026-04-28): Replaces the previous `is_direct_loopback_request`
/// bypass. The old behavior gave any 127.0.0.1 caller (S81mcp init script, SSH port
/// forwards, compromised local services) full Hacker-mode write access without a
/// token. The new check requires explicit opt-in via `X-Dcentos-Dashboard-Proxy: 1`
/// from trusted callers (dashboard reverse proxy + init scripts that need to call
/// the daemon at boot). Lateral local processes that don't set this header now
/// fall through to standard bearer-token auth.
///
/// FOLLOW-UP: The S81mcp init script must be updated to set this header on its
/// requests. Until that happens, S81mcp will get 401s.
///
/// SECURITY (wave 24, 2026-05-22, SEC-W24-1): the header VALUE is now gated on
/// the image posture. On a release image only a constant-time match against the
/// per-boot root-only nonce (`/run/dcentos/proxy_nonce`) is trusted — the
/// forgeable static `"1"` is rejected, closing the LAN-facing bypass where any
/// host could stamp the static header and inherit full Hacker-mode write
/// access. On a DEV/LAB image the legacy `"1"` is still accepted so dev/bench
/// flows are byte-identical to today. See `is_proxy_header_trusted_for_image`.
fn is_trusted_loopback_proxy_request(request: &Request<Body>) -> bool {
    // Production path: read the live image posture + the per-boot nonce. Both
    // are cached after first probe so this stays allocation/syscall-free on the
    // hot auth path. The deterministic core lives in
    // `is_trusted_loopback_proxy_request_for_image` so the test suite can prove
    // BOTH postures of the ACTUALLY-CALLED gate (not just the pure
    // `is_proxy_header_trusted_for_image` helper) without touching `/etc`.
    is_trusted_loopback_proxy_request_for_image(
        request,
        is_release_image(),
        dashboard_proxy_nonce().as_deref(),
    )
}

/// Core of `is_trusted_loopback_proxy_request` parameterized on the image
/// posture + the configured per-boot nonce.
///
/// SEC-W24-1 / DEVOPS-010 contract: the loopback-proxy bypass is granted ONLY
/// when the request is loopback-originated AND its dashboard-proxy header value
/// is trusted for the image posture. On a release image the forgeable static
/// `DASHBOARD_PROXY_LEGACY_VALUE` ("1") is REJECTED — only a constant-time
/// match against the per-boot root-only nonce is trusted (fail-closed to bearer
/// auth when no nonce is provisioned). On a DEV/LAB image the legacy static
/// value is still accepted so dev/bench flows are byte-identical to today.
fn is_trusted_loopback_proxy_request_for_image(
    request: &Request<Body>,
    release_image: bool,
    nonce: Option<&str>,
) -> bool {
    if !is_loopback_request(request) {
        return false;
    }
    let header_value = dashboard_proxy_header_value(request);
    is_proxy_header_trusted_for_image(header_value.as_deref(), release_image, nonce)
}

/// Check if a request is safe to allow before password setup.
///
/// SECURITY (2026-04-11): Drastically reduced pre-setup surface area.
/// Previously exposed /api/config (write target), /api/status (pool URL leak),
/// /api/summary (non-existent), /api/system/info (serial/MAC leak).
///
/// On first boot (no password set), we allow ONLY:
/// - Auth-exempt paths (dashboard, setup wizard, safety warnings)
/// - A small explicit allowlist of setup mutations needed by the wizard
///
/// Everything else is blocked until password is set.
fn is_write_method(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

fn is_setup_flow_mutation(path: &str, method: &Method) -> bool {
    is_setup_flow_mutation_for_image(path, method, is_release_image())
}

/// Core of `is_setup_flow_mutation` parameterized on the image posture so the
/// test suite can prove BOTH the dev/lab (freedom-first opt-out allowed) and
/// the release (opt-out 403'd) behaviors without touching `/etc`.
///
/// RELEASE GATE (matrix §7 #1, public-image trust boundary): on a release
/// image the two passwordless opt-out paths (`/api/setup/skip-password` and
/// `/api/setup/skip-safety`) are NOT setup-flow mutations — so the pre-setup
/// middleware falls through and 403s them. A release image can therefore never
/// be driven into a passwordless state via the opt-out. Every other setup-flow
/// mutation (and the freedom-first opt-out on dev/lab images) is unchanged.
fn is_setup_flow_mutation_for_image(path: &str, method: &Method, release_image: bool) -> bool {
    if !is_write_method(method) {
        return false;
    }

    // The two freedom-first opt-outs are only setup-flow mutations on a
    // DEV/LAB image. On a release image they are rejected (fall through to the
    // pre-setup 403) so the unit cannot run passwordless / safety-skipped.
    if matches!(path, "/api/setup/skip-password" | "/api/setup/skip-safety") {
        return !release_image;
    }

    matches!(
        path,
        "/api/auth/setup"
            | "/api/setup/step1-safety"
            | "/api/setup/step2-circuit"
            | "/api/setup/step3-password"
            | "/api/setup/step4-mode"
            | "/api/setup/step5-pool"
            // P2-4 (§4.E): the wizard's economics + quiet-hours steps run before
            // the device is "ready", so they MUST be setup-flow mutations or the
            // pre-device-ready gate (auth_middleware) 409s them even with a valid
            // token. They persist only `[home]` cost/comfort settings.
            | "/api/setup/step-economics"
            | "/api/setup/quiet-hours"
            | "/api/setup/test-pool"
            | "/api/setup/complete"
    )
}

fn is_auth_management_mutation(path: &str, method: &Method) -> bool {
    *method == Method::DELETE && path == "/api/auth/session/current"
}

fn is_pre_device_ready_allowed_mutation(path: &str, method: &Method) -> bool {
    is_setup_flow_mutation(path, method) || is_auth_management_mutation(path, method)
}

/// Whether a request must be REJECTED because the authenticated session is
/// read-only but the request is a mutating one.
///
/// A `ReadOnly` monitoring session may use any GET endpoint but no mutating
/// (POST/PUT/PATCH/DELETE) endpoint — with one carve-out: it may revoke its OWN
/// session (`DELETE /api/auth/session/current`) so a monitoring token can log
/// itself out cleanly. `Admin` sessions are never blocked here.
fn read_only_role_blocks_request(role: SessionRole, path: &str, method: &Method) -> bool {
    if role.can_write() {
        return false;
    }
    if !is_write_method(method) {
        return false;
    }
    if is_read_only_mcp_call_path(path, method) {
        return false;
    }
    // Read-only sessions are still allowed to revoke themselves (log out).
    if is_auth_management_mutation(path, method) {
        return false;
    }
    true
}

fn is_read_only_mcp_call_path(path: &str, method: &Method) -> bool {
    path == "/mcp" && *method == Method::POST
}

/// Build the 403 response returned when a read-only session attempts a mutating
/// request.
fn read_only_forbidden_response() -> Response<Body> {
    let body = serde_json::json!({
        "error": "Read-only session",
        "detail": "This session has read-only (monitoring) scope and cannot perform write operations",
    });
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
        .unwrap()
}

/// Extract the raw bearer token from an effective authorization header value
/// (the `"Bearer <token>"` form the middleware assembles from either the
/// `Authorization` header or the `?token=` WebSocket query param).
fn bearer_token_from_header(header: &str) -> Option<&str> {
    header.strip_prefix("Bearer ")
}

fn is_pre_setup_safe(path: &str, method: &Method) -> bool {
    // Auth-exempt paths are always allowed
    if is_auth_exempt(path) {
        return true;
    }
    // Setup wizard mutations are the only writes allowed before password setup.
    if is_setup_flow_mutation(path, method) {
        return true;
    }
    // GET to setup wizard status
    if path == "/api/setup/status" && *method == Method::GET {
        return true;
    }
    // Allow read-only GET endpoints before password setup so the dashboard
    // works after wizard skip. Write/control/debug endpoints still blocked.
    if *method == Method::GET {
        let read_safe = path.starts_with("/api/status")
            || path.starts_with("/api/system/")
            || path.starts_with("/api/config")
            || path.starts_with("/api/stats")
            || path.starts_with("/api/history")
            || path.starts_with("/api/home/")
            || path.starts_with("/api/pools")
            || path.starts_with("/api/fleet/")
            || path.starts_with("/api/led/")
            || path.starts_with("/api/autotuner/")
            || path.starts_with("/api/offgrid/")
            || path.starts_with("/api/solar/")
            // W5.1 (2026-05-07): dashboard self-detection probe must
            // remain accessible pre-setup so the React shell can decide
            // whether to prompt a hard reload before the operator
            // completes the wizard.
            || path.starts_with("/api/dashboard/")
            // Freedom-first (full-wizard-skip contract): the Standard
            // Logs view must be reachable with the ENTIRE wizard skipped
            // (no password, no safety). This is the metadata-only,
            // read-only log-source manifest the Logs page renders — it
            // reports WHERE logs live and their access status, never log
            // content. Consistent with the read-only GET posture above;
            // it widens NO write/control/debug surface (`/api/debug/*`
            // and `/ws` telemetry stay gated — that's the correct
            // posture and is not what the Logs view needs relaxed).
            || path == "/api/diagnostics/logs/manifest";
        if read_safe {
            return true;
        }
    }
    false
}

fn is_pre_setup_mutation(path: &str, method: &Method) -> bool {
    is_setup_flow_mutation(path, method)
}

/// Pure decision: does an explicit password opt-out grant pre-password write
/// access on this image?
///
/// BUG-7/8 FIX (2026-06-05): the freedom-first password opt-out
/// (`POST /api/setup/skip-password`) is an explicit operator choice to run with
/// NO owner password — a default-credential / no-auth control posture that is
/// INTENTIONAL on dev/home images (memory rule
/// ). On such an image the
/// opt-out must GRANT write/control access, otherwise the operator who declined
/// a password is locked out of EVERY mutation (restart, mining on/off, pools)
/// and the wizard's final reboot that engages mining is 403'd → mining never
/// starts.
///
/// Three conditions, ALL required:
/// - `opt_out_active`: the operator EXPLICITLY opted out (persisted flag), not
///   merely "no password yet" (a fresh unit pre-decision still requires setup).
/// - `device_ready`: first-boot setup is complete. The opt-out is recorded
///   mid-wizard, so granting write before completion would widen the surface
///   prematurely; the bug's failing call (the wizard reboot) fires only AFTER
///   `POST /api/setup/complete`. This mirrors the post-auth device-ready gate
///   that the password path is subject to.
/// - `!is_release`: a RELEASE image (`DCENT_RELEASE_IMAGE` →
///   `/etc/dcentos/release-image` marker) DISABLES the opt-out end-to-end —
///   `is_setup_flow_mutation_for_image` 403's `POST /api/setup/skip-password`
///   so the flag can never be recorded, and this predicate additionally fails
///   closed even if a stale flag somehow existed, so a release image ALWAYS
///   requires a real password (preserving the
///    posture).
///
/// Pure and parameterized so the test suite can prove every posture without
/// touching `/etc` or `/data`.
fn opt_out_grants_write_for_image(
    opt_out_active: bool,
    device_ready: bool,
    is_release: bool,
) -> bool {
    opt_out_active && device_ready && !is_release
}

/// Live wrapper: read the persisted opt-out flag, the onboarding device-ready
/// state, and the current image posture, then apply
/// `opt_out_grants_write_for_image`. Consulted only on the rare
/// passwordless-write path in `auth_middleware`.
fn opt_out_grants_write() -> bool {
    opt_out_grants_write_for_image(
        crate::rest::onboarding_password_opt_out_active(),
        crate::rest::onboarding_device_ready(),
        is_release_image(),
    )
}

fn header_origin_host(value: &str) -> &str {
    value
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("")
}

fn is_allowed_dashboard_origin(origin_host: &str, host: &str) -> bool {
    origin_host == host
}

fn is_same_origin_setup_request(request: &Request<Body>) -> bool {
    let host = match request.headers().get("host").and_then(|v| v.to_str().ok()) {
        Some(host) => host,
        None => return false,
    };

    if let Some(origin) = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
    {
        return is_allowed_dashboard_origin(header_origin_host(origin), host);
    }

    if let Some(referer) = request
        .headers()
        .get("referer")
        .and_then(|v| v.to_str().ok())
    {
        return is_allowed_dashboard_origin(header_origin_host(referer), host);
    }

    if let Some(fetch_site) = request
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
    {
        return fetch_site == "same-origin";
    }

    false
}

/// Validate a request's authentication.
///
/// Protected endpoints require a revocable bearer session. Password login is
/// handled only by `POST /api/auth/session`.
pub fn check_auth(authorization: Option<&str>) -> std::result::Result<(), Response<Body>> {
    let auth_data = match load_auth() {
        Some(data) => data,
        None => {
            // No password set yet — block all non-exempt requests
            let body = serde_json::json!({
                "error": "Password not configured",
                "detail": "Set a password via POST /api/auth/setup before using the API",
            });
            return Err(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
                .unwrap());
        }
    };

    let header = match authorization {
        Some(h) => h,
        None => {
            return Err(unauthorized_response("No Authorization header provided"));
        }
    };

    // Check Bearer token
    if let Some(token) = header.strip_prefix("Bearer ") {
        if session_for_token(&auth_data, token).is_some() {
            return Ok(());
        }
        return Err(unauthorized_response("Invalid bearer token"));
    }

    if header.starts_with("Basic ") {
        let body = serde_json::json!({
            "error": "Bearer session required",
            "detail": "Use POST /api/auth/session to exchange the owner password for a session token",
        });
        return Err(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
            .unwrap());
    }

    Err(unauthorized_response("Unsupported authorization scheme"))
}

pub fn current_session_id(
    authorization: Option<&str>,
) -> std::result::Result<String, Response<Body>> {
    let auth_data = load_auth().ok_or_else(|| unauthorized_response("Password not configured"))?;
    let header =
        authorization.ok_or_else(|| unauthorized_response("No Authorization header provided"))?;

    if let Some(token) = header.strip_prefix("Bearer ") {
        if let Some(session) = session_for_token(&auth_data, token) {
            return Ok(session.id);
        }
        return Err(unauthorized_response("Invalid bearer token"));
    }

    let body = serde_json::json!({
        "error": "Bearer session required",
        "detail": "This endpoint requires a revocable bearer session, not Basic auth",
    });
    Err(Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
        .unwrap())
}

/// Extract the WebSocket `?token=` bearer-token fallback from a query string.
///
/// SEC-2 (2026-06-20): a bearer token carried in a `?token=` query parameter can
/// be captured by intermediary proxy/access logs that record the request line.
/// The `Authorization` header is the PRIMARY transport (see
/// `resolve_effective_auth`); this query-param path exists ONLY because browsers
/// cannot set the `Authorization` header on a WebSocket upgrade request. The
/// residual exposure is limited to external proxies the operator places in front
/// of the dashboard — dcentrald itself NEVER logs the request URI or query
/// string, and any future request/access-logging path MUST route the URI through
/// [`redact_ws_token`] so the token can never reach a dcentrald log line.
fn ws_query_token(query: Option<&str>) -> Option<String> {
    query.and_then(|q| {
        q.split('&')
            .find_map(|pair| pair.strip_prefix("token=").map(|v| v.to_string()))
    })
}

fn ws_query_ticket(query: Option<&str>) -> Option<String> {
    query.and_then(|q| {
        q.split('&')
            .find_map(|pair| pair.strip_prefix("ticket=").map(|v| v.to_string()))
    })
}

/// Resolve the effective `Authorization` header value used for auth.
///
/// SEC-2: the `Authorization` header is the PRIMARY credential transport. The
/// WebSocket `?token=` query fallback (already wrapped as a `Bearer` value) is
/// used ONLY when no `Authorization` header is present. When both are supplied,
/// the header wins and the query token is ignored.
fn resolve_effective_auth(
    auth_header: Option<&str>,
    query_token: Option<String>,
) -> Option<String> {
    auth_header
        .map(|h| h.to_string())
        .or_else(|| query_token.map(|t| format!("Bearer {}", t)))
}

/// Redact the WebSocket `?token=` value from a URI or query string before it is
/// emitted to ANY log sink.
///
/// SEC-2: dcentrald must never write a bearer token to its own logs. There is no
/// request-URI logging path in dcentrald today (the auth middleware and the WS
/// handler both log only the path/outcome, never the raw URI), so this helper is
/// a forward-looking contract: any future request/access logging MUST pass the
/// URI through `redact_ws_token` first. The companion regression test asserts the
/// raw token never survives redaction.
pub fn redact_ws_token(uri_or_query: &str) -> String {
    let (prefix, query) = match uri_or_query.split_once('?') {
        Some((p, q)) => (Some(p), q),
        None => (None, uri_or_query),
    };
    let mut out = String::with_capacity(uri_or_query.len());
    if let Some(p) = prefix {
        out.push_str(p);
        out.push('?');
    }
    for (idx, pair) in query.split('&').enumerate() {
        if idx > 0 {
            out.push('&');
        }
        if pair.strip_prefix("token=").is_some() {
            out.push_str("token=REDACTED");
        } else if pair.strip_prefix("ticket=").is_some() {
            out.push_str("ticket=REDACTED");
        } else {
            out.push_str(pair);
        }
    }
    out
}

/// Axum middleware layer that enforces authentication on non-exempt routes.
///
/// Inserted into the axum router via `.layer(middleware::from_fn(...))`.
/// Runs AFTER CORS preflight handling so OPTIONS requests pass through.
pub async fn auth_middleware(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();

    if !is_password_set()
        && is_pre_setup_mutation(&path, request.method())
        && !is_same_origin_setup_request(&request)
    {
        let body = serde_json::json!({
            "error": "Dashboard-origin required",
            "detail": "First-boot setup mutations must come from the miner's own dashboard origin",
        });
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
            .unwrap();
    }

    // Skip auth for exempt paths (dashboard, setup, safety, static assets)
    if is_auth_exempt(&path) {
        return next.run(request).await;
    }

    // SECURITY (wave 8, 2026-04-28): Only bypass auth on loopback when the request
    // carries the trusted-proxy header. Bare 127.0.0.1 callers fall through to
    // standard bearer-token auth. See is_trusted_loopback_proxy_request() docs.
    if is_trusted_loopback_proxy_request(&request) {
        return next.run(request).await;
    }

    // /metrics: conditionally exempt based on metrics_require_auth config
    if is_metrics_exempt(&path, METRICS_REQUIRE_AUTH.load(Ordering::Relaxed)) {
        return next.run(request).await;
    }

    // BUG FIX (2026-04-11): Before password setup, only allow read-only paths.
    // Previously ALL routes passed through — any LAN client could hit write/debug
    // endpoints on first boot. Now only exempt paths + safe read-only GET endpoints
    // are allowed. Write/control/debug endpoints require password setup first.
    if !is_password_set() {
        if is_pre_setup_safe(&path, request.method()) {
            return next.run(request).await;
        }
        // BUG-7/8 FIX (2026-06-05): an EXPLICIT password opt-out is an operator
        // choice to run with no owner password (default-credential / no-auth
        // control posture, INTENTIONAL on dev/home images). On such an image,
        // once first-boot setup is complete the opt-out GRANTS write/control
        // access — without this the operator who declined a password is locked
        // out of every mutation (restart, mining on/off, pools) and the wizard's
        // final reboot that engages mining is 403'd, so mining never starts.
        // `opt_out_grants_write` requires opt-out + device-ready + non-release,
        // so it never widens the surface before setup completes, and the
        // dashboard-origin CSRF guard above (`is_pre_setup_mutation` /
        // `is_same_origin_setup_request`) still protects setup-flow mutations. A
        // RELEASE image never reaches here with the opt-out set (the
        // `skip-password` route is 403'd there) and `opt_out_grants_write`
        // additionally fails closed for release, so a real release image still
        // requires a password.
        if opt_out_grants_write() {
            return next.run(request).await;
        }
        let body = serde_json::json!({
            "error": "Password setup required",
            "detail": "Set a password via the dashboard or POST /api/auth/setup before using write endpoints",
        });
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
            .unwrap();
    }

    // Extract Authorization header value (clone to release borrow on request)
    let auth_header: Option<String> = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // SEC-2 (2026-06-20): the `Authorization` header is the PRIMARY bearer-token
    // transport for every surface, including the WebSocket. The `?token=` query
    // param is a DOCUMENTED FALLBACK that exists ONLY because browsers cannot set
    // the `Authorization` header on a WebSocket upgrade request (BUG FIX
    // 2026-04-11). It is scoped to `/ws` and is never consulted for any other
    // route. See `ws_query_token` / `resolve_effective_auth` for the precedence
    // and `redact_ws_token` for the logging-scrub contract.
    let query_token: Option<String> = if path == "/ws" {
        ws_query_token(request.uri().query())
    } else {
        None
    };

    if path == "/ws" && auth_header.is_none() && query_token.is_none() {
        if let Some(ticket) = ws_query_ticket(request.uri().query()) {
            if redeem_ws_ticket(&ticket) {
                return next.run(request).await;
            }
            return unauthorized_response("Invalid or expired WebSocket ticket");
        }
    }

    // Header is primary; the WebSocket query-param token is only the fallback.
    let effective_auth = resolve_effective_auth(auth_header.as_deref(), query_token);

    match check_auth(effective_auth.as_deref()) {
        Ok(()) => {
            // READ-ONLY ROLE GATE (GROUP-C MED): a monitoring session may read
            // but not mutate. Only consult the session role on write methods so
            // the hot read path never does the extra lookup. The token is the
            // same `effective_auth` that `check_auth` just validated above.
            if is_write_method(request.method()) {
                if let Some(role) = effective_auth
                    .as_deref()
                    .and_then(bearer_token_from_header)
                    .and_then(role_for_token)
                {
                    if read_only_role_blocks_request(role, &path, request.method()) {
                        return read_only_forbidden_response();
                    }
                }
            }

            if is_write_method(request.method())
                && !is_pre_device_ready_allowed_mutation(&path, request.method())
                && !crate::rest::onboarding_device_ready()
            {
                let body = serde_json::json!({
                    "error": "Setup incomplete",
                    "detail": "Finish first-boot setup before using operational write endpoints",
                });
                return Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
                    .unwrap();
            }

            next.run(request).await
        }
        Err(response) => response,
    }
}

/// Simple base64 decode (no external dependency).
fn base64_decode(input: &str) -> std::result::Result<String, ()> {
    // Minimal base64 decode for Basic auth
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let input = input.trim_end_matches('=');
    let mut output = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;

    for &byte in input.as_bytes() {
        let val = match TABLE.iter().position(|&b| b == byte) {
            Some(v) => v as u32,
            None => return Err(()),
        };
        buffer = (buffer << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    String::from_utf8(output).map_err(|_| ())
}

/// Hash a password for storage using argon2id.
///
/// Uses argon2id with a random salt — the current best practice for
/// password hashing. The output is a PHC-format string that embeds
/// algorithm, salt, and hash parameters for self-describing verification.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .expect("failed to hash password")
        .to_string()
}

/// Legacy SHA-256 password hash (kept for backward compatibility during migration).
///
/// Existing auth.json files may contain `sha256:...` hashes created before
/// the argon2id upgrade. This function reproduces the old hash for verification.
fn legacy_hash_password(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let salt = format!("dcent-{}-salt", password.len());
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    format!("sha256:{:x}", result)
}

/// Verify a password against a stored hash.
///
/// Supports both the new argon2id format (`$argon2id$...`) and the legacy
/// SHA-256 format (`sha256:...`) for seamless migration. Users with legacy
/// hashes will be verified against the old algorithm; a future migration
/// pass can re-hash them on successful login.
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    // Legacy SHA-256 check for migration
    if stored_hash.starts_with("sha256:") || !stored_hash.starts_with("$argon2") {
        let legacy = legacy_hash_password(password);
        return legacy == stored_hash;
    }
    // Argon2id verification
    let parsed = match PasswordHash::new(stored_hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Generate a cryptographically random API token (64 hex characters).
///
/// Uses the OS CSPRNG via `getrandom` — no timestamp/PID mixing needed.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("failed to generate random bytes");
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

fn unauthorized_response(detail: &str) -> Response<Body> {
    let body = serde_json::json!({
        "error": "Unauthorized",
        "detail": detail,
    });
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .header(
            "WWW-Authenticate",
            "Bearer realm=\"dcentrald\", Basic realm=\"dcentrald\"",
        )
        .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::{
        check_login_rate_limit_at, constant_time_eq, hash_session_token,
        is_dashboard_proxy_request, is_direct_loopback_request, is_password_set_at,
        is_pre_setup_safe, is_proxy_header_trusted_for_image, is_release_image_at,
        is_setup_flow_mutation, is_setup_flow_mutation_for_image, is_strong_proxy_nonce,
        is_trusted_loopback_proxy_request, is_trusted_loopback_proxy_request_for_image,
        is_write_method, load_auth_at, opt_out_grants_write_for_image,
        read_only_role_blocks_request, read_proxy_nonce_at, record_login_failure_at,
        record_login_success, redact_ws_token, resolve_effective_auth, save_auth_at,
        session_idle_ok_and_touch_at, session_idle_timeout_secs, set_session_idle_timeout_secs,
        ws_query_token, AuthData, AuthSession, DASHBOARD_PROXY_LEGACY_VALUE,
        LOGIN_RATE_LIMIT_SOFT_MAX,
    };
    use super::{SessionRole, LOGIN_RATE_LIMIT_WINDOW_SECS};
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    // Serializes the tests that mutate the process-global SESSION_IDLE_TIMEOUT_SECS
    // atomic (idle_session_expires / idle_activity / idle_timeout_zero) so they do
    // not race each other under parallel in-process test execution. Poison-tolerant.
    static IDLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn raw_write_file(path: &Path, bytes: &[u8]) {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .expect("open test file for write");
        file.write_all(bytes).expect("write test file");
    }

    fn scratch_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "dcentrald-auth-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn setup_flow_uses_exact_write_allowlist() {
        assert!(is_setup_flow_mutation("/api/auth/setup", &Method::POST));
        assert!(is_setup_flow_mutation(
            "/api/setup/step1-safety",
            &Method::POST
        ));
        assert!(is_setup_flow_mutation(
            "/api/setup/test-pool",
            &Method::POST
        ));
        assert!(is_setup_flow_mutation("/api/setup/complete", &Method::POST));
        // P2-4 (§4.E): the economics + quiet-hours setup steps must be setup-flow
        // mutations so they persist during the wizard (pre-device-ready).
        assert!(is_setup_flow_mutation(
            "/api/setup/step-economics",
            &Method::POST
        ));
        assert!(is_setup_flow_mutation(
            "/api/setup/quiet-hours",
            &Method::POST
        ));
        // Freedom-first: the explicit password opt-out must be a setup-flow
        // mutation so it works pre-auth (before any password exists) —
        // otherwise the pre-auth middleware would 403 the skip and force a
        // password anyway.
        assert!(is_setup_flow_mutation(
            "/api/setup/skip-password",
            &Method::POST
        ));
        // Freedom-first (exact parallel): the explicit circuit/safety
        // opt-out must ALSO be a setup-flow mutation so it works pre-auth
        // — otherwise the full wizard couldn't be skipped on a fresh
        // passwordless unit (the pre-auth middleware would 403 it).
        assert!(is_setup_flow_mutation(
            "/api/setup/skip-safety",
            &Method::POST
        ));
        assert!(!is_setup_flow_mutation("/api/setup/unknown", &Method::POST));
        assert!(!is_setup_flow_mutation("/api/config", &Method::POST));
        assert!(!is_setup_flow_mutation(
            "/api/setup/step2-circuit",
            &Method::GET
        ));
        // The opt-out is a mutation, not a GET.
        assert!(!is_setup_flow_mutation(
            "/api/setup/skip-password",
            &Method::GET
        ));
        // The safety opt-out is also a mutation, not a GET.
        assert!(!is_setup_flow_mutation(
            "/api/setup/skip-safety",
            &Method::GET
        ));
    }

    #[test]
    fn release_gate_disables_passwordless_optouts() {
        // RELEASE IMAGE (matrix §7 #1): the two freedom-first opt-outs are NOT
        // setup-flow mutations on a release image, so the pre-setup gate 403s
        // them — a release unit can never be driven passwordless / safety-
        // skipped. Every other setup-flow mutation is unaffected.
        assert!(
            !is_setup_flow_mutation_for_image("/api/setup/skip-password", &Method::POST, true),
            "release image must reject the passwordless opt-out"
        );
        assert!(
            !is_setup_flow_mutation_for_image("/api/setup/skip-safety", &Method::POST, true),
            "release image must reject the safety opt-out"
        );
        // The rest of the setup wizard still works on a release image (the
        // operator MUST be able to set a password and finish onboarding).
        assert!(is_setup_flow_mutation_for_image(
            "/api/auth/setup",
            &Method::POST,
            true
        ));
        assert!(is_setup_flow_mutation_for_image(
            "/api/setup/step3-password",
            &Method::POST,
            true
        ));
        assert!(is_setup_flow_mutation_for_image(
            "/api/setup/complete",
            &Method::POST,
            true
        ));
    }

    #[test]
    fn dev_lab_keeps_freedom_first_optouts() {
        // DEV/LAB IMAGE (no marker): byte-identical to today — both opt-outs
        // remain setup-flow mutations so a passwordless unit can opt out.
        assert!(is_setup_flow_mutation_for_image(
            "/api/setup/skip-password",
            &Method::POST,
            false
        ));
        assert!(is_setup_flow_mutation_for_image(
            "/api/setup/skip-safety",
            &Method::POST,
            false
        ));
        // The opt-outs are still only mutations, never GETs, on either image.
        assert!(!is_setup_flow_mutation_for_image(
            "/api/setup/skip-password",
            &Method::GET,
            false
        ));
        assert!(!is_setup_flow_mutation_for_image(
            "/api/setup/skip-safety",
            &Method::GET,
            true
        ));
    }

    #[test]
    fn opt_out_grants_write_on_dev_image_after_setup() {
        // BUG-7/8 (the .100/.138 live install): the operator OPTED OUT of a
        // password and FINISHED setup on a dev/home image. The opt-out is an
        // explicit "no owner password / no-auth control" choice, so it MUST
        // grant write/control access — otherwise the wizard's final reboot that
        // engages mining is 403'd and mining never starts.
        //  args: (opt_out_active, device_ready, is_release)
        assert!(
            opt_out_grants_write_for_image(true, true, false),
            "dev image + explicit opt-out + setup complete must GRANT write access"
        );
    }

    #[test]
    fn opt_out_does_not_grant_write_before_setup_complete() {
        // The opt-out is recorded MID-wizard. Until first-boot setup is
        // complete (device_ready) it must NOT widen the write surface — only
        // setup-flow mutations + read-only GETs are allowed pre-completion (via
        // is_pre_setup_safe), exactly like the password path's post-auth
        // device-ready gate.
        assert!(
            !opt_out_grants_write_for_image(true, false, false),
            "opt-out must not grant write access before setup completes"
        );
    }

    #[test]
    fn no_opt_out_never_grants_write() {
        // A fresh passwordless unit that made NO password decision (opt-out
        // flag false) stays locked to read-only — "no password yet" is not the
        // same as "I accept no-auth control". Regression pin for the original
        // first-boot lockdown (2026-04-11).
        assert!(
            !opt_out_grants_write_for_image(false, true, false),
            "absence of an explicit opt-out must keep the write surface closed"
        );
    }

    #[test]
    fn release_image_never_grants_write_via_opt_out() {
        // RELEASE IMAGE: a real release image ALWAYS requires a password. The
        // skip-password route is 403'd on release so the flag can never be set,
        // but even a (hypothetical) stale opt_out flag must FAIL CLOSED here —
        // preserving the placeholder-pubkey / signed-trust release posture
        //.
        assert!(
            !opt_out_grants_write_for_image(true, true, true),
            "release image must NEVER grant write access via the password opt-out"
        );
        // And of course a release image with no opt-out / no device-ready is
        // also closed.
        assert!(!opt_out_grants_write_for_image(false, false, true));
        assert!(!opt_out_grants_write_for_image(true, false, true));
    }

    #[test]
    fn release_gate_does_not_widen_other_surfaces() {
        // The release gate ONLY affects the two opt-outs. Non-setup writes are
        // STILL blocked on a release image, and unknown setup paths stay
        // rejected — the gate adds a stricter posture, it never relaxes one.
        assert!(!is_setup_flow_mutation_for_image(
            "/api/config",
            &Method::POST,
            true
        ));
        assert!(!is_setup_flow_mutation_for_image(
            "/api/action/restart",
            &Method::POST,
            true
        ));
        assert!(!is_setup_flow_mutation_for_image(
            "/api/setup/unknown",
            &Method::POST,
            true
        ));
    }

    /// Marker-file probe contract: a present marker => release image, an absent
    /// marker => dev/lab image. Both branches are exercised against scratch
    /// paths (NOT the production `/etc/dcentos/release-image`) so the probe's
    /// process-wide cache is never poisoned by the test order.
    #[test]
    fn release_image_marker_probe_reflects_file_presence() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let mut present = std::env::temp_dir();
        present.push(format!(
            "dcentrald-release-marker-present-{}-{}",
            pid, nanos
        ));
        let mut absent = std::env::temp_dir();
        absent.push(format!("dcentrald-release-marker-absent-{}-{}", pid, nanos));

        // Marker present => release image.
        raw_write_file(&present, b"1\n");
        assert!(
            is_release_image_at(&present),
            "present marker must read as a release image"
        );
        // Marker absent => dev/lab image (freedom-first preserved).
        let _ = std::fs::remove_file(&absent);
        assert!(
            !is_release_image_at(&absent),
            "absent marker must read as a dev/lab image"
        );

        let _ = std::fs::remove_file(&present);
    }

    #[test]
    fn save_auth_survives_simulated_partial_write() {
        let root = scratch_dir("partial-save");
        let data_dir = root.join("data").join("dcent");
        let auth_path = data_dir.join("auth.json");
        let release_marker = root.join("release-image");

        raw_write_file(&auth_path, br#"{"version""#);
        let auth = AuthData {
            version: 2,
            password_hash: "sha256:valid".to_string(),
            api_token: None,
            sessions: Vec::new(),
        };

        save_auth_at(&auth_path, &auth).expect("save_auth_at must replace a partial file");

        let loaded = load_auth_at(&auth_path, &release_marker).expect("saved auth must parse");
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.password_hash, "sha256:valid");
        assert!(
            !data_dir
                .read_dir()
                .expect("read auth dir")
                .filter_map(|entry| entry.ok())
                .any(|entry| entry
                    .file_name()
                    .to_str()
                    .map(|name| name.starts_with("auth.json.corrupt."))
                    .unwrap_or(false)),
            "a successful save should not quarantine the replaced partial file"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_auth_on_release_image_does_not_reopen_setup() {
        let root = scratch_dir("release-corrupt");
        let auth_path = root.join("data").join("dcent").join("auth.json");
        let release_marker = root.join("etc").join("dcentos").join("release-image");
        raw_write_file(&release_marker, b"1\n");
        raw_write_file(&auth_path, br#"{"version":2,"password_hash":"#);

        let loaded = load_auth_at(&auth_path, &release_marker)
            .expect("release corrupt auth must synthesize a configured sentinel");

        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.sessions.len(), 0, "corrupt auth revokes sessions");
        assert!(
            !super::verify_password("anything", &loaded.password_hash),
            "corrupt sentinel must never accept a password"
        );
        assert!(
            !auth_path.exists(),
            "corrupt auth.json must be moved out of the live path"
        );
        assert!(
            is_password_set_at(&auth_path, &release_marker),
            "release corrupt auth must still be treated as configured"
        );
        assert!(
            load_auth_at(&auth_path, &release_marker).is_some(),
            "release corrupt quarantine must keep returning configured/sessionless state"
        );

        let quarantined = auth_path
            .parent()
            .expect("auth parent")
            .read_dir()
            .expect("read auth dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| name.starts_with("auth.json.corrupt."))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(quarantined, 1, "corrupt auth file must be preserved");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_auth_on_dev_image_keeps_current_setup_behavior() {
        let root = scratch_dir("dev-corrupt");
        let auth_path = root.join("data").join("dcent").join("auth.json");
        let release_marker = root.join("missing-release-image");
        raw_write_file(&auth_path, br#"{"version":2,"password_hash":"#);

        assert!(
            load_auth_at(&auth_path, &release_marker).is_none(),
            "dev corrupt auth keeps the historical unconfigured behavior"
        );
        assert!(
            !is_password_set_at(&auth_path, &release_marker),
            "dev corrupt auth must not pin setup closed"
        );
        assert!(
            !auth_path.exists(),
            "dev corrupt auth is still quarantined for diagnosis"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_password_is_pre_setup_safe() {
        // The opt-out write must be allowed by the pre-setup gate (it's the
        // freedom-first half — a passwordless unit must be able to opt out).
        assert!(is_pre_setup_safe("/api/setup/skip-password", &Method::POST));
        // Non-setup writes are STILL blocked pre-password — the opt-out
        // does not widen write access.
        assert!(!is_pre_setup_safe("/api/config", &Method::POST));
        assert!(!is_pre_setup_safe("/api/action/restart", &Method::POST));
    }

    #[test]
    fn skip_safety_is_pre_setup_safe() {
        // The exact parallel: the circuit/safety opt-out write must be
        // allowed by the pre-setup gate so the full wizard is skippable
        // on a fresh passwordless unit.
        assert!(is_pre_setup_safe("/api/setup/skip-safety", &Method::POST));
        // SECURITY POSTURE UNCHANGED (regression pin): the safety opt-out
        // widens NO write access — non-setup writes are STILL blocked
        // before a password exists, identical to W1-A's password opt-out.
        assert!(!is_pre_setup_safe("/api/config", &Method::POST));
        assert!(!is_pre_setup_safe("/api/action/restart", &Method::POST));
        assert!(!is_pre_setup_safe("/api/debug/registers", &Method::POST));
    }

    #[test]
    fn pre_setup_safe_paths_block_non_setup_writes() {
        assert!(is_pre_setup_safe("/api/setup/status", &Method::GET));
        assert!(is_pre_setup_safe("/api/setup/test-pool", &Method::POST));
        assert!(!is_pre_setup_safe("/api/config", &Method::POST));
        assert!(!is_pre_setup_safe("/api/setup/unknown", &Method::POST));
    }

    /// SEC-2 (2026-06-20): the `Authorization` header is the PRIMARY bearer-token
    /// transport; the WebSocket `?token=` query param is only a documented
    /// fallback. This pins the precedence: when a header is present it wins and
    /// the query token is ignored; the query token is only used when no header is
    /// supplied.
    #[test]
    fn auth_header_takes_precedence_over_ws_query_token() {
        // Header present, query token also present -> header wins, query ignored.
        assert_eq!(
            resolve_effective_auth(
                Some("Bearer header-tok"),
                ws_query_token(Some("token=query-tok&foo=bar"))
            ),
            Some("Bearer header-tok".to_string()),
        );

        // No header -> WS query token is the fallback, wrapped as Bearer.
        assert_eq!(
            resolve_effective_auth(None, ws_query_token(Some("token=query-tok"))),
            Some("Bearer query-tok".to_string()),
        );

        // Neither -> no credential.
        assert_eq!(resolve_effective_auth(None, ws_query_token(None)), None);
        assert_eq!(
            resolve_effective_auth(None, ws_query_token(Some("foo=bar"))),
            None
        );

        // Token may appear after other params.
        assert_eq!(
            ws_query_token(Some("foo=bar&token=abc123")),
            Some("abc123".to_string())
        );
        assert_eq!(
            super::ws_query_ticket(Some("foo=bar&ticket=ticket123")),
            Some("ticket123".to_string())
        );
    }

    /// SEC-2 (2026-06-20): dcentrald must never emit a bearer token to its own
    /// logs. `redact_ws_token` is the forward-looking scrub contract for any
    /// future request/access-logging path. Assert the raw token never survives
    /// redaction, while the rest of the URI is preserved.
    #[test]
    fn redact_ws_token_scrubs_bearer_token_from_uri() {
        let secret = "s3cr3t-bearer-token-value";

        // Full WS upgrade URI with the token plus other params.
        let uri = format!("/ws?token={secret}&since=42");
        let redacted = redact_ws_token(&uri);
        assert!(
            !redacted.contains(secret),
            "raw token must not survive redaction: {redacted}"
        );
        assert_eq!(redacted, "/ws?token=REDACTED&since=42");

        // Bare query string (no path) is also scrubbed.
        let redacted_bare = redact_ws_token(&format!("token={secret}"));
        assert!(!redacted_bare.contains(secret));
        assert_eq!(redacted_bare, "token=REDACTED");

        // Token last in the param list.
        let redacted_last = redact_ws_token(&format!("/ws?foo=bar&token={secret}"));
        assert!(!redacted_last.contains(secret));
        assert_eq!(redacted_last, "/ws?foo=bar&token=REDACTED");

        // One-time WS tickets are credentials too and must be scrubbed the same
        // way as bearer query tokens.
        let redacted_ticket = redact_ws_token(&format!("/ws?ticket={secret}&foo=bar"));
        assert!(!redacted_ticket.contains(secret));
        assert_eq!(redacted_ticket, "/ws?ticket=REDACTED&foo=bar");

        // A URI with no token is passed through unchanged.
        assert_eq!(redact_ws_token("/api/status"), "/api/status");
        assert_eq!(redact_ws_token("/ws?since=42"), "/ws?since=42");
    }

    #[test]
    fn write_method_detects_mutations() {
        assert!(is_write_method(&Method::POST));
        assert!(is_write_method(&Method::PUT));
        assert!(is_write_method(&Method::PATCH));
        assert!(is_write_method(&Method::DELETE));
        assert!(!is_write_method(&Method::GET));
    }

    #[test]
    fn dashboard_proxy_marker_is_explicit() {
        let marked = Request::builder()
            .uri("/api/status")
            .header("X-Dcentos-Dashboard-Proxy", "1")
            .body(Body::empty())
            .unwrap();
        let unmarked = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let other_value = Request::builder()
            .uri("/api/status")
            .header("X-Dcentos-Dashboard-Proxy", "true")
            .body(Body::empty())
            .unwrap();

        assert!(is_dashboard_proxy_request(&marked));
        assert!(!is_dashboard_proxy_request(&unmarked));
        assert!(!is_dashboard_proxy_request(&other_value));
    }

    #[test]
    fn marked_proxy_loopback_is_not_direct_loopback() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 8080);

        let mut direct = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        direct.extensions_mut().insert(ConnectInfo(loopback));

        let mut proxied = Request::builder()
            .uri("/api/status")
            .header("X-Dcentos-Dashboard-Proxy", "1")
            .body(Body::empty())
            .unwrap();
        proxied.extensions_mut().insert(ConnectInfo(loopback));

        let mut non_loopback = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        non_loopback.extensions_mut().insert(ConnectInfo(remote));

        assert!(is_direct_loopback_request(&direct));
        assert!(!is_direct_loopback_request(&proxied));
        assert!(!is_direct_loopback_request(&non_loopback));
    }

    /// SECURITY (wave 8, 2026-04-28): Regression test for the loopback auth-bypass fix.
    ///
    /// Previously the middleware bypassed auth for ANY 127.0.0.1 caller — meaning
    /// the S81mcp init script, SSH local port forwards, and any compromised local
    /// service got full Hacker-mode write access without a token.
    ///
    /// The bypass condition is now `loopback AND X-Dcentos-Dashboard-Proxy: 1`. A
    /// loopback request without the trusted-proxy header MUST NOT be treated as
    /// trusted — it should fall through to the bearer-token check (which yields
    /// 401 without a session).
    #[test]
    fn loopback_without_proxy_header_requires_token() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

        // Bare loopback, no Authorization header, no proxy header — must NOT bypass.
        let mut bare_loopback = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .body(Body::empty())
            .unwrap();
        bare_loopback.extensions_mut().insert(ConnectInfo(loopback));
        assert!(
            !is_trusted_loopback_proxy_request(&bare_loopback),
            "bare 127.0.0.1 caller without proxy header must not bypass auth"
        );

        // Loopback WITH proxy header — bypass is allowed.
        let mut proxied_loopback = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", "1")
            .body(Body::empty())
            .unwrap();
        proxied_loopback
            .extensions_mut()
            .insert(ConnectInfo(loopback));
        assert!(
            is_trusted_loopback_proxy_request(&proxied_loopback),
            "loopback + trusted-proxy header should bypass auth"
        );

        // Non-loopback WITH proxy header — must NOT bypass (untrusted source spoofing).
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 8080);
        let mut spoofed = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", "1")
            .body(Body::empty())
            .unwrap();
        spoofed.extensions_mut().insert(ConnectInfo(remote));
        assert!(
            !is_trusted_loopback_proxy_request(&spoofed),
            "remote caller with spoofed proxy header must not bypass auth"
        );
    }

    /// SEC-W24-1 (2026-05-22): the dev/prod gate on the dashboard-proxy header.
    ///
    /// DEV/LAB image (no release marker): the legacy static "1" is STILL trusted
    /// (byte-identical to today's behaviour), and the per-boot nonce is also
    /// accepted if one is present. Nothing dev-facing breaks.
    #[test]
    fn proxy_header_dev_image_accepts_legacy_static_value() {
        // No nonce provisioned, dev image → legacy "1" trusted (today's behaviour).
        assert!(is_proxy_header_trusted_for_image(Some("1"), false, None));
        // Dev image with a nonce present: BOTH the legacy value and the nonce
        // are accepted (additive, never a regression).
        assert!(is_proxy_header_trusted_for_image(
            Some("1"),
            false,
            Some("secretnonce")
        ));
        assert!(is_proxy_header_trusted_for_image(
            Some("secretnonce"),
            false,
            Some("secretnonce")
        ));
        // A wrong value with no nonce is rejected even on dev (it isn't "1").
        assert!(!is_proxy_header_trusted_for_image(Some("2"), false, None));
        // Missing header is never trusted.
        assert!(!is_proxy_header_trusted_for_image(
            None,
            false,
            Some("secretnonce")
        ));
    }

    /// RELEASE image (marker present): the forgeable static "1" is REJECTED.
    /// Only a constant-time match against the per-boot nonce is trusted; if no
    /// nonce was provisioned, the header trusts NOTHING (fail-closed → bearer
    /// auth). This closes the LAN-facing bypass (SEC-W24-1).
    #[test]
    fn proxy_header_release_image_requires_nonce_and_rejects_static() {
        // Static "1" is rejected on a release image, with or without a nonce.
        assert!(!is_proxy_header_trusted_for_image(
            Some(DASHBOARD_PROXY_LEGACY_VALUE),
            true,
            Some("secretnonce")
        ));
        assert!(!is_proxy_header_trusted_for_image(
            Some(DASHBOARD_PROXY_LEGACY_VALUE),
            true,
            None
        ));
        // The exact STRONG (64-hex) nonce IS trusted on a release image.
        // CE-114: a release nonce must carry strong entropy, so this uses the
        // 64-hex urandom shape rather than a short placeholder.
        const STRONG_NONCE: &str =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(is_proxy_header_trusted_for_image(
            Some(STRONG_NONCE),
            true,
            Some(STRONG_NONCE)
        ));
        // A wrong nonce is rejected.
        assert!(!is_proxy_header_trusted_for_image(
            Some("wrongnonce"),
            true,
            Some(STRONG_NONCE)
        ));
        // No nonce provisioned on a release image → header trusts nothing.
        assert!(!is_proxy_header_trusted_for_image(
            Some(STRONG_NONCE),
            true,
            None
        ));
        // Missing header is never trusted.
        assert!(!is_proxy_header_trusted_for_image(
            None,
            true,
            Some("secretnonce")
        ));
    }

    /// CE-114: on a RELEASE image the per-boot nonce must carry strong entropy.
    /// A weak (non-64-hex) nonce — e.g. the S80dashboard `date+pid+uptime`
    /// fallback that only fires when /dev/urandom + od are unavailable — is NOT
    /// trusted even when the header value matches it byte-for-byte; the request
    /// falls through to bearer auth (fail-closed). DEV/LAB posture is unchanged.
    #[test]
    fn proxy_header_release_image_rejects_weak_entropy_nonce() {
        // A decimal date+pid+uptime shape (the weak fallback) — matches but is
        // rejected on release.
        let weak = "1750000000000000000012345678";
        assert!(!is_proxy_header_trusted_for_image(Some(weak), true, Some(weak)));
        // Too-short hex is rejected on release.
        assert!(!is_proxy_header_trusted_for_image(
            Some("deadbeef"),
            true,
            Some("deadbeef")
        ));
        // Uppercase hex is rejected (od emits lowercase) — fail closed.
        let upper = "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF";
        assert!(!is_proxy_header_trusted_for_image(
            Some(upper),
            true,
            Some(upper)
        ));
        // Positive control: the real 64-hex urandom nonce IS trusted on release.
        let strong = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(is_proxy_header_trusted_for_image(
            Some(strong),
            true,
            Some(strong)
        ));
        // DEV/LAB is byte-identical to today: a weak nonce match is still
        // accepted there (no entropy requirement off-release).
        assert!(is_proxy_header_trusted_for_image(Some(weak), false, Some(weak)));
    }

    /// CE-114: the strong-nonce definition mirrors the init-script's
    /// `^[0-9a-f]{64}$` — exactly 64 lowercase-hex chars.
    #[test]
    fn strong_proxy_nonce_requires_64_lower_hex() {
        assert!(is_strong_proxy_nonce(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        // Too short.
        assert!(!is_strong_proxy_nonce("deadbeef"));
        // 63 chars (one short).
        assert!(!is_strong_proxy_nonce(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde"
        ));
        // Uppercase rejected.
        assert!(!is_strong_proxy_nonce(
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
        ));
        // Non-hex character rejected.
        assert!(!is_strong_proxy_nonce(
            "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        // Empty rejected.
        assert!(!is_strong_proxy_nonce(""));
    }

    /// DEVOPS-010 (2026-06-02): the legacy static dashboard-proxy header MUST be
    /// rejected by the ACTUALLY-CALLED loopback-proxy gate under a release-image
    /// posture, while the real per-boot nonce mechanism keeps working.
    ///
    /// The pre-existing `proxy_header_release_image_requires_nonce_and_rejects_static`
    /// test proves the pure `is_proxy_header_trusted_for_image` helper, but the
    /// gate `auth_middleware` actually invokes is
    /// `is_trusted_loopback_proxy_request`, which folds in the loopback check +
    /// the live image posture + the per-boot nonce. This test exercises that
    /// gate end-to-end through its deterministic seam so a regression that
    /// re-trusts the forgeable static `"1"` on a release image (the DEVOPS-010
    /// weak-bypass) is caught here, not just at the helper layer.
    #[test]
    fn loopback_proxy_gate_rejects_static_header_under_release_image() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

        let static_header = |uri: &str| {
            let mut req = Request::builder()
                .uri(uri)
                .method(Method::POST)
                .header("X-Dcentos-Dashboard-Proxy", DASHBOARD_PROXY_LEGACY_VALUE)
                .body(Body::empty())
                .unwrap();
            req.extensions_mut().insert(ConnectInfo(loopback));
            req
        };

        // RELEASE IMAGE: the forgeable static "1" is rejected whether or not a
        // per-boot nonce is provisioned — the bypass falls through to bearer
        // auth (fail-closed). This is the core DEVOPS-010 / SEC-W24-1 close.
        assert!(
            !is_trusted_loopback_proxy_request_for_image(
                &static_header("/api/config"),
                true,
                Some("secretnonce"),
            ),
            "release image must reject the static dashboard-proxy header even when a nonce exists"
        );
        assert!(
            !is_trusted_loopback_proxy_request_for_image(&static_header("/api/config"), true, None),
            "release image with no nonce provisioned must trust nothing via the header"
        );

        // RELEASE IMAGE: the REAL per-boot nonce mechanism still works — a
        // loopback request carrying the exact STRONG (64-hex, CE-114) nonce is
        // trusted.
        const STRONG_NONCE: &str =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut nonce_req = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", STRONG_NONCE)
            .body(Body::empty())
            .unwrap();
        nonce_req.extensions_mut().insert(ConnectInfo(loopback));
        assert!(
            is_trusted_loopback_proxy_request_for_image(&nonce_req, true, Some(STRONG_NONCE)),
            "release image must still trust the real per-boot nonce on a loopback proxy request"
        );

        // RELEASE IMAGE: a wrong nonce value is rejected.
        let mut wrong_nonce = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", "wrongnonce")
            .body(Body::empty())
            .unwrap();
        wrong_nonce.extensions_mut().insert(ConnectInfo(loopback));
        assert!(
            !is_trusted_loopback_proxy_request_for_image(&wrong_nonce, true, Some(STRONG_NONCE)),
            "release image must reject a wrong nonce value"
        );
    }

    /// DEVOPS-010 (2026-06-02): the DEV/LAB posture of the actually-called gate
    /// is byte-identical to today for explicit same-host trusted helpers. The
    /// legacy static "1" is still trusted on a loopback request, and the nonce is
    /// additively accepted if present. A non-loopback caller is never trusted
    /// regardless of header value (anti-spoof). The LAN-facing dashboard server
    /// does not stamp this header; it forwards Bearer auth.
    #[test]
    fn loopback_proxy_gate_dev_image_keeps_static_header_and_blocks_remote() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 8080);

        // DEV/LAB loopback + legacy static "1", no nonce -> trusted for
        // explicit same-host helpers that still use the legacy value.
        let mut dev_static = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", DASHBOARD_PROXY_LEGACY_VALUE)
            .body(Body::empty())
            .unwrap();
        dev_static.extensions_mut().insert(ConnectInfo(loopback));
        assert!(
            is_trusted_loopback_proxy_request_for_image(&dev_static, false, None),
            "dev image must keep trusting the legacy static dashboard-proxy header"
        );

        // DEV/LAB: the real nonce is additively accepted too.
        let mut dev_nonce = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", "secretnonce")
            .body(Body::empty())
            .unwrap();
        dev_nonce.extensions_mut().insert(ConnectInfo(loopback));
        assert!(
            is_trusted_loopback_proxy_request_for_image(&dev_nonce, false, Some("secretnonce")),
            "dev image must additively accept the per-boot nonce when present"
        );

        // ANTI-SPOOF (both postures): a NON-loopback caller stamping the static
        // header is never trusted — the LAN bypass the finding warns about can
        // never authenticate off-box even on a dev image.
        let mut remote_dev = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", DASHBOARD_PROXY_LEGACY_VALUE)
            .body(Body::empty())
            .unwrap();
        remote_dev.extensions_mut().insert(ConnectInfo(remote));
        assert!(
            !is_trusted_loopback_proxy_request_for_image(&remote_dev, false, None),
            "a remote caller with the static header must never be trusted (anti-spoof)"
        );
        let mut remote_release = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", "secretnonce")
            .body(Body::empty())
            .unwrap();
        remote_release.extensions_mut().insert(ConnectInfo(remote));
        assert!(
            !is_trusted_loopback_proxy_request_for_image(
                &remote_release,
                true,
                Some("secretnonce")
            ),
            "a remote caller is never trusted even with the correct nonce on a release image"
        );
    }

    /// Smoke: the production `is_trusted_loopback_proxy_request` wrapper still
    /// compiles + runs and (on a non-release dev test host with no provisioned
    /// nonce) keeps the legacy DEV behaviour — a loopback request with the
    /// static header is trusted, a non-loopback one is not. This pins that the
    /// new deterministic seam did not alter the live wrapper's contract.
    #[test]
    fn production_loopback_proxy_wrapper_unchanged_on_dev_host() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 8080);

        let mut proxied = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", DASHBOARD_PROXY_LEGACY_VALUE)
            .body(Body::empty())
            .unwrap();
        proxied.extensions_mut().insert(ConnectInfo(loopback));
        assert!(is_trusted_loopback_proxy_request(&proxied));

        let mut remote_req = Request::builder()
            .uri("/api/config")
            .method(Method::POST)
            .header("X-Dcentos-Dashboard-Proxy", DASHBOARD_PROXY_LEGACY_VALUE)
            .body(Body::empty())
            .unwrap();
        remote_req.extensions_mut().insert(ConnectInfo(remote));
        assert!(!is_trusted_loopback_proxy_request(&remote_req));
    }

    #[test]
    fn constant_time_eq_matches_and_rejects() {
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        // Length-mismatched inputs are rejected without panicking.
        assert!(!constant_time_eq("abc", "abc123"));
        assert!(!constant_time_eq("abc123", "abc"));
        assert!(constant_time_eq("", ""));
    }

    /// The nonce reader trims trailing whitespace/newlines and treats an
    /// empty/whitespace-only file as "no nonce" (None).
    #[test]
    fn read_proxy_nonce_trims_and_handles_empty() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let mut good = std::env::temp_dir();
        good.push(format!("dcentrald-proxy-nonce-good-{}-{}", pid, nanos));
        let mut empty = std::env::temp_dir();
        empty.push(format!("dcentrald-proxy-nonce-empty-{}-{}", pid, nanos));
        let mut absent = std::env::temp_dir();
        absent.push(format!("dcentrald-proxy-nonce-absent-{}-{}", pid, nanos));

        raw_write_file(&good, b"deadbeefcafef00d\n");
        assert_eq!(
            read_proxy_nonce_at(&good).as_deref(),
            Some("deadbeefcafef00d")
        );

        raw_write_file(&empty, b"   \n");
        assert_eq!(read_proxy_nonce_at(&empty), None);

        let _ = std::fs::remove_file(&absent);
        assert_eq!(read_proxy_nonce_at(&absent), None);

        let _ = std::fs::remove_file(&good);
        let _ = std::fs::remove_file(&empty);
    }

    // ---- GROUP-C: login brute-force rate limiter (HIGH) ----------------

    /// Unique loopback IP per test so the process-wide LOGIN_RATE_LIMITER map
    /// never cross-contaminates between tests (each test owns its own IP).
    fn unique_test_ip(seed: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, seed))
    }

    #[test]
    fn login_limiter_allows_attempts_below_soft_cap() {
        let ip = unique_test_ip(11);
        let t0 = Instant::now();
        // Up to (soft_max - 1) failures stay allowed — no lockout yet.
        for _ in 0..(LOGIN_RATE_LIMIT_SOFT_MAX - 1) {
            assert!(
                check_login_rate_limit_at(ip, t0).is_ok(),
                "attempts below the soft cap must be allowed"
            );
            record_login_failure_at(ip, t0);
        }
        // Still allowed to make the attempt that will TRIP the cap.
        assert!(check_login_rate_limit_at(ip, t0).is_ok());
    }

    #[test]
    fn login_limiter_locks_out_after_soft_cap() {
        let ip = unique_test_ip(12);
        let t0 = Instant::now();
        // Drive exactly soft_max failures → lockout engages.
        for _ in 0..LOGIN_RATE_LIMIT_SOFT_MAX {
            let _ = check_login_rate_limit_at(ip, t0);
            record_login_failure_at(ip, t0);
        }
        // Now locked out: the next check returns a 429.
        let res = check_login_rate_limit_at(ip, t0);
        assert!(res.is_err(), "soft cap exceeded must lock the IP out");
        let resp = res.unwrap_err();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            resp.headers().contains_key("Retry-After"),
            "lockout response must carry Retry-After"
        );
    }

    #[test]
    fn login_limiter_unlocks_after_backoff_elapses() {
        let ip = unique_test_ip(13);
        let t0 = Instant::now();
        for _ in 0..LOGIN_RATE_LIMIT_SOFT_MAX {
            let _ = check_login_rate_limit_at(ip, t0);
            record_login_failure_at(ip, t0);
        }
        assert!(check_login_rate_limit_at(ip, t0).is_err(), "locked now");
        // Far in the future (past the max possible backoff) → unlocked again.
        let later = t0 + Duration::from_secs(super::LOGIN_LOCKOUT_MAX_SECS + 1);
        assert!(
            check_login_rate_limit_at(ip, later).is_ok(),
            "lockout must clear once the backoff window elapses"
        );
    }

    #[test]
    fn login_limiter_success_clears_failures() {
        let ip = unique_test_ip(14);
        let t0 = Instant::now();
        // A few failures, then a success clears the slate.
        for _ in 0..(LOGIN_RATE_LIMIT_SOFT_MAX - 1) {
            let _ = check_login_rate_limit_at(ip, t0);
            record_login_failure_at(ip, t0);
        }
        record_login_success(ip);
        // After success, a fresh full run of failures is needed to lock out
        // again — i.e. the prior near-cap failures were forgotten.
        for _ in 0..(LOGIN_RATE_LIMIT_SOFT_MAX - 1) {
            assert!(
                check_login_rate_limit_at(ip, t0).is_ok(),
                "post-success attempts below the (reset) cap must be allowed"
            );
            record_login_failure_at(ip, t0);
        }
        assert!(check_login_rate_limit_at(ip, t0).is_ok());
    }

    #[test]
    fn login_limiter_rolls_failure_window() {
        let ip = unique_test_ip(15);
        let t0 = Instant::now();
        // A couple of failures, then let the rolling window fully elapse.
        record_login_failure_at(ip, t0);
        record_login_failure_at(ip, t0);
        let after_window = t0 + Duration::from_secs(LOGIN_RATE_LIMIT_WINDOW_SECS + 1);
        // The window resets, so we can again take a full set of soft attempts
        // without locking out at the first one.
        for _ in 0..(LOGIN_RATE_LIMIT_SOFT_MAX - 1) {
            assert!(check_login_rate_limit_at(ip, after_window).is_ok());
            record_login_failure_at(ip, after_window);
        }
        assert!(check_login_rate_limit_at(ip, after_window).is_ok());
    }

    // ---- GROUP-C: session idle timeout (MED) ---------------------------

    /// Unique token hash per test so the process-wide SESSION_LAST_SEEN map
    /// never collides between tests.
    fn unique_token_hash(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        hash_session_token(&format!(
            "idle-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    fn auth_fixture_with_sessions(sessions: Vec<AuthSession>) -> AuthData {
        AuthData {
            version: 2,
            password_hash: "argon2id-test-fixture".to_string(),
            api_token: None,
            sessions,
        }
    }

    fn session_fixture(
        id: &str,
        created_at: u64,
        expires_at: Option<u64>,
        revoked: bool,
    ) -> AuthSession {
        AuthSession {
            id: id.to_string(),
            token_hash: hash_session_token(&format!("token-{id}")),
            created_at: created_at.to_string(),
            label: format!("session-{id}"),
            expires_at: expires_at.map(|expires_at| expires_at.to_string()),
            revoked_at: revoked.then(|| (created_at + 1).to_string()),
            role: SessionRole::Admin,
        }
    }

    #[test]
    fn issue_session_prunes_expired_and_revoked_records() {
        let future = super::now_epoch_secs() + super::SESSION_TTL_SECS;
        let mut auth = auth_fixture_with_sessions(vec![
            session_fixture("expired", 1, Some(1), false),
            session_fixture("revoked", 2, Some(future), true),
            session_fixture("active", 3, Some(future), false),
        ]);

        let issued = super::issue_session(&mut auth, Some("dashboard"));

        assert_eq!(auth.sessions.len(), 2);
        assert!(auth.sessions.iter().any(|session| session.id == "active"));
        assert!(auth.sessions.iter().any(|session| session.id == issued.id));
        assert!(!auth.sessions.iter().any(|session| session.id == "expired"));
        assert!(!auth.sessions.iter().any(|session| session.id == "revoked"));
    }

    #[test]
    fn issue_session_caps_active_records_and_keeps_new_session() {
        let future = super::now_epoch_secs() + super::SESSION_TTL_SECS;
        let sessions = (0..super::MAX_AUTH_SESSIONS)
            .map(|idx| {
                session_fixture(
                    &format!("old-{idx:02}"),
                    1_000 + idx as u64,
                    Some(future),
                    false,
                )
            })
            .collect();
        let mut auth = auth_fixture_with_sessions(sessions);

        let issued = super::issue_session(&mut auth, Some("dashboard"));

        assert_eq!(auth.sessions.len(), super::MAX_AUTH_SESSIONS);
        assert!(auth.sessions.iter().any(|session| session.id == issued.id));
        assert!(
            !auth.sessions.iter().any(|session| session.id == "old-00"),
            "oldest active session should be evicted to make room"
        );
        assert!(auth.sessions.iter().any(|session| session.id == "old-01"));
    }

    #[test]
    fn persisted_session_survives_daemon_restart_idle_map_reset() {
        let root = scratch_dir("session-restart");
        let auth_path = root.join("data").join("dcent").join("auth.json");
        let release_marker = root.join("missing-release-image");

        let mut auth = auth_fixture_with_sessions(Vec::new());
        let issued = super::issue_session(&mut auth, Some("dashboard"));
        let issued_hash = hash_session_token(&issued.token);
        save_auth_at(&auth_path, &auth).expect("save auth with issued session");

        // Simulate a daemon restart: persisted sessions remain on disk, but the
        // in-memory idle map starts empty. The first post-restart use should
        // grant a fresh idle window without extending the absolute TTL.
        super::forget_session_last_seen(&issued_hash);
        let loaded = load_auth_at(&auth_path, &release_marker).expect("reload persisted auth");
        let session = super::session_for_token(&loaded, &issued.token)
            .expect("persisted active bearer should authenticate after restart");
        assert_eq!(session.id, issued.id);
        assert_eq!(session.role, SessionRole::Admin);

        let expired_token = "expired-token";
        let expired = AuthSession {
            id: "expired".to_string(),
            token_hash: hash_session_token(expired_token),
            created_at: "1".to_string(),
            label: "expired".to_string(),
            expires_at: Some("1".to_string()),
            revoked_at: None,
            role: SessionRole::Admin,
        };
        let expired_auth = auth_fixture_with_sessions(vec![expired]);
        save_auth_at(&auth_path, &expired_auth).expect("save expired auth");
        super::forget_session_last_seen(&hash_session_token(expired_token));
        let loaded_expired =
            load_auth_at(&auth_path, &release_marker).expect("reload expired auth fixture");
        assert!(
            super::session_for_token(&loaded_expired, expired_token).is_none(),
            "restart must not revive a session past its absolute TTL"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn revoke_session_prunes_revoked_record() {
        let future = super::now_epoch_secs() + super::SESSION_TTL_SECS;
        let mut auth = auth_fixture_with_sessions(vec![
            session_fixture("keep", 10, Some(future), false),
            session_fixture("drop", 11, Some(future), false),
        ]);

        assert!(super::revoke_session(&mut auth, "drop"));

        assert_eq!(auth.sessions.len(), 1);
        assert!(auth.sessions.iter().any(|session| session.id == "keep"));
        assert!(!auth.sessions.iter().any(|session| session.id == "drop"));
    }

    #[test]
    fn ws_ticket_flow_is_default_off_short_lived_and_one_time() {
        let prev = super::WEBSOCKET_TICKETS_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
        {
            let mut tickets = super::WS_TICKETS.lock().unwrap_or_else(|e| e.into_inner());
            tickets.clear();
        }

        let now = super::now_epoch_secs();
        let auth = auth_fixture_with_sessions(vec![session_fixture(
            "ws",
            now,
            Some(now + super::SESSION_TTL_SECS),
            false,
        )]);
        let session = auth.sessions.first().expect("session fixture");

        super::WEBSOCKET_TICKETS_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
        let disabled_ticket = super::issue_ws_ticket_for_session_at(session, now);
        assert!(
            !super::redeem_ws_ticket_with_auth_at(&disabled_ticket.ticket, &auth, now + 1),
            "tickets must be default-off unless the compat flag is enabled"
        );

        super::WEBSOCKET_TICKETS_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
        let issued = super::issue_ws_ticket_for_session_at(session, now);
        assert_eq!(issued.expires_in_s, super::WS_TICKET_TTL_SECS);
        assert!(
            super::redeem_ws_ticket_with_auth_at(&issued.ticket, &auth, now + 1),
            "fresh ticket should redeem once"
        );
        assert!(
            !super::redeem_ws_ticket_with_auth_at(&issued.ticket, &auth, now + 2),
            "ticket must be one-time"
        );

        let expired = super::issue_ws_ticket_for_session_at(session, now);
        assert!(
            !super::redeem_ws_ticket_with_auth_at(
                &expired.ticket,
                &auth,
                now + super::WS_TICKET_TTL_SECS + 1,
            ),
            "ticket must expire quickly"
        );

        super::WEBSOCKET_TICKETS_ENABLED.store(prev, std::sync::atomic::Ordering::Relaxed);
        let mut tickets = super::WS_TICKETS.lock().unwrap_or_else(|e| e.into_inner());
        tickets.clear();
    }

    #[test]
    fn idle_timeout_default_is_set() {
        // The default idle timeout is the documented 8 hours. Assert the CONST,
        // not the live global: `SESSION_IDLE_TIMEOUT_SECS` is initialized to this
        // const, but sibling tests temporarily override the global via
        // `set_session_idle_timeout_secs`, and tests run in parallel in one
        // process — reading the global here races that window. The const is the
        // contract.
        assert_eq!(super::DEFAULT_SESSION_IDLE_TIMEOUT_SECS, 8 * 60 * 60);
    }

    #[test]
    fn idle_session_expires_after_timeout() {
        let _idle_guard = IDLE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = session_idle_timeout_secs();
        set_session_idle_timeout_secs(100);
        let th = unique_token_hash("expire");
        let t0 = Instant::now();
        // First sight records last-seen and is allowed.
        assert!(session_idle_ok_and_touch_at(&th, t0));
        // Just inside the window → still OK (and re-touches).
        assert!(session_idle_ok_and_touch_at(
            &th,
            t0 + Duration::from_secs(50)
        ));
        // Past the window from the LAST touch → idle, rejected.
        assert!(
            !session_idle_ok_and_touch_at(&th, t0 + Duration::from_secs(50 + 101)),
            "a session idle longer than the timeout must be rejected"
        );
        set_session_idle_timeout_secs(prev);
    }

    #[test]
    fn idle_activity_keeps_session_alive() {
        let _idle_guard = IDLE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = session_idle_timeout_secs();
        set_session_idle_timeout_secs(100);
        let th = unique_token_hash("active");
        let mut now = Instant::now();
        assert!(session_idle_ok_and_touch_at(&th, now));
        // Repeated activity at sub-timeout intervals keeps it alive far past
        // the raw timeout from the original creation instant.
        for _ in 0..20 {
            now += Duration::from_secs(50);
            assert!(
                session_idle_ok_and_touch_at(&th, now),
                "continuous activity must never trip the idle timeout"
            );
        }
        set_session_idle_timeout_secs(prev);
    }

    #[test]
    fn idle_timeout_zero_disables_check() {
        let _idle_guard = IDLE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = session_idle_timeout_secs();
        set_session_idle_timeout_secs(0);
        let th = unique_token_hash("disabled");
        let t0 = Instant::now();
        assert!(session_idle_ok_and_touch_at(&th, t0));
        // Even a year later, the idle check is disabled (absolute TTL still
        // applies elsewhere, but the idle gate is off).
        let far = t0 + Duration::from_secs(365 * 24 * 60 * 60);
        assert!(
            session_idle_ok_and_touch_at(&th, far),
            "idle timeout of 0 must disable the idle gate"
        );
        set_session_idle_timeout_secs(prev);
    }

    // ---- GROUP-C: read-only role gate (MED) ----------------------------

    #[test]
    fn read_only_role_blocks_writes_allows_reads() {
        // Read-only: GET allowed, every mutating method blocked.
        assert!(!read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/status",
            &Method::GET
        ));
        assert!(read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/config",
            &Method::POST
        ));
        assert!(read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/action/restart",
            &Method::POST
        ));
        assert!(read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/pools",
            &Method::PUT
        ));
        assert!(read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/led/config",
            &Method::DELETE
        ));
    }

    #[test]
    fn read_only_role_may_revoke_own_session() {
        // Carve-out: a read-only monitoring token can log itself out.
        assert!(!read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/auth/session/current",
            &Method::DELETE
        ));
    }

    #[test]
    fn read_only_role_may_call_read_only_mcp_mount() {
        // MCP uses POST for read-only tool calls. The only allowed carve-out is
        // the dedicated `/mcp` mount; the handler itself exposes no write tools.
        assert!(!read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/mcp",
            &Method::POST
        ));
        assert!(read_only_role_blocks_request(
            SessionRole::ReadOnly,
            "/api/action/restart",
            &Method::POST
        ));
    }

    #[test]
    fn admin_role_never_blocked() {
        // Admin sessions retain full write access on every method/path.
        assert!(!read_only_role_blocks_request(
            SessionRole::Admin,
            "/api/config",
            &Method::POST
        ));
        assert!(!read_only_role_blocks_request(
            SessionRole::Admin,
            "/api/action/restart",
            &Method::DELETE
        ));
        assert!(!read_only_role_blocks_request(
            SessionRole::Admin,
            "/api/status",
            &Method::GET
        ));
    }

    #[test]
    fn session_role_can_write_semantics() {
        assert!(SessionRole::Admin.can_write());
        assert!(!SessionRole::ReadOnly.can_write());
    }

    #[test]
    fn session_role_defaults_to_admin_for_legacy_sessions() {
        // An on-disk session JSON written before the `role` field existed must
        // deserialize to Admin (full access) so existing sessions never regress.
        let legacy = r#"{
            "id": "abc",
            "token_hash": "sha256:deadbeef",
            "created_at": "1000",
            "label": "dashboard",
            "expires_at": null,
            "revoked_at": null
        }"#;
        let session: super::AuthSession =
            serde_json::from_str(legacy).expect("legacy session JSON must parse");
        assert_eq!(
            session.role,
            SessionRole::Admin,
            "a role-less on-disk session must default to Admin"
        );
    }

    #[test]
    fn session_role_serde_roundtrip_is_snake_case() {
        // Explicit wire contract: read_only / admin lowercase snake_case.
        assert_eq!(
            serde_json::to_string(&SessionRole::ReadOnly).unwrap(),
            "\"read_only\""
        );
        assert_eq!(
            serde_json::to_string(&SessionRole::Admin).unwrap(),
            "\"admin\""
        );
        let parsed: SessionRole = serde_json::from_str("\"read_only\"").unwrap();
        assert_eq!(parsed, SessionRole::ReadOnly);
    }

    /// SECURITY (W1.5, 2026-05-07): unit tests for the auth-file perm hardening.
    ///
    /// On Unix targets, `save_auth_at()` must land the file at 0o600 and the
    /// parent dir at 0o700; `verify_auth_file_perms_at()` must auto-correct
    /// wider perms in place. These assertions are gated `#[cfg(unix)]` because
    /// `PermissionsExt::mode()` is Unix-only — on Windows dev hosts the body
    /// of `set_mode()`/`check_and_tighten()` is a no-op so there is nothing
    /// to assert about.
    #[cfg(unix)]
    mod perms {
        use super::super::{save_auth_at, verify_auth_file_perms_at, AuthData};
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        /// Per-test scratch dir under the system temp root. Avoids pulling in
        /// the `tempfile` crate just for two assertions, while still keeping
        /// each test fully isolated (process pid + monotonic counter +
        /// nanosecond timestamp).
        fn scratch_dir(label: &str) -> PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let mut p = std::env::temp_dir();
            p.push(format!("dcentrald-auth-{}-{}-{}-{}", label, pid, nanos, n));
            std::fs::create_dir_all(&p).expect("scratch dir create");
            p
        }

        #[test]
        fn save_auth_lands_file_at_0o600_and_dir_at_0o700() {
            let root = scratch_dir("save");
            let data_dir = root.join("data").join("dcent");
            let auth_path = data_dir.join("auth.json");

            let auth = AuthData {
                version: 2,
                password_hash: "stub".to_string(),
                api_token: None,
                sessions: Vec::new(),
            };
            save_auth_at(&auth_path, &auth).expect("save_auth_at");

            let dir_mode = std::fs::metadata(&data_dir)
                .expect("dir metadata")
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&auth_path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777;

            assert_eq!(file_mode, 0o600, "auth file must be exactly 0o600");
            assert_eq!(dir_mode, 0o700, "parent dir must be exactly 0o700");

            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn verify_auth_file_perms_corrects_wide_file_to_0o600() {
            let root = scratch_dir("file");
            let data_dir = root.join("data").join("dcent");
            let auth_path = data_dir.join("auth.json");

            // Create dir + file with deliberately wide perms (0o755 / 0o644).
            std::fs::create_dir_all(&data_dir).expect("mkdir");
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o755))
                .expect("set dir 0755");
            super::raw_write_file(&auth_path, b"{}");
            std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o644))
                .expect("set file 0644");

            verify_auth_file_perms_at(&auth_path).expect("verify_auth_file_perms_at");

            let dir_mode = std::fs::metadata(&data_dir)
                .expect("dir metadata")
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&auth_path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777;

            assert_eq!(file_mode, 0o600, "verify must tighten file 0o644 -> 0o600");
            assert_eq!(dir_mode, 0o700, "verify must tighten dir 0o755 -> 0o700");

            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn verify_auth_file_perms_leaves_already_tight_file_unchanged() {
            let root = scratch_dir("tight");
            let data_dir = root.join("data").join("dcent");
            let auth_path = data_dir.join("auth.json");

            std::fs::create_dir_all(&data_dir).expect("mkdir");
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))
                .expect("set dir 0700");
            super::raw_write_file(&auth_path, b"{}");
            std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
                .expect("set file 0600");

            verify_auth_file_perms_at(&auth_path).expect("verify_auth_file_perms_at");

            let dir_mode = std::fs::metadata(&data_dir)
                .expect("dir metadata")
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&auth_path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777;

            assert_eq!(file_mode, 0o600);
            assert_eq!(dir_mode, 0o700);

            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn verify_auth_file_perms_is_a_noop_when_file_missing() {
            // No file present (first-boot, no password set yet) — must not
            // error and must not create the file.
            let root = scratch_dir("missing");
            let data_dir = root.join("data").join("dcent");
            let auth_path = data_dir.join("auth.json");

            verify_auth_file_perms_at(&auth_path).expect("verify_auth_file_perms_at");
            assert!(!auth_path.exists(), "verify must not create missing file");

            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
