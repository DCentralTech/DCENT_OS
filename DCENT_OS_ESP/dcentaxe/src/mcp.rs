// DCENT_axe MCP Server — AI-Native Mining Control
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Model Context Protocol (MCP) server for AI-controlled mining.
// JSON-RPC 2.0 over HTTP POST /mcp
//
// Tools — canonical cross-firmware superset `dcent.cross-firmware.tuning.v1`
// (12 = 5 READ / 7 CONTROL; names == dcent-schema tuning_profile()):
//   READ:    get_status, get_device_info, get_swarm_status, get_network, get_history
//   CONTROL: identify_device, restart_mining, set_pool, set_frequency,
//            set_core_voltage, set_fan_speed, run_autotune
// Legacy aliases accepted INBOUND ONLY (never emitted): get_asic_info -> get_device_info,
//   get_swarm -> get_swarm_status. S5 convergence pin: dcentaxe-core s5_axe_mcp_superset_guards.
//
// Resources: bitaxe://status, bitaxe://config, bitaxe://history, bitaxe://swarm

use std::io::Write;

use dcent_schema::mcp::{minimal_profile, MINIMAL_PROFILE_ID};
use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::http::Method;
use log::*;
use serde_json::{json, Value};

use crate::auth;
use crate::shared::{AutotuneMode, SharedState};

const MCP_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "dcentaxe";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MCP_MAX_REQUEST_BYTES: usize = 2048;
const MCP_STATUS_RECENT_EVENT_LIMIT: usize = 8;

// ── MCP-6: /mcp request rate limiter ────────────────────────────────────────
// Device-wide fixed-window limiter mirroring auth.rs's LOGIN_TRACKER. Every
// POST /mcp otherwise runs up to ~4 NVS get_blob reads (authorize_mcp +
// authorize_mcp_control) plus a JSON parse before any short-circuit, so an
// unauthenticated burst can hold the shared NVS/config mutex and degrade the
// mining loop. State is reboot-clearing, which is fine for single-owner
// firmware. Window/limit are deliberately generous so legitimate AI agents and
// the dashboard are never throttled.
const MCP_RATE_WINDOW_SECS: u64 = 1;
const MCP_RATE_MAX_PER_WINDOW: u32 = 20;

struct McpRateLimiter {
    window_start: u64,
    count: u32,
}

static MCP_RATE_LIMITER: std::sync::Mutex<McpRateLimiter> = std::sync::Mutex::new(McpRateLimiter {
    window_start: 0,
    count: 0,
});

/// Pure fixed-window decision: given the current window state and `now`, returns
/// the updated state and whether this request is allowed. Factored out so the
/// window-roll / token-count math is reviewable in isolation from the static.
fn mcp_rate_limit_decide(
    window_start: u64,
    count: u32,
    now: u64,
    window_secs: u64,
    max_per_window: u32,
) -> (u64, u32, bool) {
    // Roll the window if it has never started or has elapsed.
    if window_start == 0 || now.saturating_sub(window_start) >= window_secs {
        // First request of a fresh window — always allowed.
        return (now, 1, true);
    }
    if count >= max_per_window {
        // Window still open and the budget is spent — reject without touching
        // the count (avoids unbounded growth under a sustained flood).
        return (window_start, count, false);
    }
    (window_start, count + 1, true)
}

fn mcp_now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns true if this POST /mcp may proceed; false if the rate limit is hit.
/// Fails OPEN if the limiter mutex is poisoned (recovers the inner state) — a
/// rate limiter must never become a self-inflicted lockout of the control plane.
fn mcp_rate_limit_allow() -> bool {
    let mut limiter = MCP_RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    let now = mcp_now_epoch_secs();
    let (window_start, count, allowed) = mcp_rate_limit_decide(
        limiter.window_start,
        limiter.count,
        now,
        MCP_RATE_WINDOW_SECS,
        MCP_RATE_MAX_PER_WINDOW,
    );
    limiter.window_start = window_start;
    limiter.count = count;
    allowed
}

#[derive(Clone, Copy)]
struct McpAuth {
    read_authorized: bool,
    read_denied_detail: Option<&'static str>,
    control_authorized: bool,
    control_denied_detail: Option<&'static str>,
}

fn auth_failure_detail(failure: &auth::AuthFailure) -> &'static str {
    match failure {
        auth::AuthFailure::Unauthorized(detail) | auth::AuthFailure::Forbidden(detail) => detail,
    }
}

fn jsonrpc_auth_error(detail: &'static str) -> Value {
    json!({"code": -32001, "message": "Unauthorized", "data": {"detail": detail}})
}

/// MCP-7: build the public GET /mcp discovery descriptor. The exact firmware
/// build version is a gratuitous fingerprinting aid (lets a scanner map the
/// device to known-vulnerable builds), so it is included ONLY for an authorized
/// reader (`include_version`). On a hardened device (owner password set) an
/// unauthenticated reader gets the protocol/profile/transport — everything a
/// real MCP client needs to discover and connect — but not the precise build.
/// The profile itself carries only the fixed MCP protocol version, no build id.
fn discovery_descriptor(include_version: bool) -> Value {
    let mut descriptor = json!({
        "name": SERVER_NAME,
        "protocol": MCP_VERSION,
        "transport": "http-jsonrpc",
        "profileId": MINIMAL_PROFILE_ID,
        "profile": minimal_profile("http-jsonrpc"),
    });
    if include_version {
        if let Some(obj) = descriptor.as_object_mut() {
            obj.insert("version".into(), json!(SERVER_VERSION));
        }
    }
    descriptor
}

