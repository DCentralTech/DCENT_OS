use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::shared::{stratum_status_snapshots, SharedState};

const CGMINER_PORT: u16 = 4028;
const CGMINER_DESCRIPTION: &str = "cgminer 4.11.1";
const CGMINER_API_VERSION: &str = "3.7";

/// AUX-5: the cgminer-compat RPC binds `0.0.0.0:4028` with NO authentication
/// (fleet tools — Awesome Miner, ckpool dashboards, asic-rs — expect to read it
/// unauthenticated). That is acceptable ONLY because the surface is strictly
/// read-only. This is the single allowlist of verbs the socket answers; ANY verb
/// not in this set is denied with cgminer error code 14 ("invalid command") by
/// the deny-by-default arm in `handle_request`.
///
/// LOAD-BEARING: every entry here is a pure telemetry READ. A future contributor
/// must NOT add a mutating verb (`pause`/`quit`/`restart`/`disable`/`enable`/
/// `ascset`/`pgaset`/`failover-only`/`zero`/`save`/`addpool`/`removepool` …) to
/// this list — those would be reachable unauthenticated and turn a privacy leak
/// into a remote-control hole. Mutating control belongs behind the password-gated
/// REST/MCP write surfaces (`authorize_rest_write` / `authorize_mcp_control`),
/// never here. `is_read_only_verb` + the `cgminer_read_only_contract` tests pin
/// this so the allowlist can't silently grow a write verb past review.
const READ_ONLY_VERBS: &[&str] = &["version", "summary", "stats", "pools", "devs", "devdetails"];

/// Returns `true` only for verbs on the read-only allowlist. Pure + total: any
/// unknown or mutating verb returns `false` (deny-by-default). Case-insensitive
/// to match the lowercasing in `handle_request`.
pub(crate) fn is_read_only_verb(verb: &str) -> bool {
    let verb = verb.trim().to_ascii_lowercase();
    READ_ONLY_VERBS.contains(&verb.as_str())
}

/// AUX-10: cumulative work done in MH from accepted pool difficulty, matching
/// cgminer's "Total MH" semantics (`difficulty × 2^32 hashes`, expressed in
/// mega-hashes). Pure + total: a non-finite or negative input yields 0.0 so the
/// field never serialises NaN/Inf to a fleet tool.
pub(crate) fn mh_from_difficulty(difficulty_accepted: f64) -> f64 {
    if !difficulty_accepted.is_finite() || difficulty_accepted <= 0.0 {
        return 0.0;
    }
    // 2^32 hashes per difficulty-1 share; /1e6 to express in MH.
    difficulty_accepted * 4_294_967_296.0 / 1_000_000.0
}

pub fn start_cgminer_tcp(state: SharedState) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("0.0.0.0", CGMINER_PORT)) {
            Ok(listener) => listener,
            Err(err) => {
                log::warn!("CGMiner RPC: failed to bind port {}: {}", CGMINER_PORT, err);
                return;
            }
        };

        log::info!("CGMiner RPC: listening on port {}", CGMINER_PORT);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let state = state.clone();
                    std::thread::spawn(move || handle_connection(stream, &state));
                }
                Err(err) => log::warn!("CGMiner RPC: accept failed: {}", err),
            }
        }
    });
}

fn handle_connection(mut stream: TcpStream, state: &SharedState) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(err) => {
            let _ = write_response(&mut stream, &error_response(json!(1), 14, &err));
            return;
        }
    };

    let response = handle_request(&request, state);
    let _ = write_response(&mut stream, &response);
}

