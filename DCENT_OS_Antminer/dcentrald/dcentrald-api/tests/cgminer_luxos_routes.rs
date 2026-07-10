//! Integration tests for the LuxOS session model + mutating-command
//! contract layered on the CGMiner :4028 surface
//! (`dcentrald_api::cgminer_luxos` + `dcentrald_api::cgminer`).
//!
//! These drive the REAL dispatcher (`cgminer::handle_command_arc`) against
//! a minimal `AppState`, exactly as the TCP listener does.
//!
//! NOTE ON SERIALIZATION: the LuxOS single-session mutex is *process-wide*
//! by design (the CGMiner protocol is connect-per-command, so session
//! state cannot live on a connection — that IS the contract). All
//! dispatcher tests therefore acquire `SERIAL` first so they don't race
//! each other for the one global lock. This mirrors how a real fleet runs
//! it: one controller at a time.
//!
//! The headline test
//! (`cgminer_voltageset_goes_through_same_gated_config_path_no_bypass`) is
//! the LOAD-BEARING safety proof: a cgminer `voltageset` is session-gated
//! AND delegates to the SAME `rest::post_config` validated-reload path the
//! dashboard uses — there is no cgminer-specific voltage write, so the
//! `config.rs::validate()` am2 ≤14_500 mV clamp and the runtime autotuner
//! clamps cannot be bypassed via :4028.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use dcentrald_api::cgminer::{handle_command_arc, handle_command_from_peer, CgMinerCommand};
use dcentrald_api::cgminer_luxos::{global_session_manager, SessionManager};
use dcentrald_api::{
    build_minimal_app_state, ApiConfig, AppState, MinimalAppStateInputs, NetworkBlockConfig,
};

