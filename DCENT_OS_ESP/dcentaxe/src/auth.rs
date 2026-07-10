use std::io::{Read as IoRead, Write};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::http::server::{EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::{EspNvs, NvsDefault};
use log::*;
use pbkdf2::pbkdf2_hmac_array;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::shared::SharedState;

// ── Login rate limiter ─────────────────────────────────────────────────
// Brute-force defence for `POST /api/auth/session`. Tracks failures in a
// 15-minute rolling window; after `LOGIN_FAIL_THRESHOLD` misses we lock the
// endpoint for `LOGIN_LOCKOUT_SECS`. Counter resets on successful login or
// when the window expires. State is device-wide (single-user firmware) and
// reboot-clears, which is fine: power-cycling after getting locked out is an
// acceptable recovery mechanism for a physically-present owner.
const LOGIN_FAIL_WINDOW_SECS: u64 = 15 * 60;
const LOGIN_FAIL_THRESHOLD: u8 = 5;
const LOGIN_LOCKOUT_SECS: u64 = 5 * 60;

struct LoginTracker {
    fail_count: u8,
    window_start: u64,
    lockout_until: u64,
}

static LOGIN_TRACKER: Mutex<LoginTracker> = Mutex::new(LoginTracker {
    fail_count: 0,
    window_start: 0,
    lockout_until: 0,
});

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns `Some(retry_after_secs)` if the endpoint is currently locked out.
fn login_lockout_remaining() -> Option<u64> {
    let tracker = LOGIN_TRACKER.lock().ok()?;
    let now = now_epoch_secs();
    if tracker.lockout_until > now {
        Some(tracker.lockout_until - now)
    } else {
        None
    }
}

fn record_login_failure() -> u64 {
    let now = now_epoch_secs();
    let mut tracker = match LOGIN_TRACKER.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    if tracker.window_start == 0
        || now.saturating_sub(tracker.window_start) > LOGIN_FAIL_WINDOW_SECS
    {
        tracker.window_start = now;
        tracker.fail_count = 0;
    }
    tracker.fail_count = tracker.fail_count.saturating_add(1);
    if tracker.fail_count >= LOGIN_FAIL_THRESHOLD {
        tracker.lockout_until = now + LOGIN_LOCKOUT_SECS;
        warn!(
            "AUTH: owner login locked out for {} s after {} failed attempts",
            LOGIN_LOCKOUT_SECS, tracker.fail_count
        );
        return LOGIN_LOCKOUT_SECS;
    }
    0
}

fn record_login_success() {
    if let Ok(mut tracker) = LOGIN_TRACKER.lock() {
        tracker.fail_count = 0;
        tracker.window_start = 0;
        tracker.lockout_until = 0;
    }
}

const NVS_KEY_AUTH: &str = "auth";
const AUTH_VERSION: u8 = 1;
const AUTH_MAX_SIZE: usize = 4096;
const SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const PBKDF2_ITERATIONS: u32 = 120_000;
pub const MIN_OWNER_PASSWORD_LEN: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthSession {
    pub id: String,
    pub token_hash: String,
    pub created_at: u64,
    pub label: String,
    pub expires_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthData {
    pub version: u8,
    pub password_hash: String,
    pub sessions: Vec<AuthSession>,
}

#[derive(Debug)]
pub enum AuthFailure {
    Unauthorized(&'static str),
    Forbidden(&'static str),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthSetupRequest {
    password: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthSessionRequest {
    password: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthPasswordRequest {
    current_password: String,
    new_password: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatusResponse {
    password_set: bool,
    session_auth: bool,
    metrics_require_auth: bool,
    active_sessions: usize,
}

fn now_epoch_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    unsafe {
        esp_idf_svc::sys::esp_fill_random(bytes.as_mut_ptr() as *mut _, bytes.len());
    }
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hash_password(password: &str) -> String {
    let salt = random_hex(16);
    let derived =
        pbkdf2_hmac_array::<Sha256, 32>(password.as_bytes(), salt.as_bytes(), PBKDF2_ITERATIONS);
    format!(
        "pbkdf2-sha256:{}:{}:{}",
        PBKDF2_ITERATIONS,
        salt,
        hex_bytes(&derived),
    )
}

fn verify_password_hash(stored: &str, password: &str) -> bool {
    let mut parts = stored.split(':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("pbkdf2-sha256"), Some(iterations), Some(salt)) => {
            let expected = parts.next().unwrap_or_default();
            let iterations = iterations.parse::<u32>().unwrap_or(PBKDF2_ITERATIONS);
            let derived =
                pbkdf2_hmac_array::<Sha256, 32>(password.as_bytes(), salt.as_bytes(), iterations);
            // AOTA-3: constant-time compare of the derived password digest.
            crate::ota_signature::ct_str_eq(&hex_bytes(&derived), expected)
        }
        (Some("sha256"), Some(salt), Some(expected)) => {
            // AOTA-3: constant-time compare of the legacy SHA-256 password digest.
            crate::ota_signature::ct_str_eq(
                &sha256_hex(&format!("{}:{}", salt, password)),
                expected,
            )
        }
        _ => false,
    }
}

fn password_needs_rehash(stored: &str) -> bool {
    !stored.starts_with("pbkdf2-sha256:")
}

fn hash_token(token: &str) -> String {
    format!("sha256:{}", sha256_hex(token))
}

fn session_is_active(session: &AuthSession) -> bool {
    if session.revoked_at.is_some() {
        return false;
    }
    session
        .expires_at
        .map(|expires_at| expires_at > now_epoch_s())
        .unwrap_or(true)
}

fn load_auth_from_nvs(nvs: &mut EspNvs<NvsDefault>) -> AuthData {
    let mut buf = vec![0u8; AUTH_MAX_SIZE];
    match nvs.get_blob(NVS_KEY_AUTH, &mut buf) {
        Ok(Some(data)) => serde_json::from_slice::<AuthData>(data).unwrap_or_default(),
        _ => AuthData::default(),
    }
}

fn save_auth_to_nvs(nvs: &mut EspNvs<NvsDefault>, auth: &AuthData) -> Result<(), String> {
    let json = serde_json::to_vec(auth).map_err(|e| format!("Auth serialize failed: {}", e))?;
    if json.len() > AUTH_MAX_SIZE {
        return Err(format!("Auth data too large: {} bytes", json.len()));
    }
    nvs.set_blob(NVS_KEY_AUTH, &json)
        .map_err(|e| format!("Auth write failed: {:?}", e))
}

pub fn validate_owner_password(password: &str) -> Result<(), &'static str> {
    if password.len() < MIN_OWNER_PASSWORD_LEN {
        Err("Password must be at least 8 characters")
    } else {
        Ok(())
    }
}

pub fn password_is_set_in_nvs(nvs: &mut EspNvs<NvsDefault>) -> bool {
    let mut auth = load_auth_from_nvs(nvs);
    auth.version = AUTH_VERSION;
    !auth.password_hash.trim().is_empty()
}

pub fn clear_owner_auth(nvs: &mut EspNvs<NvsDefault>) -> Result<(), String> {
    let mut auth = AuthData::default();
    auth.version = AUTH_VERSION;
    save_auth_to_nvs(nvs, &auth)
}

pub fn bootstrap_owner_password(
    nvs: &mut EspNvs<NvsDefault>,
    password: &str,
    label: &str,
) -> Result<(String, String, Option<u64>), String> {
    validate_owner_password(password).map_err(|e| e.to_string())?;
    let mut auth = load_auth_from_nvs(nvs);
    auth.version = AUTH_VERSION;
    if !auth.password_hash.trim().is_empty() {
        return Err("Owner password already configured".to_string());
    }
    auth.password_hash = hash_password(password);
    auth.sessions.clear();
    let issued = issue_session_record(&mut auth, label);
    save_auth_to_nvs(nvs, &auth)?;
    Ok(issued)
}

fn with_auth_data<T, F>(state: &SharedState, mutate: F) -> Result<T, String>
where
    F: FnOnce(&mut AuthData) -> Result<T, String>,
{
    let mut nvs_guard = state
        .nvs
        .lock()
        .map_err(|_| "Failed to lock NVS".to_string())?;
    let nvs = nvs_guard
        .as_mut()
        .ok_or_else(|| "NVS handle not available".to_string())?;
    let mut auth = load_auth_from_nvs(nvs);
    auth.version = AUTH_VERSION;
    let result = mutate(&mut auth)?;
    save_auth_to_nvs(nvs, &auth)?;
    Ok(result)
}

fn read_auth_data(state: &SharedState) -> Option<AuthData> {
    let mut nvs_guard = state.nvs.lock().ok()?;
    let nvs = nvs_guard.as_mut()?;
    let mut auth = load_auth_from_nvs(nvs);
    auth.version = AUTH_VERSION;
    Some(auth)
}

pub fn password_is_set(state: &SharedState) -> bool {
    read_auth_data(state)
        .map(|auth| !auth.password_hash.trim().is_empty())
        .unwrap_or(false)
}

fn active_session_count(state: &SharedState) -> usize {
    read_auth_data(state)
        .map(|auth| {
            auth.sessions
                .iter()
                .filter(|s| session_is_active(s))
                .count()
        })
        .unwrap_or(0)
}

fn bearer_token(req: &Request<&mut EspHttpConnection>) -> Option<String> {
    req.header("Authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value.trim().to_string())
}

fn is_bearer_authorized(req: &Request<&mut EspHttpConnection>, state: &SharedState) -> bool {
    let token = match bearer_token(req) {
        Some(token) => token,
        None => return false,
    };
    read_auth_data(state)
        .map(|auth| {
            let expected = hash_token(&token);
            auth.sessions.iter().any(|session| {
                // AOTA-3: constant-time compare of the session-token hash.
                session_is_active(session)
                    && crate::ota_signature::ct_str_eq(&session.token_hash, &expected)
            })
        })
        .unwrap_or(false)
}

/// Returns true when the request carries a valid, active owner bearer session.
///
/// This is the public session predicate used by the fail-closed OTA /
/// unsigned-policy gates (AOTA-1/AOTA-4). It does NOT consider the
/// passwordless bypass that ordinary `authorize_rest_write` allows: a caller
/// that holds no valid owner session is never treated as the owner here.
pub fn request_has_owner_session(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> bool {
    is_bearer_authorized(req, state)
}

pub fn authorize_rest_write(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> Result<(), AuthFailure> {
    if !crate::api::check_csrf(req) {
        return Err(AuthFailure::Forbidden(
            "CSRF: X-Requested-With header required",
        ));
    }
    if !password_is_set(state) {
        return Ok(());
    }
    if is_bearer_authorized(req, state) {
        Ok(())
    } else {
        Err(AuthFailure::Unauthorized(
            "Bearer session required for write endpoints",
        ))
    }
}

pub fn authorize_mcp(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> Result<(), AuthFailure> {
    if !password_is_set(state) {
        return Ok(());
    }
    if is_bearer_authorized(req, state) {
        Ok(())
    } else {
        Err(AuthFailure::Unauthorized(
            "Bearer session required for MCP access",
        ))
    }
}

pub fn authorize_mcp_control(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> Result<(), AuthFailure> {
    // XPH-5: the fail-closed decision lives in the host-tested pure predicate
    // `ota_signature::mcp_control_authorized`; this wrapper only feeds it the
    // two booleans the esp-idf `Request` resolves to, then maps the typed
    // denial to the wire 401 detail. Keeping the decision in one host-tested fn
    // means a regression (e.g. accidentally allowing the passwordless bypass)
    // is caught by `cargo test -p dcentaxe-core`.
    use crate::ota_signature::{mcp_control_authorized, McpControlDenied};
    match mcp_control_authorized(password_is_set(state), is_bearer_authorized(req, state)) {
        Ok(()) => Ok(()),
        Err(McpControlDenied::PasswordNotSet) => Err(AuthFailure::Unauthorized(
            "Owner password must be configured before MCP control tools can mutate device state",
        )),
        Err(McpControlDenied::BearerRequired) => Err(AuthFailure::Unauthorized(
            "Bearer session required for MCP control tools",
        )),
    }
}

pub fn authorize_rest_read(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> Result<(), AuthFailure> {
    if !password_is_set(state) {
        return Ok(());
    }
    if is_bearer_authorized(req, state) {
        Ok(())
    } else {
        Err(AuthFailure::Unauthorized(
            "Bearer session required for this device detail endpoint",
        ))
    }
}

pub fn authorize_metrics(
    req: &Request<&mut EspHttpConnection>,
    state: &SharedState,
) -> Result<(), AuthFailure> {
    let metrics_require_auth = state
        .config
        .lock()
        .map(|cfg| cfg.metrics_require_auth)
        .unwrap_or(true);
    if !metrics_require_auth || !password_is_set(state) {
        return Ok(());
    }
    if is_bearer_authorized(req, state) {
        Ok(())
    } else {
        Err(AuthFailure::Unauthorized(
            "Bearer session required for /metrics",
        ))
    }
}

pub fn write_auth_failure(
    req: Request<&mut EspHttpConnection>,
    failure: AuthFailure,
) -> Result<(), Box<dyn std::error::Error>> {
    let (status, detail) = match failure {
        AuthFailure::Unauthorized(detail) => (401, detail),
        AuthFailure::Forbidden(detail) => (403, detail),
    };
    let body = serde_json::to_string(&json!({
        "error": if status == 401 { "Unauthorized" } else { "Forbidden" },
        "detail": detail,
    }))
    .unwrap_or_default();
    let mut resp = req.into_response(status, None, &[("Content-Type", "application/json")])?;
    resp.write(body.as_bytes())?;
    Ok(())
}

fn write_json(
    req: Request<&mut EspHttpConnection>,
    status: u16,
    body: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::to_string(&body).unwrap_or_default();
    let mut resp = req.into_response(status, None, &[("Content-Type", "application/json")])?;
    resp.write(payload.as_bytes())?;
    Ok(())
}

fn post_auth_setup(
    state: &SharedState,
    mut req: Request<&mut EspHttpConnection>,
) -> Result<(), Box<dyn std::error::Error>> {
    if password_is_set(state) {
        return write_json(
            req,
            409,
            json!({"error": "Conflict", "detail": "Owner password already configured"}),
        );
    }
    if !crate::api::check_csrf(&req) {
        return write_auth_failure(
            req,
            AuthFailure::Forbidden("CSRF: X-Requested-With header required"),
        );
    }
    let mut body = vec![0u8; 512];
    let len = req.read(&mut body).unwrap_or(0);
    let parsed = serde_json::from_slice::<AuthSetupRequest>(&body[..len]);
    let setup = match parsed {
        Ok(setup) => {
            if let Err(detail) = validate_owner_password(&setup.password) {
                return write_json(req, 400, json!({"error": "Bad Request", "detail": detail}));
            }
            setup
        }
        Err(e) => {
            return write_json(
                req,
                400,
                json!({"error": "Bad Request", "detail": format!("Invalid auth setup JSON: {}", e)}),
            )
        }
    };

    match with_auth_data(state, |auth| {
        auth.password_hash = hash_password(&setup.password);
        auth.sessions.clear();
        Ok(issue_session_record(
            auth,
            setup.label.as_deref().unwrap_or("dashboard"),
        ))
    }) {
        Ok((id, token, expires_at)) => {
            info!("AUTH: owner password configured");
            write_json(
                req,
                200,
                json!({
                    "status": "ok",
                    "passwordSet": true,
                    "session": {
                        "id": id,
                        "token": token,
                        "expiresAt": expires_at,
                    }
                }),
            )
        }
        Err(e) => write_json(
            req,
            500,
            json!({"error": "Internal Server Error", "detail": e}),
        ),
    }
}

fn issue_session_record(auth: &mut AuthData, label: &str) -> (String, String, Option<u64>) {
    let token = random_hex(24);
    let now = now_epoch_s();
    let expires_at = Some(now + SESSION_TTL_SECS);
    let session = AuthSession {
        id: random_hex(16),
        token_hash: hash_token(&token),
        created_at: now,
        label: label.to_string(),
        expires_at,
        revoked_at: None,
    };
    let id = session.id.clone();
    auth.sessions.push(session);
    (id, token, expires_at)
}

fn post_auth_session(
    state: &SharedState,
    mut req: Request<&mut EspHttpConnection>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = vec![0u8; 512];
    let len = req.read(&mut body).unwrap_or(0);
    let parsed = serde_json::from_slice::<AuthSessionRequest>(&body[..len]);
    let session_req = match parsed {
        Ok(value) => value,
        Err(e) => {
            return write_json(
                req,
                400,
                json!({"error": "Bad Request", "detail": format!("Invalid auth session JSON: {}", e)}),
            )
        }
    };
    // Reject before a password compare if the rate limiter is engaged.
    if let Some(retry_after) = login_lockout_remaining() {
        warn!(
            "AUTH: rejecting login attempt — locked out for {} more seconds",
            retry_after
        );
        return write_json(
            req,
            429,
            json!({
                "error": "Too Many Requests",
                "detail": format!(
                    "Too many failed login attempts. Try again in {} seconds.",
                    retry_after
                ),
                "retryAfter": retry_after,
            }),
        );
    }
    let auth = read_auth_data(state).unwrap_or_default();
    if auth.password_hash.trim().is_empty() {
        return write_json(
            req,
            428,
            json!({"error": "Precondition Required", "detail": "Set the owner password via POST /api/auth/setup first"}),
        );
    }
    if !verify_password_hash(&auth.password_hash, &session_req.password) {
        let locked_for = record_login_failure();
        if locked_for > 0 {
            return write_json(
                req,
                429,
                json!({
                    "error": "Too Many Requests",
                    "detail": format!(
                        "Too many failed attempts. Locked for {} seconds.",
                        locked_for
                    ),
                    "retryAfter": locked_for,
                }),
            );
        }
        return write_json(
            req,
            401,
            json!({"error": "Unauthorized", "detail": "Invalid owner password"}),
        );
    }
    record_login_success();

    match with_auth_data(state, |auth| {
        if !verify_password_hash(&auth.password_hash, &session_req.password) {
            return Err("Invalid owner password".to_string());
        }
        if password_needs_rehash(&auth.password_hash) {
            auth.password_hash = hash_password(&session_req.password);
            info!("AUTH: upgraded legacy password hash to PBKDF2-SHA256");
        }
        Ok(issue_session_record(
            auth,
            session_req.label.as_deref().unwrap_or("dashboard"),
        ))
    }) {
        Ok((id, token, expires_at)) => write_json(
            req,
            200,
            json!({
                "status": "ok",
                "session": {
                    "id": id,
                    "token": token,
                    "expiresAt": expires_at,
                }
            }),
        ),
        Err(e) => write_json(
            req,
            500,
            json!({"error": "Internal Server Error", "detail": e}),
        ),
    }
}

fn post_auth_password(
    state: &SharedState,
    mut req: Request<&mut EspHttpConnection>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(err) = authorize_rest_write(&req, state) {
        return write_auth_failure(req, err);
    }
    let mut body = vec![0u8; 768];
    let len = req.read(&mut body).unwrap_or(0);
    let change = match serde_json::from_slice::<AuthPasswordRequest>(&body[..len]) {
        Ok(change) => change,
        Err(e) => {
            return write_json(
                req,
                400,
                json!({"error": "Bad Request", "detail": format!("Invalid password change JSON: {}", e)}),
            )
        }
    };
    if let Err(detail) = validate_owner_password(&change.new_password) {
        return write_json(req, 400, json!({"error": "Bad Request", "detail": detail}));
    }

    let auth = read_auth_data(state).unwrap_or_default();
    if auth.password_hash.trim().is_empty() {
        return write_json(
            req,
            428,
            json!({"error": "Precondition Required", "detail": "Set the owner password first"}),
        );
    }
    if !verify_password_hash(&auth.password_hash, &change.current_password) {
        return write_json(
            req,
            401,
            json!({"error": "Unauthorized", "detail": "Current password is incorrect"}),
        );
    }

    match with_auth_data(state, |auth| {
        if !verify_password_hash(&auth.password_hash, &change.current_password) {
            return Err("Current password is incorrect".to_string());
        }
        auth.password_hash = hash_password(&change.new_password);
        auth.sessions.clear();
        Ok(issue_session_record(
            auth,
            change.label.as_deref().unwrap_or("dashboard"),
        ))
    }) {
        Ok((id, token, expires_at)) => write_json(
            req,
            200,
            json!({
                "status": "ok",
                "message": "Owner password updated",
                "session": {
                    "id": id,
                    "token": token,
                    "expiresAt": expires_at,
                }
            }),
        ),
        Err(e) => write_json(
            req,
            500,
            json!({"error": "Internal Server Error", "detail": e}),
        ),
    }
}

fn delete_current_session(
    state: &SharedState,
    req: Request<&mut EspHttpConnection>,
) -> Result<(), Box<dyn std::error::Error>> {
    let token = match bearer_token(&req) {
        Some(token) => token,
        None => {
            return write_auth_failure(
                req,
                AuthFailure::Unauthorized("Bearer session required for session revocation"),
            )
        }
    };
    let token_hash = hash_token(&token);
    match with_auth_data(state, |auth| {
        if let Some(session) = auth.sessions.iter_mut().find(|session| {
            // AOTA-3: constant-time compare of the session-token hash.
            session_is_active(session)
                && crate::ota_signature::ct_str_eq(&session.token_hash, &token_hash)
        }) {
            session.revoked_at = Some(now_epoch_s());
            Ok(true)
        } else {
            Ok(false)
        }
    }) {
        Ok(true) => write_json(req, 200, json!({"status": "ok", "revoked": true})),
        Ok(false) => write_json(
            req,
            404,
            json!({"error": "Not Found", "detail": "Session not found or already revoked"}),
        ),
        Err(e) => write_json(
            req,
            500,
            json!({"error": "Internal Server Error", "detail": e}),
        ),
    }
}

pub fn register_auth_api(server: &mut EspHttpServer, state: SharedState) {
    let state_status = state.clone();
    server
        .fn_handler(
            "/api/auth/status",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let response = AuthStatusResponse {
                    password_set: password_is_set(&state_status),
                    session_auth: true,
                    metrics_require_auth: state_status
                        .config
                        .lock()
                        .map(|cfg| cfg.metrics_require_auth)
                        .unwrap_or(true),
                    active_sessions: active_session_count(&state_status),
                };
                write_json(req, 200, serde_json::to_value(response).unwrap_or_default())
            },
        )
        .expect("Failed to register GET /api/auth/status");

    let state_setup = state.clone();
    server
        .fn_handler("/api/auth/setup", Method::Post, move |req| {
            post_auth_setup(&state_setup, req)
        })
        .expect("Failed to register POST /api/auth/setup");

    let state_session = state.clone();
    server
        .fn_handler("/api/auth/session", Method::Post, move |req| {
            post_auth_session(&state_session, req)
        })
        .expect("Failed to register POST /api/auth/session");

    let state_revoke = state.clone();
    server
        .fn_handler("/api/auth/session/current", Method::Delete, move |req| {
            delete_current_session(&state_revoke, req)
        })
        .expect("Failed to register DELETE /api/auth/session/current");

    let state_password = state.clone();
    server
        .fn_handler("/api/auth/password", Method::Post, move |req| {
            post_auth_password(&state_password, req)
        })
        .expect("Failed to register POST /api/auth/password");
}