fn read_request(stream: &mut TcpStream) -> Result<Value, String> {
    let mut raw = Vec::with_capacity(1024);
    let mut buf = [0u8; 512];

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.contains(&0) || raw.len() >= 4096 {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(err) => return Err(format!("socket read failed: {}", err)),
        }
    }

    if let Some(pos) = raw.iter().position(|byte| *byte == 0) {
        raw.truncate(pos);
    }

    while raw.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        raw.pop();
    }

    if raw.is_empty() {
        return Err("empty request".to_string());
    }

    match serde_json::from_slice(&raw) {
        Ok(value) => Ok(value),
        Err(_) => {
            let raw_command =
                String::from_utf8(raw).map_err(|err| format!("invalid command utf8: {}", err))?;
            let mut parts = raw_command.trim().splitn(2, '|');
            let command = parts.next().unwrap_or("").trim();
            let parameter = parts.next().unwrap_or("").trim();
            Ok(json!({ "command": command, "parameter": parameter }))
        }
    }
}

fn write_response(stream: &mut TcpStream, response: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(response)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
    bytes.push(0);
    stream.write_all(&bytes)
}

fn handle_request(request: &Value, state: &SharedState) -> Value {
    let command = request
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = request.get("id").cloned().unwrap_or_else(|| json!(1));

    if command.is_empty() {
        return error_response(id, 14, "missing command");
    }

    let commands: Vec<String> = command
        .split('+')
        .map(|cmd| cmd.trim().to_ascii_lowercase())
        .filter(|cmd| !cmd.is_empty())
        .collect();

    if commands.is_empty() {
        return error_response(id, 14, "missing command");
    }

    let mut response = Map::new();
    let mut statuses = Vec::new();

    for command in commands {
        // AUX-5: deny-by-default. The cgminer socket is unauthenticated, so it
        // answers ONLY read-only verbs. Reject anything not on the allowlist
        // BEFORE the match — this makes `is_read_only_verb` the single
        // enforcement point, so a write verb added to the match below without
        // also being whitelisted here is unreachable (and tripped by review).
        if !is_read_only_verb(&command) {
            return error_response(id, 14, &format!("invalid command: {}", command));
        }
        match command.as_str() {
            "version" => {
                statuses.push(ok_status(22, "CGMiner versions"));
                response.insert("VERSION".to_string(), build_version(state));
            }
            "summary" => {
                statuses.push(ok_status(11, "Summary"));
                response.insert("SUMMARY".to_string(), build_summary(state));
            }
            "stats" => {
                statuses.push(ok_status(70, "CGMiner stats"));
                response.insert("STATS".to_string(), build_stats(state));
            }
            "pools" => {
                let count = build_pools(state);
                let msg = format!("{} Pool(s)", count.as_array().map(|p| p.len()).unwrap_or(0));
                statuses.push(ok_status(7, &msg));
                response.insert("POOLS".to_string(), count);
            }
            "devs" => {
                statuses.push(ok_status(9, "Devs"));
                response.insert("DEVS".to_string(), build_devs(state));
            }
            "devdetails" => {
                statuses.push(ok_status(69, "Device Details"));
                response.insert("DEVDETAILS".to_string(), build_devdetails(state));
            }
            _ => return error_response(id, 14, &format!("invalid command: {}", command)),
        }
    }

    response.insert("STATUS".to_string(), Value::Array(statuses));
    response.insert("id".to_string(), id);
    Value::Object(response)
}

fn ok_status(code: i32, msg: &str) -> Value {
    json!({
        "STATUS": "S",
        "When": unix_time_s(),
        "Code": code,
        "Msg": msg,
        "Description": CGMINER_DESCRIPTION,
    })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({
        "STATUS": [{
            "STATUS": "E",
            "When": unix_time_s(),
            "Code": code,
            "Msg": message,
            "Description": CGMINER_DESCRIPTION,
        }],
        "id": id,
    })
}

fn build_version(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board = config.board_config();
    let model = stock_device_model_name(&board);
    let miner_name = rpc_miner_name(model);
    let type_name = rpc_type_name(model);

    json!([{
        "BMMiner": env!("CARGO_PKG_VERSION"),
        "API": CGMINER_API_VERSION,
        "Miner": miner_name,
        "CompileTime": format!("DCENT_axe {}", env!("CARGO_PKG_VERSION")),
        "Type": type_name,
    }])
}