/// Process-wide serialization for the dispatcher tests (the global
/// session mutex is by-design shared, so the tests must not run in
/// parallel against it).
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    let m = SERIAL.get_or_init(|| Mutex::new(()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn make_state() -> Arc<AppState> {
    build_minimal_app_state(MinimalAppStateInputs {
        api_config: ApiConfig::default(),
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode: dcentrald_api_types::OperatingMode::Standard,
        firmware_version: "luxos-contract-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    })
}

/// Build a state whose `[api] cgminer_lan_writes` flag is set (API-4).
fn make_state_lan_writes(lan_writes: bool) -> Arc<AppState> {
    build_minimal_app_state(MinimalAppStateInputs {
        api_config: ApiConfig {
            cgminer_lan_writes: lan_writes,
            ..ApiConfig::default()
        },
        pool_url: String::new(),
        pool_protocol: "sv1".to_string(),
        mode: dcentrald_api_types::OperatingMode::Standard,
        firmware_version: "luxos-contract-test".to_string(),
        fan_pwm: 10,
        network_block: NetworkBlockConfig::default(),
        profile_path: "/tmp/profiles".to_string(),
        control_board_label: "test".to_string(),
        chip_type_label: "test".to_string(),
        external_state_rx: None,
    })
}

fn cmd(name: &str, param: Option<&str>) -> CgMinerCommand {
    CgMinerCommand {
        command: name.to_string(),
        parameter: param.map(|s| s.to_string()),
    }
}

/// Force-release any active session (kill semantics) so each serialized
/// test starts from a known no-session state.
fn clear_session() {
    let _ = global_session_manager().release(None, false);
}

/// Pull the "S"/"E"/… status letter out of a response. The CGMiner :4028
/// contract (cgminer-3.12.0 `_STATUS`) requires the inner key to be all-caps
/// `STATUS` so pyasic/hass-miner can read `data["STATUS"][0]["STATUS"]`. Both
/// the LuxOS layer and the legacy `CgMinerStatus` (via `#[serde(rename =
/// "STATUS")]`) now emit it all-caps — require it exactly, so the prior
/// PascalCase "Status" deviation cannot return.
fn status_letter(v: &serde_json::Value) -> Option<String> {
    let s0 = v.get("STATUS")?.get(0)?;
    s0.get("STATUS")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

fn logon_sid(state: &Arc<AppState>) -> String {
    futures_block(handle_command_arc(state, &cmd("logon", None)))["SESSION"][0]["SessionID"]
        .as_str()
        .expect("logon must return a SessionID")
        .to_string()
}

/// Tiny single-threaded block-on so the test bodies stay flat. (The
/// crate already pulls tokio; using a current-thread runtime keeps the
/// global session deterministic across awaits.)
fn futures_block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

// ─── Session lifecycle through the real dispatcher ──────────────────────

#[test]
fn logon_returns_8char_sessionid_and_mutex_blocks_second() {
    let _g = serial();
    clear_session();
    let state = make_state();

    let r = futures_block(handle_command_arc(&state, &cmd("logon", None)));
    let sid = r["SESSION"][0]["SessionID"]
        .as_str()
        .expect("logon must return a SessionID")
        .to_string();
    assert_eq!(sid.len(), 8, "SessionID must be 8 chars");
    assert!(sid.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(status_letter(&r).as_deref(), Some("S"));

    // Second logon must fail (single-session mutex).
    let r2 = futures_block(handle_command_arc(&state, &cmd("logon", None)));
    assert_eq!(
        status_letter(&r2).as_deref(),
        Some("E"),
        "second logon must be rejected by the single-session mutex"
    );

    // session reports active.
    let s = futures_block(handle_command_arc(&state, &cmd("session", None)));
    assert_eq!(s["SESSION"][0]["Session"], 1);

    // logoff with the right sid releases; a fresh logon then succeeds.
    let off = futures_block(handle_command_arc(&state, &cmd("logoff", Some(&sid))));
    assert_eq!(status_letter(&off).as_deref(), Some("S"));
    let r3 = futures_block(handle_command_arc(&state, &cmd("logon", None)));
    assert!(r3["SESSION"][0]["SessionID"].is_string());
    clear_session();
}

#[test]
fn kill_force_terminates_any_session() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let _ = futures_block(handle_command_arc(&state, &cmd("logon", None)));
    // kill with a bogus sid still drops the session (auth=None gateway).
    let k = futures_block(handle_command_arc(&state, &cmd("kill", Some("ZZZZZZZZ"))));
    assert_eq!(k["KILLED"], true);
    let s = futures_block(handle_command_arc(&state, &cmd("session", None)));
    assert_eq!(s["SESSION"][0]["Session"], 0);
    clear_session();
}

// ─── Session-gated mutation rejected without a valid session ────────────

#[test]
fn mutating_command_rejected_without_session() {
    let _g = serial();
    clear_session();
    let state = make_state();

    // No session at all.
    let r = futures_block(handle_command_arc(
        &state,
        &cmd("voltageset", Some("13800")),
    ));
    assert_eq!(
        status_letter(&r).as_deref(),
        Some("E"),
        "voltageset must be refused with no active session"
    );

    // Session exists but wrong sid supplied.
    let _ = futures_block(handle_command_arc(&state, &cmd("logon", None)));
    let r2 = futures_block(handle_command_arc(
        &state,
        &cmd("voltageset", Some("WRONGSID,13800")),
    ));
    assert_eq!(
        status_letter(&r2).as_deref(),
        Some("E"),
        "voltageset must be refused when the SessionID does not match"
    );
    clear_session();
}

#[test]
fn fanset_requires_session_first_param() {
    let _g = serial();
    clear_session();
    let state = make_state();
    // Missing sid entirely.
    let r = futures_block(handle_command_arc(&state, &cmd("fanset", Some("speed=20"))));
    assert_eq!(status_letter(&r).as_deref(), Some("E"));
    clear_session();
}

// ─── `+` batching (parameterless reads only) ────────────────────────────

#[test]
fn plus_batch_merges_parameterless_reads() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let r = futures_block(handle_command_arc(&state, &cmd("version+summary", None)));
    assert!(r.get("VERSION").is_some(), "batch must include version");
    assert!(r.get("SUMMARY").is_some(), "batch must include summary");
    assert!(r["STATUS"].is_array());
    assert_eq!(
        r["STATUS"].as_array().unwrap().len(),
        1,
        "batch shares one STATUS"
    );
}

#[test]
fn plus_batch_rejects_parameterized_command() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let r = futures_block(handle_command_arc(
        &state,
        &CgMinerCommand {
            command: "version+summary".to_string(),
            parameter: Some("x".to_string()),
        },
    ));
    assert_eq!(
        status_letter(&r).as_deref(),
        Some("E"),
        "parameterized batch must be rejected (LuxOS contract: only \
         parameterless commands batch). got: {r}"
    );
}

// ─── Legacy read verbs unaffected (byte-shape parity) ───────────────────

#[test]
fn legacy_read_verbs_keep_their_shape() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let v = futures_block(handle_command_arc(&state, &cmd("version", None)));
    assert!(v["VERSION"][0]["CGMiner"].is_string());
    let s = futures_block(handle_command_arc(&state, &cmd("summary", None)));
    assert!(s["SUMMARY"][0]["Elapsed"].is_number());
    let p = futures_block(handle_command_arc(&state, &cmd("pools", None)));
    assert!(p["POOLS"].is_array());
    // Unknown verb still errors with the canonical legacy envelope.
    let u = futures_block(handle_command_arc(
        &state,
        &cmd("definitely_not_a_command", None),
    ));
    assert_eq!(status_letter(&u).as_deref(), Some("E"));
}

// ─── Telemetry in LuxOS JSON shape (no session needed) ──────────────────

#[test]
fn telemetry_commands_are_open_and_luxos_shaped() {
    let _g = serial();
    clear_session();
    let state = make_state();
    for (name, key) in [
        ("metrics", "METRICS"),
        ("events", "EVENTS"),
        ("systemaudit", "SYSTEMAUDIT"),
        ("limits", "LIMITS"),
        ("healthchipget", "HEALTHCHIP"),
    ] {
        let r = futures_block(handle_command_arc(&state, &cmd(name, None)));
        assert!(
            r.get(key).is_some(),
            "{name} must return a {key} array (LuxOS shape)"
        );
        assert_eq!(
            status_letter(&r).as_deref(),
            Some("S"),
            "{name} must succeed"
        );
    }
    // `limits` must advertise the load-bearing am2 voltage ceiling so
    // fleet tools see the clamp before they ever issue a voltageset.
    let lim = futures_block(handle_command_arc(&state, &cmd("limits", None)));
    assert_eq!(lim["LIMITS"][0]["voltage_mv"]["max"], 14500);
}

// ─── LOAD-BEARING SAFETY PROOF ──────────────────────────────────────────

/// A cgminer `voltageset` MUST (a) be session-gated and (b) delegate to
/// the SAME `rest::post_config` validated-reload path the dashboard uses —
/// it must NOT touch a voltage register. We prove this structurally:
/// after a valid logon, `voltageset` produces a `CONFIG`-keyed response
/// (the signature of `rest::post_config`'s delegated body), NOT any
/// voltage-write acknowledgement. The actual `config.rs::validate()` am2
/// ≤14_500 mV clamp + the runtime autotuner clamp live downstream on that
/// exact path and are therefore unbypassable via :4028.
#[test]
fn cgminer_voltageset_goes_through_same_gated_config_path_no_bypass() {
    let _g = serial();
    clear_session();
    let state = make_state();

    // (a) Gated: no session ⇒ refused.
    let denied = futures_block(handle_command_arc(
        &state,
        &cmd("voltageset", Some("13800")),
    ));
    assert_eq!(
        status_letter(&denied).as_deref(),
        Some("E"),
        "voltageset without a session must be refused (no bypass of the \
         session gate)"
    );

    // (b) Authorized ⇒ delegates to rest::post_config. The response is
    // the CONFIG-shaped delegated body — proving the value went through
    // the config-write→validate-on-reload path, NOT a voltage register.
    let sid = logon_sid(&state);
    let r = futures_block(handle_command_arc(
        &state,
        &cmd("voltageset", Some(&format!("{sid},13800"))),
    ));
    assert!(
        r.get("CONFIG").is_some(),
        "voltageset MUST delegate to rest::post_config (CONFIG-keyed \
         response). A voltage-register write or a bespoke ack would be a \
         clamp bypass. got: {r}"
    );
    // The delegated body must NOT contain any voltage-controller / PIC /
    // devmem write acknowledgement — only a config-write outcome.
    let body = serde_json::to_string(&r).unwrap();
    assert!(
        !body.contains("set_voltage")
            && !body.contains("dsPIC")
            && !body.contains("devmem")
            && !body.contains("voltage_register"),
        "voltageset delegated body must NOT contain any direct \
         voltage-write surface. body={body}"
    );

    // An out-of-envelope voltage stays on the SAME gated path: either the
    // delegated config writer/validator rejects it (E) or it is persisted
    // for the on-reload validator — in BOTH cases the cgminer path
    // produced NO direct voltage write (the property under test) and the
    // response is always a structured CONFIG envelope, never a voltage
    // ack.
    let over = futures_block(handle_command_arc(
        &state,
        &cmd("voltageset", Some(&format!("{sid},19000"))),
    ));
    assert!(
        over.get("CONFIG").is_some() || status_letter(&over).as_deref() == Some("E"),
        "out-of-envelope voltageset must stay on the gated config path \
         (CONFIG-shaped or a visible E), never a voltage write. got: {over}"
    );
    clear_session();
}

/// `frequencyset` rides the same gated `rest::post_config` path (no
/// frequency register write on the cgminer surface).
#[test]
fn cgminer_frequencyset_uses_same_gated_config_path() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let sid = logon_sid(&state);
    let r = futures_block(handle_command_arc(
        &state,
        &cmd("frequencyset", Some(&format!("{sid},525"))),
    ));
    assert!(
        r.get("CONFIG").is_some(),
        "frequencyset must delegate to rest::post_config, got {r}"
    );
    clear_session();
}

/// `curtail` delegates to the existing curtailment controller (the SAME
/// path /api/action/sleep|wake uses) — not a bespoke power cut.
#[test]
fn cgminer_curtail_delegates_to_curtailment_controller() {
    let _g = serial();
    clear_session();
    let state = make_state();
    let sid = logon_sid(&state);
    let r = futures_block(handle_command_arc(
        &state,
        &cmd("curtail", Some(&format!("{sid},sleep"))),
    ));
    assert!(
        r.get("CURTAIL").is_some(),
        "curtail must delegate via the curtailment controller, got {r}"
    );
    clear_session();
}

// ─── Isolated SessionManager unit-level guarantees ──────────────────────

#[test]
fn session_manager_single_use_mutex_is_strict() {
    // Uses a *local* manager — no global, no serial guard needed.
    let mgr = SessionManager::new();
    let a = mgr.logon().expect("first logon");
    assert!(mgr.logon().is_none(), "mutex must block the 2nd logon");
    assert!(
        !mgr.release(Some("bad"), true),
        "wrong-sid logoff is a no-op"
    );
    assert!(mgr.release(Some(&a), true), "right-sid logoff releases");
    assert!(mgr.logon().is_some(), "re-logon after release works");
}

// ─── API-4: LAN-write gate on the peer-aware dispatcher ─────────────────
//
// `cgminer_bind_lan=true` is a documented MONITORING opt-in, but the same
// listener serves mutating LuxOS verbs gated only by a credential-less
// `logon`. The peer-aware entry (`handle_command_from_peer`) must refuse a
// mutating verb from a non-loopback peer unless `cgminer_lan_writes=true`,
// while leaving loopback control and LAN *reads* untouched.

const LAN_DENIED_NEEDLE: &str = "LAN writes disabled";

fn loopback_peer() -> SocketAddr {
    "127.0.0.1:54000".parse().unwrap()
}

fn lan_peer() -> SocketAddr {
    "203.0.113.77:54000".parse().unwrap()
}

/// A NON-loopback peer issuing a mutating verb (`voltageset`) with the gate
/// disabled (default) must be refused with the LAN-writes-disabled error —
/// BEFORE the session check (so even a would-be logon'd controller on LAN is
/// stopped at the gate). This is the regression: pre-fix, LAN monitoring
/// silently enabled LAN control.
#[test]
fn api4_lan_peer_mutation_refused_when_lan_writes_disabled() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("voltageset", Some("sid,13800")),
        lan_peer(),
    ));
    assert_eq!(
        status_letter(&r).as_deref(),
        Some("E"),
        "LAN voltageset must be refused when cgminer_lan_writes=false: {r}"
    );
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        msg.contains(LAN_DENIED_NEEDLE),
        "refusal must be the LAN-write gate (not a session error): {msg}"
    );
    clear_session();
}

