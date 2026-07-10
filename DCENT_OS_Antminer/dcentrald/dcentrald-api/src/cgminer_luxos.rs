//! LuxOS session model + mutating-command contract on the CGMiner :4028
//! surface — makes DCENT_OS a drop-in for every LuxOS/CGMiner-speaking
//! fleet tool (Foreman, Awesome Miner, pyasic, luxos-tooling).
//!
//! ## Why
//!
//! `cgminer.rs` ships a 13-command **read-only** dispatcher. Real fleet
//! tools (Foreman/Awesome Miner/luxos-tooling) speak the *LuxOS* dialect of
//! the CGMiner protocol: they `logon` to obtain an 8-char `SessionID`, pass
//! it as the **first comma-separated parameter** of every mutating command,
//! and expect the canonical LuxOS JSON envelope back. Without this, those
//! tools can monitor a DCENT_OS miner but cannot *control* it — the single
//! highest ecosystem-leverage gap.
//!
//! ## Session model (RE:  §3)
//!
//! - `logon` → returns `{"SESSION":[{"SessionID":"<8 alnum>"}]}`. Fails if a
//!   session already exists (single-session **mutex** — only one controller
//!   at a time). No username/password — the session *is* the lock.
//! - `logoff <sid>` / `kill <sid>` → release the lock.
//! - `session` → report whether a session is active.
//! - Every mutating command takes `<sid>` as the **first** comma token and
//!   is rejected with a LuxOS-shaped error when the sid is missing/stale.
//! - Sessions **expire** after [`SESSION_TTL`] of inactivity so a crashed
//!   fleet tool can't permanently wedge the lock (matches LuxOS behavior;
//!   `kill` is the manual override).
//!
//! ## Safety contract (LOAD-BEARING — see module test
//! `cgminer_voltageset_goes_through_same_config_validate_clamp`)
//!
//! This module introduces **ZERO new control/voltage/NAND path**. Every
//! mutating command **delegates to the exact REST handler the dashboard
//! already calls** (`rest::post_pools`, `rest::post_config`,
//! `rest::post_fan`, `rest::post_action_sleep` / `post_action_wake`,
//! `rest::post_profiles`, `rest::post_led_locate`,
//! `rest::dispatch_autotuner_mode_command` / `rest::persist_autotuner_mode`).
//!
//! Consequently every existing safety gate stays in force on the delegated
//! path, *unchanged*:
//!   - `voltageset` writes `[mining].voltage_mv` through the SAME
//!     `rest::post_config` TOML write that `DcentraldConfig::validate()`
//!     re-validates on reload — including the am2 14_500 mV chip-rail
//!     ceiling and the 5000-20000 mV envelope (`config.rs::validate()`).
//!     The cgminer surface CANNOT bypass it because it does not write a
//!     voltage register — it writes the same config key through the same
//!     validated reload, and the runtime autotuner re-clamps to
//!     `VOLTAGE_CLAMP_MV` / PVT.
//!   - `fanset` goes through `rest::post_fan`, which keeps the
//!     per-`OperatingMode` PWM floor/ceiling (Home ≤30).
//!   - `autotunerset`/`atmset`/`powertargetset`/`profileset` go through
//!     `rest::persist_autotuner_mode` + `rest::dispatch_autotuner_mode_command`
//!     so the runtime PI controller still owns the ≤14_500 mV / fan≤30 /
//!     PVT clamps.
//!   - The 4 corruption-prevention guarantees (EEPROM 0x50-0x57 HAL
//!     write-deny, recovery-tool-gated destructive PIC ops, fw=0x86
//!     refusal, toolbox preflight) are *downstream* of all of the above
//!     and are not touched.
//!
//! Auth-tier / kind classification comes verbatim from the already-modeled
//! `dcentrald_api_types::luxos_rest_command` 68-variant contract.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use dcentrald_api_types::cgminer_status_codes::CgminerStatusCode;
use dcentrald_api_types::luxos_rest_command::{descriptor, LuxosAuthTier, LuxosCommand};

use crate::cgminer::{CgMinerCommand, CGMINER_VERSION};
use crate::AppState;

/// Session token length (LuxOS uses an 8-char alphanumeric string).
pub const SESSION_ID_LEN: usize = 8;

/// Inactivity TTL after which a session is auto-reaped so a crashed fleet
/// tool cannot permanently hold the single-session mutex. LuxOS reaps on
/// the same principle; `kill` is the manual override.
pub const SESSION_TTL: Duration = Duration::from_secs(600);

/// Body-size ceiling when draining a delegated REST handler's response.
const DELEGATE_BODY_LIMIT: usize = 1 << 20;

/// The single active session (LuxOS allows at most one controller).
#[derive(Debug, Clone)]
struct ActiveSession {
    id: String,
    created: Instant,
    last_seen: Instant,
}

/// Process-wide single-session mutex. `None` ⇒ no controller bound.
#[derive(Debug, Default)]
pub struct SessionManager {
    inner: Mutex<Option<ActiveSession>>,
}