fn build_summary(state: &SharedState) -> Value {
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let snap = stats.snapshot();
    let stratum_statuses = stratum_status_snapshots(state);
    let (_submitted, shares_accepted, shares_rejected, difficulty_accepted, difficulty_rejected) =
        if stratum_statuses.is_empty() {
            (0, 0, 0, 0.0, 0.0)
        } else {
            stratum_statuses
                .iter()
                .fold((0_u64, 0_u64, 0_u64, 0.0, 0.0), |acc, status| {
                    (
                        acc.0 + status.shares_submitted,
                        acc.1 + status.shares_accepted,
                        acc.2 + status.shares_rejected,
                        acc.3 + status.difficulty_accepted,
                        acc.4 + status.difficulty_rejected,
                    )
                })
        };
    let total_hw_errors: u64 = snap.per_chip.iter().map(|chip| chip.errors as u64).sum();
    let total_hashrate_ghs = snap.hashrate_5s_ghs.max(0.0);
    let total_hashrate_avg_ghs = snap.hashrate_5m_ghs.max(0.0);
    let elapsed_secs = telem.uptime_secs.max(snap.uptime_secs);
    let elapsed_minutes = (elapsed_secs as f64 / 60.0).max(1.0 / 60.0);
    let rejected_pct = if (shares_accepted + shares_rejected) > 0 {
        shares_rejected as f64 / (shares_accepted + shares_rejected) as f64 * 100.0
    } else {
        0.0
    };
    let utility = shares_accepted as f64 / elapsed_minutes;
    let work_utility = difficulty_accepted / elapsed_minutes;
    // AUX-10: populate the cgminer parity fields fleet tools key off, instead of
    // hardcoding 0 (which made a healthy miner read as idle/never-fetched-work).
    //   Getworks      ← clean_jobs_count (our only job-received counter; each
    //                   new-block mining.notify bumps it).
    //   Local Work    ← nonces_found (local work units the ASIC actually returned).
    //   Total MH      ← cumulative accepted difficulty × 2^32 / 1e6, the canonical
    //                   cgminer "total work done in MH" used by tools that derive
    //                   average hashrate from it.
    //   Network Blocks← real chain tip (snap.block_height from the BIP34 coinbase),
    //                   NOT clean_jobs_count (which is a new-job counter, never the
    //                   block height — the old mapping was semantically wrong).
    let getworks = snap.clean_jobs_count;
    let local_work = snap.nonces_found;
    let total_mh = mh_from_difficulty(difficulty_accepted);
    let network_blocks = snap.block_height;

    json!([{
        "Elapsed": elapsed_secs,
        "GHS 5s": total_hashrate_ghs,
        "GHS av": total_hashrate_avg_ghs,
        "Found Blocks": 0,
        "Getworks": getworks,
        "Accepted": shares_accepted,
        "Rejected": shares_rejected,
        "Hardware Errors": total_hw_errors,
        "Utility": utility,
        "Discarded": 0,
        "Stale": 0,
        "Get Failures": 0,
        "Local Work": local_work,
        "Remote Failures": 0,
        "Network Blocks": network_blocks,
        "Total MH": total_mh,
        "Work Utility": work_utility,
        "Difficulty Accepted": difficulty_accepted,
        "Difficulty Rejected": difficulty_rejected,
        "Difficulty Stale": 0.0,
        "Best Share": snap.best_difficulty,
        "Device Hardware%": if snap.nonces_found > 0 {
            total_hw_errors as f64 / snap.nonces_found as f64 * 100.0
        } else {
            0.0
        },
        "Device Rejected%": rejected_pct,
    }])
}