/// Register the MCP endpoint on the HTTP server.
pub fn register_mcp(server: &mut EspHttpServer, state: SharedState) {
    let get_state = state.clone();
    server
        .fn_handler(
            "/mcp",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                // MCP-7: only an authorized reader (or an open, passwordless
                // device — authorize_mcp returns Ok when no password is set) sees
                // the precise build version. authorize_mcp does no NVS write and
                // only a cheap blob read, so this stays a lightweight GET.
                let include_version = auth::authorize_mcp(&req, &get_state).is_ok();
                let body = serde_json::to_string(&discovery_descriptor(include_version))
                    .unwrap_or_default();
                let mut resp =
                    req.into_response(200, None, &[("Content-Type", "application/json")])?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /mcp");

    server
        .fn_handler(
            "/mcp",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                // MCP-6: device-wide fixed-window rate limit, checked BEFORE the
                // two NVS-backed auth reads and the JSON parse, so a burst of
                // trivial POSTs can't serialize on the shared NVS/config mutex and
                // starve the mining/telemetry threads. Cheap reject, no auth, no
                // parse, no big alloc.
                if !mcp_rate_limit_allow() {
                    let response = jsonrpc_error(
                        -32000,
                        "Too Many Requests: rate limit exceeded",
                        Value::Null,
                    );
                    let body = serde_json::to_string(&response).unwrap_or_default();
                    let mut resp = req.into_response(
                        429,
                        Some("Too Many Requests"),
                        &[("Content-Type", "application/json")],
                    )?;
                    let _ = resp.write(body.as_bytes());
                    return Ok(());
                }

                let read_auth = auth::authorize_mcp(&req, &state);
                let control_auth = auth::authorize_mcp_control(&req, &state);
                let mcp_auth = McpAuth {
                    read_authorized: read_auth.is_ok(),
                    read_denied_detail: read_auth.as_ref().err().map(auth_failure_detail),
                    control_authorized: control_auth.is_ok(),
                    control_denied_detail: control_auth.as_ref().err().map(auth_failure_detail),
                };

                let content_len = req
                    .header("Content-Length")
                    .and_then(|h| h.parse::<usize>().ok())
                    .unwrap_or(0);

                // MCP-1: a single non-looping `req.read` truncates a multi-segment
                // body (ESP-IDF httpd's recv returns one chunk, not the whole body),
                // turning valid JSON-RPC into a -32700 Parse error. Accumulate up to
                // the hard cap, mirroring the OTA/provisioning bounded read loop, and
                // REJECT (not truncate) anything over the cap.
                let response = if content_len > MCP_MAX_REQUEST_BYTES {
                    Some(jsonrpc_error(
                        -32600,
                        "Invalid Request: request body too large",
                        Value::Null,
                    ))
                } else {
                    match read_mcp_body(&mut req, MCP_MAX_REQUEST_BYTES) {
                        Ok(body) => match serde_json::from_slice::<Value>(&body) {
                            Ok(request) => handle_jsonrpc(&state, &request, mcp_auth),
                            Err(e) => Some(jsonrpc_error(
                                -32700,
                                format!("Parse error: {}", e),
                                Value::Null,
                            )),
                        },
                        Err(McpBodyError::TooLarge) => Some(jsonrpc_error(
                            -32600,
                            "Invalid Request: request body too large",
                            Value::Null,
                        )),
                        Err(McpBodyError::Io(detail)) => Some(jsonrpc_error(
                            -32700,
                            format!("Parse error: body read failed: {}", detail),
                            Value::Null,
                        )),
                    }
                };

                // MCP-2: a JSON-RPC Notification (no `id` member) MUST NOT receive a
                // response. `handle_jsonrpc` returns `None` for those; emit an empty
                // 202 Accepted body so the side effects ran but no response frame is
                // sent. Everything else gets the normal 200 JSON-RPC frame.
                match response {
                    Some(response) => {
                        let body = serde_json::to_string(&response).unwrap_or_default();
                        let mut resp =
                            req.into_response(200, None, &[("Content-Type", "application/json")])?;
                        let _ = resp.write(body.as_bytes());
                    }
                    None => {
                        // No body frame for a notification — 202 Accepted, empty.
                        let headers: &[(&str, &str)] = &[];
                        let _ = req.into_response(202, None, headers)?;
                    }
                }
                Ok(())
            },
        )
        .expect("Failed to register POST /mcp");

    info!("MCP server registered at /mcp");
}

enum McpBodyError {
    /// The accumulated body exceeded the hard cap — reject, never truncate.
    TooLarge,
    /// A non-timeout transport error occurred mid-read.
    Io(String),
}

/// MCP-1: read the full request body, looping until EOF or the `max` cap,
/// retrying on the esp-idf `Timeout` pseudo-error exactly like the OTA /
/// provisioning receive loops. A single `req.read` only returns the first recv
/// chunk, so a body split across TCP segments would otherwise be silently
/// truncated and a valid JSON-RPC request rejected as a parse error.
fn read_mcp_body(
    req: &mut esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
    max: usize,
) -> Result<Vec<u8>, McpBodyError> {
    // `req.read(...)` resolves to the same `embedded_svc::io::Read`-backed method
    // the original single-read used and that provisioning.rs's read_full_body
    // loops on; no extra `use` is needed (matching those call sites).
    let mut body: Vec<u8> = Vec::new();
    let mut scratch = [0u8; 512];
    loop {
        let remaining = max.saturating_sub(body.len());
        if remaining == 0 {
            // At the cap — probe one more byte. More data ⇒ over-cap ⇒ reject;
            // EOF ⇒ it fit exactly.
            match req.read(&mut scratch[..1]) {
                Ok(0) => break,
                Ok(_) => return Err(McpBodyError::TooLarge),
                Err(e) => {
                    if format!("{e}").contains("Timeout") {
                        continue;
                    }
                    return Err(McpBodyError::Io(format!("{e}")));
                }
            }
        }
        let take = remaining.min(scratch.len());
        match req.read(&mut scratch[..take]) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if body.len().saturating_add(n) > max {
                    return Err(McpBodyError::TooLarge);
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => {
                if format!("{e}").contains("Timeout") {
                    continue;
                }
                return Err(McpBodyError::Io(format!("{e}")));
            }
        }
    }
    Ok(body)
}

/// Returns `None` for a JSON-RPC Notification (a request object with no `id`
/// member), which per JSON-RPC 2.0 §4.1 MUST NOT receive a response. Returns
/// `Some(value)` for every request that must be answered with a frame.
fn handle_jsonrpc(state: &SharedState, request: &Value, auth: McpAuth) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(jsonrpc_error(
            -32600,
            "Invalid Request: request must be an object",
            Value::Null,
        ));
    };
    // A Notification is a request object with NO `id` member (distinct from an
    // explicit `"id": null`). It MUST NOT receive a response — we still run the
    // method for its side effects below, but emit no frame (`None`).
    let is_notification = jsonrpc_is_notification(object);
    if !matches!(
        object.get("id"),
        None | Some(Value::Null) | Some(Value::String(_)) | Some(Value::Number(_))
    ) {
        return Some(jsonrpc_error(
            -32600,
            "Invalid Request: id must be string, number, or null",
            Value::Null,
        ));
    }
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        let err = jsonrpc_error(-32600, "Invalid Request: jsonrpc must be \"2.0\"", id);
        return if is_notification { None } else { Some(err) };
    }
    let Some(method) = object.get("method").and_then(|m| m.as_str()) else {
        let err = jsonrpc_error(-32600, "Invalid Request: method must be a string", id);
        return if is_notification { None } else { Some(err) };
    };
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = match method {
        // MCP protocol methods
        "initialize" => handle_initialize(),
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tool_call(state, &params, auth),
        "resources/list" => handle_resources_list(),
        "resources/read" => handle_resource_read(state, &params, auth),
        "notifications/initialized" => Ok(json!({})),
        "ping" => Ok(json!({})),

        _ => Err(jsonrpc_method_not_found(method)),
    };

    // MCP-2: notifications get no response frame even though the method ran.
    if is_notification {
        return None;
    }

    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "result": result, "id": id}),
        Err(error) => json!({"jsonrpc": "2.0", "error": error, "id": id}),
    })
}