/// Outcome of validating a `<sid>` against the live session.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionCheck {
    /// `<sid>` matches the live, non-expired session.
    Valid,
    /// No session is active (caller must `logon` first).
    NoSession,
    /// A session exists but the supplied `<sid>` does not match it.
    Mismatch,
    /// A `<sid>` was required but the parameter was absent/empty.
    Missing,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn reap_if_expired(slot: &mut Option<ActiveSession>, now: Instant) {
        if let Some(s) = slot {
            if now.duration_since(s.last_seen) >= SESSION_TTL {
                *slot = None;
            }
        }
    }

    /// `logon`: acquire the single-session mutex. Returns the new
    /// `SessionID`, or `None` if a (non-expired) session already exists.
    pub fn logon(&self) -> Option<String> {
        let now = Instant::now();
        let mut slot = self.inner.lock().expect("session mutex poisoned");
        Self::reap_if_expired(&mut slot, now);
        if slot.is_some() {
            return None;
        }
        let id = generate_session_id();
        *slot = Some(ActiveSession {
            id: id.clone(),
            created: now,
            last_seen: now,
        });
        Some(id)
    }

    /// `logoff`/`kill` semantics. `require_match=true` (logoff) only
    /// releases when `<sid>` matches; `false` (kill) force-releases any
    /// session. Returns true if a session was released.
    pub fn release(&self, sid: Option<&str>, require_match: bool) -> bool {
        let now = Instant::now();
        let mut slot = self.inner.lock().expect("session mutex poisoned");
        Self::reap_if_expired(&mut slot, now);
        match slot.as_ref() {
            None => false,
            Some(s) => {
                if require_match && Some(s.id.as_str()) != sid {
                    return false;
                }
                *slot = None;
                true
            }
        }
    }

    /// Snapshot for the `session` command: `(active, age_secs)`.
    pub fn status(&self) -> (bool, u64) {
        let now = Instant::now();
        let mut slot = self.inner.lock().expect("session mutex poisoned");
        Self::reap_if_expired(&mut slot, now);
        match slot.as_ref() {
            Some(s) => (true, now.duration_since(s.created).as_secs()),
            None => (false, 0),
        }
    }

    /// Validate a `<sid>` for a mutating command and, on success, refresh
    /// the activity timestamp (so an actively-controlling tool keeps its
    /// lock past the TTL).
    pub fn check(&self, sid: Option<&str>) -> SessionCheck {
        let now = Instant::now();
        let mut slot = self.inner.lock().expect("session mutex poisoned");
        Self::reap_if_expired(&mut slot, now);
        let Some(active) = slot.as_mut() else {
            return SessionCheck::NoSession;
        };
        match sid {
            None => SessionCheck::Missing,
            Some("") => SessionCheck::Missing,
            Some(s) if s == active.id => {
                active.last_seen = now;
                SessionCheck::Valid
            }
            Some(_) => SessionCheck::Mismatch,
        }
    }
}

/// Process-global session manager (the CGMiner protocol is connect-per-
/// command, so the session state cannot live on the connection).
pub fn global_session_manager() -> &'static SessionManager {
    static MGR: OnceLock<SessionManager> = OnceLock::new();
    MGR.get_or_init(SessionManager::new)
}

/// Generate an 8-char alphanumeric SessionID. Uses a non-crypto PRNG
/// seeded from time + address entropy — the session is a mutex token, not
/// a credential (LuxOS itself uses no authentication on it), so this only
/// needs to be collision-resistant for the single-session window.
fn generate_session_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let stk = 0u8;
        t ^ (&stk as *const u8 as u64).rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15
    };
    let mut out = String::with_capacity(SESSION_ID_LEN);
    for _ in 0..SESSION_ID_LEN {
        // xorshift64*
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let v = seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.push(ALPHABET[(v % ALPHABET.len() as u64) as usize] as char);
    }
    out
}

/// LuxOS STATUS envelope. `Code` mirrors the canonical CGMiner code
/// catalog so Foreman/Awesome Miner read the values they expect.
fn luxos_status(code: CgminerStatusCode, msg: impl Into<String>) -> serde_json::Value {
    let when = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::json!({
        "STATUS": "S",
        "When": when,
        "Code": code.code(),
        "Msg": msg.into(),
        "Description": CGMINER_VERSION,
    })
}

fn luxos_error(code: CgminerStatusCode, msg: impl Into<String>) -> serde_json::Value {
    let when = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::json!({
        "STATUS": "E",
        "When": when,
        "Code": code.code(),
        "Msg": msg.into(),
        "Description": CGMINER_VERSION,
    })
}

/// A complete LuxOS-shaped response: `{ <KEY>:[...], "STATUS":[...], "id":1 }`.
fn envelope(extra: serde_json::Value, status: serde_json::Value) -> serde_json::Value {
    let mut obj = match extra {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("STATUS".into(), serde_json::json!([status]));
    obj.insert("id".into(), serde_json::json!(1));
    serde_json::Value::Object(obj)
}

fn error_envelope(code: CgminerStatusCode, msg: impl Into<String>) -> serde_json::Value {
    envelope(serde_json::json!({}), luxos_error(code, msg))
}

/// Split a LuxOS comma parameter into `(session_id, rest_args)`. The
/// session id is ALWAYS the first comma token for mutating commands.
fn split_param(param: &Option<String>) -> (Option<String>, Vec<String>) {
    match param {
        None => (None, vec![]),
        Some(p) => {
            let mut it = p.split(',');
            let sid = it
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let rest: Vec<String> = it.map(|s| s.trim().to_string()).collect();
            (sid, rest)
        }
    }
}

/// True iff `name` is a LuxOS command this module handles (session
/// lifecycle OR a delegated mutating/telemetry verb). Read-only verbs
/// already served by `cgminer.rs` are intentionally excluded so the legacy
/// dispatcher keeps owning them.
pub fn is_luxos_command(name: &str) -> bool {
    matches!(
        name,
        // session lifecycle
        "logon" | "logoff" | "session" | "kill"
        // pools (mutating)
        | "addpool" | "removepool" | "switchpool" | "enablepool" | "disablepool"
        // profiles & autotuner & power
        | "profileset" | "profilenew" | "profilerem"
        | "autotunerset" | "atmset" | "powertargetset"
        // thermal/power/voltage/frequency
        | "fanset" | "tempctrlset" | "voltageset" | "frequencyset"
        | "psuset" | "immersionswitch" | "curtail"
        // networking / led / lifecycle-ish
        | "netset" | "ledset"
        // telemetry in LuxOS JSON shape
        | "metrics" | "events" | "systemaudit" | "limits" | "healthchipget"
    )
}

/// Resolve a wire token to the modeled `LuxosCommand` (for auth-tier/kind).
fn luxos_command_for(name: &str) -> Option<LuxosCommand> {
    use LuxosCommand::*;
    Some(match name {
        "logon" => Logon,
        "logoff" => Logoff,
        "session" => Session,
        "kill" => Kill,
        "addpool" => Addpool,
        "removepool" => Removepool,
        "switchpool" => Switchpool,
        "enablepool" => Enablepool,
        "disablepool" => Disablepool,
        "profileset" => Profileset,
        "profilenew" => Profilenew,
        "profilerem" => Profilerem,
        "autotunerset" => Autotunerset,
        "atmset" => Atmset,
        "powertargetset" => Powertargetset,
        "fanset" => Fanset,
        "tempctrlset" => Tempctrlset,
        "psuset" => Psuset,
        "immersionswitch" => Immersionswitch,
        "curtail" => Curtail,
        "netset" => Netset,
        "ledset" => Ledset,
        "metrics" => Metrics,
        "events" => Events,
        "systemaudit" => Systemaudit,
        "limits" => Limits,
        "healthchipget" => Healthchipget,
        // voltageset/frequencyset are LuxOS extension commands not in the
        // 68-variant REST catalog (that catalog tracks the :8080 SPA
        // surface); treat them as session-required Write like every other
        // tuning mutation.
        _ => return None,
    })
}

/// Is this verb a session-gated mutation? Uses the modeled contract where
/// available, else the conservative default (any *set / curtail / pool
/// mutation requires a session).
///
/// Also consumed by `cgminer::is_mutating_verb` (API-1 LAN-write gate) as the
/// canonical "is this a mutating LuxOS verb" predicate — keep it the single
/// source of truth so the two surfaces cannot drift.
pub fn requires_session(name: &str) -> bool {
    if let Some(cmd) = luxos_command_for(name) {
        return descriptor(cmd).auth == LuxosAuthTier::Session;
    }
    // voltageset / frequencyset — not in the SPA catalog but unmistakably
    // session-gated tuning mutations.
    matches!(name, "voltageset" | "frequencyset")
}

/// Drain a delegated axum handler `Response` into its JSON body so we can
/// re-wrap it in the LuxOS envelope. This is what guarantees byte-for-byte
/// reuse of the gated REST logic — we call the SAME function the dashboard
/// calls and report its real outcome.
async fn body_json(resp: axum::response::Response) -> (bool, serde_json::Value) {
    let status_ok = resp.status().is_success();
    let bytes = axum::body::to_bytes(resp.into_body(), DELEGATE_BODY_LIMIT)
        .await
        .unwrap_or_default();
    let json = serde_json::from_slice::<serde_json::Value>(&bytes)
        .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status_ok, json)
}