fn build_stats(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let board = config.board_config();
    let snap = stats.snapshot();
    let model = stock_device_model_name(&board);
    let miner_name = rpc_miner_name(model);
    let type_name = rpc_type_name(model);
    let chain_count = usize::from(config.expected_asic_count()).max(1);
    let mut live = Map::new();

    live.insert(
        "Elapsed".to_string(),
        json!(telem.uptime_secs.max(snap.uptime_secs)),
    );
    live.insert(
        "GHS 5s".to_string(),
        json!(format!("{:.2}", snap.hashrate_5s_ghs.max(0.0))),
    );
    live.insert(
        "total_rateideal".to_string(),
        json!(
            config.target_frequency as f64
                * small_core_count(&board.asic_model) as f64
                * chain_count as f64
                / 1000.0
        ),
    );
    live.insert("rate_unit".to_string(), json!("GH"));
    live.insert("temp1".to_string(), json!(telem.chip_temp_c));
    live.insert("temp2_1".to_string(), json!(telem.board_temp_c));
    live.insert("fan1".to_string(), json!(telem.fan_rpm));
    live.insert("fan2".to_string(), json!(telem.fan2_rpm));
    live.insert("fan3".to_string(), json!(0));
    live.insert("fan4".to_string(), json!(0));
    live.insert("chain_acn1".to_string(), json!(chain_count));
    live.insert(
        "chain_rate1".to_string(),
        json!(format!("{:.2}", snap.hashrate_5m_ghs.max(0.0))),
    );
    live.insert("chain_acs1".to_string(), json!("o".repeat(chain_count)));

    json!([
        {
            "BMMiner": env!("CARGO_PKG_VERSION"),
            "Miner": miner_name,
            "CompileTime": format!("DCENT_axe {}", env!("CARGO_PKG_VERSION")),
            "Type": type_name,
        },
        Value::Object(live),
    ])
}