/// MCP-2 pure decision: a JSON-RPC request is a Notification iff it carries no
/// `id` member at all. An explicit `"id": null` is a normal request that wants
/// a response with `id: null`.
fn jsonrpc_is_notification(object: &serde_json::Map<String, Value>) -> bool {
    !object.contains_key("id")
}

/// MCP-5: build a `-32601 Method not found` error for an unknown top-level
/// JSON-RPC method.
fn jsonrpc_method_not_found(method: &str) -> Value {
    json!({"code": -32601, "message": format!("Method not found: {}", method)})
}

fn jsonrpc_error(code: i32, message: impl Into<String>, id: Value) -> Value {
    json!({"jsonrpc": "2.0", "error": {"code": code, "message": message.into()}, "id": id})
}

fn mcp_tool_requires_control(name: &str) -> bool {
    matches!(
        name,
        "set_frequency"
            | "set_core_voltage"
            | "set_fan_speed"
            | "set_pool"
            | "restart_mining"
            | "identify_device"
            | "run_autotune"
    )
}

/// data-model-fields §3: a control tool that drives a physical actuator
/// (ASIC freq/voltage, PSU/fan) sets `hardware_writes:true` in the self-describing
/// response envelope, distinguishing it from a control tool that only mutates
/// config/state (set_pool persists config; identify_device toggles a soft flag).
/// Derived from the same name-set as the control classifier — descriptive only,
/// drives NO behavior and is NOT an auth-posture change.
fn mcp_tool_writes_hardware(name: &str) -> bool {
    matches!(
        name,
        "set_frequency" | "set_core_voltage" | "set_fan_speed" | "restart_mining" | "run_autotune"
    )
}

// ==== LORA-MCP-REGION BEGIN (feature = "lora") ==============================
// ALL LoRa `/mcp` wiring lives inside this ONE cfg-gated region so a non-LoRa
// image's tools/list + tools/call dispatch are BYTE-IDENTICAL to today (project
//  "no lying UI" — a non-functional advertised tool would be a lying
// surface). The two call sites (`handle_tools_list` / `handle_tool_call`) reference
// only `lora_mcp::*` helpers — NEVER a raw lora tool-name string — so the tool
// names appear ONLY inside this region (pinned by dcentaxe-core
// `mcp_lora_contract_guards::lora_mcp_names_only_in_cfg_region`).
#[cfg(feature = "lora")]
mod lora_mcp {
    use super::{jsonrpc_auth_error, McpAuth};
    use crate::shared::SharedState;
    use dcentaxe_lora::mcp::tools;
    use serde_json::{json, Value};

    /// True for any of the three LoRa MCP tools (`lora_status`, `lora_send_beacon`,
    /// `get_mesh_peers`) — the single source of the name set is the shared crate.
    pub fn is_lora_tool(name: &str) -> bool {
        tools().iter().any(|t| t.name == name)
    }

    /// Per-tool JSON-RPC input schema.
    fn input_schema(name: &str) -> Value {
        if name == "lora_send_beacon" {
            json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string",
                             "enum": ["block_found", "identify", "telemetry", "custom"],
                             "description": "Beacon kind to broadcast over the $DCM mesh"},
                    "message": {"type": "string",
                                "description": "Free text for kind=custom (ignored otherwise)"}
                },
                "required": ["kind"]
            })
        } else {
            json!({"type": "object", "properties": {}})
        }
    }

    /// Append the LoRa tool descriptors to the tools/list array, each carrying the
    /// correct `annotations.readOnlyHint` (read tool → true; `lora_send_beacon`, the
    /// single mutating tool → false). Baked in here rather than derived by the
    /// generic annotation loop (which reads `mcp_tool_requires_control`, unaware of
    /// lora tools) so the owner-control tool is never mis-annotated read-only.
    pub fn append_tools(list: &mut Vec<Value>) {
        for t in tools() {
            let read_only = !t.access.requires_auth();
            list.push(json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": input_schema(t.name),
                "annotations": {"readOnlyHint": read_only},
            }));
        }
    }

    /// Dispatch a LoRa tool. `lora_send_beacon` is OWNER-CONTROL: refused fail-closed
    /// unless `authorize_mcp_control()` passed (`auth.control_authorized`) — identical
    /// posture to the 7 built-in control tools — and it routes through the region
    /// duty governor (honest `queued:false, reason:"duty_budget"` on clamp). The two
    /// reads read live `Sx1262`/`PeerTable` state (read auth already enforced by the
    /// caller's pre-dispatch gate).
    pub fn dispatch(
        state: &SharedState,
        name: &str,
        args: &Value,
        auth: McpAuth,
    ) -> Result<Value, Value> {
        let result = match name {
            "lora_status" => crate::lora_task::status_json(),
            "get_mesh_peers" => crate::lora_task::mesh_peers_json(),
            "lora_send_beacon" => {
                if !auth.control_authorized {
                    return Err(jsonrpc_auth_error(
                        auth.control_denied_detail
                            .unwrap_or("Bearer session required for MCP control tools"),
                    ));
                }
                let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                crate::lora_task::request_beacon(state, kind, message)
            }
            _ => return Err(json!({"code": -32601, "message": format!("Unknown tool: {name}")})),
        };
        // Same self-describing 3-flag envelope the built-in tools use.
        let is_control = name == "lora_send_beacon";
        Ok(json!({
            "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}],
            "read_only": !is_control,
            "control_actions": is_control,
            "hardware_writes": is_control,
        }))
    }
}
// ==== LORA-MCP-REGION END ====================================================