/// Build a LuxOS response from a delegated REST outcome. A delegated
/// `"status":"error"` body is surfaced as a LuxOS `E` status (so fleet
/// tools see the rejection — e.g. a clamp refusal) instead of a fake `S`.
fn from_delegated(
    key: &str,
    ok: bool,
    body: serde_json::Value,
    ok_code: CgminerStatusCode,
) -> serde_json::Value {
    let delegated_ok = ok
        && body
            .get("status")
            .and_then(|s| s.as_str())
            .map(|s| s != "error")
            .unwrap_or(true);
    let msg = body
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(if delegated_ok { "OK" } else { "rejected" })
        .to_string();
    if delegated_ok {
        envelope(
            serde_json::json!({ key: [body] }),
            luxos_status(ok_code, msg),
        )
    } else {
        envelope(
            serde_json::json!({ key: [body] }),
            luxos_error(CgminerStatusCode::MissingValue, msg),
        )
    }
}

/// Entry point: handle a LuxOS-dialect command. `cgminer.rs` routes here
/// for any token `is_luxos_command()` accepts (after its own 13 read verbs
/// and the `+` batch split). Returns the full JSON response value.
pub async fn handle_luxos_command(
    state: &Arc<AppState>,
    cmd: &CgMinerCommand,
) -> serde_json::Value {
    let name = cmd.command.as_str();
    let mgr = global_session_manager();

    // ── Session lifecycle ────────────────────────────────────────────
    match name {
        "logon" => {
            return match mgr.logon() {
                Some(sid) => envelope(
                    serde_json::json!({ "SESSION": [{ "SessionID": sid, "Session": 0 }] }),
                    luxos_status(CgminerStatusCode::Version, "Session created"),
                ),
                None => error_envelope(
                    CgminerStatusCode::AccessDenied,
                    "A session is already active (single-session mutex). \
                     logoff/kill it first.",
                ),
            };
        }
        "session" => {
            let (active, age) = mgr.status();
            return envelope(
                serde_json::json!({ "SESSION": [{
                    "Session": if active { 1 } else { 0 },
                    "SessionID": "",
                    "AgeSeconds": age,
                }] }),
                luxos_status(
                    CgminerStatusCode::Version,
                    if active {
                        "Session active"
                    } else {
                        "No active session"
                    },
                ),
            );
        }
        "logoff" => {
            let (sid, _) = split_param(&cmd.parameter);
            let released = mgr.release(sid.as_deref(), true);
            return if released {
                envelope(
                    serde_json::json!({}),
                    luxos_status(CgminerStatusCode::Version, "Session ended"),
                )
            } else {
                error_envelope(
                    CgminerStatusCode::MissingId,
                    "logoff: no matching active session for the supplied SessionID",
                )
            };
        }
        "kill" => {
            // LuxOS `kill` force-terminates ANY active session (auth=None
            // gateway, like logon). It still takes <sid> as the first
            // token by convention but does not require a match.
            let (sid, _) = split_param(&cmd.parameter);
            let released = mgr.release(sid.as_deref(), false);
            return envelope(
                serde_json::json!({ "KILLED": released }),
                luxos_status(
                    CgminerStatusCode::Version,
                    if released {
                        "Active session force-terminated"
                    } else {
                        "No session to terminate"
                    },
                ),
            );
        }
        _ => {}
    }

    // ── Session gate for mutating commands ───────────────────────────
    let (sid, rest) = split_param(&cmd.parameter);
    if requires_session(name) {
        match mgr.check(sid.as_deref()) {
            SessionCheck::Valid => {}
            SessionCheck::NoSession => {
                return error_envelope(
                    CgminerStatusCode::MissingId,
                    format!(
                        "{}: no active session. logon first; pass SessionID as \
                         the first comma parameter.",
                        name
                    ),
                );
            }
            SessionCheck::Missing => {
                return error_envelope(
                    CgminerStatusCode::MissingId,
                    format!(
                        "{}: SessionID required as the first comma parameter \
                         (LuxOS contract: <sid>,<args...>).",
                        name
                    ),
                );
            }
            SessionCheck::Mismatch => {
                return error_envelope(
                    CgminerStatusCode::AccessDenied,
                    format!(
                        "{}: supplied SessionID does not match the active session.",
                        name
                    ),
                );
            }
        }
    }

    // ── Delegated mutating + telemetry commands ──────────────────────
    // EVERY arm below calls the SAME gated REST function the dashboard
    // calls. No control/voltage/NAND logic is reimplemented here.
    match name {
        // -- Pools: delegate to rest::post_pools (writes [pool] + reload)
        "addpool" => delegate_addpool(state, &rest).await,
        "switchpool" | "enablepool" | "disablepool" | "removepool" => {
            // LuxOS pool-index ops. DCENT_OS persists pools via the
            // structured /api/pools writer; expose an honest LuxOS error
            // (matching cgminer.rs's existing posture) rather than a fake
            // success so Foreman/Awesome Miner don't think it took.
            error_envelope(
                CgminerStatusCode::MissingId,
                format!(
                    "{}: per-index pool {} is not supported on DCENT_OS — use \
                     `addpool <sid>,<url>,<user>,<pass>` (rewrites the pool set) \
                     or the REST /api/pools surface.",
                    name, name
                ),
            )
        }

        // -- Voltage / frequency: delegate to rest::post_config. The TOML
        //    write is re-validated by DcentraldConfig::validate() on
        //    reload (am2 14_500 mV ceiling + 5000-20000 mV envelope) and
        //    re-clamped by the runtime autotuner. SAME path as the
        //    dashboard config editor — NO cgminer-specific voltage write.
        "voltageset" => delegate_mining_config(state, &rest, "voltage_mv", name).await,
        "frequencyset" => delegate_mining_config(state, &rest, "frequency_mhz", name).await,

        // -- Fan: delegate to rest::post_fan (keeps per-mode PWM clamp).
        "fanset" => delegate_fanset(state, &rest).await,

        // -- Curtailment: safe→sleep, active/wakeup→wake (existing
        //    curtailment controller; SAME as /api/action/sleep|wake).
        "curtail" => delegate_curtail(state, &rest).await,

        // -- Autotuner / ATM / power target: delegate to the SAME
        //    persist-then-dispatch path the REST autotuner endpoints use,
        //    so the runtime PI controller still owns the voltage/fan/PVT
        //    clamps.
        "autotunerset" => delegate_autotuner_enable(state, &rest).await,
        "atmset" => delegate_autotuner_enable(state, &rest).await,
        "powertargetset" => delegate_powertarget(state, &rest).await,
        "profileset" => delegate_profileset(state, &rest).await,
        "profilenew" => delegate_profilenew(state, &rest).await,
        "profilerem" => delegate_profilerem(state, &rest).await,

        // -- LED: delegate to rest::post_led_locate.
        "ledset" => delegate_ledset(state, &rest).await,

        // -- Network / PSU / temp-ctrl / immersion: route through the
        //    same gated config writer (rest::post_config) for the keys it
        //    whitelists; immersion has no DCENT_OS analogue → honest error.
        "tempctrlset" => delegate_tempctrl(state, &rest).await,
        "netset" => delegate_netset(state, &rest).await,
        "psuset" => error_envelope(
            CgminerStatusCode::MissingId,
            "psuset: PSU is configured via /api/config/psu-override on \
             DCENT_OS (PSU I2C probe is disabled by design). Not exposed \
             on the cgminer surface.",
        ),
        "immersionswitch" => error_envelope(
            CgminerStatusCode::MissingId,
            "immersionswitch: immersion mode is not a DCENT_OS feature.",
        ),

        // -- Telemetry in LuxOS JSON shape (read existing DCENT telemetry;
        //    no mutation, no session needed).
        "metrics" => telemetry_metrics(state),
        "events" => telemetry_events(state),
        "systemaudit" => telemetry_systemaudit(state),
        "limits" => telemetry_limits(state),
        "healthchipget" => telemetry_healthchip(state),

        _ => error_envelope(
            CgminerStatusCode::InvalidCommand,
            format!("Unhandled LuxOS command: {}", name),
        ),
    }
}