fn build_pools(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let stratum_statuses = stratum_status_snapshots(state);
    let snap = stats.snapshot();
    let mut pools = Vec::new();
    let primary_quota = config
        .split_pool
        .as_ref()
        .map(|split| 100_u8.saturating_sub(split.hashrate_pct))
        .unwrap_or(100);
    let mut configured = vec![(0_u8, &config.stratum, primary_quota)];

    if let Some(split) = &config.split_pool {
        configured.push((1, &split.pool, split.hashrate_pct));
    }

    for (index, pool, target_pct) in configured {
        let runtime = stratum_statuses.get(index as usize);
        let connected = runtime.map(|entry| entry.connected).unwrap_or(false);
        let accepted = runtime
            .map(|entry| entry.shares_accepted)
            .unwrap_or(if index == 0 { snap.accepted_shares } else { 0 });
        let rejected = runtime
            .map(|entry| entry.shares_rejected)
            .unwrap_or(if index == 0 { snap.rejected_shares } else { 0 });
        let difficulty = runtime
            .map(|entry| entry.difficulty)
            .filter(|value| *value > 0.0)
            .unwrap_or(pool.suggest_difficulty as f64);
        let difficulty_accepted = runtime
            .map(|entry| entry.difficulty_accepted)
            .unwrap_or(0.0);
        let difficulty_rejected = runtime
            .map(|entry| entry.difficulty_rejected)
            .unwrap_or(0.0);
        let last_share_time = runtime.map(|entry| entry.last_share_time).unwrap_or(0);
        let last_share_difficulty = runtime
            .map(|entry| entry.last_share_difficulty)
            .unwrap_or(0.0);
        let active_url = runtime
            .map(|entry| entry.active_url.as_str())
            .unwrap_or(&pool.url);
        let active_port = runtime.map(|entry| entry.active_port).unwrap_or(pool.port);
        // B-ESP-10: strip any user:pass@ creds from the pool URL on this CGMiner
        // read API (mirrors the Antminer cgminer "User"/URL masking, TEL-4).
        let active_url = crate::shared::sanitize_pool_url(active_url);
        let pool_url = if active_url.contains("://") {
            active_url.to_string()
        } else {
            format!("stratum+tcp://{}:{}", active_url, active_port)
        };

        pools.push(json!({
            "POOL": index,
            "URL": pool_url,
            "Status": if connected { "Alive" } else { "Dead" },
            "Priority": index,
            "Quota": if target_pct == 0 { 1 } else { target_pct },
            "Long Poll": "N",
            "Getworks": 0,
            "Accepted": accepted,
            "Rejected": rejected,
            "Discarded": 0,
            "Stale": 0,
            "Get Failures": 0,
            "Remote Failures": 0,
            // B-ESP-10: mask the worker (operator BTC payout address).
            "User": crate::shared::mask_wallet(&pool.worker_name),
            "Last Share Time": last_share_time,
            "Diff": format!("{:.6}", difficulty),
            "Diff1 Shares": difficulty_accepted + difficulty_rejected,
            "Proxy Type": "",
            "Proxy": "",
            "Difficulty Accepted": difficulty_accepted,
            "Difficulty Rejected": difficulty_rejected,
            "Difficulty Stale": 0.0,
            "Last Share Difficulty": last_share_difficulty,
            "Has Stratum": true,
            "Stratum Active": connected,
            "Stratum URL": active_url,
            "Has GBT": false,
            "Best Share": snap.best_difficulty,
            "Pool Rejected%": if (accepted + rejected) > 0 {
                rejected as f64 / (accepted + rejected) as f64 * 100.0
            } else {
                0.0
            },
            "Pool Stale%": 0.0,
        }));
    }

    if let Some(fallback) = &config.fallback_pool {
        let primary = stratum_statuses.first();
        let connected = primary
            .map(|entry| {
                entry.failover_active
                    && entry.active_url == fallback.url
                    && entry.active_port == fallback.port
            })
            .unwrap_or(false);
        // B-ESP-10: strip any user:pass@ creds from the fallback pool URL.
        let fb_display_url = crate::shared::sanitize_pool_url(&fallback.url);
        let pool_url = if fb_display_url.contains("://") {
            fb_display_url.clone()
        } else {
            format!("stratum+tcp://{}:{}", fb_display_url, fallback.port)
        };
        pools.push(json!({
            "POOL": pools.len(),
            "URL": pool_url,
            "Status": if connected { "Alive" } else { "Dead" },
            "Priority": pools.len(),
            "Quota": 1,
            "Long Poll": "N",
            "Getworks": 0,
            "Accepted": 0,
            "Rejected": 0,
            "Discarded": 0,
            "Stale": 0,
            "Get Failures": 0,
            "Remote Failures": 0,
            // B-ESP-10: mask the worker (operator BTC payout address).
            "User": crate::shared::mask_wallet(&fallback.worker_name),
            "Last Share Time": 0,
            "Diff": format!("{:.6}", fallback.suggest_difficulty as f64),
            "Diff1 Shares": 0.0,
            "Proxy Type": "",
            "Proxy": "",
            "Difficulty Accepted": 0.0,
            "Difficulty Rejected": 0.0,
            "Difficulty Stale": 0.0,
            "Last Share Difficulty": 0.0,
            "Has Stratum": true,
            "Stratum Active": connected,
            // B-ESP-10: use the credential-stripped URL, matching the "URL"
            // field above; the raw `fallback.url` can embed `user:pass@`.
            "Stratum URL": fb_display_url,
            "Has GBT": false,
            "Best Share": snap.best_difficulty,
            "Pool Rejected%": 0.0,
            "Pool Stale%": 0.0,
        }));
    }

    Value::Array(pools)
}

fn build_devdetails(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board = config.board_config();
    let model = stock_device_model_name(&board);
    let type_name = rpc_type_name(model);

    json!([{
        "Name": "BitAxe",
        "ID": 0,
        "Driver": "bitaxe",
        "Kernel": "",
        "Model": type_name,
    }])
}