fn handle_initialize() -> Result<Value, Value> {
    Ok(json!({
        "protocolVersion": MCP_VERSION,
        "capabilities": {
            "tools": {},
            "resources": {}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
            "profileId": MINIMAL_PROFILE_ID
        },
        "profile": minimal_profile("http-jsonrpc")
    }))
}

fn handle_tools_list() -> Result<Value, Value> {
    let mut listing = tools_list_descriptor();
    // data-model-fields §3: make the read/control split SELF-DESCRIBING on the
    // wire by stamping `annotations.readOnlyHint` per tool, derived purely from
    // the existing `mcp_tool_requires_control` classifier (NO new auth logic;
    // posture unchanged). Read tools => readOnlyHint:true; the 7 control tools
    // (set_frequency/set_core_voltage/set_fan_speed/set_pool/restart_mining/
    // identify_device/run_autotune) => readOnlyHint:false. Mirrors DCENT_OS's
    // `mcp_read_tool_descriptors` annotations EXACTLY so a cross-firmware MCP
    // consumer reads the same structure on both sides.
    if let Some(tools) = listing
        .get_mut("tools")
        .and_then(|tools| tools.as_array_mut())
    {
        for tool in tools.iter_mut() {
            let read_only = tool
                .get("name")
                .and_then(|name| name.as_str())
                .map(|name| !mcp_tool_requires_control(name))
                .unwrap_or(true);
            if let Some(obj) = tool.as_object_mut() {
                obj.insert("annotations".into(), json!({ "readOnlyHint": read_only }));
            }
        }
        // LoRa mesh tools (feature-gated) are appended WITH annotations already
        // baked in, so the generic readOnlyHint loop above never mis-annotates the
        // owner-control `lora_send_beacon`. Compiled out entirely when `lora` is
        // OFF ⇒ the tools/list is byte-identical to a non-LoRa build.
        #[cfg(feature = "lora")]
        lora_mcp::append_tools(tools);
    }
    Ok(listing)
}

fn tools_list_descriptor() -> Value {
    json!({
        "tools": [
            {
                "name": "get_status",
                "description": "Get current miner status: hashrate, temperature, power, shares, uptime",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "get_device_info",
                "description": "Get device identity, board target, and ASIC metadata",
                "inputSchema": {"type": "object", "properties": {}}
            },
            // NOTE: `get_asic_info` is the legacy alias of `get_device_info`
            // (dcent-schema minimal_profile() + mcp-auth-contract.md §2.1 hard
            // rule: "the canonical tool name is the only name a surface EMITS").
            // It is still ACCEPTED inbound (see handle_tool_call dispatch), but is
            // no longer emitted as a standalone tools/list entry. Do NOT re-add it
            // here; do NOT delete the inbound match arm (back-compat for old
            // agents/toolbox callers).
            {
                "name": "set_frequency",
                "description": "Set ASIC hash frequency in MHz",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "frequency_mhz": {"type": "number", "description": "Target frequency in MHz (50-650 depending on model)"}
                    },
                    "required": ["frequency_mhz"]
                }
            },
            {
                "name": "set_core_voltage",
                "description": "Set ASIC core voltage in millivolts",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "voltage_mv": {"type": "integer", "description": "Core voltage in mV (800-2000 depending on model)"}
                    },
                    "required": ["voltage_mv"]
                }
            },
            {
                "name": "set_fan_speed",
                "description": "Set fan speed as percentage (0-100)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "percent": {"type": "integer", "description": "Fan speed 0-100%"}
                    },
                    "required": ["percent"]
                }
            },
            {
                "name": "set_pool",
                "description": "Configure mining pool connection",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Pool hostname"},
                        "port": {"type": "integer", "description": "Pool port"},
                        "worker": {"type": "string", "description": "Worker name (BTC address)"},
                        "password": {"type": "string", "description": "Pool password"}
                    },
                    "required": ["url", "port", "worker"]
                }
            },
            {
                "name": "get_network",
                "description": "Get WiFi network information: SSID, IP, signal strength",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "get_history",
                "description": "Get recent mining performance history (1min snapshots)",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "restart_mining",
                "description": "Restart the mining process (re-init ASIC, reconnect to pool)",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "identify_device",
                "description": "Toggle the physical identify signal for the device",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "run_autotune",
                "description": "Start or stop the autotuner with a specific optimization target",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "enabled": {"type": "boolean", "description": "Enable or disable autotuner"},
                        "mode": {"type": "string", "enum": ["max_hashrate", "target_watts", "best_efficiency", "target_temp"],
                                 "description": "Optimization mode"},
                        "target": {"type": "number", "description": "Target value (watts for target_watts, celsius for target_temp)"}
                    },
                    "required": ["enabled"]
                }
            },
            {
                "name": "get_swarm_status",
                "description": "Get shared swarm and discovery metadata",
                "inputSchema": {"type": "object", "properties": {}}
            }
            // NOTE: `get_swarm` is the legacy alias of `get_swarm_status`
            // (dcent-schema minimal_profile() + mcp-auth-contract.md §2.1 hard
            // rule: emit only the canonical name). It is still ACCEPTED inbound
            // (see handle_tool_call dispatch) but is no longer emitted as a
            // standalone tools/list entry. Do NOT re-add it here; do NOT delete
            // the inbound match arm (back-compat).
        ]
    })
}