// ─── Delegation helpers — every one calls a gated rest:: function ────────

async fn delegate_addpool(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    if rest.len() < 3 {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "addpool: expected <sid>,<url>,<user>,<password>",
        );
    }
    let body = serde_json::json!({
        "pools": [{ "url": rest[0], "worker": rest[1], "password": rest[2] }]
    });
    let req: crate::rest::PoolConfigRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return error_envelope(
                CgminerStatusCode::MissingValue,
                format!("addpool: invalid pool spec: {}", e),
            )
        }
    };
    let resp = crate::rest::post_pools(State(state.clone()), Json(req))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("POOLS", ok, json, CgminerStatusCode::Pool)
}

/// `voltageset`/`frequencyset` → write the `[mining]` config key via the
/// SAME `rest::post_config` path the dashboard uses. The clamp
/// (`config.rs::validate()` am2 ≤14_500 mV + 5000-20000 mV envelope) runs
/// on daemon reload; the runtime autotuner re-clamps to VOLTAGE_CLAMP_MV/
/// PVT. There is deliberately NO voltage register write here.
async fn delegate_mining_config(
    state: &Arc<AppState>,
    rest: &[String],
    key: &str,
    name: &str,
) -> serde_json::Value {
    let Some(raw) = rest.first() else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            format!("{}: expected <sid>,<value>", name),
        );
    };
    let Ok(value) = raw.parse::<i64>() else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            format!("{}: '{}' is not an integer", name, raw),
        );
    };
    let body = serde_json::json!({ "mining": { key: value } });
    let resp = crate::rest::post_config(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("CONFIG", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_fanset(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS fanset: <sid>,speed=<pct>[,...]. Map to the rest::post_fan
    // custom-PWM body so the per-OperatingMode PWM clamp applies.
    let mut pwm: Option<u64> = None;
    for kv in rest {
        if let Some((k, v)) = kv.split_once('=') {
            if matches!(k.trim(), "speed" | "pwm" | "target_pwm") {
                pwm = v.trim().parse::<u64>().ok();
            }
        } else if let Ok(n) = kv.trim().parse::<u64>() {
            pwm = Some(n);
        }
    }
    let Some(pwm) = pwm else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "fanset: expected <sid>,speed=<percent>",
        );
    };
    let body = serde_json::json!({ "mode": "custom", "target_pwm": pwm });
    let resp = crate::rest::post_fan(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("FAN", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_curtail(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS: `curtail <sid>,sleep` or `curtail <sid>,wakeup`. "safe" mode
    // is documented as a synonym for sleep; "unsafe"/"active"/"wakeup"
    // resume. Either way it delegates to the existing curtailment
    // controller via the SAME handlers /api/action/sleep|wake call.
    let arg = rest.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let resp = match arg.as_str() {
        "sleep" | "safe" | "curtail" | "" => crate::rest::post_action_sleep(State(state.clone()))
            .await
            .into_response(),
        "wakeup" | "wake" | "active" | "unsafe" => {
            crate::rest::post_action_wake(State(state.clone()))
                .await
                .into_response()
        }
        other => {
            return error_envelope(
                CgminerStatusCode::MissingValue,
                format!("curtail: unknown mode '{}' (use sleep|wakeup)", other),
            )
        }
    };
    let (ok, json) = body_json(resp).await;
    from_delegated("CURTAIL", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_autotuner_enable(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS `autotunerset <sid>,enabled=<bool>` / `atmset <sid>,enabled=..`.
    // enabled=false → Manual is NOT inferred (we don't know a safe fixed
    // point); enabled=true selects the runtime default Efficiency mode,
    // persisted+dispatched via the SAME gated path REST uses (the runtime
    // PI controller keeps the ≤14_500 mV / fan≤30 / PVT clamps).
    let mut enabled: Option<bool> = None;
    for kv in rest {
        if let Some((k, v)) = kv.split_once('=') {
            if k.trim() == "enabled" {
                enabled = match v.trim() {
                    "true" | "1" | "yes" | "on" => Some(true),
                    "false" | "0" | "no" | "off" => Some(false),
                    _ => None,
                };
            }
        }
    }
    let Some(enabled) = enabled else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "autotunerset/atmset: expected <sid>,enabled=<true|false>",
        );
    };
    if !enabled {
        // No mutation: report state. Disabling the autotuner without a
        // target would be a fixed-operating-point decision we won't infer.
        return envelope(
            serde_json::json!({ "AUTOTUNER": [{ "enabled": false, "applied": false }] }),
            luxos_status(
                CgminerStatusCode::MineConfig,
                "autotuner disable is a no-op on the cgminer surface — set a \
                 profile/power target to pin a fixed point instead",
            ),
        );
    }
    let mode = dcentrald_autotuner::config::TunerMode::Efficiency;
    if let Err(e) = crate::rest::persist_autotuner_mode(&mode) {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            format!("autotunerset: persist failed: {}", e),
        );
    }
    let runtime = crate::rest::dispatch_autotuner_mode_command(state, mode).await;
    envelope(
        serde_json::json!({ "AUTOTUNER": [{ "enabled": true, "runtime": runtime }] }),
        luxos_status(
            CgminerStatusCode::MineConfig,
            "Autotuner enabled (Efficiency)",
        ),
    )
}

async fn delegate_powertarget(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS `powertargetset <sid>,power=<watts>`. Same persist+dispatch
    // path as REST's power-target endpoints → the runtime PI controller
    // owns the clamps; we never write voltage directly.
    let mut watts: Option<u32> = None;
    for kv in rest {
        if let Some((k, v)) = kv.split_once('=') {
            if k.trim() == "power" {
                watts = v.trim().parse::<u32>().ok();
            }
        } else if let Ok(n) = kv.trim().parse::<u32>() {
            watts = Some(n);
        }
    }
    let Some(watts) = watts else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "powertargetset: expected <sid>,power=<watts>",
        );
    };
    let mode = dcentrald_autotuner::config::TunerMode::PowerTarget { watts };
    if let Err(e) = crate::rest::persist_autotuner_mode(&mode) {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            format!("powertargetset: persist failed: {}", e),
        );
    }
    let runtime = crate::rest::dispatch_autotuner_mode_command(state, mode).await;
    envelope(
        serde_json::json!({ "POWER": [{ "target_watts": watts, "runtime": runtime }] }),
        luxos_status(CgminerStatusCode::MineConfig, "Power target set"),
    )
}