fn build_devs(state: &SharedState) -> Value {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let stratum_statuses = stratum_status_snapshots(state);
    let board = config.board_config();
    let snap = stats.snapshot();
    let model = stock_device_model_name(&board);
    let type_name = rpc_type_name(model);
    let primary = stratum_statuses.first();
    let connected = primary.map(|status| status.connected).unwrap_or(false);
    let shares_accepted = primary.map(|status| status.shares_accepted).unwrap_or(0);
    let shares_rejected = primary.map(|status| status.shares_rejected).unwrap_or(0);
    let difficulty_accepted = primary
        .map(|status| status.difficulty_accepted)
        .unwrap_or(0.0);
    let difficulty_rejected = primary
        .map(|status| status.difficulty_rejected)
        .unwrap_or(0.0);
    let last_share_time = primary.map(|status| status.last_share_time).unwrap_or(0);
    let last_share_difficulty = primary
        .map(|status| status.last_share_difficulty)
        .unwrap_or(0.0);
    let total_hw_errors: u64 = snap.per_chip.iter().map(|chip| chip.errors as u64).sum();
    let elapsed_secs = telem.uptime_secs.max(snap.uptime_secs);
    let elapsed_minutes = (elapsed_secs as f64 / 60.0).max(1.0 / 60.0);
    let rejected_pct = if (shares_accepted + shares_rejected) > 0 {
        shares_rejected as f64 / (shares_accepted + shares_rejected) as f64 * 100.0
    } else {
        0.0
    };
    let hardware_pct = if snap.nonces_found > 0 {
        total_hw_errors as f64 / snap.nonces_found as f64 * 100.0
    } else {
        0.0
    };

    json!([{
        "ASC": 0,
        "Name": "BitAxe",
        "ID": 0,
        "Enabled": "Y",
        "Status": if connected || snap.hashrate_5s_ghs > 0.0 || snap.hashrate_5m_ghs > 0.0 { "Alive" } else { "Sick" },
        "Temperature": telem.chip_temp_c,
        "MHS av": snap.hashrate_5m_ghs * 1000.0,
        "MHS 5s": snap.hashrate_5s_ghs * 1000.0,
        "Accepted": shares_accepted,
        "Rejected": shares_rejected,
        "Hardware Errors": total_hw_errors,
        "Utility": shares_accepted as f64 / elapsed_minutes,
        "Last Share Pool": 0,
        "Last Share Time": last_share_time,
        // AUX-10: real cumulative work in MH (was hardcoded 0.0).
        "Total MH": mh_from_difficulty(difficulty_accepted),
        "Diff1 Work": difficulty_accepted + difficulty_rejected,
        "Difficulty Accepted": difficulty_accepted,
        "Difficulty Rejected": difficulty_rejected,
        "Last Share Difficulty": last_share_difficulty,
        "Device Hardware%": hardware_pct,
        "Device Rejected%": rejected_pct,
        "Device Elapsed": elapsed_secs,
        "Model": type_name,
    }])
}

fn rpc_miner_name(model: &str) -> String {
    format!("DCENT_axe {}", model)
}

fn rpc_type_name(model: &str) -> String {
    format!("BITAXE {}", model)
}