fn handle_tool_call(state: &SharedState, params: &Value, auth: McpAuth) -> Result<Value, Value> {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    if mcp_tool_requires_control(name) {
        if !auth.control_authorized {
            return Err(jsonrpc_auth_error(
                auth.control_denied_detail
                    .unwrap_or("Bearer session required for MCP control tools"),
            ));
        }
    } else if !auth.read_authorized {
        return Err(jsonrpc_auth_error(
            auth.read_denied_detail
                .unwrap_or("Bearer session required for MCP read tools"),
        ));
    }

    let result = match name {
        "get_status" => tool_get_status(state),
        "get_device_info" => tool_get_asic_info(state),
        "get_asic_info" => tool_get_asic_info(state),
        "set_frequency" => tool_set_frequency(state, &args),
        "set_core_voltage" => tool_set_core_voltage(state, &args),
        "set_fan_speed" => tool_set_fan_speed(state, &args),
        "set_pool" => tool_set_pool(state, &args),
        "get_network" => tool_get_network(state),
        "get_history" => tool_get_history(state),
        "restart_mining" => tool_restart_mining(),
        "identify_device" => tool_identify_device(state),
        "run_autotune" => tool_run_autotune(state, &args),
        "get_swarm_status" => tool_get_swarm(state),
        "get_swarm" => tool_get_swarm(state),
        // LoRa mesh tools (feature-gated). Routed to the cfg-gated `lora_mcp`
        // region so no lora tool-name string leaks into the un-gated dispatch;
        // `lora_send_beacon` enforces `authorize_mcp_control()` inside dispatch.
        // Compiled out entirely when `lora` is OFF ⇒ dispatch is byte-identical.
        #[cfg(feature = "lora")]
        n if lora_mcp::is_lora_tool(n) => return lora_mcp::dispatch(state, n, &args, auth),
        // MCP-5: an unrecognized tool name is "method not found" (-32601), not
        // an invalid-arguments error. -32602 is reserved for bad arguments to a
        // tool that DOES exist. Matches the top-level method dispatch convention.
        _ => return Err(json!({"code": -32601, "message": format!("Unknown tool: {}", name)})),
    };

    // data-model-fields §3: mirror DCENT_OS's 3-flag read/control envelope
    // {read_only, control_actions, hardware_writes} onto the tools/call result,
    // derived purely from the existing classifiers. A read tool reports
    // read_only:true/control_actions:false/hardware_writes:false; a control tool
    // reports control_actions:true (+ hardware_writes:true for ASIC/PSU/fan
    // actuators). This is contract-STRUCTURE alignment only — it adds keys
    // ALONGSIDE the existing `content[]` payload (never altering it), drives no
    // behavior, and does NOT change axe's control-first open-device posture
    // (the standing operator decision). axe stays self-describing in the SAME
    // structure as OS without converging the enforcement posture.
    let is_control = mcp_tool_requires_control(name);
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}],
        "read_only": !is_control,
        "control_actions": is_control,
        "hardware_writes": is_control && mcp_tool_writes_hardware(name),
    }))
}

fn tool_get_status(state: &SharedState) -> Value {
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let snap = stats.snapshot();
    let stratum_statuses = crate::shared::stratum_status_snapshots_with_recent_event_limit(
        state,
        MCP_STATUS_RECENT_EVENT_LIMIT,
    );
    let active_status = stratum_statuses
        .iter()
        .find(|status| status.connected)
        .or(stratum_statuses.first());
    let pools: Vec<Value> = stratum_statuses
        .iter()
        .map(|status| {
            let accounting_gap = status.shares_submitted as i128
                - status.shares_accepted as i128
                - status.shares_rejected as i128
                - status.shares_pending as i128
                - status.shares_unresolved as i128;
            json!({
                "index": status.pool_index,
                "connected": status.connected,
                "authorized": status.authorized,
                "active_pool": format!("{}:{}", crate::shared::sanitize_pool_url(&status.active_url), status.active_port),
                "difficulty": status.difficulty,
                "shares_submitted": status.shares_submitted,
                "shares_accepted": status.shares_accepted,
                "shares_rejected": status.shares_rejected,
                "shares_pending": status.shares_pending,
                "shares_unresolved": status.shares_unresolved,
                "oldest_pending_submit_age_ms": status.oldest_pending_submit_age_ms,
                "shares_accounting_gap": accounting_gap,
                "last_share_response_ms": status.last_share_response_ms,
                "last_reject_reason": status.last_reject_reason.clone(),
                "failover_active": status.failover_active,
            })
        })
        .collect();
    let pool_truth = active_status.map(|status| {
        let accounting_gap = status.shares_submitted as i128
            - status.shares_accepted as i128
            - status.shares_rejected as i128
            - status.shares_pending as i128
            - status.shares_unresolved as i128;
        json!({
            // B-ESP-10: strip any `user:pass@` creds from the active pool URL,
            // matching the sibling pools[] entry above.
            "active_pool": format!("{}:{}", crate::shared::sanitize_pool_url(&status.active_url), status.active_port),
            "connected": status.connected,
            "difficulty": status.difficulty,
            "shares_submitted": status.shares_submitted,
            "shares_accepted": status.shares_accepted,
            "shares_rejected": status.shares_rejected,
            "shares_pending": status.shares_pending,
            "shares_unresolved": status.shares_unresolved,
            "oldest_pending_submit_age_ms": status.oldest_pending_submit_age_ms,
            "shares_accounting_gap": accounting_gap,
            "response_time_ms": status.last_share_response_ms,
            "response_time_unix_ms": status.last_share_response_unix_ms,
            "last_reject_reason": status.last_reject_reason.clone(),
            "reject_reason_counts": status.reject_reason_counts.clone(),
            "recent_events": status.recent_events.clone(),
            "failover_active": status.failover_active,
        })
    });
    json!({
        "hashrate_ghs": snap.hashrate_5m_ghs,
        "hashrate_1m_ghs": snap.hashrate_1m_ghs,
        "hashrate_15m_ghs": snap.hashrate_15m_ghs,
        "shares_accepted": snap.accepted_shares,
        "shares_rejected": snap.rejected_shares,
        "filtered_nonces": snap.filtered_shares,
        "stale_nonces": snap.stale_nonces,
        "slot_recoveries": snap.slot_recoveries,
        "ticket_difficulty": snap.ticket_difficulty,
        "shares_submitted": active_status.map(|status| status.shares_submitted).unwrap_or(0),
        "pool_shares_accepted": active_status.map(|status| status.shares_accepted).unwrap_or(snap.accepted_shares),
        "pool_shares_rejected": active_status.map(|status| status.shares_rejected).unwrap_or(snap.rejected_shares),
        "pool_shares_pending": active_status.map(|status| status.shares_pending).unwrap_or(0),
        "pool_shares_unresolved": active_status.map(|status| status.shares_unresolved).unwrap_or(0),
        "pool_oldest_pending_submit_age_ms": active_status.map(|status| status.oldest_pending_submit_age_ms).unwrap_or(0),
        "pool_connected": active_status.map(|status| status.connected).unwrap_or(telem.pool_connected),
        "failover_active": active_status.map(|status| status.failover_active).unwrap_or(false),
        "pool_response_ms": active_status.map(|status| status.last_share_response_ms).unwrap_or(0.0),
        "last_reject_reason": active_status.map(|status| status.last_reject_reason.clone()).unwrap_or_default(),
        "pool_truth": pool_truth.unwrap_or_else(|| json!({})),
        "pools": pools,
        "best_difficulty": snap.best_difficulty,
        "uptime_seconds": snap.uptime_secs,
        "reset_reason": telem.reset_reason.clone(),
        "safe_mode": telem.safe_mode,
        "wdt_reset_count": telem.wdt_reset_count,
        "coredump_present": telem.coredump_present,
        "temperature_c": telem.chip_temp_c,
        "board_temp_c": telem.board_temp_c,
        "power_watts": telem.power_w,
        "voltage_mv": telem.voltage_mv,
        "fan_speed_pct": telem.fan_speed_pct,
        "efficiency_jth": if snap.hashrate_5m_ghs > 0.0 { telem.power_w as f64 / snap.hashrate_5m_ghs * 1000.0 } else { 0.0 }
    })
}