async fn delegate_profileset(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    let Some(profile) = rest.first() else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "profileset: expected <sid>,<profile_name>",
        );
    };
    let body = serde_json::json!({ "name": profile, "action": "activate" });
    let resp = crate::rest::post_profiles(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("PROFILE", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_profilenew(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    if rest.len() < 3 {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "profilenew: expected <sid>,<name>,<freq>,<voltage>",
        );
    }
    let body = serde_json::json!({
        "name": rest[0],
        "frequency_mhz": rest[1].parse::<i64>().unwrap_or(0),
        "voltage_mv": rest[2].parse::<i64>().unwrap_or(0),
    });
    let resp = crate::rest::post_profiles(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("PROFILE", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_profilerem(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    let Some(profile) = rest.first() else {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "profilerem: expected <sid>,<profile_name>",
        );
    };
    // No destructive REST delete is wired for profiles; report honestly
    // rather than fake-succeed (matches cgminer.rs posture).
    let _ = state;
    error_envelope(
        CgminerStatusCode::MissingId,
        format!(
            "profilerem('{}'): profile deletion is not exposed on the cgminer \
             surface — manage profiles via /api/profiles.",
            profile
        ),
    )
}

async fn delegate_ledset(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS `ledset <sid>,<color>,<state>` → DCENT_OS locate pattern.
    let pattern = rest.first().cloned();
    let body = crate::rest::LocateRequest {
        pattern_id: pattern,
    };
    let resp = crate::rest::post_led_locate(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("LED", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_tempctrl(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS `tempctrlset <sid>,target=<c>,hot=<c>,...` → the [thermal]
    // config section via the SAME gated rest::post_config path.
    let mut thermal = serde_json::Map::new();
    for kv in rest {
        if let Some((k, v)) = kv.split_once('=') {
            let key = match k.trim() {
                "target" => "target_temp_c",
                "hot" => "hot_temp_c",
                "panic" | "dangerous" => "dangerous_temp_c",
                other => other,
            };
            if let Ok(n) = v.trim().parse::<i64>() {
                thermal.insert(key.to_string(), serde_json::json!(n));
            }
        }
    }
    if thermal.is_empty() {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "tempctrlset: expected <sid>,target=<c>[,hot=<c>,panic=<c>]",
        );
    }
    let body = serde_json::json!({ "thermal": thermal });
    let resp = crate::rest::post_config(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("TEMPCTRL", ok, json, CgminerStatusCode::MineConfig)
}

async fn delegate_netset(state: &Arc<AppState>, rest: &[String]) -> serde_json::Value {
    // LuxOS `netset <sid>,dhcp=<bool>,hostname=..,ipaddress=..` → the
    // [general] config section via the SAME gated rest::post_config path.
    let mut general = serde_json::Map::new();
    for kv in rest {
        if let Some((k, v)) = kv.split_once('=') {
            match k.trim() {
                "hostname" => {
                    general.insert("hostname".into(), serde_json::json!(v.trim()));
                }
                "dhcp" => {
                    let b = matches!(v.trim(), "true" | "1" | "yes" | "on");
                    general.insert("dhcp".into(), serde_json::json!(b));
                }
                _ => {}
            }
        }
    }
    if general.is_empty() {
        return error_envelope(
            CgminerStatusCode::MissingValue,
            "netset: expected <sid>,dhcp=<bool>[,hostname=<name>]",
        );
    }
    let body = serde_json::json!({ "general": general });
    let resp = crate::rest::post_config(State(state.clone()), Json(body))
        .await
        .into_response();
    let (ok, json) = body_json(resp).await;
    from_delegated("NETWORK", ok, json, CgminerStatusCode::MineConfig)
}

// ─── Telemetry in LuxOS JSON shape (read-only; no session needed) ────────

fn telemetry_metrics(state: &Arc<AppState>) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let power_projection = crate::rest::(&power, &miner, &hardware);
    let power_fields = luxos_metrics_power_fields(&power_projection);
    envelope(
        serde_json::json!({ "METRICS": [{
            "Elapsed": miner.uptime_s,
            "MHS av": miner.hashrate_ghs * 1000.0,
            "MHS 5s": miner.hashrate_5s_ghs * 1000.0,
            "Accepted": miner.accepted,
            "Rejected": miner.rejected,
            "Power": power_fields.board_watts,
            "Wall Power": power_fields.wall_watts,
            "Power Source": power_projection.source,
            "Power Source Detail": power_projection.source_detail,
            "Power Live Available": power_projection.live_power_available,
            "Power Modeled": power_projection.modeled,
            "Power Calibrated": power_projection.calibrated,
            "Power Calibration Multiplier": power_projection.calibration_multiplier,
            "Power Note": power_fields.note,
            "Temperature": miner
                .chains
                .iter()
                .map(|c| c.temp_c as f64)
                .fold(0.0_f64, f64::max),
        }] }),
        luxos_status(CgminerStatusCode::MineStats, "Metrics"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LuxosMetricsPowerFields {
    board_watts: u32,
    wall_watts: u32,
    note: &'static str,
}

fn luxos_metrics_power_fields(
    projection: &crate::rest::PowerTelemetryProjection,
) -> LuxosMetricsPowerFields {
    if projection.live_power_available {
        LuxosMetricsPowerFields {
            board_watts: projection.board_watts,
            wall_watts: projection.wall_watts,
            note: projection.note,
        }
    } else {
        LuxosMetricsPowerFields {
            board_watts: 0,
            wall_watts: 0,
            note: "Live power has not published a positive reading; LuxOS-compatible Power fields are suppressed to 0 instead of using static fallback power.",
        }
    }
}

fn telemetry_events(state: &Arc<AppState>) -> serde_json::Value {
    // DCENT_OS surfaces recent audit/share events; expose them in the
    // LuxOS `EVENTS` array shape. Read-only snapshot of existing data.
    let events: Vec<serde_json::Value> = state
        .recent_share_history
        .lock()
        .ok()
        .map(|h| {
            h.iter()
                .rev()
                .take(32)
                .map(|e| serde_json::to_value(e).unwrap_or(serde_json::json!({})))
                .collect()
        })
        .unwrap_or_default();
    envelope(
        serde_json::json!({ "EVENTS": events }),
        luxos_status(CgminerStatusCode::Version, "Events"),
    )
}

fn telemetry_systemaudit(state: &Arc<AppState>) -> serde_json::Value {
    let audit: Vec<serde_json::Value> = state
        .audit_ring
        .lock()
        .ok()
        .map(|r| {
            r.snapshot(64)
                .iter()
                .map(|e| serde_json::to_value(e).unwrap_or(serde_json::json!({})))
                .collect()
        })
        .unwrap_or_default();
    envelope(
        serde_json::json!({ "SYSTEMAUDIT": audit }),
        luxos_status(CgminerStatusCode::Version, "System audit"),
    )
}

fn telemetry_limits(state: &Arc<AppState>) -> serde_json::Value {
    // Parameter ranges. Voltage upper bound mirrors the load-bearing am2
    // chip-rail ceiling (14_500 mV) so fleet tools that read `limits`
    // before a `voltageset` already see the clamp.
    let _ = state;
    envelope(
        serde_json::json!({ "LIMITS": [{
            "voltage_mv": { "min": 5000, "max": 14500, "default": 13700 },
            "frequency_mhz": { "min": 100, "max": 900, "default": 500 },
            "fan_pwm": { "min": 0, "max": 100, "default": 30 },
            "_DCENTFieldSources": {
                "voltage_mv.max": "config.rs::validate am2 chip-rail ceiling (14500 mV)",
                "fan_pwm.default": "home-mode fan cap (PWM 30)"
            }
        }] }),
        luxos_status(CgminerStatusCode::MineConfig, "Limits"),
    )
}

fn telemetry_healthchip(state: &Arc<AppState>) -> serde_json::Value {
    let miner = state.state_rx.borrow().clone();
    let chips: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .map(|c| {
            serde_json::json!({
                "ID": c.id,
                "Chips": c.chips,
                "Frequency": c.frequency_mhz,
                "Temperature": c.temp_c,
                "Voltage": c.voltage_mv as f64 / 1000.0,
                "HardwareErrors": c.errors,
                "Status": c.status,
            })
        })
        .collect();
    envelope(
        serde_json::json!({ "HEALTHCHIP": chips }),
        luxos_status(CgminerStatusCode::Devs, "Chip health"),
    )
}

/// Expand a LuxOS `+`-batched command into its parts. Only PARAMETERLESS
/// commands may be batched (LuxOS contract §2). Returns `None` when the
/// raw command has no `+`.
pub fn expand_batch(command: &str) -> Option<Vec<String>> {
    if !command.contains('+') {
        return None;
    }
    Some(command.split('+').map(|s| s.trim().to_string()).collect())
}

/// Build a LuxOS multi-response object for a `+` batch: each sub-command's
/// result keyed by its own name, sharing one STATUS. Mirrors how LuxOS
/// returns `version+summary` etc.
pub fn merge_batch(parts: HashMap<String, serde_json::Value>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (_name, mut val) in parts {
        if let serde_json::Value::Object(m) = &mut val {
            for (k, v) in m.iter() {
                if k != "STATUS" && k != "id" {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }
    obj.insert(
        "STATUS".into(),
        serde_json::json!([luxos_status(CgminerStatusCode::Version, "Batch")]),
    );
    obj.insert("id".into(), serde_json::json!(1));
    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_8_alphanumeric() {
        for _ in 0..200 {
            let id = generate_session_id();
            assert_eq!(id.len(), SESSION_ID_LEN);
            assert!(id.chars().all(|c| c.is_ascii_alphanumeric()), "id={id}");
        }
    }

    #[test]
    fn single_session_mutex_blocks_second_logon() {
        let mgr = SessionManager::new();
        let a = mgr.logon().expect("first logon must succeed");
        assert!(mgr.logon().is_none(), "second logon must fail (mutex)");
        // After logoff, a fresh logon must succeed again.
        assert!(mgr.release(Some(&a), true));
        let b = mgr.logon().expect("logon after logoff must succeed");
        assert_ne!(a, b);
    }

    #[test]
    fn logoff_requires_matching_sid_kill_does_not() {
        let mgr = SessionManager::new();
        let sid = mgr.logon().unwrap();
        // wrong sid cannot logoff
        assert!(!mgr.release(Some("ZZZZZZZZ"), true));
        assert!(mgr.status().0, "session still active after bad logoff");
        // kill force-releases regardless of sid
        assert!(mgr.release(Some("ZZZZZZZZ"), false));
        assert!(!mgr.status().0, "kill must drop the session");
        let _ = sid;
    }

    #[test]
    fn session_check_rejects_missing_mismatch_and_nosession() {
        let mgr = SessionManager::new();
        assert_eq!(mgr.check(Some("abc")), SessionCheck::NoSession);
        let sid = mgr.logon().unwrap();
        assert_eq!(mgr.check(None), SessionCheck::Missing);
        assert_eq!(mgr.check(Some("")), SessionCheck::Missing);
        assert_eq!(mgr.check(Some("wrongsid")), SessionCheck::Mismatch);
        assert_eq!(mgr.check(Some(&sid)), SessionCheck::Valid);
    }

    #[test]
    fn expired_session_is_reaped_and_relogon_allowed() {
        let mgr = SessionManager::new();
        let sid = mgr.logon().unwrap();
        // Force expiry by back-dating last_seen past the TTL.
        {
            let mut slot = mgr.inner.lock().unwrap();
            let s = slot.as_mut().unwrap();
            s.last_seen = Instant::now() - SESSION_TTL - Duration::from_secs(1);
        }
        // A stale sid now reads as NoSession (reaped), and logon works.
        assert_eq!(mgr.check(Some(&sid)), SessionCheck::NoSession);
        assert!(mgr.logon().is_some(), "logon after expiry must succeed");
    }

    #[test]
    fn active_session_refreshes_ttl_on_use() {
        let mgr = SessionManager::new();
        let sid = mgr.logon().unwrap();
        {
            let mut slot = mgr.inner.lock().unwrap();
            slot.as_mut().unwrap().last_seen =
                Instant::now() - SESSION_TTL + Duration::from_secs(30);
        }
        // A valid check refreshes last_seen so the lock survives.
        assert_eq!(mgr.check(Some(&sid)), SessionCheck::Valid);
        {
            let slot = mgr.inner.lock().unwrap();
            let age = Instant::now().duration_since(slot.as_ref().unwrap().last_seen);
            assert!(age < Duration::from_secs(5), "last_seen must be refreshed");
        }
    }

    #[test]
    fn split_param_extracts_sid_first_then_args() {
        let (sid, rest) = split_param(&Some("abc12345,public-pool.io,worker,x".into()));
        assert_eq!(sid.as_deref(), Some("abc12345"));
        assert_eq!(rest, vec!["public-pool.io", "worker", "x"]);

        let (sid, rest) = split_param(&Some("  sid7777  ".into()));
        assert_eq!(sid.as_deref(), Some("sid7777"));
        assert!(rest.is_empty());

        let (sid, rest) = split_param(&None);
        assert_eq!(sid, None);
        assert!(rest.is_empty());

        // empty first token ⇒ no sid (Missing path)
        let (sid, _) = split_param(&Some(",foo".into()));
        assert_eq!(sid, None);
    }

    #[test]
    fn requires_session_matches_modeled_contract() {
        // Mutating verbs need a session…
        for v in [
            "addpool",
            "switchpool",
            "profileset",
            "autotunerset",
            "atmset",
            "powertargetset",
            "fanset",
            "tempctrlset",
            "curtail",
            "netset",
            "ledset",
            "voltageset",
            "frequencyset",
        ] {
            assert!(requires_session(v), "{v} must require a session");
        }
        // …read-class telemetry + lifecycle gateways do NOT.
        for v in [
            "metrics",
            "events",
            "systemaudit",
            "limits",
            "healthchipget",
        ] {
            assert!(!requires_session(v), "{v} must NOT require a session");
        }
    }

    #[test]
    fn luxos_metrics_power_fields_suppress_static_fallback() {
        let projection = crate::rest::PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };

        let fields = luxos_metrics_power_fields(&projection);
        assert_eq!(fields.board_watts, 0);
        assert_eq!(fields.wall_watts, 0);
        assert!(fields.note.contains("suppressed to 0"));
    }

    #[test]
    fn luxos_metrics_power_fields_keep_live_modeled_power_with_note() {
        let projection = crate::rest::PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "live".to_string(),
            source_detail: "live_runtime_model",
            live_power_available: true,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement.",
        };

        let fields = luxos_metrics_power_fields(&projection);
        assert_eq!(fields.board_watts, 1_000);
        assert_eq!(fields.wall_watts, 1_100);
        assert_eq!(fields.note, projection.note);
    }

    #[test]
    fn is_luxos_command_excludes_legacy_read_verbs() {
        // cgminer.rs still owns these — must not be claimed here.
        for legacy in [
            "summary", "stats", "pools", "devs", "version", "coin", "config",
        ] {
            assert!(!is_luxos_command(legacy), "{legacy} stays with cgminer.rs");
        }
        for owned in [
            "logon",
            "logoff",
            "session",
            "kill",
            "voltageset",
            "fanset",
            "curtail",
            "metrics",
        ] {
            assert!(is_luxos_command(owned), "{owned} must be handled here");
        }
    }

    #[test]
    fn expand_batch_only_splits_on_plus() {
        assert_eq!(expand_batch("version"), None);
        assert_eq!(
            expand_batch("version+summary+pools"),
            Some(vec![
                "version".to_string(),
                "summary".to_string(),
                "pools".to_string()
            ])
        );
    }

    #[test]
    fn luxos_envelope_shape_matches_contract() {
        let env = envelope(
            serde_json::json!({ "FOO": [{ "a": 1 }] }),
            luxos_status(CgminerStatusCode::Version, "ok"),
        );
        assert!(env.get("FOO").is_some());
        let status = &env["STATUS"][0];
        assert_eq!(status["STATUS"], "S");
        assert!(status.get("When").is_some());
        assert!(status.get("Code").is_some());
        assert!(status.get("Msg").is_some());
        assert_eq!(env["id"], 1);
    }

    #[test]
    fn error_envelope_is_e_status() {
        let env = error_envelope(CgminerStatusCode::MissingId, "nope");
        assert_eq!(env["STATUS"][0]["STATUS"], "E");
        assert_eq!(env["STATUS"][0]["Msg"], "nope");
    }

    #[test]
    fn from_delegated_surfaces_rest_error_as_luxos_e() {
        // A delegated handler that returned `{"status":"error",...}` must
        // become a LuxOS E status — fleet tools must SEE clamp refusals,
        // not a fake success. THIS is the contract that makes the safety
        // delegation observable end-to-end.
        let body = serde_json::json!({
            "status": "error",
            "message": "mining.voltage_mv (15000) exceeds am2 chip-rail ceiling 14500 mV"
        });
        let env = from_delegated("CONFIG", true, body, CgminerStatusCode::MineConfig);
        assert_eq!(env["STATUS"][0]["STATUS"], "E");
        assert!(env["STATUS"][0]["Msg"]
            .as_str()
            .unwrap()
            .contains("14500 mV"));

        let ok = serde_json::json!({ "status": "ok", "message": "saved" });
        let env = from_delegated("CONFIG", true, ok, CgminerStatusCode::MineConfig);
        assert_eq!(env["STATUS"][0]["STATUS"], "S");
    }

    #[test]
    fn merge_batch_collapses_subresponses_under_one_status() {
        let mut parts = HashMap::new();
        parts.insert(
            "version".to_string(),
            serde_json::json!({ "VERSION": [{ "API": "3.7" }], "STATUS": [{}], "id": 1 }),
        );
        parts.insert(
            "summary".to_string(),
            serde_json::json!({ "SUMMARY": [{ "MHS av": 1.0 }], "STATUS": [{}], "id": 1 }),
        );
        let merged = merge_batch(parts);
        assert!(merged.get("VERSION").is_some());
        assert!(merged.get("SUMMARY").is_some());
        assert!(merged["STATUS"].is_array());
        assert_eq!(merged["STATUS"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cmd_parse_round_trips_for_session_param() {
        // The LuxOS wire form `{"command":"voltageset","parameter":"sid,13800"}`
        // must split into sid + ["13800"].
        let cmd =
            CgMinerCommand::parse(r#"{"command":"voltageset","parameter":"sidABCDEF,13800"}"#)
                .unwrap();
        assert_eq!(cmd.command, "voltageset");
        let (sid, rest) = split_param(&cmd.parameter);
        assert_eq!(sid.as_deref(), Some("sidABCDEF"));
        assert_eq!(rest, vec!["13800"]);
    }
}