/// The SAME mutating verb from a LOOPBACK peer is allowed past the LAN gate
/// (it then hits the normal session gate — it must NOT be the LAN-write
/// rejection). Local control is never blocked by this flag.
#[test]
fn api4_loopback_mutation_allowed_past_lan_gate() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("voltageset", Some("sid,13800")),
        loopback_peer(),
    ));
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        !msg.contains(LAN_DENIED_NEEDLE),
        "loopback mutation must pass the LAN gate (session gate may still \
         apply, but not the LAN-write refusal): {msg}"
    );
    clear_session();
}

/// With `cgminer_lan_writes=true`, a non-loopback peer is allowed past the
/// LAN gate (operator opt-in for a trusted LAN). It must NOT be the
/// LAN-write rejection; the normal session gate still applies downstream.
#[test]
fn api4_lan_peer_mutation_allowed_when_lan_writes_enabled() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(true);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("voltageset", Some("sid,13800")),
        lan_peer(),
    ));
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        !msg.contains(LAN_DENIED_NEEDLE),
        "with cgminer_lan_writes=true the LAN gate must NOT refuse the \
         mutation (session gate may still apply): {msg}"
    );
    clear_session();
}

/// A READ verb (`summary`) from a non-loopback peer is ALWAYS served, even
/// with the write gate disabled — LAN monitoring must keep working.
#[test]
fn api4_lan_peer_read_always_allowed() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("summary", None),
        lan_peer(),
    ));
    assert!(
        r["SUMMARY"][0]["Elapsed"].is_number(),
        "LAN summary read must be served regardless of the write gate: {r}"
    );
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        !msg.contains(LAN_DENIED_NEEDLE),
        "read must never be LAN-gated"
    );
    clear_session();
}