fn tool_get_asic_info(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board = config.board_config();
    json!({
        "model": board.device_model,
        "board_version": board.board_version,
        "board_target": config.board_target(),
        "asic_model": board.asic_model,
        "asic_count": if config.asic_count > 0 { config.asic_count } else { config.expected_asic_count() },
        "frequency_mhz": config.target_frequency,
        "voltage_mv": config.target_voltage_mv,
    })
}

fn tool_set_frequency(state: &SharedState, args: &Value) -> Value {
    if let Some(freq) = args.get("frequency_mhz").and_then(|v| v.as_f64()) {
        if let Ok(mut autotune) = state.autotuner.lock() {
            if autotune.enabled {
                info!("MCP: disabling autotuner due to manual frequency override");
                autotune.enabled = false;
                autotune.status = "manual override".to_string();
            }
        }
        let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let qualified = config.qualify_operating_point(
            freq as f32,
            config.target_voltage_mv,
            crate::config::ControlSurface::Mcp,
        );
        config.target_frequency = qualified.frequency_mhz;
        // Persist to NVS
        if let Ok(mut nvs_guard) = state.nvs.lock() {
            if let Some(ref mut nvs) = *nvs_guard {
                if let Err(e) = crate::nvs_config::save_config(nvs, &config) {
                    error!("MCP: NVS save failed after set_frequency: {}", e);
                } else {
                    info!("MCP: config saved to NVS (set_frequency)");
                }
            } else {
                warn!("MCP: NVS handle not available — set_frequency not persisted");
            }
        }
        json!({"success": true, "frequency_mhz": qualified.frequency_mhz, "voltage_mv": qualified.voltage_mv, "clamped": qualified.clamped, "note": "Will apply on next work dispatch"})
    } else {
        json!({"success": false, "error": "frequency_mhz required"})
    }
}

fn tool_set_core_voltage(state: &SharedState, args: &Value) -> Value {
    if let Some(mv) = args.get("voltage_mv").and_then(|v| v.as_u64()) {
        let Ok(mv) = u16::try_from(mv) else {
            return json!({"success": false, "error": "voltage_mv out of range"});
        };
        if let Ok(mut autotune) = state.autotuner.lock() {
            if autotune.enabled {
                info!("MCP: disabling autotuner due to manual voltage override");
                autotune.enabled = false;
                autotune.status = "manual override".to_string();
            }
        }
        let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let qualified = config.qualify_operating_point(
            config.target_frequency,
            mv,
            crate::config::ControlSurface::Mcp,
        );
        config.target_voltage_mv = qualified.voltage_mv;
        // Persist to NVS
        if let Ok(mut nvs_guard) = state.nvs.lock() {
            if let Some(ref mut nvs) = *nvs_guard {
                if let Err(e) = crate::nvs_config::save_config(nvs, &config) {
                    error!("MCP: NVS save failed after set_core_voltage: {}", e);
                } else {
                    info!("MCP: config saved to NVS (set_core_voltage)");
                }
            } else {
                warn!("MCP: NVS handle not available — set_core_voltage not persisted");
            }
        }
        json!({"success": true, "frequency_mhz": qualified.frequency_mhz, "voltage_mv": qualified.voltage_mv, "clamped": qualified.clamped, "note": "Will apply on next heartbeat"})
    } else {
        json!({"success": false, "error": "voltage_mv required"})
    }
}

fn tool_set_fan_speed(state: &SharedState, args: &Value) -> Value {
    if let Some(pct) = args.get("percent").and_then(|v| v.as_u64()) {
        let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        config.fan_speed_pct = pct.clamp(20, 100) as u8;
        config.fan_target_temp_c = 0;
        // Persist to NVS
        if let Ok(mut nvs_guard) = state.nvs.lock() {
            if let Some(ref mut nvs) = *nvs_guard {
                if let Err(e) = crate::nvs_config::save_config(nvs, &config) {
                    error!("MCP: NVS save failed after set_fan_speed: {}", e);
                } else {
                    info!("MCP: config saved to NVS (set_fan_speed)");
                }
            } else {
                warn!("MCP: NVS handle not available — set_fan_speed not persisted");
            }
        }
        json!({"success": true, "fan_speed_pct": config.fan_speed_pct})
    } else {
        json!({"success": false, "error": "percent required"})
    }
}

/// Maximum accepted lengths for set_pool string fields, well under the 2KB MCP
/// request cap and the NVS config budget.
const MCP_POOL_URL_MAX_LEN: usize = 255;
const MCP_POOL_WORKER_MAX_LEN: usize = 255;
const MCP_POOL_PASSWORD_MAX_LEN: usize = 255;

/// A validated set_pool change set. Only the fields the caller supplied are
/// `Some`; all are pre-validated so applying them to the live config cannot
/// half-apply or persist garbage.
struct PoolUpdate {
    url: Option<String>,
    port: Option<u16>,
    worker: Option<String>,
    password: Option<String>,
}