fn stock_device_model_name(board: &dcentaxe_hal::board::BoardConfig) -> &'static str {
    match board.board_version.as_str() {
        "302" | "303" => "Hex",
        "650" => "GammaDuo",
        "701" | "702" => "SupraHex",
        "801" => "GammaTurbo",
        _ => match board.model {
            dcentaxe_hal::board::BitAxeModel::Max => "Max",
            dcentaxe_hal::board::BitAxeModel::Ultra => "Ultra",
            dcentaxe_hal::board::BitAxeModel::Supra => "Supra",
            dcentaxe_hal::board::BitAxeModel::Gamma => "Gamma",
            dcentaxe_hal::board::BitAxeModel::HexUltra => "Hex",
            dcentaxe_hal::board::BitAxeModel::HexSupra => "SupraHex",
            dcentaxe_hal::board::BitAxeModel::GammaDuo => "GammaDuo",
            dcentaxe_hal::board::BitAxeModel::GammaTurbo => "GammaTurbo",
            dcentaxe_hal::board::BitAxeModel::NerdNOS => "Max",
            dcentaxe_hal::board::BitAxeModel::NerdAxe => "Gamma",
            dcentaxe_hal::board::BitAxeModel::NerdQaxePlus => "Supra",
            dcentaxe_hal::board::BitAxeModel::NerdQaxePP => "Gamma",
            dcentaxe_hal::board::BitAxeModel::Touch => "Touch",
            dcentaxe_hal::board::BitAxeModel::GtTouch => "GtTouch",
            dcentaxe_hal::board::BitAxeModel::DcentAxeBm1397 => "DCENT_axe BM1397",
            dcentaxe_hal::board::BitAxeModel::DcentAxeQuadBm1397 => "DCENT_axe Quad BM1397",
            dcentaxe_hal::board::BitAxeModel::DcentAxeHexBm1397 => "DCENT_axe Hex BM1397",
        },
    }
}

fn small_core_count(asic_model: &str) -> u32 {
    match asic_model {
        "BM1397" => 672,
        "BM1366" => 894,
        "BM1368" => 1276,
        "BM1370" => 2040,
        _ => 0,
    }
}

fn unix_time_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod cgminer_read_only_contract {
    use super::*;

    // AUX-5: the allowlist is exactly the read verbs, and NOTHING in it mutates.
    #[test]
    fn allowlist_is_only_known_read_verbs() {
        for verb in ["version", "summary", "stats", "pools", "devs", "devdetails"] {
            assert!(
                is_read_only_verb(verb),
                "{verb} must be allowed (read-only)"
            );
            // case-insensitive + whitespace-tolerant, matching handle_request.
            assert!(is_read_only_verb(&verb.to_uppercase()));
            assert!(is_read_only_verb(&format!("  {verb}  ")));
        }
        assert_eq!(
            READ_ONLY_VERBS.len(),
            6,
            "the unauthenticated cgminer allowlist changed — every entry MUST be a \
             pure read; a mutating verb here is reachable without auth (AUX-5)"
        );
    }

    // AUX-5: deny-by-default. Mutating / privileged cgminer verbs must NEVER be
    // answerable on the unauthenticated socket.
    #[test]
    fn mutating_and_unknown_verbs_are_denied() {
        for verb in [
            "quit",
            "restart",
            "pause",
            "disable",
            "enable",
            "ascset",
            "pgaset",
            "zero",
            "save",
            "addpool",
            "removepool",
            "switchpool",
            "failover-only",
            "poolpriority",
            "privileged",
            "",
            "summaryx",
            "bogus",
        ] {
            assert!(
                !is_read_only_verb(verb),
                "{verb:?} must be denied on the unauthenticated cgminer socket (AUX-5)"
            );
        }
    }

    // AUX-10: Total MH derivation is correct and fail-safe (never NaN/Inf).
    #[test]
    fn total_mh_from_difficulty_is_finite_and_correct() {
        assert_eq!(mh_from_difficulty(0.0), 0.0);
        assert_eq!(mh_from_difficulty(-5.0), 0.0);
        assert_eq!(mh_from_difficulty(f64::NAN), 0.0);
        assert_eq!(mh_from_difficulty(f64::INFINITY), 0.0);
        // diff 1 == 2^32 hashes == 4294.967296 MH.
        let one = mh_from_difficulty(1.0);
        assert!((one - 4294.967296).abs() < 1e-6, "got {one}");
        // monotonic with accepted difficulty.
        assert!(mh_from_difficulty(100.0) > mh_from_difficulty(10.0));
    }
}