/// Cross-check: the loopback-equivalent `handle_command_arc` (the in-process
/// / test API) must produce the SAME result as `handle_command_from_peer`
/// with a loopback peer — i.e. the gate is a strict ADD on the LAN path, it
/// does not change loopback behavior.
#[test]
fn api4_loopback_peer_matches_handle_command_arc() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let a = futures_block(handle_command_from_peer(
        &state,
        &cmd("version", None),
        loopback_peer(),
    ));
    let b = futures_block(handle_command_arc(&state, &cmd("version", None)));
    assert_eq!(a["VERSION"][0]["CGMiner"], b["VERSION"][0]["CGMiner"]);
    assert_eq!(status_letter(&a), status_letter(&b));
    clear_session();
}

/// API-1 BLOCKER regression (`+`-batch bypass): a parameterless batch that
/// smuggles a mutating verb (`summary+restart`) from a NON-loopback peer with
/// cgminer_lan_writes=false MUST be refused. Pre-fix the whole-string gate
/// missed it (the token "summary+restart" matched no mutating verb) and the
/// batch loop dispatched the real `restart` — an unauthenticated LAN
/// denial-of-mining.
#[test]
fn api1_batch_restart_from_lan_peer_refused() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("summary+restart", None),
        lan_peer(),
    ));
    assert_eq!(
        status_letter(&r).as_deref(),
        Some("E"),
        "LAN summary+restart must be refused: {r}"
    );
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        msg.contains(LAN_DENIED_NEEDLE),
        "must be the LAN-write gate refusal (not a dispatched restart): {msg}"
    );
    clear_session();
}