/// MCP-3 pure validation: reject empty/whitespace/over-long url and worker and a
/// zero/out-of-range port BEFORE any field is applied to the live config. A
/// prompt-injected or misbehaving (owner-authorized) AI agent can otherwise
/// persist an empty/garbage pool config that silently breaks mining. Returns the
/// validated change set, or a stable error string for a JSON-RPC failure reply.
fn validate_pool_update(args: &Value) -> Result<PoolUpdate, &'static str> {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err("url must be a non-empty hostname");
            }
            if trimmed.len() > MCP_POOL_URL_MAX_LEN {
                return Err("url too long");
            }
            // A bare host (no scheme, no whitespace) is what stratum expects.
            if trimmed.contains(char::is_whitespace) {
                return Err("url must not contain whitespace");
            }
            // H4/SV2-AVAIL: a V2 (stratum2/sv2) URL would route to the
            // sleep-forever SV2 stub on a build without the stratum-v2 feature,
            // silently killing mining with no failover. Reject it so set_pool
            // fails loud instead of persisting an unusable pool. Compiled out
            // (no-op) when the stratum-v2 feature IS present.
            #[cfg(not(feature = "stratum-v2"))]
            {
                let lower = trimmed.to_ascii_lowercase();
                if lower.starts_with("stratum2+tcp://")
                    || lower.starts_with("stratum2://")
                    || lower.starts_with("sv2://")
                {
                    return Err(
                        "Stratum V2 is not available in this firmware build; use a Stratum V1 pool URL",
                    );
                }
            }
            Some(trimmed.to_string())
        }
        None => None,
    };

    let port = match args.get("port") {
        Some(v) => {
            let raw = v.as_u64().ok_or("port must be an integer")?;
            let port = u16::try_from(raw).map_err(|_| "port out of range")?;
            if port == 0 {
                return Err("port must be 1-65535");
            }
            Some(port)
        }
        None => None,
    };

    let worker = match args.get("worker").and_then(|v| v.as_str()) {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err("worker must be non-empty");
            }
            if trimmed.len() > MCP_POOL_WORKER_MAX_LEN {
                return Err("worker too long");
            }
            Some(trimmed.to_string())
        }
        None => None,
    };

    let password = match args.get("password").and_then(|v| v.as_str()) {
        Some(raw) => {
            if raw.len() > MCP_POOL_PASSWORD_MAX_LEN {
                return Err("password too long");
            }
            Some(raw.to_string())
        }
        None => None,
    };

    Ok(PoolUpdate {
        url,
        port,
        worker,
        password,
    })
}

fn tool_set_pool(state: &SharedState, args: &Value) -> Value {
    // MCP-3: validate everything up front so a bad field can't half-apply or
    // persist garbage. Only touch the live config once all inputs are clean.
    let update = match validate_pool_update(args) {
        Ok(update) => update,
        Err(error) => return json!({"success": false, "error": error}),
    };
    let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(url) = update.url {
        config.stratum.url = url;
    }
    if let Some(port) = update.port {
        config.stratum.port = port;
    }
    if let Some(worker) = update.worker {
        config.stratum.worker_name = worker;
    }
    if let Some(pass) = update.password {
        config.stratum.password = pass;
    }
    // Persist to NVS
    if let Ok(mut nvs_guard) = state.nvs.lock() {
        if let Some(ref mut nvs) = *nvs_guard {
            if let Err(e) = crate::nvs_config::save_config(nvs, &config) {
                error!("MCP: NVS save failed after set_pool: {}", e);
            } else {
                info!("MCP: config saved to NVS (set_pool)");
            }
        } else {
            warn!("MCP: NVS handle not available — set_pool not persisted");
        }
    }
    json!({"success": true, "pool": format!("{}:{}", crate::shared::sanitize_pool_url(&config.stratum.url), config.stratum.port), "note": "Reboot required to reconnect"})
}

fn tool_get_network(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let profile = config.board_profile();
    json!({
        "ssid": config.wifi_ssid,
        "rssi": telem.wifi_rssi,
        "ip": telem.device_ip,
        "hostname": if config.hostname.is_empty() { format!("dcentaxe-{}", profile.device_model) } else { config.hostname.clone() },
        "board_target": config.board_target(),
    })
}

fn tool_get_history(state: &SharedState) -> Value {
    let history = state.history.lock().unwrap_or_else(|e| e.into_inner());
    json!({
        "samples": history.samples,
        "events": history.events,
    })
}

fn tool_restart_mining() -> Value {
    info!("MCP: restart_mining requested — rebooting");
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(2));
        unsafe {
            esp_idf_svc::sys::esp_restart();
        }
    });
    json!({"success": true, "message": "Rebooting in 2 seconds..."})
}

// MCP-4: sane per-mode bounds for the autotune target setpoint. target_temp
// stays below the 95/105 C thermal-ladder backstop; target_watts spans 1 W up
// to a generous ceiling above any board's ~90 W target. Modes without a numeric
// setpoint (max_hashrate, best_efficiency) ignore `target`, but we still
// finite-check it so an `inf`/`NaN` can never reach the autotuner state.
const MCP_AUTOTUNE_TEMP_MIN_C: f32 = 40.0;
const MCP_AUTOTUNE_TEMP_MAX_C: f32 = 95.0;
const MCP_AUTOTUNE_WATTS_MIN: f32 = 1.0;
const MCP_AUTOTUNE_WATTS_MAX: f32 = 600.0;

/// MCP-4 pure validation: reject a non-finite target and clamp a finite one to
/// the mode's sane band. Unlike set_frequency/set_core_voltage (which route
/// through qualify_operating_point), the autotune target is a *setpoint the loop
/// optimizes toward*, so an `inf`/absurd value would otherwise poison the
/// optimizer's state. Returns `Err` for a non-finite target; `Ok(clamped)` for a
/// finite one (clamped only for the modes that consume a numeric setpoint).
fn sanitize_autotune_target(mode: AutotuneMode, raw: f64) -> Result<f32, &'static str> {
    if !raw.is_finite() {
        return Err("target must be a finite number");
    }
    let target = raw as f32;
    // `raw as f32` can still overflow to ±inf for huge finite f64 magnitudes.
    if !target.is_finite() {
        return Err("target out of representable range");
    }
    let clamped = match mode {
        AutotuneMode::TargetTemp => target.clamp(MCP_AUTOTUNE_TEMP_MIN_C, MCP_AUTOTUNE_TEMP_MAX_C),
        AutotuneMode::TargetWatts => target.clamp(MCP_AUTOTUNE_WATTS_MIN, MCP_AUTOTUNE_WATTS_MAX),
        // No numeric setpoint consumed; keep it finite but otherwise untouched.
        AutotuneMode::MaxHashrate | AutotuneMode::BestEfficiency => target,
    };
    Ok(clamped)
}

fn tool_run_autotune(state: &SharedState, args: &Value) -> Value {
    let mut autotune = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(enabled) = args.get("enabled").and_then(|v| v.as_bool()) {
        autotune.enabled = enabled;
    }
    if let Some(mode_str) = args.get("mode").and_then(|v| v.as_str()) {
        autotune.mode = match mode_str {
            "max_hashrate" => AutotuneMode::MaxHashrate,
            "target_watts" => AutotuneMode::TargetWatts,
            "best_efficiency" => AutotuneMode::BestEfficiency,
            "target_temp" => AutotuneMode::TargetTemp,
            _ => autotune.mode,
        };
    }
    // MCP-4: validate/clamp the target against the (possibly just-updated) mode
    // before it becomes the setpoint the optimizer chases.
    if let Some(target) = args.get("target").and_then(|v| v.as_f64()) {
        match sanitize_autotune_target(autotune.mode, target) {
            Ok(clamped) => autotune.target_value = clamped,
            Err(error) => return json!({"success": false, "error": error}),
        }
    }

    autotune.status = if autotune.enabled {
        "starting".into()
    } else {
        "stopped".into()
    };

    json!({
        "success": true,
        "enabled": autotune.enabled,
        "mode": format!("{:?}", autotune.mode),
        "target": autotune.target_value,
    })
}