/// Defense-in-depth: a batch containing a mutating verb is an INVALID command
/// for ANY caller (even loopback / in-process), because batches are read-only
/// by the LuxOS contract — so the dispatch loop can never reach a mutation.
#[test]
fn api1_batch_with_mutation_is_invalid_even_loopback() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_arc(&state, &cmd("summary+restart", None)));
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        msg.contains("cannot be batched"),
        "a batch with a mutating verb must be InvalidCommand: {r}"
    );
    clear_session();
}

/// API-1 MINOR regression: `kill` force-evicts ANY LuxOS session (including the
/// local operator's), so a NON-loopback peer must not be able to call it with
/// the gate disabled — remote denial-of-control.
#[test]
fn api1_kill_from_lan_peer_refused() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("kill", None),
        lan_peer(),
    ));
    assert_eq!(
        status_letter(&r).as_deref(),
        Some("E"),
        "LAN kill must be refused: {r}"
    );
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        msg.contains(LAN_DENIED_NEEDLE),
        "must be the LAN-write gate: {msg}"
    );
    clear_session();
}

/// No over-blocking: a legitimate read-only batch from a LAN peer is still
/// served when the gate is disabled (LAN monitoring keeps working).
#[test]
fn api1_readonly_batch_from_lan_allowed() {
    let _g = serial();
    clear_session();
    let state = make_state_lan_writes(false);

    let r = futures_block(handle_command_from_peer(
        &state,
        &cmd("summary+version", None),
        lan_peer(),
    ));
    let msg = r["STATUS"][0]["Msg"].as_str().unwrap_or_default();
    assert!(
        !msg.contains(LAN_DENIED_NEEDLE),
        "a read-only batch must not be LAN-gated: {r}"
    );
    clear_session();
}