fn tool_identify_device(state: &SharedState) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (active, message) = {
        let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
        if swarm.identify_until_epoch_s > now {
            swarm.identify_until_epoch_s = 0;
            (false, "The device no longer says \"Hi!\".")
        } else {
            swarm.identify_until_epoch_s = now + 30;
            (true, "The device says \"Hi!\" for 30 seconds.")
        }
    };
    json!({"success": true, "active": active, "message": message})
}

fn tool_get_swarm(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
    let snap = stats.snapshot();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let observed_room_temp_c = match (swarm.observed_room_temp_c, swarm.room_temp_expires_epoch_s) {
        (Some(temp), Some(expires)) if expires >= now => Some(temp),
        (Some(temp), None) => Some(temp),
        _ => None,
    };
    json!({
        "schema": 1,
        "nodeId": &swarm.local.id,
        "role": &swarm.role,
        "clusterId": &swarm.cluster_id,
        "queenId": &swarm.queen_id,
        "hashrateGhs": snap.hashrate_5m_ghs,
        "powerWatts": telem.power_w,
        "heatWatts": telem.power_w,
        "heatBtuH": (telem.power_w as f64) * 3.412_142,
        "controlMode": if config.fan_target_temp_c > 0 { "thermal" } else { "manual" },
        "observedRoomTempC": observed_room_temp_c,
        "targetRoomTempC": serde_json::Value::Null,
        "targetWatts": serde_json::Value::Null,
        "heatingActive": telem.mining_enabled && snap.hashrate_5m_ghs > 0.0,
        "updatedAt": now,
        "local": &swarm.local,
        "peers": &swarm.peers,
        "peerCount": swarm.peers.len(),
        "discovery": {
            "mdnsEnabled": swarm.discovery.mdns_enabled,
            "mdnsHostname": &swarm.discovery.mdns_hostname,
            "discoveryHint": &swarm.discovery.discovery_hint,
            "apiUrl": &swarm.discovery.api_url,
            "mcpUrl": &swarm.discovery.mcp_url,
            "mcpTransport": &swarm.discovery.mcp_transport,
            "mcpProfile": &swarm.discovery.mcp_profile,
        },
        "coordination": &swarm.coordination,
    })
}

fn handle_resources_list() -> Result<Value, Value> {
    Ok(json!({
        "resources": [
            {
                "uri": "bitaxe://status",
                "name": "Miner Status",
                "description": "Live mining status including hashrate, temperature, and power",
                "mimeType": "application/json"
            },
            {
                "uri": "bitaxe://config",
                "name": "Miner Configuration",
                "description": "Current miner configuration (pool, frequency, voltage, fan)",
                "mimeType": "application/json"
            },
            {
                "uri": "bitaxe://history",
                "name": "Mining History",
                "description": "Recent mining performance data",
                "mimeType": "application/json"
            },
            {
                "uri": "bitaxe://swarm",
                "name": "Swarm State",
                "description": "Local swarm metadata and reported peers",
                "mimeType": "application/json"
            }
        ]
    }))
}

fn handle_resource_read(
    state: &SharedState,
    params: &Value,
    auth: McpAuth,
) -> Result<Value, Value> {
    if !auth.read_authorized {
        return Err(jsonrpc_auth_error(
            auth.read_denied_detail
                .unwrap_or("Bearer session required for MCP resources"),
        ));
    }
    let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");

    let content = match uri {
        "bitaxe://status" => {
            let result = tool_get_status(state);
            serde_json::to_string(&result).unwrap_or_default()
        }
        "bitaxe://config" => {
            let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            // Mask sensitive fields before serialization
            let mut safe_config = config.clone();
            safe_config.wifi_password = "***".into();
            safe_config.stratum.password = "***".into();
            // B-ESP-10: the pool worker is the operator's FULL BTC payout address
            // and a pool URL can embed `user:pass@` creds — mask BOTH on this
            // read resource (mirrors the Antminer rule; mask_wallet → <first6>…
            // <last4>, sanitize_pool_url strips the authority).
            safe_config.stratum.worker_name =
                crate::shared::mask_wallet(&safe_config.stratum.worker_name);
            safe_config.stratum.url = crate::shared::sanitize_pool_url(&safe_config.stratum.url);
            // MQTT broker credential: mask like wifi/stratum so the read-only
            // bitaxe://config resource never leaks it in cleartext.
            if !safe_config.mqtt.password.is_empty() {
                safe_config.mqtt.password = "***".into();
            }
            // MCP-SEC: the fallback and split pools each carry their OWN
            // StratumConfig.password, which would otherwise serialize in
            // cleartext on the bitaxe://config read resource. Mask them too —
            // masking only the primary leaked the backup-pool credentials. The
            // fallback/split worker + URL are masked here too (B-ESP-10).
            if let Some(fb) = safe_config.fallback_pool.as_mut() {
                fb.password = "***".into();
                fb.worker_name = crate::shared::mask_wallet(&fb.worker_name);
                fb.url = crate::shared::sanitize_pool_url(&fb.url);
            }
            if let Some(sp) = safe_config.split_pool.as_mut() {
                sp.pool.password = "***".into();
                sp.pool.worker_name = crate::shared::mask_wallet(&sp.pool.worker_name);
                sp.pool.url = crate::shared::sanitize_pool_url(&sp.pool.url);
            }
            serde_json::to_string(&safe_config).unwrap_or_default()
        }
        "bitaxe://history" => {
            let result = tool_get_history(state);
            serde_json::to_string(&result).unwrap_or_default()
        }
        "bitaxe://swarm" => {
            let result = tool_get_swarm(state);
            serde_json::to_string(&result).unwrap_or_default()
        }
        // MCP-5: an unknown resource URI is "method not found" (-32601), aligned
        // with the unknown-tool and top-level-method conventions; -32602 stays
        // reserved for invalid arguments to a known target.
        _ => return Err(json!({"code": -32601, "message": format!("Unknown resource: {}", uri)})),
    };

    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": content
        }]
    }))
}
