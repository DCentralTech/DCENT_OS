// DCENT_axe REST API
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// HTTP REST API served on port 80 alongside the web dashboard.
// Fully compatible with AxeOS/ESP-Miner API for tool interop
// (Swarm, BitAxeHQ, AxeOS web UI, etc.).
//
// Field names and JSON structure in GET /api/system/info match ESP-Miner's
// GET_system_info exactly so monitoring tools work out of the box.

use std::io::Write as StdWrite;
use std::time::Duration;

use dcent_schema::config::{
    SharedAuthConfig, SharedConfigPatch, SharedConfigSnapshot, SharedMiningConfig,
    SharedNetworkConfig, SharedPoolConfig, SharedThermalConfig, CONFIG_SCHEMA_VERSION,
};
use dcent_schema::swarm::{
    DcentSwarmInfo, HomeControlMode, SwarmPeerReport, SwarmRoomTempRequest, SwarmStatus,
};
use dcent_schema::update::{ToolboxPackageInfo, UpdateMetadata, UPDATE_SCHEMA_VERSION};
use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write as EspWrite;
use esp_idf_svc::wifi::AuthMethod;
use log::*;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::auth;
use crate::nvs_config;
use crate::shared::{
    stratum_metric_snapshots, stratum_status_snapshots,
    stratum_status_snapshots_with_recent_event_limit, SharedState,
};

// Host-pure Prometheus `/metrics` body renderer (single source of truth).
// Re-included into `dcentaxe-core` via `#[path]` so its `#[cfg(test)]` tests
// host-run under `cargo test -p dcentaxe-core`. `register_prometheus` below is a
// thin caller that gathers the live values into `metrics_render`'s plain structs.
// (`metrics_render` is declared at the crate root in main.rs — a `mod` here would
// resolve to `src/api/metrics_render.rs`; referenced below as `crate::metrics_render`.)

/// Version string for DCENT_axe firmware.
const DCENTAXE_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_BOARD_TARGET: &str = env!("DCENTAXE_BOARD_TARGET");
const JSON_HEADERS: [(&str, &str); 1] = [("Content-Type", "application/json")];
const COMPAT_HEADERS: [(&str, &str); 1] = [("Content-Type", "application/json")];
const HOT_POLL_STRATUM_EVENT_LIMIT: usize = 8;

/// MAINAPI-3: error from the bounded API body reader.
enum ApiBodyError {
    /// The body would exceed the supplied cap — reject (HTTP 413) rather than
    /// silently truncate.
    TooLarge,
    /// A non-retryable read error from the request connection.
    Io(String),
}

/// MAINAPI-3: read the full request body, looping `req.read()` until EOF.
///
/// A single fixed-size `req.read()` truncates a body split across TCP segments
/// (embedded-io's Read returns only what is currently buffered), silently
/// dropping config / returning a 400. This mirrors the proven bounded loop from
/// `provisioning::read_full_body` and the OTA receive path: accumulate until EOF,
/// retry on the esp-idf `Timeout` pseudo-error, and REJECT (`TooLarge`) the moment
/// the next chunk would push past `max` — never truncating to a partial body. The
/// per-read request is clamped to the remaining capacity via the host-tested
/// `config::next_take` / `config::body_read_capacity_ok` helpers. Kept local to
/// api.rs (this lane); the capacity math is shared, single-source-of-truth logic.
fn read_full_body(
    req: &mut esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
    max: usize,
) -> Result<Vec<u8>, ApiBodyError> {
    let mut body: Vec<u8> = Vec::new();
    let mut scratch = [0u8; 512];
    loop {
        let remaining = crate::config::next_take(body.len(), max);
        if remaining == 0 {
            // Already at the cap. Probe one more byte: more data => over-cap;
            // EOF => fit exactly.
            match req.read(&mut scratch[..1]) {
                Ok(0) => break,
                Ok(_) => return Err(ApiBodyError::TooLarge),
                Err(e) => {
                    if format!("{e}").contains("Timeout") {
                        continue;
                    }
                    return Err(ApiBodyError::Io(format!("{e}")));
                }
            }
        }
        let take = remaining.min(scratch.len());
        match req.read(&mut scratch[..take]) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if !crate::config::body_read_capacity_ok(body.len(), n, max) {
                    return Err(ApiBodyError::TooLarge);
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => {
                if format!("{e}").contains("Timeout") {
                    continue;
                }
                return Err(ApiBodyError::Io(format!("{e}")));
            }
        }
    }
    Ok(body)
}

/// MAINAPI-3 helper: write a uniform 400/413 response for an `ApiBodyError`.
fn api_body_error_response(
    req: esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
    err: ApiBodyError,
) -> Result<(), Box<dyn std::error::Error>> {
    let (status, reason, msg) = match err {
        ApiBodyError::TooLarge => (
            413,
            "Payload Too Large",
            "{\"error\":\"request body exceeds size limit\"}",
        ),
        ApiBodyError::Io(_) => (400, "Bad Request", "{\"error\":\"body read error\"}"),
    };
    let mut resp = req.into_response(status, Some(reason), &JSON_HEADERS)?;
    let _ = resp.write(msg.as_bytes());
    Ok(())
}

struct StdJsonWriter<'a, W>(&'a mut W);

impl<W> StdWrite for StdJsonWriter<'_, W>
where
    W: EspWrite,
    W::Error: core::fmt::Display,
{
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        EspWrite::write(self.0, buf)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        EspWrite::flush(self.0)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))
    }
}

/// Check CSRF protection — require X-Requested-With header on state-changing requests.
/// Browsers can't set custom headers in <form> tags or <img> src, preventing CSRF.
pub(crate) fn check_csrf(
    req: &esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
) -> bool {
    req.header("X-Requested-With").is_some()
}

/// Register all API routes on the HTTP server.
pub fn register_api(server: &mut EspHttpServer, state: SharedState) {
    auth::register_auth_api(server, state.clone());
    register_system_info(server, state.clone());
    register_donation_info(server, state.clone());
    register_capabilities(server, state.clone());
    register_system_asic(server, state.clone());
    register_system_statistics(server, state.clone());
    register_system_wifi_scan(server, state.clone());
    register_system_config(server, state.clone());
    register_shared_config(server, state.clone());
    register_mining_status(server, state.clone());
    register_mining_block(server, state.clone());
    register_mining_control(server, state.clone());
    register_system_restart(server, state.clone());
    register_system_identify(server, state.clone());
    register_system_coredump(server, state.clone());
    register_system_clear_safe_mode(server, state.clone());
    register_system_self_test(server, state.clone());
    register_block_found_dismiss(server, state.clone());
    register_setup_mode(server, state.clone());
    register_owner_reset(server, state.clone());
    register_ota(server, state.clone());
    register_presets(server, state.clone());
    register_achievements(server);
    register_autotuner_modes(server);
    register_mining_toggle(server, state.clone());
    register_swarm_status(server, state.clone());
    register_swarm_report(server, state.clone());
    register_swarm_room_temp(server, state.clone());
    register_swarm_config(server, state.clone());
    register_update_metadata(server, state.clone());
    register_stock_cgi(server, state.clone());
    register_pwa_manifest(server);
    register_prometheus(server, state.clone());
    register_schedule(server, state.clone());
    register_pools(server, state.clone());
    info!("REST API registered");
}

/// Format a difficulty value into a human-readable string with suffix.
/// ESP-Miner uses this format for display: "1.2K", "3.4M", "5.6G", etc.
fn format_difficulty(diff: f64) -> String {
    if diff >= 1.0e18 {
        format!("{:.2}E", diff / 1.0e18)
    } else if diff >= 1.0e15 {
        format!("{:.2}P", diff / 1.0e15)
    } else if diff >= 1.0e12 {
        format!("{:.2}T", diff / 1.0e12)
    } else if diff >= 1.0e9 {
        format!("{:.2}G", diff / 1.0e9)
    } else if diff >= 1.0e6 {
        format!("{:.2}M", diff / 1.0e6)
    } else if diff >= 1.0e3 {
        format!("{:.2}K", diff / 1.0e3)
    } else {
        format!("{:.0}", diff)
    }
}

// MAINAPI-6: the dead `asic_model_name(board_model)` board->ASIC map (defaulting
// unknown boards to BM1366, zero call sites) was removed so it can't drift from
// the single source of truth (`DcentAxeConfig::asic_model_name()` in config.rs,
// backed by BoardConfig/AsicModel).

fn unix_time_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn schedule_status_json(
    config: &crate::config::DcentAxeConfig,
    uptime_secs: u64,
) -> serde_json::Value {
    let (minute, source) =
        crate::config::schedule_minute_of_day(config.schedule_timezone_offset_minutes, uptime_secs);
    let active = if config.schedule_enabled {
        crate::config::active_power_schedule(&config.power_schedule, minute)
    } else {
        None
    };
    let next_change_minutes = if config.schedule_enabled {
        crate::config::next_schedule_change_minutes(&config.power_schedule, minute)
    } else {
        None
    };

    json!({
        "enabled": config.schedule_enabled,
        "timezoneOffsetMinutes": config.schedule_timezone_offset_minutes,
        "currentMinuteOfDay": minute,
        "currentHour": minute / 60,
        "currentMinute": minute % 60,
        "timeSource": source,
        "activeIndex": active.map(|(idx, _)| idx),
        "active": active.map(|(_, entry)| entry.clone()),
        "nextChangeMinutes": next_change_minutes,
        "entries": config.power_schedule.clone(),
    })
}

fn sanitize_schedule_label(label: &str) -> String {
    label.chars().take(32).collect::<String>()
}

fn normalize_schedule_autotune_mode(mode: &str) -> Option<String> {
    crate::shared::AutotuneMode::from_api_str(mode).map(|m| m.as_api_str().to_string())
}

fn compatibility_hostname(config: &crate::config::DcentAxeConfig, device_model: &str) -> String {
    if config.hostname.is_empty() {
        format!("dcentaxe-{}", device_model)
    } else {
        config.hostname.clone()
    }
}

fn partition_labels() -> (String, String) {
    use esp_idf_svc::sys::*;

    let running_partition = unsafe { esp_ota_get_running_partition() };
    let running_label = if running_partition.is_null() {
        "unknown".to_string()
    } else {
        let label = unsafe { (*running_partition).label };
        let c_str = unsafe { std::ffi::CStr::from_ptr(label.as_ptr()) };
        c_str.to_string_lossy().into_owned()
    };

    let next_partition = unsafe { esp_ota_get_next_update_partition(std::ptr::null()) };
    let next_label = if next_partition.is_null() {
        "none".to_string()
    } else {
        let label = unsafe { (*next_partition).label };
        let c_str = unsafe { std::ffi::CStr::from_ptr(label.as_ptr()) };
        c_str.to_string_lossy().into_owned()
    };

    (running_label, next_label)
}

fn stock_device_model(config: &crate::config::DcentAxeConfig) -> &'static str {
    let board_cfg = config.board_config();
    match board_cfg.board_version.as_str() {
        "302" | "303" => "Hex",
        "650" => "GammaDuo",
        "701" | "702" => "SupraHex",
        "801" => "GammaTurbo",
        _ => match board_cfg.model {
            dcentaxe_hal::board::BitAxeModel::Max => "Max",
            dcentaxe_hal::board::BitAxeModel::Ultra => "Ultra",
            dcentaxe_hal::board::BitAxeModel::Supra => "Supra",
            dcentaxe_hal::board::BitAxeModel::Gamma => "Gamma",
            dcentaxe_hal::board::BitAxeModel::GammaDuo => "GammaDuo",
            dcentaxe_hal::board::BitAxeModel::GammaTurbo => "GammaTurbo",
            dcentaxe_hal::board::BitAxeModel::HexUltra => "Hex",
            dcentaxe_hal::board::BitAxeModel::HexSupra => "SupraHex",
            dcentaxe_hal::board::BitAxeModel::NerdNOS => "Max",
            dcentaxe_hal::board::BitAxeModel::NerdAxe => "Gamma",
            dcentaxe_hal::board::BitAxeModel::NerdQaxePlus => "Supra",
            dcentaxe_hal::board::BitAxeModel::NerdQaxePP => "Gamma",
            // Surface Touch variants with their own marketing names so the
            // dashboard / swarm / cgminer stock protocol identify them correctly.
            dcentaxe_hal::board::BitAxeModel::Touch => "Touch",
            dcentaxe_hal::board::BitAxeModel::GtTouch => "GtTouch",
            // DCENT_axe BM1397 SKUs.
            dcentaxe_hal::board::BitAxeModel::DcentAxeBm1397 => "DCENT_axe BM1397",
            dcentaxe_hal::board::BitAxeModel::DcentAxeQuadBm1397 => "DCENT_axe Quad BM1397",
            dcentaxe_hal::board::BitAxeModel::DcentAxeHexBm1397 => "DCENT_axe Hex BM1397",
        },
    }
}

fn stock_swarm_color(device_model: &str) -> &'static str {
    match device_model {
        "Max" => "red",
        "Ultra" => "purple",
        "Hex" => "orange",
        "Supra" => "blue",
        "Gamma" => "green",
        "GammaDuo" => "green",
        "SupraHex" => "darkblue",
        "GammaTurbo" => "cyan",
        _ => "gray",
    }
}

fn stock_hash_domains(asic_model: &str) -> u8 {
    match asic_model {
        "BM1397" => 1,
        "BM1366" | "BM1368" | "BM1370" => 4,
        _ => 1,
    }
}

fn auth_mode_code(auth_method: Option<AuthMethod>) -> u8 {
    match auth_method.unwrap_or(AuthMethod::None) {
        AuthMethod::None => 0,
        AuthMethod::WEP => 1,
        AuthMethod::WPA => 2,
        AuthMethod::WPA2Personal => 3,
        AuthMethod::WPAWPA2Personal => 4,
        AuthMethod::WPA2Enterprise => 5,
        AuthMethod::WPA3Personal => 6,
        AuthMethod::WPA2WPA3Personal => 7,
        AuthMethod::WAPIPersonal => 8,
    }
}

fn package_prefix(board_target: &str, version: &str) -> String {
    format!("dcentaxe-{}-{}", board_target, version)
}

fn factory_package_name(board_target: &str, version: &str) -> String {
    format!("{}-factory.bin", package_prefix(board_target, version))
}

fn update_package_name(board_target: &str, version: &str) -> String {
    format!("{}-update.bin", package_prefix(board_target, version))
}

fn stock_frequency_options(config: &crate::config::DcentAxeConfig) -> Vec<u16> {
    config.qualified_frequency_options()
}

fn stock_voltage_options(config: &crate::config::DcentAxeConfig) -> Vec<u16> {
    config.qualified_voltage_options()
}

fn recommended_preset_name(model: dcentaxe_hal::board::BitAxeModel) -> &'static str {
    match model {
        dcentaxe_hal::board::BitAxeModel::GammaTurbo => "Recommended Safe",
        _ => "Default",
    }
}

fn preset_description(name: &str) -> &'static str {
    match name {
        "Low Power" => "Quietest and coolest option for experimentation, sleep-friendly rooms, or weak power supplies.",
        "Balanced" => "A moderate middle ground between thermals, efficiency, and hashrate.",
        "Default" => "Factory-style baseline for normal daily mining.",
        "Recommended Safe" => "D-Central qualified safe point for stable long-running GT mining.",
        "Efficient" => "Prioritizes better joules per terahash over raw speed.",
        "High Perf" => "Faster setting with more heat and tighter power margins.",
        "Max (OC)" => "Experimental overclock territory. Monitor power, temps, and hardware stability closely.",
        _ => "Preset profile for this board family.",
    }
}

fn aggregate_stratum_counters(
    statuses: &[dcentaxe_stratum::StratumStatus],
) -> Option<(u64, u64, u64)> {
    if statuses.is_empty() {
        None
    } else {
        Some(statuses.iter().fold((0_u64, 0_u64, 0_u64), |acc, status| {
            (
                acc.0 + status.shares_submitted,
                acc.1 + status.shares_accepted,
                acc.2 + status.shares_rejected,
            )
        }))
    }
}

fn device_mac_string() -> String {
    let mut mac = [0u8; 6];
    let err = unsafe { esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if err == esp_idf_svc::sys::ESP_OK {
        mac.iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<Vec<_>>()
            .join(":")
    } else {
        String::new()
    }
}

fn device_serial(board_target: &str) -> String {
    let mac = device_mac_string().replace(':', "").to_uppercase();
    if mac.is_empty() {
        format!("DCENT-{}", board_target.to_uppercase())
    } else {
        let suffix = &mac[mac.len().saturating_sub(6)..];
        format!("DCENT-{}-{}", board_target.to_uppercase(), suffix)
    }
}

fn active_room_temp_c(swarm: &crate::shared::SwarmState) -> Option<f32> {
    let now = unix_time_s();
    match (swarm.observed_room_temp_c, swarm.room_temp_expires_epoch_s) {
        (Some(temp), Some(expires)) if expires >= now => Some(temp),
        (Some(temp), None) => Some(temp),
        _ => None,
    }
}

fn swarm_control_mode(config: &crate::config::DcentAxeConfig) -> HomeControlMode {
    if config.fan_target_temp_c > 0 {
        HomeControlMode::Thermal
    } else {
        HomeControlMode::Manual
    }
}

fn dcent_swarm_info(
    config: &crate::config::DcentAxeConfig,
    telem: &crate::shared::Telemetry,
    swarm: &crate::shared::SwarmState,
    hashrate_ghs: f64,
) -> DcentSwarmInfo {
    DcentSwarmInfo {
        schema: dcent_schema::swarm::SWARM_SCHEMA_VERSION,
        node_id: swarm.local.id.clone(),
        family: "bitaxe".to_string(),
        role: swarm.role,
        cluster_id: swarm.cluster_id.clone(),
        queen_id: swarm.queen_id.clone(),
        capabilities: dcent_schema::swarm::SwarmCapabilities {
            can_coordinate: false,
            room_temp_input: true,
            target_temp_control: false,
            target_watts_control: false,
            identify: true,
            mcp: true,
        },
        home: dcent_schema::swarm::SwarmHomeStatus {
            control_mode: swarm_control_mode(config),
            observed_room_temp_c: active_room_temp_c(swarm),
            target_room_temp_c: None,
            target_watts: None,
            heat_watts: telem.power_w as f64,
            heat_btu_h: (telem.power_w as f64) * 3.412_142,
            heating_active: telem.mining_enabled && hashrate_ghs > 0.0,
        },
    }
}

fn swarm_status_response(
    config: &crate::config::DcentAxeConfig,
    telem: &crate::shared::Telemetry,
    swarm: &crate::shared::SwarmState,
    hashrate_ghs: f64,
) -> SwarmStatus {
    SwarmStatus {
        schema: dcent_schema::swarm::SWARM_SCHEMA_VERSION,
        node_id: swarm.local.id.clone(),
        role: swarm.role,
        cluster_id: swarm.cluster_id.clone(),
        queen_id: swarm.queen_id.clone(),
        hashrate_ghs,
        power_watts: telem.power_w as f64,
        heat_watts: telem.power_w as f64,
        heat_btu_h: (telem.power_w as f64) * 3.412_142,
        control_mode: swarm_control_mode(config),
        observed_room_temp_c: active_room_temp_c(swarm),
        target_room_temp_c: None,
        target_watts: None,
        heating_active: telem.mining_enabled && hashrate_ghs > 0.0,
        updated_at: unix_time_s(),
        local: Some(swarm.local.clone()),
        peers: swarm.peers.clone(),
        peer_count: swarm.peers.len(),
        discovery: Some(swarm.discovery.clone()),
        coordination: swarm.coordination.clone(),
    }
}

fn shared_pool_config(pool: &dcentaxe_stratum::StratumConfig, enabled: bool) -> SharedPoolConfig {
    let protocol = match pool.protocol() {
        dcentaxe_stratum::StratumProtocol::V1 => "stratum-v1",
        dcentaxe_stratum::StratumProtocol::V2 => "stratum-v2",
    };
    SharedPoolConfig {
        // B-ESP-10 read-surface masking: GET/POST /api/config/shared emit this
        // for the primary AND fallback pool. The URL can embed `user:pass@`
        // creds and the worker is the operator's full BTC payout address, so
        // both go through the canonical sanitizers (paired with the WRITE-path
        // echo-guard in `apply_shared_config_patch` so a re-POST of the masked
        // value never clobbers the stored full value).
        url: crate::shared::sanitize_pool_url(&pool.url),
        port: Some(pool.port),
        worker: crate::shared::mask_wallet(&pool.worker_name),
        password_set: !pool.password.trim().is_empty(),
        protocol: Some(protocol.to_string()),
        enabled,
    }
}

fn stratum_protocol_short(pool: &dcentaxe_stratum::StratumConfig) -> &'static str {
    match pool.protocol() {
        dcentaxe_stratum::StratumProtocol::V1 => "v1",
        dcentaxe_stratum::StratumProtocol::V2 => "v2",
    }
}

fn normalize_stratum_url_protocol(
    url: &str,
    protocol: dcentaxe_stratum::StratumProtocol,
) -> String {
    let host = dcentaxe_stratum::endpoint_host_from_url(url);
    if host.is_empty() {
        return String::new();
    }
    match protocol {
        dcentaxe_stratum::StratumProtocol::V1 => host,
        dcentaxe_stratum::StratumProtocol::V2 => format!("stratum2+tcp://{}", host),
    }
}

fn stratum_port_from_url(url: &str) -> Option<u16> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split('@')
        .last()
        .unwrap_or(without_scheme);
    if let Some(rest) = authority.strip_prefix('[') {
        let (_, after_host) = rest.split_once(']')?;
        let port_text = after_host.strip_prefix(':')?;
        return port_text.parse::<u16>().ok();
    }
    let (host, port_text) = authority.rsplit_once(':')?;
    if host.contains(':') {
        return None;
    }
    port_text.parse::<u16>().ok()
}

fn requested_stratum_protocol(
    value: &serde_json::Value,
) -> Option<dcentaxe_stratum::StratumProtocol> {
    match value.as_str()?.trim().to_ascii_lowercase().as_str() {
        "v2" | "sv2" | "stratum-v2" | "stratum2" | "stratum2+tcp" => {
            Some(dcentaxe_stratum::StratumProtocol::V2)
        }
        "v1" | "sv1" | "stratum-v1" | "stratum" | "stratum+tcp" => {
            Some(dcentaxe_stratum::StratumProtocol::V1)
        }
        _ => None,
    }
}

/// H4/SV2-AVAIL: true when a config-update JSON body asks to switch ANY pool
/// (primary, fallback, or split) to Stratum V2 — via an explicit `*Protocol`
/// field that resolves to V2, a URL carrying a V2 scheme, or the SV2
/// own-templates toggle (which forces the primary pool onto V2). Used to
/// fail-closed on firmware built WITHOUT the `stratum-v2` feature, where the SV2
/// client is a sleep-forever stub that cannot mine. Only compiled on that build
/// (the V2 build accepts V2 normally, so this is dead there).
#[cfg(not(feature = "stratum-v2"))]
fn config_update_requests_stratum_v2(updates: &serde_json::Value) -> bool {
    // Explicit protocol selectors.
    for field in [
        "stratumProtocol",
        "fallbackStratumProtocol",
        "splitPoolProtocol",
    ] {
        if updates.get(field).and_then(requested_stratum_protocol)
            == Some(dcentaxe_stratum::StratumProtocol::V2)
        {
            return true;
        }
    }
    // URL scheme selectors (stratum2+tcp:// / stratum2:// / sv2://).
    for field in ["stratumURL", "fallbackStratumURL", "splitPoolURL"] {
        if let Some(url) = updates.get(field).and_then(|v| v.as_str()) {
            let lower = url.trim().to_ascii_lowercase();
            if lower.starts_with("stratum2+tcp://")
                || lower.starts_with("stratum2://")
                || lower.starts_with("sv2://")
            {
                return true;
            }
        }
    }
    // SV2 own-templates toggle forces the primary pool onto V2.
    updates
        .get("sv2OwnTemplatesEnabled")
        .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
        == Some(true)
}

fn shared_config_snapshot(state: &SharedState) -> SharedConfigSnapshot {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
    let board_cfg = config.board_config();
    SharedConfigSnapshot {
        schema: CONFIG_SCHEMA_VERSION,
        family: "bitaxe".to_string(),
        device_model: board_cfg.device_model.clone(),
        board_target: config.board_target().to_string(),
        board_version: Some(board_cfg.board_version.clone()),
        network: SharedNetworkConfig {
            hostname: compatibility_hostname(&config, &board_cfg.device_model),
            ipv4: if telem.device_ip.is_empty() {
                None
            } else {
                Some(telem.device_ip.clone())
            },
            ssid: if config.wifi_ssid.is_empty() {
                None
            } else {
                Some(config.wifi_ssid.clone())
            },
        },
        primary_pool: shared_pool_config(&config.stratum, true),
        fallback_pool: config
            .fallback_pool
            .as_ref()
            .map(|pool| shared_pool_config(pool, true)),
        mining: SharedMiningConfig {
            enabled: telem.mining_enabled,
            frequency_mhz: Some(config.target_frequency),
            voltage_mv: Some(config.target_voltage_mv),
            overclock_enabled: Some(config.overclock_enabled),
        },
        thermal: SharedThermalConfig {
            target_temp_c: if config.fan_target_temp_c == 0 {
                None
            } else {
                Some(config.fan_target_temp_c)
            },
            manual_fan_speed_pct: Some(config.fan_speed_pct),
        },
        auth: SharedAuthConfig {
            password_set: auth::password_is_set(state),
            metrics_require_auth: config.metrics_require_auth,
            allow_unsigned_ota: config.allow_unsigned_ota,
            session_auth: true,
        },
    }
}

fn apply_shared_config_patch(
    state: &SharedState,
    patch: SharedConfigPatch,
) -> Result<SharedConfigSnapshot, String> {
    let mining_enabled_patch = patch.mining.as_ref().and_then(|mining| mining.enabled);
    let mut wifi_changed = false;
    {
        let mut config = state
            .config
            .lock()
            .map_err(|_| "Failed to lock config".to_string())?;

        if let Some(network) = patch.network {
            if let Some(hostname) = network.hostname {
                config.hostname = hostname;
            }
            if let Some(ssid) = network.ssid {
                if ssid != config.wifi_ssid {
                    config.wifi_ssid = ssid;
                    wifi_changed = true;
                }
            }
            if let Some(password) = network.wifi_password {
                config.wifi_password = password;
                wifi_changed = true;
            }
        }

        if let Some(primary_pool) = patch.primary_pool {
            if let Some(url) = primary_pool.url {
                // B-ESP-10 round-trip guard (mirrors apply_config_updates): now
                // that GET /api/config/shared masks the URL, a dashboard re-POST
                // of the sanitized echo must KEEP the stored `user:pass@` creds
                // instead of clobbering them. A genuinely new URL still applies.
                if !crate::shared::is_sanitized_url_echo(&url, &config.stratum.url) {
                    config.stratum.url = url;
                }
            }
            if let Some(port) = primary_pool.port {
                config.stratum.port = port;
            }
            if let Some(worker) = primary_pool.worker {
                // B-ESP-10 round-trip guard: never overwrite the stored full BTC
                // payout address with its own read-mask echo from the form.
                if !crate::shared::is_masked_worker_echo(&worker, &config.stratum.worker_name) {
                    config.stratum.worker_name = worker;
                }
            }
            if let Some(password) = primary_pool.password {
                config.stratum.password = password;
            }
        }

        if let Some(fallback_patch) = patch.fallback_pool {
            let fallback_enabled = fallback_patch.enabled.unwrap_or(true);
            if !fallback_enabled {
                config.fallback_pool = None;
            } else {
                let version_rolling = config.stratum.version_rolling;
                let fallback =
                    config
                        .fallback_pool
                        .get_or_insert_with(|| dcentaxe_stratum::StratumConfig {
                            url: String::new(),
                            port: 3333,
                            worker_name: String::new(),
                            password: "x".to_string(),
                            suggest_difficulty: 0,
                            version_rolling,
                        });
                if let Some(url) = fallback_patch.url {
                    // B-ESP-10 round-trip guard: keep the stored fallback URL's
                    // `user:pass@` creds when the form re-POSTs the sanitized
                    // echo. A freshly-inserted fallback has an empty stored URL,
                    // so `is_sanitized_url_echo` is false and the new URL applies.
                    if !crate::shared::is_sanitized_url_echo(&url, &fallback.url) {
                        fallback.url = url;
                    }
                }
                if let Some(port) = fallback_patch.port {
                    fallback.port = port;
                }
                if let Some(worker) = fallback_patch.worker {
                    // B-ESP-10 round-trip guard: keep the stored fallback worker
                    // (full BTC payout address) on a masked-echo re-POST.
                    if !crate::shared::is_masked_worker_echo(&worker, &fallback.worker_name) {
                        fallback.worker_name = worker;
                    }
                }
                if let Some(password) = fallback_patch.password {
                    fallback.password = password;
                }
            }
        }

        if let Some(mining) = patch.mining {
            if mining.frequency_mhz.is_some() || mining.voltage_mv.is_some() {
                if let Ok(mut autotune) = state.autotuner.lock() {
                    if autotune.enabled {
                        info!("API: disabling autotuner due to shared-config manual operating-point override");
                        autotune.enabled = false;
                        autotune.status = "manual override".to_string();
                    }
                }
            }
            if let Some(overclock_enabled) = mining.overclock_enabled {
                config.overclock_enabled = overclock_enabled;
            }
            let qualified = config.qualify_operating_point(
                mining.frequency_mhz.unwrap_or(config.target_frequency),
                mining.voltage_mv.unwrap_or(config.target_voltage_mv),
                crate::config::ControlSurface::RestPatch,
            );
            config.target_frequency = qualified.frequency_mhz;
            config.target_voltage_mv = qualified.voltage_mv;
        }

        if let Some(thermal) = patch.thermal {
            if let Some(target_temp_c) = thermal.target_temp_c {
                let target_temp_c: u8 = target_temp_c;
                // MAINAPI-4: an explicit target_temp_c == 0 is ambiguous (manual vs
                // unset/auto) and would silently disable the thermal fan curve.
                // Reject it (-> HTTP 400); the operator must select manual mode
                // explicitly instead of relying on a stray 0.
                if target_temp_c == 0 {
                    return Err(
                        "Set fanMode=manual to disable the thermal curve; target_temp=0 is ambiguous"
                            .to_string(),
                    );
                }
                config.fan_target_temp_c = target_temp_c.clamp(0u8, 90u8);
            }
            if let Some(manual_fan_speed_pct) = thermal.manual_fan_speed_pct {
                let manual_fan_speed_pct: u8 = manual_fan_speed_pct;
                config.fan_speed_pct = manual_fan_speed_pct.clamp(20u8, 100u8);
            }
        }

        if let Some(auth_patch) = patch.auth {
            if let Some(metrics_require_auth) = auth_patch.metrics_require_auth {
                config.metrics_require_auth = metrics_require_auth;
            }
            if let Some(allow_unsigned_ota) = auth_patch.allow_unsigned_ota {
                config.allow_unsigned_ota = allow_unsigned_ota;
            }
        }

        config.canonicalize_identity();

        let mut nvs_guard = state
            .nvs
            .lock()
            .map_err(|_| "Failed to lock NVS".to_string())?;
        let nvs = nvs_guard
            .as_mut()
            .ok_or_else(|| "NVS handle not available".to_string())?;
        nvs_config::save_config(nvs, &config)?;
    }

    if let Some(enabled) = mining_enabled_patch {
        let mut telem = state
            .telemetry
            .lock()
            .map_err(|_| "Failed to lock telemetry".to_string())?;
        telem.mining_enabled = enabled;
    }

    if wifi_changed {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            unsafe {
                esp_idf_svc::sys::esp_restart();
            }
        });
    }

    Ok(shared_config_snapshot(state))
}

fn update_metadata(state: &SharedState) -> UpdateMetadata {
    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board_target = BUILD_BOARD_TARGET.to_string();
    let device_model = config.bitaxe_model().canonical_key().to_string();
    let factory_name = factory_package_name(&board_target, DCENTAXE_VERSION);
    let update_name = update_package_name(&board_target, DCENTAXE_VERSION);
    let signature_capable = crate::ota_signature::signature_required();
    let key_id = crate::ota_signature::compiled_key_id().map(|value| value.to_string());
    UpdateMetadata {
        schema: UPDATE_SCHEMA_VERSION,
        product: "DCENT_axe".to_string(),
        family: "bitaxe".to_string(),
        device_model: device_model.clone(),
        board_target: board_target.clone(),
        current_version: DCENTAXE_VERSION.to_string(),
        package_type: "esp32-factory-and-ota-bundle".to_string(),
        upload_endpoint: Some("/api/system/OTA".to_string()),
        board_target_header: Some("X-DCENT-Board-Target".to_string()),
        device_model_header: Some("X-DCENT-Device-Model".to_string()),
        inactive_slot_supported: true,
        signature_capable,
        signature_required: crate::ota_signature::ota_signature_required_for_display(
            signature_capable,
            config.allow_unsigned_ota,
            auth::password_is_set(state),
        ),
        allow_unsigned: config.allow_unsigned_ota,
        key_id: key_id.clone(),
        install_intent: None,
        toolbox: ToolboxPackageInfo {
            install_command: format!("dcent flash --serial <port> -f {}", factory_name),
            update_command: format!("dcent ota update <ip> -f {}", update_name),
            upload_endpoint: Some("/api/system/OTA".to_string()),
            board_target_header: Some("X-DCENT-Board-Target".to_string()),
            device_model_header: Some("X-DCENT-Device-Model".to_string()),
            requires_inactive_slot: true,
        },
    }
}

/// GET /api/system/info — AxeOS-compatible system info.
///
/// Returns ALL fields that ESP-Miner's GET_system_info handler returns so that
/// monitoring tools (Swarm, BitAxeHQ, etc.) work out of the box.
/// Field names and types match ESP-Miner http_server.c GET_system_info().
fn register_system_info(server: &mut EspHttpServer, state: SharedState) {
    server.fn_handler("/api/system/info", Method::Get, move |req| -> Result<(), Box<dyn std::error::Error>> {
        if let Err(err) = auth::authorize_rest_read(&req, &state) {
            return auth::write_auth_failure(req, err);
        }

        let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
        let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
        let autotune = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
        let swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
        let snap = stats.snapshot();

        // Compute derived values
        let board = config.board_config();
        let hostname_str = compatibility_hostname(&config, &board.device_model);
        let uptime_secs = telem.uptime_secs.max(snap.uptime_secs);
        let stratum_statuses = stratum_status_snapshots_with_recent_event_limit(
            &state,
            HOT_POLL_STRATUM_EVENT_LIMIT,
        );
        let primary_status = stratum_statuses.first();
        let fallback_status = stratum_statuses.get(1);
        let active_status = stratum_statuses
            .iter()
            .find(|status| status.connected)
            .or(primary_status);
        let (_shares_submitted, shares_accepted, shares_rejected) =
            aggregate_stratum_counters(&stratum_statuses)
                .unwrap_or((0, snap.accepted_shares, snap.rejected_shares));
        let error_pct = if (shares_accepted + shares_rejected) > 0 {
            shares_rejected as f64 / (shares_accepted + shares_rejected) as f64 * 100.0
        } else {
            0.0
        };

        let asic_model = board.asic_model.clone();
        let board_version = board.board_version.clone();
        let board_version_recognized = config.board_version_recognized();
        let support_status = config.support_status();
        let device_model = stock_device_model(&config);
        let swarm_color = stock_swarm_color(device_model);
        let auto_fan = matches!(swarm_control_mode(&config), HomeControlMode::Thermal) as u8;
        let (running_partition, _) = partition_labels();
        let fallback = config.fallback_pool.as_ref();
        let ota_signature_capable = crate::ota_signature::signature_required();
        let ota_signature_required = crate::ota_signature::ota_signature_required_for_display(
            ota_signature_capable,
            config.allow_unsigned_ota,
            auth::password_is_set(&state),
        );
        let primary_extranonce_subscribe = primary_status
            .map(|status| status.extranonce_subscribe_accepted as u8)
            .unwrap_or(0);
        let fallback_extranonce_subscribe = fallback_status
            .map(|status| status.extranonce_subscribe_accepted as u8)
            .unwrap_or(0);

        // MAC address from WiFi STA interface
        let mac_str = {
            let mut mac = [0u8; 6];
            unsafe { esp_idf_svc::sys::esp_wifi_get_mac(esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr()) };
            format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
        };

        // ESP-IDF version string
        let idf_ver = unsafe {
            let ptr = esp_idf_svc::sys::esp_get_idf_version();
            if ptr.is_null() { "unknown".to_string() }
            else { std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned() }
        };

        // Pre-compute values that can't use match/if inside json! macro
        let small_cores_per_chip: u32 = match config.asic_model() {
            dcentaxe_asic::AsicModel::BM1397 => 672,
            dcentaxe_asic::AsicModel::BM1366 => 894,
            dcentaxe_asic::AsicModel::BM1368 => 1276,
            dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 2040,
        };
        let cores_per_chip: u32 = match config.asic_model() {
            dcentaxe_asic::AsicModel::BM1397 => 168,
            dcentaxe_asic::AsicModel::BM1366 => 112,
            dcentaxe_asic::AsicModel::BM1368 => 80,
            dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 128,
        };
        let asic_count_val = if config.asic_count > 0 { config.asic_count as u32 } else { config.expected_asic_count() as u32 };
        // M-dash-1: expected hashrate is the host-pure, unit-tested derivation
        // (extraction only — byte-identical to the legacy inline calc).
        let expected_hr = crate::derived_metrics::expected_hashrate_ghs(
            config.target_frequency,
            small_cores_per_chip,
            asic_count_val,
        );
        let dcent_swarm = dcent_swarm_info(&config, &telem, &swarm, snap.hashrate_5m_ghs);
        let schedule_status = schedule_status_json(&config, uptime_secs);

        // M-dash-1: efficiency (J/TH) via the host-pure, unit-tested derivation
        // (extraction only — div-by-zero-guarded, byte-identical to the legacy calc).
        let efficiency =
            crate::derived_metrics::efficiency_jth(telem.power_w as f64, snap.hashrate_5m_ghs);
        let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
        let free_heap_internal = unsafe {
            esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL as u32)
        };
        let free_heap_spiram = unsafe {
            esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_SPIRAM as u32)
        };
        let psram_available = if free_heap_spiram > 0 { 1 } else { 0 };
        let wifi_status = {
            let mut wifi = state.wifi.lock().unwrap_or_else(|e| e.into_inner());
            match wifi.as_mut() {
                Some(wifi) if wifi.is_connected().unwrap_or(false) => "Connected",
                Some(_) => "Disconnected",
                None => "Unavailable",
            }
        };

        // M-dash-1 data-honesty: acceptance rate via the host-pure derivation.
        // `acceptance_rate_pct` returns None when NO shares have resolved yet, so
        // we do NOT fabricate a 100.0 "success" reading on a freshly-booted miner
        // (the legacy inline default). The wire carries a documented out-of-range
        // sentinel (-1.0 = unknown) plus the additive `acceptanceRateKnown`
        // companion so consumers distinguish "no data" from a real 100% accept rate.
        let acceptance_rate_opt =
            crate::derived_metrics::acceptance_rate_pct(shares_accepted, shares_rejected);
        let acceptance_rate_known = acceptance_rate_opt.is_some();
        let acceptance_rate = acceptance_rate_opt.unwrap_or(-1.0);
        let pool_difficulty = active_status
            .map(|status| status.difficulty)
            .filter(|diff| *diff > 0.0)
            .unwrap_or_else(|| {
                if telem.pool_difficulty > 0.0 {
                    telem.pool_difficulty
                } else {
                    config.stratum.suggest_difficulty as f64
                }
            });
        let pool_connection_info = active_status
            .map(|status| {
                format!(
                    "{}:{}",
                    crate::shared::sanitize_pool_url(&status.active_url),
                    status.active_port
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    crate::shared::sanitize_pool_url(&config.stratum.url),
                    config.stratum.port
                )
            });
        let pool_connected = active_status
            .map(|status| status.connected)
            .unwrap_or(telem.pool_connected);
        let is_using_fallback = primary_status
            .map(|status| status.failover_active as u8)
            .unwrap_or(0);
        let donating = primary_status.map(|status| status.donating).unwrap_or(false);
        let response_time_ms = active_status
            .map(|status| status.last_share_response_ms)
            .unwrap_or(0.0);
        let pool_shares_pending = active_status
            .map(|status| status.shares_pending)
            .unwrap_or(0);
        let pool_shares_unresolved = active_status
            .map(|status| status.shares_unresolved)
            .unwrap_or(0);
        let oldest_pending_submit_age_ms = active_status
            .map(|status| status.oldest_pending_submit_age_ms)
            .unwrap_or(0);
        let rejected_reasons: Vec<String> = primary_status
            .and_then(|status| {
                if status.last_reject_reason.is_empty() {
                    None
                } else {
                    Some(vec![status.last_reject_reason.clone()])
                }
            })
            .unwrap_or_default();

        // Pre-compute voltage domains and chip health for the json! macro
        let voltage_domains_json = {
            let model = config.bitaxe_model();
            let voltage = telem.voltage_mv as u32;
            match model {
                dcentaxe_hal::board::BitAxeModel::HexUltra |
                dcentaxe_hal::board::BitAxeModel::HexSupra |
                dcentaxe_hal::board::BitAxeModel::DcentAxeHexBm1397 => serde_json::json!([
                    {"mv": voltage, "chips": [0, 1]},
                    {"mv": voltage, "chips": [2, 3]},
                    {"mv": voltage, "chips": [4, 5]}
                ]),
                // Quad: 4 BM1397 on one parallel voltage domain.
                dcentaxe_hal::board::BitAxeModel::DcentAxeQuadBm1397 => serde_json::json!([
                    {"mv": voltage, "chips": [0, 1, 2, 3]}
                ]),
                dcentaxe_hal::board::BitAxeModel::GammaDuo |
                dcentaxe_hal::board::BitAxeModel::GammaTurbo => serde_json::json!([
                    {"mv": voltage, "chips": [0, 1]}
                ]),
                _ => serde_json::json!([
                    {"mv": voltage, "chips": [0]}
                ]),
            }
        };
        let chip_health_json = {
            let chips = &telem.chip_data;
            if chips.is_empty() {
                serde_json::json!(null)
            } else {
                let temps: Vec<f32> = chips.iter().filter_map(|c| c.temp_c).filter(|t| *t > 0.0).collect();
                if temps.len() != chips.len() {
                    serde_json::json!(null)
                } else {
                    let avg_t = if temps.is_empty() { 0.0 } else { temps.iter().sum::<f32>() / temps.len() as f32 };
                    let max_t = temps.iter().cloned().fold(0.0f32, f32::max);
                    let min_t = temps.iter().cloned().fold(f32::MAX, f32::min);
                    let spread = if temps.is_empty() { 0.0 } else { max_t - min_t };
                    let nonces: u32 = chips.iter().map(|c| c.shares).sum();
                    let errs: u32 = chips.iter().map(|c| c.hw_errors).sum();
                    let rate = if (nonces + errs) > 0 { errs as f64 / (nonces + errs) as f64 * 100.0 } else { 0.0 };
                    serde_json::json!({
                        "avgTemp": (avg_t * 10.0).round() / 10.0,
                        "maxTemp": (max_t * 10.0).round() / 10.0,
                        "minTemp": if temps.is_empty() { 0.0 } else { (min_t * 10.0).round() / 10.0 },
                        "tempSpread": (spread * 10.0).round() / 10.0,
                        "totalNonces": nonces,
                        "totalErrs": errs,
                        "errRate": (rate * 100.0).round() / 100.0
                    })
                }
            }
        };
        // Coinbase decode (populated by dispatcher on every mining.notify).
        // The user-share is computed client-side in dashboard/block-tile.js
        // from the stratum address; firmware exposes the raw outputs + total.
        // Phase T: borrow &[CoinbaseOutput] directly into the response struct
        // (eliminates the json!{} -> Value -> serialize round-trip).
        const EMPTY_COINBASE: &[dcentaxe_stratum::CoinbaseOutput] = &[];
        let (coinbase_outputs_slice, coinbase_total_sats, coinbase_user_sats) = match snap
            .current_block
            .as_ref()
            .and_then(|b| b.coinbase_outputs.as_ref().map(|outs| (b, outs)))
        {
            Some((b, outs)) => (
                outs.as_slice(),
                b.coinbase_total_sats.unwrap_or(0),
                b.coinbase_user_sats.unwrap_or(0),
            ),
            None => (EMPTY_COINBASE, 0u64, 0u64),
        };

        let hash_domains = stock_hash_domains(&asic_model) as usize;
        let hashrate_monitor_asics: Vec<serde_json::Value> = if telem.chip_data.is_empty()
            || telem.chip_data.iter().any(|chip| chip.hashrate_ghs.is_none())
        {
            Vec::new()
        } else {
            telem
                .chip_data
                .iter()
                .map(|chip| {
                    let chip_hashrate = chip.hashrate_ghs.unwrap_or(0.0);
                    let per_domain = if hash_domains > 0 {
                        chip_hashrate as f64 / hash_domains as f64
                    } else {
                        chip_hashrate as f64
                    };
                    serde_json::json!({
                        "total": chip_hashrate,
                        "domains": vec![per_domain; hash_domains.max(1)],
                        "errorCount": chip.hw_errors,
                    })
                })
                .collect()
        };

        // Phase T: pre-bind the borrow-only locals required by SystemInfoResponse.
        // The struct fields hold &str / &[T] borrows from these locals, so they
        // must outlive the `info` value below.
        let mode_str = format!("{:?}", autotune.mode);
        let pool_truth_last_reject = active_status
            .map(|status| status.last_reject_reason.clone())
            .unwrap_or_default();
        let pool_truth_active_pool = active_status
            .map(|status| {
                format!(
                    "{}:{}",
                    crate::shared::sanitize_pool_url(&status.active_url),
                    status.active_port
                )
            })
            .unwrap_or_else(|| pool_connection_info.clone());
        let pool_truth_failback_state = active_status
            .map(|status| status.primary_failback_state)
            .unwrap_or_default();
        let pool_truth_failback_detail = active_status
            .map(|status| status.primary_failback_detail.clone())
            .unwrap_or_default();
        let pool_truth_reject_counts = active_status
            .map(|status| status.reject_reason_counts.clone())
            .unwrap_or_default();
        let pool_truth_recent_events = active_status
            .map(|status| status.recent_events.clone())
            .unwrap_or_default();
        let pool_truth_protocol = if active_status.map(|status| status.pool_index) == Some(1) {
            config
                .split_pool
                .as_ref()
                .map(|split| match split.pool.protocol() {
                    dcentaxe_stratum::StratumProtocol::V1 => "stratum-v1",
                    dcentaxe_stratum::StratumProtocol::V2 => "stratum-v2",
                })
                .unwrap_or("stratum-v1")
        } else {
            match config.stratum.protocol() {
                dcentaxe_stratum::StratumProtocol::V1 => "stratum-v1",
                dcentaxe_stratum::StratumProtocol::V2 => "stratum-v2",
            }
        };
        let creature_stage_clone = telem.creature_stage.clone();
        let silicon_grade_clone = autotune.silicon_grade.clone();
        let autotune_status_clone = autotune.status.clone();
        // data-model-fields §7.4(b): owned clone so the borrow survives past the
        // autotuner guard drop, same pattern as silicon_grade/status above.
        let autotune_phase_clone = autotune.phase.clone();
        let runtime_board_target_str = config.board_target().to_string();
        let runtime_device_model_key = config.bitaxe_model().canonical_key().to_string();
        let build_device_model_key = crate::config::default_model_for_build()
            .canonical_key()
            .to_string();
        let build_board_version_str = crate::config::default_profile_for_build().board_version;
        let ota_key_id = crate::ota_signature::compiled_key_id().unwrap_or("").to_string();

        // B-ESP-10 read-surface masking: the worker is the operator's FULL BTC
        // payout address and a pool URL can embed `user:pass@` creds. Mirror the
        // Antminer rule (mask_wallet → <first6>…<last4>; sanitize_pool_url strips
        // the authority). Owned locals so the struct's `&str` borrows outlive
        // serialization.
        let stratum_url_masked = crate::shared::sanitize_pool_url(&config.stratum.url);
        let stratum_user_masked = crate::shared::mask_wallet(&config.stratum.worker_name);
        let fallback_url_masked = fallback
            .map(|fb| crate::shared::sanitize_pool_url(&fb.url))
            .unwrap_or_default();
        let fallback_user_masked = fallback
            .map(|fb| crate::shared::mask_wallet(&fb.worker_name))
            .unwrap_or_default();

        let info = crate::api_system_info::SystemInfoResponse {
            // ---- Power / Electrical (ESP-Miner: POWER_MANAGEMENT_MODULE) ----
            power: telem.power_w,
            voltage: telem.voltage_mv,
            current: telem.current_ma,
            max_power: config.power_limits().max_power_w,
            nominal_voltage: board.default_voltage_mv,

            // ---- Temperatures ----
            temp: telem.chip_temp_c,
            temp2: telem.board_temp_c,
            vr_temp: telem.vreg_temp_c,
            sensors_ok: telem.sensors_ok,

            // ---- Derived metrics ----
            uptime: telem.uptime_secs,
            wifi_rssi: telem.wifi_rssi,
            efficiency,
            acceptance_rate,
            acceptance_rate_known,

            // ---- Hashrate (ESP-Miner: GH/s) ----
            hash_rate: snap.hashrate_5m_ghs,
            hash_rate_1m: snap.hashrate_1m_ghs,
            hash_rate_5m: snap.hashrate_5m_ghs,
            hash_rate_10m: snap.hashrate_10m_ghs,
            hash_rate_15m: snap.hashrate_15m_ghs,
            hash_rate_1h: None,
            expected_hashrate: expected_hr,
            error_percentage: error_pct,

            // ---- Difficulty (ESP-Miner: uint64 nonce diff values) ----
            best_diff: snap.best_difficulty as u64,
            best_session_diff: snap.best_difficulty as u64,
            best_ever_diff: telem.best_diff_ever as u64,
            pool_difficulty,
            block_height: snap.block_height,

            // ---- Stratum fallback state ----
            pool_connected,
            is_using_fallback_stratum: is_using_fallback,
            pool_connection_info: pool_connection_info.as_str(),
            donation_set: config.donation.enabled,
            donating,
            donation_percent: config.donation.percent,

            // ---- Memory (ESP-Miner: esp_get_free_heap_size) ----
            is_psram_available: psram_available,
            free_heap,
            free_heap_internal,
            free_heap_spiram,

            // ---- Voltage / Frequency ----
            core_voltage: config.target_voltage_mv,
            core_voltage_actual: telem.voltage_mv as u16,
            frequency: config.target_frequency,

            // ---- WiFi ----
            ssid: config.wifi_ssid.as_str(),
            mac_addr: mac_str.as_str(),
            hostname: hostname_str.as_str(),
            ipv4: telem.device_ip.as_str(),
            ipv6: "",
            wifi_status,
            wifi_rssi_alias: telem.wifi_rssi,
            ap_enabled: 0,

            // ---- Shares (ESP-Miner: shares_accepted/rejected uint64) ----
            shares_accepted,
            shares_rejected,
            shares_rejected_reasons: rejected_reasons.as_slice(),
            stale_nonces: snap.stale_nonces,
            slot_recoveries: snap.slot_recoveries,

            // ---- Uptime ----
            uptime_seconds: uptime_secs,

            // ---- ASIC info ----
            asic_count: asic_count_val,
            core_count: cores_per_chip,
            small_core_count: small_cores_per_chip,
            dcent_total_small_core_count: small_cores_per_chip * asic_count_val,
            asic_model: asic_model.as_str(),
            device_model,
            swarm_color,

            // ---- Primary stratum config ----
            stratum_url: stratum_url_masked.as_str(),
            stratum_port: config.stratum.port,
            stratum_user: stratum_user_masked.as_str(),
            stratum_suggested_difficulty: config.stratum.suggest_difficulty,
            stratum_extranonce_subscribe: primary_extranonce_subscribe,
            stratum_tls: 0,
            stratum_cert: "",
            stratum_decode_coinbase: 0,
            // Protocol is derived from the URL scheme. The firmware accepts
            // `stratum+tcp://` (V1) and `stratum2+tcp://` (V2/Noise_NX).
            stratum_protocol: stratum_protocol_short(&config.stratum),
            stratum_v2_available: cfg!(feature = "stratum-v2"),
            // ESP-5: SV2 is mock-validated/experimental, not live-proven.
            stratum_v2_experimental: true,
            stratum_v2_status: "implemented + unit-tested; live delivery pending",
            mining_mode: config.mining_mode.as_str(),

            // ---- Fallback stratum config ----
            fallback_stratum_url: fallback_url_masked.as_str(),
            fallback_stratum_port: fallback.map(|fb| fb.port).unwrap_or(0),
            fallback_stratum_user: fallback_user_masked.as_str(),
            fallback_stratum_suggested_difficulty: fallback.map(|fb| fb.suggest_difficulty).unwrap_or(0),
            fallback_stratum_extranonce_subscribe: fallback_extranonce_subscribe,
            fallback_stratum_tls: 0,
            fallback_stratum_cert: "",
            fallback_stratum_decode_coinbase: 0,

            // ---- Response time ----
            response_time: response_time_ms,

            // ---- Firmware / Board version ----
            version: DCENTAXE_VERSION,
            axeos_version: DCENTAXE_VERSION,
            git_hash: env!("DCENTAXE_GIT_HASH"),
            git_dirty: env!("DCENTAXE_GIT_DIRTY") == "1",
            build_epoch: env!("DCENTAXE_BUILD_EPOCH").parse::<u64>().unwrap_or(0),
            has_bap: cfg!(feature = "bap"),
            display_name: config.board_config().model.name(),
            idf_version: idf_ver.as_str(),
            board_version: board_version.as_str(),
            board_version_recognized,
            support_status,
            board_target: BUILD_BOARD_TARGET,
            reset_reason: telem.reset_reason.as_str(),
            safe_mode: telem.safe_mode,
            wdt_reset_count: telem.wdt_reset_count,
            coredump_present: telem.coredump_present,
            // last_panic / last_restart_reason use skip_serializing_if so when
            // None they are omitted — keeping wire format byte-identical to
            // the legacy json!{} block which never emitted them.
            last_panic: None,
            last_restart_reason: None,
            running_partition: running_partition.as_str(),
            scriptsig: "",
            network_difficulty: 0,
            coinbase_outputs: coinbase_outputs_slice,
            coinbase_value_total_satoshis: coinbase_total_sats,
            coinbase_value_user_satoshis: coinbase_user_sats,

            // ---- Display / Screen settings ----
            overheat_mode: 0,
            overclock_enabled: config.overclock_enabled as u8,
            display: "",
            rotation: 0,
            invertscreen: if config.display_inverted { 1 } else { 0 },
            display_timeout: 0,

            // ---- Fan (ESP-Miner: fan_perc, fan_rpm) ----
            autofanspeed: auto_fan,
            fanspeed: telem.fan_speed_pct as f32,
            manual_fan_speed: config.fan_speed_pct,
            min_fan_speed: 20,
            temptarget: config.fan_target_temp_c,
            fanrpm: telem.fan_rpm,
            fan2rpm: telem.fan2_rpm,

            // ---- Statistics frequency ----
            stats_frequency: 5,

            // ---- Block found ----
            block_found: 0,
            show_new_block: false,

            // ---- Per-ASIC hashrate monitor ----
            hashrate_monitor: crate::api_system_info::HashrateMonitor {
                asics: hashrate_monitor_asics.as_slice(),
            },

            // ---- DCENT_axe extensions (ignored by stock AxeOS tools) ----
            firmware_type: "DCENT_axe",
            dcent_swarm: &dcent_swarm,
            // MQTT + HA config (read surface) — password masked to password_set.
            mqtt: crate::api_system_info::MqttView {
                enabled: config.mqtt.enabled,
                commands_enabled: config.mqtt.commands_enabled,
                broker_host: config.mqtt.broker_host.as_str(),
                broker_port: config.mqtt.broker_port,
                username: config.mqtt.username.as_str(),
                password_set: !config.mqtt.password.is_empty(),
                tls: config.mqtt.tls,
                publish_interval_s: config.mqtt.publish_interval_s,
            },
            dcentaxe: crate::api_system_info::DcentaxeExt {
                runtime_board_target: runtime_board_target_str.as_str(),
                build_board_target: BUILD_BOARD_TARGET,
                runtime_device_model: runtime_device_model_key.as_str(),
                build_device_model: build_device_model_key.as_str(),
                build_board_version: build_board_version_str,
                autotuner: crate::api_system_info::AutotunerView {
                    enabled: autotune.enabled,
                    mode: mode_str.as_str(),
                    target_value: autotune.target_value,
                    current_frequency: autotune.current_frequency,
                    current_voltage_mv: autotune.current_voltage_mv,
                    best_efficiency: autotune.best_efficiency,
                    hashrate15s: snap.hashrate_15s_ghs,
                    hashrate30s: snap.hashrate_30s_ghs,
                    last_good_frequency: autotune.last_good_frequency,
                    last_good_voltage_mv: autotune.last_good_voltage_mv,
                    last_good_jth: autotune.last_good_jth,
                    last_good_error_rate: autotune.last_good_error_rate,
                    silicon_grade: silicon_grade_clone.as_str(),
                    status: autotune_status_clone.as_str(),
                    phase: autotune_phase_clone.as_str(),
                },
                power_limits: crate::api_system_info::PowerLimitsView {
                    max_power_w: config.power_limits().max_power_w,
                    max_current_a: config.power_limits().max_current_a,
                    max_frequency: config.power_limits().max_frequency,
                    max_voltage_mv: config.power_limits().max_voltage_mv,
                },
                schedule: schedule_status,
                ota: crate::api_system_info::OtaView {
                    signature_capable: ota_signature_capable,
                    signature_required: ota_signature_required,
                    allow_unsigned: config.allow_unsigned_ota,
                    key_id: ota_key_id.as_str(),
                },
                pool_truth: crate::api_system_info::PoolTruthView {
                    connected: active_status.map(|status| status.connected).unwrap_or(pool_connected),
                    difficulty: pool_difficulty,
                    shares_submitted: active_status.map(|status| status.shares_submitted).unwrap_or(0),
                    shares_accepted: active_status.map(|status| status.shares_accepted).unwrap_or(shares_accepted),
                    shares_rejected: active_status.map(|status| status.shares_rejected).unwrap_or(shares_rejected),
                    shares_pending: pool_shares_pending,
                    shares_unresolved: pool_shares_unresolved,
                    oldest_pending_submit_age_ms,
                    response_time_ms,
                    last_share_submit_unix_ms: active_status.map(|status| status.last_share_submit_unix_ms).unwrap_or(0),
                    last_share_response_unix_ms: active_status.map(|status| status.last_share_response_unix_ms).unwrap_or(0),
                    last_share_accepted_unix_ms: active_status.map(|status| status.last_share_accepted_unix_ms).unwrap_or(0),
                    last_share_rejected_unix_ms: active_status.map(|status| status.last_share_rejected_unix_ms).unwrap_or(0),
                    failover_active: active_status.map(|status| status.failover_active).unwrap_or(false),
                    primary_failback_state: pool_truth_failback_state,
                    primary_failback_detail: pool_truth_failback_detail.as_str(),
                    last_primary_reprobe_unix_ms: active_status.map(|status| status.last_primary_reprobe_unix_ms).unwrap_or(0),
                    last_primary_failback_unix_ms: active_status.map(|status| status.last_primary_failback_unix_ms).unwrap_or(0),
                    last_reject_reason: pool_truth_last_reject.as_str(),
                    active_pool: pool_truth_active_pool.as_str(),
                    protocol: pool_truth_protocol,
                    reject_reason_counts: pool_truth_reject_counts.as_slice(),
                    recent_events: pool_truth_recent_events.as_slice(),
                },
                dispatcher: crate::api_system_info::DispatcherView {
                    stale_nonces: snap.stale_nonces,
                    slot_recoveries: snap.slot_recoveries,
                    filtered_nonces: snap.filtered_shares,
                    nonces_found: snap.nonces_found,
                    ticket_difficulty: snap.ticket_difficulty,
                },
                board_temp: telem.board_temp_c,
                inlet_temp: telem.inlet_temp_c,
                outlet_temp: telem.outlet_temp_c,
                input_voltage: telem.input_voltage_mv,
                achievements: telem.achievements,
                achievement_count: telem.achievement_count,
                lifetime_shares: telem.lifetime_shares,
                creature_stage: creature_stage_clone.as_str(),
                creature_mood: telem.creature_mood,
                mining_enabled: telem.mining_enabled,
                // Per-chip data for multi-chip boards (GT, Hex). Phase R
                // streaming serializer (`ChipsView`) borrows directly from
                // telem.chip_data — no Vec<Value> allocation.
                chips: crate::api_system_info::ChipsView(telem.chip_data.as_slice()),
                voltage_domains: voltage_domains_json,
                chip_health: chip_health_json,
                // data-model-fields §2/§4: power provenance + calibration honesty.
                // INA260 board watts are `measured`; any wall-watts/sats/cost
                // estimate is `estimated` + `calibrated:false` (axe has no
                // wall-meter input). network_difficulty mirrors the top-level u8.
                power: crate::api_system_info::DcentaxePowerView {
                    source: "measured",
                    calibrated: false,
                    estimate_source: "estimated",
                    network_difficulty: 0,
                },
                // data-model-fields §1: temp-provenance token DERIVED from two
                // existing board booleans — no new measurement. `ambient_proxy`
                // when the EMC2101 internal-die proxy is in use; `board_sensor`
                // when a real read is available; honest null (omitted) when no
                // sensor returned data at all.
                temp_source: if !telem.sensors_ok {
                    None
                } else if telem.chip_temp_is_ambient_proxy {
                    Some("ambient_proxy")
                } else {
                    Some("board_sensor")
                },
            },
            // On-board LoRa mesh snapshot (feature-gated, additive). `None` until
            // the radio task publishes a snapshot ⇒ key omitted (skip_if None);
            // whole field compiled out when `lora` is OFF ⇒ byte-identical wire.
            #[cfg(feature = "lora")]
            lora: crate::lora_task::system_info_view(),
        };

        let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
        serde_json::to_writer(&mut StdJsonWriter(&mut resp), &info)?;
        Ok(())
    }).expect("Failed to register GET /api/system/info");
}

/// Public, read-only trust-but-verify disclosure. No credentials are returned;
/// the payout address is firmware-baked and auditable on-chain.
fn register_donation_info(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/donation/info",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
                let statuses = stratum_status_snapshots(&state);
                let donating = statuses.first().map(|status| status.donating).unwrap_or(false);
                let pool_url = crate::shared::sanitize_pool_url(&config.donation.pool_url);
                let pool_host = pool_url
                    .strip_prefix("stratum+tcp://")
                    .unwrap_or(pool_url.as_str());
                let explorer_url = format!(
                    "https://mempool.space/address/{}",
                    crate::config::DONATION_PAYOUT_ADDRESS
                );
                let body = json!({
                    "pool_url": pool_url,
                    "pool_host": pool_host,
                    "worker": config.donation.worker,
                    "payout_address": crate::config::DONATION_PAYOUT_ADDRESS,
                    "explorer_url": explorer_url,
                    "explorer_name": "mempool.space",
                    "verify_label": "View on-chain payout history",
                    "trust_model": "trust_but_verify",
                    "disclosure": "Donation slice flows to the address above. Verify on the block explorer.",
                    "enabled": config.donation.enabled,
                    "donating": donating,
                    "percent": config.donation.percent,
                });
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                serde_json::to_writer(&mut StdJsonWriter(&mut resp), &body)?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/donation/info");
}

/// GET /api/v1/capabilities -- shared DCENT_OS capability descriptor.
///
/// Additive endpoint. Do not fold this into `/api/system/info`, whose AxeOS
/// compatibility shape is intentionally byte-stable for existing tools.
fn register_capabilities(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/v1/capabilities",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_read(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }

                let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
                let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
                let descriptor = crate::capabilities::build_esp_capability_descriptor(
                    &config,
                    telem.mining_enabled,
                    state.board_limits.min_frequency,
                    state.board_limits.max_frequency,
                    state.board_limits.min_voltage_mv,
                    state.board_limits.max_voltage_mv,
                );

                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                serde_json::to_writer(&mut StdJsonWriter(&mut resp), &descriptor)?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/v1/capabilities");
}

/// GET /api/system/asic — AxeOS-compatible ASIC settings and family metadata.
fn register_system_asic(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/asic",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
                let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
                let swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());

                let board = config.board_config();
                let device_model = stock_device_model(&config);
                let asic_model = board.asic_model.as_str();
                let asic_count = if config.asic_count > 0 {
                    config.asic_count
                } else {
                    config.expected_asic_count()
                };
                let stock_settings = crate::config::stock_asic_settings(config.bitaxe_model());
                let frequency_options = stock_frequency_options(&config);
                let voltage_options = stock_voltage_options(&config);
                let dcent_swarm = dcent_swarm_info(&config, &telem, &swarm, 0.0);

                let body = serde_json::to_string(&serde_json::json!({
                    "ASICModel": asic_model,
                    "deviceModel": device_model,
                    "boardVersion": board.board_version,
                    "boardVersionRecognized": config.board_version_recognized(),
                    "supportStatus": config.support_status(),
                    "boardTarget": BUILD_BOARD_TARGET,
                    "runtimeBoardTarget": config.board_target(),
                    "swarmColor": stock_swarm_color(device_model),
                    "asicCount": asic_count,
                    "hashDomains": stock_hash_domains(asic_model),
                    "defaultFrequency": stock_settings.default_frequency,
                    "frequencyOptions": frequency_options,
                    "defaultVoltage": stock_settings.default_voltage_mv,
                    "voltageOptions": voltage_options,
                    "dcentSwarm": dcent_swarm,
                }))
                .unwrap_or_default();

                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/system/asic");
}

/// GET /api/system/statistics — compact AxeOS-style statistics history.
fn register_system_statistics(server: &mut EspHttpServer, state: SharedState) {
    let register = |path: &str, state: SharedState, server: &mut EspHttpServer| {
        server
            .fn_handler(
                path,
                Method::Get,
                move |req| -> Result<(), Box<dyn std::error::Error>> {
                    let history = state
                        .history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
                    let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
                    let snap = stats.snapshot();
                    let stratum_statuses = stratum_status_snapshots(&state);
                    let primary_status = stratum_statuses.first();
                    let current_timestamp = unix_time_ms();
                    let labels = vec![
                        "hashrate",
                        "hashrate_15s",
                        "hashrate_30s",
                        "power",
                        "asicTemp",
                        "localAcceptedShares",
                        "localRejectedShares",
                        "sharesSubmitted",
                        "poolAcceptedShares",
                        "poolRejectedShares",
                        "responseTime",
                        "failoverActive",
                        "connected",
                        "timestamp",
                    ];

                    let requested_columns = req.uri().split('?').nth(1).and_then(|query| {
                        query
                            .split('&')
                            .find_map(|pair| pair.strip_prefix("columns="))
                            .map(|columns| {
                                columns
                                    .split(',')
                                    .filter(|column| !column.is_empty())
                                    .collect::<Vec<_>>()
                            })
                    });

                    let effective_labels: Vec<&str> = match requested_columns {
                        Some(columns) if !columns.is_empty() => labels
                            .iter()
                            .copied()
                            .filter(|label| columns.iter().any(|column| column == label))
                            .collect(),
                        _ => labels.clone(),
                    };

                    let statistics: Vec<Vec<f64>> = if history.samples.is_empty() {
                        vec![effective_labels
                            .iter()
                            .map(|label| match *label {
                                "hashrate" => snap.hashrate_5m_ghs,
                                "hashrate_15s" => snap.hashrate_15s_ghs,
                                "hashrate_30s" => snap.hashrate_30s_ghs,
                                "power" => telem.power_w as f64,
                                "asicTemp" => telem.chip_temp_c as f64,
                                "localAcceptedShares" => snap.accepted_shares as f64,
                                "localRejectedShares" => snap.rejected_shares as f64,
                                "sharesSubmitted" => primary_status
                                    .map(|status| status.shares_submitted)
                                    .unwrap_or(0)
                                    as f64,
                                "poolAcceptedShares" => primary_status
                                    .map(|status| status.shares_accepted)
                                    .unwrap_or(snap.accepted_shares)
                                    as f64,
                                "poolRejectedShares" => primary_status
                                    .map(|status| status.shares_rejected)
                                    .unwrap_or(snap.rejected_shares)
                                    as f64,
                                "responseTime" => primary_status
                                    .map(|status| status.last_share_response_ms)
                                    .unwrap_or(0.0),
                                "failoverActive" => {
                                    if primary_status
                                        .map(|status| status.failover_active)
                                        .unwrap_or(false)
                                    {
                                        1.0
                                    } else {
                                        0.0
                                    }
                                }
                                "connected" => {
                                    if primary_status
                                        .map(|status| status.connected)
                                        .unwrap_or(telem.pool_connected)
                                    {
                                        1.0
                                    } else {
                                        0.0
                                    }
                                }
                                "timestamp" => current_timestamp as f64,
                                _ => 0.0,
                            })
                            .collect()]
                    } else {
                        history
                            .samples
                            .iter()
                            .map(|sample| {
                                let mut row = Vec::new();
                                for label in &effective_labels {
                                    row.push(match *label {
                                        "hashrate" => sample.hashrate_ghs,
                                        "hashrate_15s" => sample.hashrate_15s_ghs,
                                        "hashrate_30s" => sample.hashrate_30s_ghs,
                                        "power" => sample.power_w as f64,
                                        "asicTemp" => sample.temp_c as f64,
                                        "localAcceptedShares" => {
                                            sample.local_accepted_shares as f64
                                        }
                                        "localRejectedShares" => {
                                            sample.local_rejected_shares as f64
                                        }
                                        "sharesSubmitted" => sample.submitted_shares as f64,
                                        "poolAcceptedShares" => sample.pool_accepted_shares as f64,
                                        "poolRejectedShares" => sample.pool_rejected_shares as f64,
                                        "responseTime" => sample.response_time_ms,
                                        "failoverActive" => {
                                            if sample.failover_active {
                                                1.0
                                            } else {
                                                0.0
                                            }
                                        }
                                        "connected" => {
                                            if sample.connected {
                                                1.0
                                            } else {
                                                0.0
                                            }
                                        }
                                        "timestamp" => sample.ts_unix_ms as f64,
                                        _ => 0.0,
                                    });
                                }
                                row
                            })
                            .collect()
                    };

                    let body = serde_json::to_string(&serde_json::json!({
                        "currentTimestamp": current_timestamp,
                        "labels": effective_labels,
                        "statistics": statistics
                    }))
                    .unwrap_or_default();

                    let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                    let _ = resp.write(body.as_bytes());
                    Ok(())
                },
            )
            .expect("Failed to register statistics endpoint");
    };

    register("/api/system/statistics", state.clone(), server);
    register("/api/system/statistics/dashboard", state.clone(), server);
}

fn register_system_wifi_scan(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/wifi/scan",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut wifi = state.wifi.lock().unwrap_or_else(|e| e.into_inner());
                let networks = match wifi.as_mut() {
                    Some(wifi) => match wifi.scan() {
                        Ok(networks) => networks,
                        Err(err) => {
                            let body = serde_json::json!({
                                "message": format!("WiFi scan failed: {:?}", err),
                                "networks": [],
                            })
                            .to_string();
                            let mut resp = req.into_response(500, None, &JSON_HEADERS)?;
                            let _ = resp.write(body.as_bytes());
                            return Ok(());
                        }
                    },
                    None => {
                        let body = serde_json::json!({
                            "message": "WiFi interface unavailable",
                            "networks": [],
                        })
                        .to_string();
                        let mut resp = req.into_response(503, None, &JSON_HEADERS)?;
                        let _ = resp.write(body.as_bytes());
                        return Ok(());
                    }
                };

                let networks: Vec<serde_json::Value> = networks
                    .into_iter()
                    .map(|ap| {
                        serde_json::json!({
                            "ssid": ap.ssid.as_str(),
                            "rssi": ap.signal_strength,
                            "authmode": auth_mode_code(ap.auth_method),
                        })
                    })
                    .collect();

                let body = serde_json::json!({ "networks": networks }).to_string();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/system/wifi/scan");
}

fn register_stock_cgi(server: &mut EspHttpServer, state: SharedState) {
    let system_info_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/get_system_info.cgi",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let config = system_info_state
                    .config
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let board = config.board_config();
                let body = serde_json::to_string(&json!({
                    "hostname": compatibility_hostname(&config, &board.device_model),
                    "macaddr": device_mac_string(),
                    "serinum": device_serial(config.board_target()),
                    "minertype": stock_device_model(&config),
                    "board_version": board.board_version,
                    "board_target": config.board_target(),
                    "firmware": env!("CARGO_PKG_VERSION"),
                }))
                .unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /cgi-bin/get_system_info.cgi");

    let miner_conf_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/get_miner_conf.cgi",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let config = miner_conf_state
                    .config
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let telem = miner_conf_state
                    .telemetry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let body = serde_json::to_string(&json!({
                    "pools": [{
                        // B-ESP-10: mask worker (BTC payout addr) + sanitize URL.
                        "url": crate::shared::sanitize_pool_url(&config.stratum.url),
                        "port": config.stratum.port,
                        "user": crate::shared::mask_wallet(&config.stratum.worker_name),
                        "pass": "x",
                    }],
                    "bitmain-freq": config.target_frequency,
                    "bitmain-voltage": config.target_voltage_mv,
                    "bitmain-use-vil": 0,
                    "bitmain-fan-ctrl": if config.fan_target_temp_c > 0 { 1 } else { 0 },
                    "is_mining": telem.mining_enabled,
                }))
                .unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /cgi-bin/get_miner_conf.cgi");

    let summary_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/summary.cgi",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let stats = summary_state
                    .stats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let telem = summary_state
                    .telemetry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let snap = stats.snapshot();
                let body = serde_json::to_string(&json!({
                    "SUMMARY": [{
                        "elapsed": telem.uptime_secs.max(snap.uptime_secs),
                        "ghs5s": snap.hashrate_5s_ghs,
                        "ghsav": snap.hashrate_5m_ghs,
                        "accepted": snap.accepted_shares,
                        "rejected": snap.rejected_shares,
                        "bestshare": snap.best_difficulty,
                        "temperature": telem.chip_temp_c,
                    }]
                }))
                .unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /cgi-bin/summary.cgi");

    let blink_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/get_blink_status.cgi",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let swarm = blink_state.swarm.lock().unwrap_or_else(|e| e.into_inner());
                let active = swarm.identify_until_epoch_s > unix_time_s();
                let body = serde_json::to_string(&json!({ "blink": active })).unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /cgi-bin/get_blink_status.cgi");

    let blink_post_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/blink.cgi",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &blink_post_state) {
                    return auth::write_auth_failure(req, err);
                }
                let mut swarm = blink_post_state
                    .swarm
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                swarm.identify_until_epoch_s = unix_time_s() + 30;
                let body = serde_json::to_string(&json!({ "success": true, "blink": true }))
                    .unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register POST /cgi-bin/blink.cgi");

    let reboot_state = state.clone();
    server
        .fn_handler(
            "/cgi-bin/reboot.cgi",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &reboot_state) {
                    return auth::write_auth_failure(req, err);
                }
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_secs(2));
                    unsafe { esp_idf_svc::sys::esp_restart() }
                });
                let body = serde_json::to_string(&json!({
                    "success": true,
                    "message": "Rebooting in 2 seconds",
                }))
                .unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register POST /cgi-bin/reboot.cgi");
}

/// GET /api/system — pool and board configuration (AxeOS-compatible).
///
/// ESP-Miner uses PATCH /api/system for settings updates. We support
/// PATCH (native), POST (legacy), and GET for reading current config.
fn register_system_config(server: &mut EspHttpServer, state: SharedState) {
    let state_get = state.clone();
    server.fn_handler("/api/system", Method::Get, move |req| -> Result<(), Box<dyn std::error::Error>> {
        let config = state_get.config.lock().unwrap_or_else(|e| e.into_inner());
        let board = config.board_config();
        let hostname_str = compatibility_hostname(&config, &board.device_model);

        let asic_model = board.asic_model.as_str();
        let auto_fan = if config.fan_target_temp_c > 0 { 1 } else { 0 };
        let fallback = config.fallback_pool.as_ref();
        let extranonce_subscribe = 1;

        // Return config using the same field names ESP-Miner's PATCH endpoint accepts
        let info = serde_json::json!({
            // Stratum primary — B-ESP-10: worker (BTC payout addr) masked, URL sanitized.
            "stratumURL": crate::shared::sanitize_pool_url(&config.stratum.url),
            "stratumPort": config.stratum.port,
            "stratumUser": crate::shared::mask_wallet(&config.stratum.worker_name),
            "stratumProtocol": stratum_protocol_short(&config.stratum),
            "stratumV2Available": cfg!(feature = "stratum-v2"),
            "stratumV2Experimental": true,
            "stratumV2Status": "implemented + unit-tested; live delivery pending",
            "miningMode": config.mining_mode.as_str(),
            "sv2OwnTemplatesEnabled": config.sv2_own_templates.enabled,
            "sv2TemplateProxyURL": config.sv2_own_templates.mining_proxy_url,
            "sv2TemplateProviderURL": config.sv2_own_templates.template_provider_url,
            "sv2JobDeclaratorURL": config.sv2_own_templates.job_declarator_url,
            "stratumPassword": "x",
            "stratumSuggestedDifficulty": config.stratum.suggest_difficulty,
            "stratumExtranonceSubscribe": extranonce_subscribe,
            "stratumTLS": 0,
            "stratumCert": "",
            "stratumDecodeCoinbase": 0,

            // Stratum fallback — B-ESP-10 masked.
            "fallbackStratumURL": fallback.map(|fb| crate::shared::sanitize_pool_url(&fb.url)).unwrap_or_default(),
            "fallbackStratumPort": fallback.map(|fb| fb.port).unwrap_or(0),
            "fallbackStratumUser": fallback.map(|fb| crate::shared::mask_wallet(&fb.worker_name)).unwrap_or_default(),
            "fallbackStratumProtocol": fallback.map(stratum_protocol_short).unwrap_or("v1"),
            "fallbackStratumPassword": "x",
            "fallbackStratumSuggestedDifficulty": fallback.map(|fb| fb.suggest_difficulty).unwrap_or(0),
            "fallbackStratumExtranonceSubscribe": extranonce_subscribe,
            "fallbackStratumTLS": 0,
            "fallbackStratumCert": "",
            "fallbackStratumDecodeCoinbase": 0,

            // Hashrate splitting — B-ESP-10 masked.
            "splitPoolEnabled": config.split_pool.is_some(),
            "splitPoolURL": config.split_pool.as_ref().map(|s| crate::shared::sanitize_pool_url(&s.pool.url)).unwrap_or_default(),
            "splitPoolPort": config.split_pool.as_ref().map(|s| s.pool.port).unwrap_or(0),
            "splitPoolUser": config.split_pool.as_ref().map(|s| crate::shared::mask_wallet(&s.pool.worker_name)).unwrap_or_default(),
            "splitPoolProtocol": config.split_pool.as_ref().map(|s| stratum_protocol_short(&s.pool)).unwrap_or("v1"),
            "splitPoolPct": config.split_pool.as_ref().map(|s| s.hashrate_pct).unwrap_or(0),

            // Voluntary time-sliced donation. Passwords are never returned.
            "donationEnabled": config.donation.enabled,
            "donationPercent": config.donation.percent,
            "donationPoolURL": crate::shared::sanitize_pool_url(&config.donation.pool_url),
            "donationWorker": config.donation.worker,
            "donationPasswordSet": !config.donation.password.is_empty(),
            "donationFallbackEnabled": config.donation.fallback_enabled,
            "donationFallbackPoolURL": crate::shared::sanitize_pool_url(&config.donation.fallback_pool_url),
            "donationFallbackWorker": config.donation.fallback_worker,
            "donationFallbackPasswordSet": !config.donation.fallback_password.is_empty(),
            "donationCycleDuration": config.donation.cycle_duration_s,

            // Outbound notifications. Secrets/webhook URLs are never returned.
            "notificationsEnabled": config.notifications.enabled,
            "telegramConfigured": !config.notifications.telegram_bot_token.is_empty() && !config.notifications.telegram_chat_id.is_empty(),
            "discordConfigured": !config.notifications.discord_webhook_url.is_empty(),
            "slackConfigured": !config.notifications.slack_webhook_url.is_empty(),
            "notificationShareMilestone": config.notifications.share_milestone,
            "notificationThermalAlerts": config.notifications.thermal_alerts,
            "notificationFailoverAlerts": config.notifications.failover_alerts,
            "notificationOtaAlerts": config.notifications.ota_alerts,

            // Device identity
            "hostname": hostname_str,
            "ssid": config.wifi_ssid,
            "wifiPass": "",  // never expose
            "boardVersion": board.board_version,
            "boardVersionRecognized": config.board_version_recognized(),
            "supportStatus": config.support_status(),
            "boardTarget": BUILD_BOARD_TARGET,
            "runtimeBoardTarget": config.board_target(),
            "deviceModel": stock_device_model(&config),
            "ASICModel": asic_model,

            // ASIC settings
            "frequency": config.target_frequency,
            "coreVoltage": config.target_voltage_mv,
            "asicCount": if config.asic_count > 0 { config.asic_count } else { config.expected_asic_count() },
            "coreCount": match config.asic_model() {
                dcentaxe_asic::AsicModel::BM1397 => 168,
                dcentaxe_asic::AsicModel::BM1366 => 112,
                dcentaxe_asic::AsicModel::BM1368 => 80,
                dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 128,
            },
            "smallCoreCount": match config.asic_model() {
                dcentaxe_asic::AsicModel::BM1397 => 672,
                dcentaxe_asic::AsicModel::BM1366 => 894,
                dcentaxe_asic::AsicModel::BM1368 => 1276,
                dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 2040,
            },
            "versionRolling": config.stratum.version_rolling,

            // Fan
            "autofanspeed": auto_fan,
            "fanspeed": config.fan_speed_pct,
            "manualFanSpeed": config.fan_speed_pct,
            "minFanSpeed": 20,
            "temptarget": config.fan_target_temp_c,

            // Display
            "overheat_mode": 0,
            "overclockEnabled": config.overclock_enabled as u8,
            "invertscreen": if config.display_inverted { 1 } else { 0 },
            "display": "",
            "rotation": 0,
            "displayTimeout": 0,
        });
        let body = serde_json::to_string(&info).unwrap_or_default();
        let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
        let _ = resp.write(body.as_bytes());
        Ok(())
    }).expect("Failed to register GET /api/system");

    server
        .fn_handler(
            "/api/system",
            Method::Options,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"");
                Ok(())
            },
        )
        .expect("Failed to register OPTIONS /api/system");

    // PATCH /api/system — update config (ESP-Miner's native method)
    let state_patch = state.clone();
    server
        .fn_handler(
            "/api/system",
            Method::Patch,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_patch) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF (a PATCH split
                // across TCP segments must not be truncated to a partial config).
                let body = match read_full_body(&mut req, 4096) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                // MAINAPI-4: apply_config_updates now rejects an ambiguous explicit
                // target_temp=0 with an error message -> 400.
                if let Err(msg) = apply_config_updates(&state_patch, &body) {
                    let mut resp = req.into_response(400, Some("Bad Request"), &COMPAT_HEADERS)?;
                    let _ = resp.write(
                        json!({ "success": false, "message": msg })
                            .to_string()
                            .as_bytes(),
                    );
                    return Ok(());
                }
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                // ESP-Miner returns empty body on PATCH success
                let _ = resp.write(b"");
                Ok(())
            },
        )
        .expect("Failed to register PATCH /api/system");

    // POST /api/system — update config (legacy/additional compatibility)
    let state_post = state.clone();
    server
        .fn_handler(
            "/api/system",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_post) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 2048) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                // MAINAPI-4: reject an ambiguous explicit target_temp=0 -> 400.
                if let Err(msg) = apply_config_updates(&state_post, &body) {
                    let mut resp = req.into_response(400, Some("Bad Request"), &COMPAT_HEADERS)?;
                    let _ = resp.write(
                        json!({ "success": false, "message": msg })
                            .to_string()
                            .as_bytes(),
                    );
                    return Ok(());
                }
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(
                    b"{\"success\":true,\"note\":\"Changes applied. Use reboot to persist.\"}",
                );
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system");
}

fn register_shared_config(server: &mut EspHttpServer, state: SharedState) {
    let state_get = state.clone();
    server
        .fn_handler(
            "/api/config/shared",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_read(&req, &state_get) {
                    return auth::write_auth_failure(req, err);
                }
                let body =
                    serde_json::to_string(&shared_config_snapshot(&state_get)).unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/config/shared");

    let state_post = state.clone();
    server
        .fn_handler(
            "/api/config/shared",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_post) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 2048) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                let patch = serde_json::from_slice::<SharedConfigPatch>(&body);
                match patch {
                    Ok(patch) => {
                        // AOTA-1 defense-in-depth: refuse to flip the
                        // unsigned-OTA policy unless the caller is an
                        // authenticated owner (password set + valid bearer
                        // session). Closes the "PATCH allow_unsigned_ota=true"
                        // leg independently of the OTA-handler enforcement;
                        // other (non-security) shared-config writes keep the
                        // existing passwordless-write semantics.
                        if patch
                            .auth
                            .as_ref()
                            .and_then(|a| a.allow_unsigned_ota)
                            .is_some()
                            && !crate::ota_signature::owner_action_authorized(
                                auth::password_is_set(&state_post),
                                auth::request_has_owner_session(&req, &state_post),
                            )
                        {
                            return auth::write_auth_failure(
                                req,
                                auth::AuthFailure::Unauthorized(
                                    "Claim the device (set an owner password) and sign in before changing the unsigned-OTA policy",
                                ),
                            );
                        }
                        match apply_shared_config_patch(&state_post, patch) {
                            Ok(snapshot) => {
                                let body = serde_json::to_string(&json!({
                                    "status": "ok",
                                    "config": snapshot,
                                }))
                                .unwrap_or_default();
                                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                                resp.write(body.as_bytes())?;
                            }
                            Err(e) => {
                                let mut resp = req.into_response(400, None, &JSON_HEADERS)?;
                                resp.write(
                                    serde_json::json!({"status": "error", "message": e})
                                        .to_string()
                                        .as_bytes(),
                                )?;
                            }
                        }
                    }
                    Err(e) => {
                        let mut resp = req.into_response(400, None, &JSON_HEADERS)?;
                        resp.write(
                            serde_json::json!({"status": "error", "message": format!("Invalid shared config patch: {}", e)})
                                .to_string()
                                .as_bytes(),
                        )?;
                    }
                }
                Ok(())
            },
        )
        .expect("Failed to register POST /api/config/shared");
}

/// Apply config updates from a JSON body. Shared by PATCH and POST handlers.
///
/// Accepts all field names that ESP-Miner's check_settings_and_update() handles,
/// plus DCENT_axe-specific fields.
///
/// MAINAPI-4: returns `Err(msg)` (-> HTTP 400) only for an ambiguous explicit
/// request — currently `temptarget`/`fanTargetTemp == 0`, which is overloaded as
/// BOTH "manual mode" and the unset default, so a stray 0 would silently disable
/// the thermal fan curve. The caller must use `fanMode=manual` /
/// `autofanspeed=0` to disable the curve unambiguously. Returns `Ok(())` for all
/// other inputs (including an unparseable body, preserving the prior tolerant
/// behaviour).
fn apply_config_updates(state: &SharedState, body: &[u8]) -> Result<(), String> {
    if let Ok(updates) = serde_json::from_slice::<serde_json::Value>(body) {
        // H4/SV2-AVAIL: on a firmware build WITHOUT the stratum-v2 feature, the SV2
        // client is a no-op stub that sleeps forever — accepting a V2 pool config
        // here would silently dead-end mining with no failover. Reject the request
        // up-front (mirrors the MAINAPI-4 Err -> HTTP 400 pattern) BEFORE any
        // mutation, so an unusable config is never persisted. No-op (compiled out)
        // when the stratum-v2 feature IS present, so V2 behaves exactly as before.
        #[cfg(not(feature = "stratum-v2"))]
        {
            if config_update_requests_stratum_v2(&updates) {
                return Err(
                    "Stratum V2 is not available in this firmware build; select a Stratum V1 (TCP) pool instead"
                        .to_string(),
                );
            }
        }
        let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut pool_changed = false;

        // ---- Stratum primary ----
        let requested_primary_protocol = updates
            .get("stratumProtocol")
            .and_then(requested_stratum_protocol);
        if let Some(raw) = updates.get("stratumURL").and_then(|v| v.as_str()) {
            // B-ESP-10 round-trip guard: the dashboard form re-POSTs the
            // read-sanitized URL it last rendered; treat that echo as "keep the
            // stored URL" so stored `user:pass@` creds are not stripped on a
            // re-save. A genuinely new URL (≠ the sanitized echo) still applies.
            let v = if crate::shared::is_sanitized_url_echo(raw, &config.stratum.url) {
                config.stratum.url.clone()
            } else {
                raw.to_string()
            };
            let protocol = requested_primary_protocol.unwrap_or_else(|| config.stratum.protocol());
            let normalized = normalize_stratum_url_protocol(&v, protocol);
            info!("API: stratumURL changed (redacted) ({:?})", protocol);
            pool_changed |= config.stratum.url != normalized;
            config.stratum.url = normalized;
        } else if let Some(protocol) = requested_primary_protocol {
            let normalized = normalize_stratum_url_protocol(&config.stratum.url, protocol);
            pool_changed |= config.stratum.url != normalized;
            config.stratum.url = normalized;
        }
        if let Some(v) = updates.get("stratumPort").and_then(|v| v.as_u64()) {
            info!("API: stratumPort {} -> {}", config.stratum.port, v);
            pool_changed |= config.stratum.port != v as u16;
            config.stratum.port = v as u16;
        }
        if let Some(v) = updates.get("stratumUser").and_then(|v| v.as_str()) {
            // B-ESP-10 round-trip guard: never clobber the stored full BTC payout
            // address with its own read-mask echo from the dashboard form. The
            // worker is never logged in cleartext (mirrors the password log).
            if !crate::shared::is_masked_worker_echo(v, &config.stratum.worker_name) {
                info!("API: stratumUser changed (redacted)");
                pool_changed |= config.stratum.worker_name != v;
                config.stratum.worker_name = v.to_string();
            }
        }
        if let Some(v) = updates.get("stratumPassword").and_then(|v| v.as_str()) {
            info!("API: stratumPassword changed (redacted)");
            pool_changed |= config.stratum.password != v;
            config.stratum.password = v.to_string();
        }

        // ---- SV2 own-template proxy hint ----
        if let Some(v) = updates
            .get("sv2OwnTemplatesEnabled")
            .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
        {
            config.sv2_own_templates.enabled = v;
        }
        if let Some(v) = updates.get("sv2TemplateProxyURL").and_then(|v| v.as_str()) {
            let trimmed = v.trim().to_string();
            config.sv2_own_templates.mining_proxy_url = trimmed.clone();
            if config.sv2_own_templates.enabled && !trimmed.is_empty() {
                let normalized =
                    normalize_stratum_url_protocol(&trimmed, dcentaxe_stratum::StratumProtocol::V2);
                pool_changed |= config.stratum.url != normalized;
                config.stratum.url = normalized;
                let port = stratum_port_from_url(&trimmed).unwrap_or_else(|| {
                    if updates.get("stratumPort").is_some() {
                        config.stratum.port
                    } else {
                        3336
                    }
                });
                pool_changed |= config.stratum.port != port;
                config.stratum.port = port;
            }
        }
        if let Some(v) = updates
            .get("sv2TemplateProviderURL")
            .and_then(|v| v.as_str())
        {
            config.sv2_own_templates.template_provider_url = v.trim().to_string();
        }
        if let Some(v) = updates.get("sv2JobDeclaratorURL").and_then(|v| v.as_str()) {
            config.sv2_own_templates.job_declarator_url = v.trim().to_string();
        }

        // ---- Stratum fallback ----
        let fallback_requested = updates.get("fallbackStratumURL").is_some()
            || updates.get("fallbackStratumPort").is_some()
            || updates.get("fallbackStratumUser").is_some()
            || updates.get("fallbackStratumProtocol").is_some()
            || updates.get("fallbackStratumPassword").is_some();
        if fallback_requested {
            let version_rolling = config.stratum.version_rolling;
            let fallback =
                config
                    .fallback_pool
                    .get_or_insert_with(|| dcentaxe_stratum::StratumConfig {
                        url: String::new(),
                        port: 3333,
                        worker_name: String::new(),
                        password: "x".into(),
                        suggest_difficulty: 0,
                        version_rolling,
                    });
            if let Some(raw) = updates.get("fallbackStratumURL").and_then(|v| v.as_str()) {
                // B-ESP-10 round-trip guard (see stratumURL).
                let v = if crate::shared::is_sanitized_url_echo(raw, &fallback.url) {
                    fallback.url.clone()
                } else {
                    raw.to_string()
                };
                let protocol = updates
                    .get("fallbackStratumProtocol")
                    .and_then(requested_stratum_protocol)
                    .unwrap_or_else(|| fallback.protocol());
                let normalized = normalize_stratum_url_protocol(&v, protocol);
                pool_changed |= fallback.url != normalized;
                fallback.url = normalized;
            } else if let Some(protocol) = updates
                .get("fallbackStratumProtocol")
                .and_then(requested_stratum_protocol)
            {
                let normalized = normalize_stratum_url_protocol(&fallback.url, protocol);
                pool_changed |= fallback.url != normalized;
                fallback.url = normalized;
            }
            if let Some(v) = updates.get("fallbackStratumPort").and_then(|v| v.as_u64()) {
                pool_changed |= fallback.port != v as u16;
                fallback.port = v as u16;
            }
            if let Some(v) = updates.get("fallbackStratumUser").and_then(|v| v.as_str()) {
                // B-ESP-10 round-trip guard (see stratumUser).
                if !crate::shared::is_masked_worker_echo(v, &fallback.worker_name) {
                    pool_changed |= fallback.worker_name != v;
                    fallback.worker_name = v.to_string();
                }
            }
            if let Some(v) = updates
                .get("fallbackStratumPassword")
                .and_then(|v| v.as_str())
            {
                pool_changed |= fallback.password != v;
                fallback.password = v.to_string();
            }
            if fallback.url.trim().is_empty() {
                pool_changed = true;
                config.fallback_pool = None;
            }
        }

        // ---- Hashrate split secondary pool ----
        let split_requested = updates.get("splitPoolEnabled").is_some()
            || updates.get("splitPoolURL").is_some()
            || updates.get("splitPoolPort").is_some()
            || updates.get("splitPoolUser").is_some()
            || updates.get("splitPoolProtocol").is_some()
            || updates.get("splitPoolPassword").is_some()
            || updates.get("splitPoolPct").is_some();
        if split_requested {
            let split_enabled = updates
                .get("splitPoolEnabled")
                .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
                .unwrap_or(config.split_pool.is_some());

            if !split_enabled {
                pool_changed |= config.split_pool.is_some();
                config.split_pool = None;
            } else {
                let version_rolling = config.stratum.version_rolling;
                let mut split =
                    config
                        .split_pool
                        .clone()
                        .unwrap_or_else(|| crate::config::SplitPoolConfig {
                            pool: dcentaxe_stratum::StratumConfig {
                                url: String::new(),
                                port: 3333,
                                worker_name: String::new(),
                                password: "x".into(),
                                suggest_difficulty: 0,
                                version_rolling,
                            },
                            hashrate_pct: 20,
                        });

                if let Some(raw) = updates.get("splitPoolURL").and_then(|v| v.as_str()) {
                    // B-ESP-10 round-trip guard (see stratumURL).
                    let v = if crate::shared::is_sanitized_url_echo(raw, &split.pool.url) {
                        split.pool.url.clone()
                    } else {
                        raw.to_string()
                    };
                    let protocol = updates
                        .get("splitPoolProtocol")
                        .and_then(requested_stratum_protocol)
                        .unwrap_or_else(|| split.pool.protocol());
                    let normalized = normalize_stratum_url_protocol(&v, protocol);
                    pool_changed |= split.pool.url != normalized;
                    split.pool.url = normalized;
                } else if let Some(protocol) = updates
                    .get("splitPoolProtocol")
                    .and_then(requested_stratum_protocol)
                {
                    let normalized = normalize_stratum_url_protocol(&split.pool.url, protocol);
                    pool_changed |= split.pool.url != normalized;
                    split.pool.url = normalized;
                }
                if let Some(v) = updates.get("splitPoolPort").and_then(|v| v.as_u64()) {
                    let port = v.clamp(1, u16::MAX as u64) as u16;
                    pool_changed |= split.pool.port != port;
                    split.pool.port = port;
                }
                if let Some(v) = updates.get("splitPoolUser").and_then(|v| v.as_str()) {
                    // B-ESP-10 round-trip guard (see stratumUser).
                    if !crate::shared::is_masked_worker_echo(v, &split.pool.worker_name) {
                        pool_changed |= split.pool.worker_name != v;
                        split.pool.worker_name = v.to_string();
                    }
                }
                if let Some(v) = updates.get("splitPoolPassword").and_then(|v| v.as_str()) {
                    pool_changed |= split.pool.password != v;
                    split.pool.password = v.to_string();
                }
                if let Some(v) = updates.get("splitPoolPct").and_then(|v| v.as_u64()) {
                    let pct = v.clamp(1, 99) as u8;
                    pool_changed |= split.hashrate_pct != pct;
                    split.hashrate_pct = pct;
                }

                if split.pool.url.trim().is_empty() {
                    pool_changed |= config.split_pool.is_some();
                    config.split_pool = None;
                } else {
                    pool_changed |= config
                        .split_pool
                        .as_ref()
                        .map(|existing| {
                            existing.hashrate_pct != split.hashrate_pct
                                || existing.pool.url != split.pool.url
                                || existing.pool.port != split.pool.port
                                || existing.pool.worker_name != split.pool.worker_name
                                || existing.pool.password != split.pool.password
                        })
                        .unwrap_or(true);
                    config.split_pool = Some(split);
                }
            }
        }

        let requested_frequency = updates
            .get("frequency")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let requested_voltage = updates
            .get("coreVoltage")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16);

        // ---- Fan settings (accept both ESP-Miner and legacy field names) ----
        if let Some(v) = updates
            .get("fanspeed")
            .or_else(|| updates.get("fanSpeed"))
            .or_else(|| updates.get("manualFanSpeed"))
            .and_then(|v| v.as_u64())
        {
            let new_speed = (v as u8).clamp(20, 100);
            info!("API: fanSpeed {} -> {}%", config.fan_speed_pct, new_speed);
            config.fan_speed_pct = new_speed;
            config.fan_target_temp_c = 0;
        }
        if let Some(mode) = updates.get("fanMode").and_then(|v| v.as_str()) {
            match mode {
                "manual" => config.fan_target_temp_c = 0,
                "auto" if config.fan_target_temp_c == 0 => config.fan_target_temp_c = 65,
                _ => {}
            }
        }
        if let Some(v) = updates.get("autofanspeed").and_then(|v| v.as_u64()) {
            if v == 0 {
                config.fan_target_temp_c = 0; // manual mode
            } else if config.fan_target_temp_c == 0 {
                config.fan_target_temp_c = 65;
            }
        }
        if let Some(v) = updates
            .get("temptarget")
            .or_else(|| updates.get("fanTargetTemp"))
            .and_then(|v| v.as_u64())
        {
            // MAINAPI-4: target_temp == 0 is ambiguous (manual vs unset/auto). An
            // explicit 0 here would silently disable the thermal fan curve. Force
            // the caller to use the unambiguous manual selectors instead of
            // accidentally disabling the curve. The explicit fanMode/autofanspeed
            // manual writes above (which intentionally set fan_target_temp_c=0)
            // are untouched.
            if v == 0 {
                return Err(
                    "Set fanMode=manual to disable the thermal curve; target_temp=0 is ambiguous"
                        .to_string(),
                );
            }
            config.fan_target_temp_c = (v as u8).clamp(0, 90);
        }

        // ---- Display settings ----
        if let Some(v) = updates
            .get("invertscreen")
            .or_else(|| updates.get("flipscreen"))
            .and_then(|v| v.as_u64())
        {
            let new_val = v != 0;
            info!(
                "API: display_inverted {} -> {}",
                config.display_inverted, new_val
            );
            config.display_inverted = new_val;
        }

        // ---- Overclock ----
        let overclock_changed = if let Some(v) = updates.get("overclockEnabled") {
            let enabled = if v.is_boolean() {
                v.as_bool().unwrap_or(false)
            } else {
                v.as_u64().unwrap_or(0) != 0
            };
            config.overclock_enabled = enabled;
            let limits = config.power_limits();
            info!(
                "API: overclock {} — max {:.0}W {:.0}MHz",
                if enabled { "ENABLED" } else { "disabled" },
                limits.max_power_w,
                limits.max_frequency
            );
            true
        } else {
            false
        };

        if requested_frequency.is_some() || requested_voltage.is_some() {
            if let Ok(mut autotune) = state.autotuner.lock() {
                if autotune.enabled {
                    info!("API: disabling autotuner due to manual operating-point override");
                    autotune.enabled = false;
                    autotune.status = "manual override".to_string();
                }
            }
        }

        if requested_frequency.is_some() || requested_voltage.is_some() || overclock_changed {
            let qualified = config.qualify_operating_point(
                requested_frequency.unwrap_or(config.target_frequency),
                requested_voltage.unwrap_or(config.target_voltage_mv),
                crate::config::ControlSurface::LegacyRest,
            );
            if requested_frequency.is_some() {
                info!(
                    "API: frequency {:.0} -> {:.0} MHz",
                    config.target_frequency, qualified.frequency_mhz
                );
            }
            if requested_voltage.is_some() {
                info!(
                    "API: coreVoltage {} -> {} mV",
                    config.target_voltage_mv, qualified.voltage_mv
                );
            }
            config.target_frequency = qualified.frequency_mhz;
            config.target_voltage_mv = qualified.voltage_mv;
        }

        // ---- Hostname ----
        if let Some(v) = updates.get("hostname").and_then(|v| v.as_str()) {
            info!("API: hostname '{}' -> '{}'", config.hostname, v);
            config.hostname = v.to_string();
        }

        config.canonicalize_identity();

        // ---- WiFi credentials ----
        // Changing WiFi requires a reboot to take effect (can't switch STA while running).
        // The reboot is triggered automatically if SSID changes.
        let mut wifi_changed = false;
        if let Some(v) = updates
            .get("ssid")
            .or_else(|| updates.get("wifiSSID"))
            .and_then(|v| v.as_str())
        {
            if !v.is_empty() && v != config.wifi_ssid {
                info!("API: WiFi SSID '{}' -> '{}'", config.wifi_ssid, v);
                config.wifi_ssid = v.to_string();
                wifi_changed = true;
            }
        }
        if let Some(v) = updates
            .get("wifiPass")
            .or_else(|| updates.get("wifiPassword"))
            .or_else(|| updates.get("wifi_password"))
            .and_then(|v| v.as_str())
        {
            info!("API: WiFi password changed (redacted)");
            config.wifi_password = v.to_string();
            wifi_changed = true;
        }

        // ---- Voluntary donation routing ----
        // Stage then validate so a rejected PATCH never mutates live config.
        let donation_keys = [
            "donationEnabled",
            "donationPercent",
            "donationPoolURL",
            "donationWorker",
            "donationPassword",
            "donationFallbackEnabled",
            "donationFallbackPoolURL",
            "donationFallbackWorker",
            "donationFallbackPassword",
            "donationCycleDuration",
        ];
        if donation_keys.iter().any(|key| updates.get(*key).is_some()) {
            let mut donation = config.donation.clone();
            if let Some(value) = updates
                .get("donationEnabled")
                .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
            {
                donation.enabled = value;
            }
            if let Some(value) = updates
                .get("donationPercent")
                .and_then(|value| value.as_f64())
            {
                donation.percent = value as f32;
            }
            if let Some(value) = updates
                .get("donationPoolURL")
                .and_then(|value| value.as_str())
            {
                donation.pool_url = value.trim().to_string();
            }
            if let Some(value) = updates
                .get("donationWorker")
                .and_then(|value| value.as_str())
            {
                donation.worker = value.trim().to_string();
            }
            if let Some(value) = updates
                .get("donationPassword")
                .and_then(|value| value.as_str())
            {
                if !value.is_empty() && value != "***" {
                    info!("API: donation password changed (redacted)");
                    donation.password = value.to_string();
                }
            }
            if let Some(value) = updates
                .get("donationFallbackEnabled")
                .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
            {
                donation.fallback_enabled = value;
            }
            if let Some(value) = updates
                .get("donationFallbackPoolURL")
                .and_then(|value| value.as_str())
            {
                donation.fallback_pool_url = value.trim().to_string();
            }
            if let Some(value) = updates
                .get("donationFallbackWorker")
                .and_then(|value| value.as_str())
            {
                donation.fallback_worker = value.trim().to_string();
            }
            if let Some(value) = updates
                .get("donationFallbackPassword")
                .and_then(|value| value.as_str())
            {
                if !value.is_empty() && value != "***" {
                    info!("API: donation fallback password changed (redacted)");
                    donation.fallback_password = value.to_string();
                }
            }
            if let Some(value) = updates
                .get("donationCycleDuration")
                .and_then(|value| value.as_u64())
            {
                donation.cycle_duration_s = value;
            }
            donation.validate()?;
            config.donation = donation;
            pool_changed = true;
        }

        // ---- Outbound notifications ----
        if let Some(value) = updates
            .get("notificationsEnabled")
            .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
        {
            config.notifications.enabled = value;
        }
        if let Some(value) = updates
            .get("telegramBotToken")
            .and_then(|value| value.as_str())
        {
            if !value.is_empty() && value != "***" {
                info!("API: Telegram bot token changed (redacted)");
                config.notifications.telegram_bot_token = value.to_string();
            }
        }
        if let Some(value) = updates
            .get("telegramChatId")
            .and_then(|value| value.as_str())
        {
            config.notifications.telegram_chat_id = value.trim().to_string();
        }
        if let Some(value) = updates
            .get("discordWebhookURL")
            .and_then(|value| value.as_str())
        {
            if !value.is_empty() && value != "***" {
                info!("API: Discord webhook changed (redacted)");
                config.notifications.discord_webhook_url = value.to_string();
            }
        }
        if let Some(value) = updates
            .get("slackWebhookURL")
            .and_then(|value| value.as_str())
        {
            if !value.is_empty() && value != "***" {
                info!("API: Slack webhook changed (redacted)");
                config.notifications.slack_webhook_url = value.to_string();
            }
        }
        if let Some(value) = updates
            .get("notificationShareMilestone")
            .and_then(|value| value.as_u64())
        {
            config.notifications.share_milestone = value;
        }
        if let Some(value) = updates
            .get("notificationThermalAlerts")
            .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
        {
            config.notifications.thermal_alerts = value;
        }
        if let Some(value) = updates
            .get("notificationFailoverAlerts")
            .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
        {
            config.notifications.failover_alerts = value;
        }
        if let Some(value) = updates
            .get("notificationOtaAlerts")
            .and_then(|value| value.as_bool().or_else(|| value.as_u64().map(|n| n != 0)))
        {
            config.notifications.ota_alerts = value;
        }

        // ---- MQTT + Home Assistant auto-discovery ----
        // Persisted through the SAME authenticated config path as everything else.
        // The publisher thread is spawned once at boot and re-reads broker/creds on
        // each reconnect, so a broker move applies without a reboot; toggling
        // `enabled` on/off takes effect after a restart (dashboard says so).
        if let Some(v) = updates
            .get("mqttEnabled")
            .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
        {
            config.mqtt.enabled = v;
        }
        if let Some(v) = updates.get("mqttBrokerHost").and_then(|v| v.as_str()) {
            config.mqtt.broker_host = v.trim().to_string();
        }
        if let Some(v) = updates.get("mqttBrokerPort").and_then(|v| v.as_u64()) {
            if v > 0 && v <= u16::MAX as u64 {
                config.mqtt.broker_port = v as u16;
            }
        }
        if let Some(v) = updates.get("mqttUsername").and_then(|v| v.as_str()) {
            config.mqtt.username = v.to_string();
        }
        if let Some(v) = updates.get("mqttPassword").and_then(|v| v.as_str()) {
            // Mirror the wifi/stratum redaction posture: never log the value.
            info!("API: MQTT password changed (redacted)");
            config.mqtt.password = v.to_string();
        }
        if let Some(v) = updates
            .get("mqttTls")
            .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
        {
            config.mqtt.tls = v;
        }
        if let Some(v) = updates.get("mqttPublishInterval").and_then(|v| v.as_u64()) {
            if v > 0 && v <= u16::MAX as u64 {
                config.mqtt.publish_interval_s = v as u16;
            }
        }

        // Persist config to NVS so changes survive reboot
        if let Ok(mut nvs_guard) = state.nvs.lock() {
            if let Some(ref mut nvs) = *nvs_guard {
                if let Err(e) = nvs_config::save_config(nvs, &config) {
                    error!("API: NVS save failed: {}", e);
                } else {
                    info!("API: config saved to NVS");
                }
            } else {
                error!("API: NVS handle not available for config save");
            }
        } else {
            error!("API: failed to lock NVS for config save");
        }

        // Auto-reboot if WiFi credentials changed (must reconnect to new network)
        if wifi_changed {
            info!("API: WiFi credentials changed — rebooting in 2s to apply");
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                unsafe {
                    esp_idf_svc::sys::esp_restart();
                }
            });
        } else if pool_changed {
            info!("API: pool configuration changed — rebooting in 2s to apply new Stratum clients");
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                unsafe {
                    esp_idf_svc::sys::esp_restart();
                }
            });
        }
    }
    // MAINAPI-4: success (an unparseable body is tolerated as a no-op Ok, matching
    // the prior behaviour — only an explicit ambiguous target_temp=0 returns Err).
    Ok(())
}

/// GET /api/mining — detailed mining statistics
fn register_mining_status(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/mining",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
                let snap = stats.snapshot();
                let autotune = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());

                let info = serde_json::json!({
                    "hashrate": {
                        "1m": snap.hashrate_1m_ghs,
                        "5m": snap.hashrate_5m_ghs,
                        "15m": snap.hashrate_15m_ghs,
                        "unit": "GH/s"
                    },
                    "shares": {
                        "accepted": snap.accepted_shares,
                        "rejected": snap.rejected_shares,
                        "total": snap.accepted_shares + snap.rejected_shares,
                    },
                    "noncesFound": snap.nonces_found,
                    "filteredNonces": snap.filtered_shares,
                    "staleNonces": snap.stale_nonces,
                    "slotRecoveries": snap.slot_recoveries,
                    "ticketDifficulty": snap.ticket_difficulty,
                    "bestDifficulty": snap.best_difficulty,
                    "bestDiffString": format_difficulty(snap.best_difficulty),
                    "uptimeSeconds": snap.uptime_secs,
                    "autotuner": *autotune,
                });

                let body = serde_json::to_string(&info).unwrap_or_default();
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "application/json"),
                        ("Access-Control-Allow-Origin", "*"),
                    ],
                )?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/mining");
}

/// GET /api/mining/block — current Stratum block context (read-only).
///
/// Returns the block the miner is currently working on. When no block has been
/// received yet (cold boot), returns `{}` so the dashboard can cleanly check
/// for presence. Carries the full context from the latest mining.notify:
/// block height, previous hash, job id, ntime, clean_jobs flag, and the
/// wall-clock time we received the notify (for "X sec ago" rendering).
fn register_mining_block(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/mining/block",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let snap = {
                    let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
                    stats.snapshot()
                };

                let body = if let Some(b) = snap.current_block {
                    serde_json::json!({
                        "blockHeight":                b.height,
                        "prevHash":                   b.prev_hash,
                        "jobId":                      b.job_id,
                        "ntime":                      b.ntime,
                        "ntimeUnix":                  b.ntime_unix,
                        "cleanJobs":                  b.clean_jobs,
                        "receivedUnixMs":             b.received_unix_ms,
                        "coinbaseOutputs":            b.coinbase_outputs.as_deref().unwrap_or(&[]),
                        "coinbaseValueTotalSatoshis": b.coinbase_total_sats.unwrap_or(0),
                        "coinbaseValueUserSatoshis": b.coinbase_user_sats.unwrap_or(0),
                        "nbits":                      b.nbits,
                        "merkleBranchCount":          b.merkle_branch_count,
                    })
                    .to_string()
                } else {
                    "{}".to_string()
                };

                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/mining/block");
}

/// POST /api/mining/* — runtime mining controls
fn register_mining_control(server: &mut EspHttpServer, state: SharedState) {
    // POST /api/mining/autotune — start/stop/configure autotuner
    server
        .fn_handler(
            "/api/mining/autotune",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 512) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };

                if let Ok(cmd) = serde_json::from_slice::<serde_json::Value>(&body) {
                    // A5 / XPAUTO-5: validate the requested `target` against the
                    // EFFECTIVE mode (the mode this request ends in) BEFORE
                    // applying anything, so an out-of-band target (TargetTemp=0/
                    // 500, TargetWatts<=0/over-budget, NaN/inf) is rejected with a
                    // 400 and NOTHING is stored (atomic). The single host-tested
                    // `validate_autotune_target` owns the accept/reject rule; the
                    // espidf-only handler just maps AutotuneMode -> BestPointMode
                    // (same pattern as the `best_point_for_mode` delegation).
                    let validation: Result<(), &'static str> = {
                        let mut autotune =
                            state.autotuner.lock().unwrap_or_else(|e| e.into_inner());

                        let new_enabled = cmd.get("enabled").and_then(|v| v.as_bool());
                        let new_mode = cmd
                            .get("mode")
                            .and_then(|v| v.as_str())
                            .and_then(crate::shared::AutotuneMode::from_api_str);
                        let effective_mode = new_mode.unwrap_or(autotune.mode);

                        let target_result = cmd.get("target").and_then(|v| v.as_f64()).map(|t| {
                            let pure_mode = match effective_mode {
                                crate::shared::AutotuneMode::MaxHashrate => {
                                    crate::chip_profiles_bitaxe::BestPointMode::MaxHashrate
                                }
                                crate::shared::AutotuneMode::TargetWatts => {
                                    crate::chip_profiles_bitaxe::BestPointMode::TargetWatts
                                }
                                crate::shared::AutotuneMode::BestEfficiency => {
                                    crate::chip_profiles_bitaxe::BestPointMode::BestEfficiency
                                }
                                crate::shared::AutotuneMode::TargetTemp => {
                                    crate::chip_profiles_bitaxe::BestPointMode::TargetTemp
                                }
                            };
                            crate::chip_profiles_bitaxe::validate_autotune_target(
                                pure_mode, t as f32,
                            )
                        });

                        match target_result {
                            Some(Err(msg)) => Err(msg),
                            _ => {
                                if let Some(enabled) = new_enabled {
                                    autotune.enabled = enabled;
                                }
                                if let Some(mode) = new_mode {
                                    autotune.mode = mode;
                                }
                                if let Some(Ok(v)) = target_result {
                                    autotune.target_value = v;
                                }
                                info!(
                                    "API: autotuner updated: enabled={}, mode={:?}",
                                    autotune.enabled, autotune.mode
                                );
                                Ok(())
                            }
                        }
                    };

                    if let Err(msg) = validation {
                        let mut resp =
                            req.into_response(400, Some("Bad Request"), &COMPAT_HEADERS)?;
                        let _ = resp.write(
                            json!({ "success": false, "message": msg })
                                .to_string()
                                .as_bytes(),
                        );
                        return Ok(());
                    }
                }

                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "application/json"),
                        ("Access-Control-Allow-Origin", "*"),
                    ],
                )?;
                let _ = resp.write(b"{\"success\":true}");
                Ok(())
            },
        )
        .expect("Failed to register POST /api/mining/autotune");
}

/// POST /api/system/setup — wipe owner state and reboot into secure setup mode.
///
/// This clears the owner password/sessions plus network and pool secrets, then
/// reboots. On the next boot, the device enters setup mode with the
/// "DCENTaxe_XXXX" WiFi hotspot + captive portal at 192.168.71.1.
fn register_setup_mode(server: &mut EspHttpServer, state: SharedState) {
    server.fn_handler("/api/system/setup", Method::Post, move |req| -> Result<(), Box<dyn std::error::Error>> {
        if let Err(err) = auth::authorize_rest_write(&req, &state) {
            return auth::write_auth_failure(req, err);
        }
        info!("API: entering setup mode — clearing owner state, network, and pool secrets");

        {
            let mut config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            config.wifi_ssid.clear();
            config.wifi_password.clear();
            config.hostname.clear();
            config.stratum = dcentaxe_stratum::StratumConfig::default();
            config.fallback_pool = None;
            config.split_pool = None;
            config.schedule_enabled = true;
            config.schedule_timezone_offset_minutes = 0;
            config.power_schedule.clear();
            let board = config.board_config();
            config.target_frequency = board.default_frequency;
            config.target_voltage_mv = board.default_voltage_mv;
            config.overclock_enabled = false;
            config.fan_target_temp_c = 65;
            config.fan_speed_pct = 100;

            if let Ok(mut nvs_guard) = state.nvs.lock() {
                if let Some(ref mut nvs) = *nvs_guard {
                    let auth_reset = auth::clear_owner_auth(nvs);
                    let force_setup = nvs.set_u8("force_setup", 1u8);
                    let retry_reset = nvs.set_u8("wifi_retries", 0u8);
                    if let Err(e) = nvs_config::save_config(nvs, &config) {
                        error!("API: failed to clear NVS config: {}", e);
                    } else if let Err(e) = auth_reset {
                        error!("API: failed to clear owner auth: {}", e);
                    } else if let Err(e) = force_setup {
                        error!("API: failed to set force_setup flag: {:?}", e);
                    } else if let Err(e) = retry_reset {
                        error!("API: failed to reset wifi_retries flag: {:?}", e);
                    } else {
                        info!("API: setup mode state persisted and owner auth cleared");
                    }
                }
            }
        }

        let mut resp = req.into_response(200, None, &[
            ("Content-Type", "application/json"),
            ("Access-Control-Allow-Origin", "*"),
        ])?;
        let _ = resp.write(b"{\"message\":\"Entering secure setup mode. Owner access, WiFi, and pool secrets were cleared. Connect to the DCENTaxe WiFi hotspot to claim and reconfigure the miner.\"}");

        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            unsafe { esp_idf_svc::sys::esp_restart(); }
        });
        Ok(())
    }).expect("Failed to register POST /api/system/setup");
}

/// POST /api/auth/owner-reset - authenticated owner auth reset.
///
/// Clears the owner password and all active sessions. Keeps WiFi, pool config,
/// achievements, and all other device state intact. This route requires a
/// current owner bearer session and never sets the owner-claim skip flag.
fn register_owner_reset(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/auth/owner-reset",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if !auth::password_is_set(&state) {
                    let mut resp = req.into_response(428, None, &JSON_HEADERS)?;
                    let _ = resp.write(
                        b"{\"error\":\"Precondition Required\",\"detail\":\"Owner password is not configured\"}",
                    );
                    return Ok(());
                }
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                info!("API: owner-reset — clearing owner password and sessions");
                let cleared = {
                    if let Ok(mut guard) = state.nvs.lock() {
                        if let Some(nvs) = guard.as_mut() {
                            let r = auth::clear_owner_auth(nvs);
                            match r {
                                Ok(()) => true,
                                Err(e) => {
                                    error!("API: owner-reset failed: {}", e);
                                    false
                                }
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if !cleared {
                    let mut resp = req.into_response(500, None, &JSON_HEADERS)?;
                    let _ = resp
                        .write(b"{\"error\":\"ServerError\",\"detail\":\"Failed to clear owner auth\"}");
                    return Ok(());
                }
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(
                    b"{\"message\":\"Owner access cleared. Rebooting. WiFi and pool config retained.\"}",
                );
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    unsafe {
                        esp_idf_svc::sys::esp_restart();
                    }
                });
                Ok(())
            },
        )
        .expect("Failed to register POST /api/auth/owner-reset");
}

/// POST /api/system/restart — reboot the device
fn register_system_restart(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/restart",
            Method::Options,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"");
                Ok(())
            },
        )
        .expect("Failed to register OPTIONS /api/system/restart");

    server
        .fn_handler(
            "/api/system/restart",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                info!("API: reboot requested");
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"{\"message\":\"System will restart shortly.\"}");
                // Delay to let response flush, then reboot
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    unsafe {
                        esp_idf_svc::sys::esp_restart();
                    }
                });
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/restart");
}

/// POST /api/system/identify — stock AxeOS-compatible locate action.
fn register_system_identify(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/identify",
            Method::Options,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"");
                Ok(())
            },
        )
        .expect("Failed to register OPTIONS /api/system/identify");

    server
        .fn_handler(
            "/api/system/identify",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                let now = unix_time_s();
                let (active, message) = {
                    let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
                    let was_active = swarm.identify_until_epoch_s > now;
                    if was_active {
                        swarm.identify_until_epoch_s = 0;
                        (false, "The device no longer says \"Hi!\".")
                    } else {
                        swarm.identify_until_epoch_s = now + 30;
                        (true, "The device says \"Hi!\" for 30 seconds.")
                    }
                };
                info!("API: identify requested (active={})", active);

                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(
                    serde_json::json!({
                        "message": message,
                        "active": active,
                    })
                    .to_string()
                    .as_bytes(),
                );
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/identify");
}

/// GET /api/system/coredump — presence + size of any stored panic coredump.
/// GET /api/system/coredump?download=1 — raw ELF body (auth-gated).
/// DELETE /api/system/coredump — erase the coredump partition after retrieval.
///
/// ESP-IDF populates the coredump partition on `ESP_RST_PANIC` when
/// `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y` is set in sdkconfig.defaults.
fn register_system_coredump(server: &mut EspHttpServer, state: SharedState) {
    let state_get = state.clone();
    server
        .fn_handler(
            "/api/system/coredump",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_get) {
                    return auth::write_auth_failure(req, err);
                }
                let download = req.uri().contains("download=1");

                // Probe the stored coredump image.
                let mut out_addr: usize = 0;
                let mut out_size: usize = 0;
                let probe = unsafe {
                    esp_idf_svc::sys::esp_core_dump_image_get(&mut out_addr, &mut out_size)
                };
                if probe != 0 || out_size == 0 {
                    let mut resp = req.into_response(404, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"present\":false}");
                    return Ok(());
                }

                if !download {
                    let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(
                        serde_json::json!({
                            "present": true,
                            "size": out_size,
                            "offset": out_addr,
                            "format": "elf",
                        })
                        .to_string()
                        .as_bytes(),
                    );
                    return Ok(());
                }

                // Stream the image from the coredump partition as octet-stream.
                let part = unsafe {
                    esp_idf_svc::sys::esp_partition_find_first(
                        esp_idf_svc::sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                        esp_idf_svc::sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_DATA_COREDUMP,
                        std::ptr::null(),
                    )
                };
                if part.is_null() {
                    let mut resp = req.into_response(500, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"error\":\"coredump partition not found\"}");
                    return Ok(());
                }
                let mut buf = vec![0u8; out_size];
                let rc = unsafe {
                    esp_idf_svc::sys::esp_partition_read(
                        part,
                        0,
                        buf.as_mut_ptr() as *mut _,
                        out_size,
                    )
                };
                if rc != 0 {
                    let mut resp = req.into_response(500, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"error\":\"partition read failed\"}");
                    return Ok(());
                }
                let mut headers = COMPAT_HEADERS.to_vec();
                headers.push(("Content-Type", "application/octet-stream"));
                headers.push((
                    "Content-Disposition",
                    "attachment; filename=\"dcentaxe-coredump.elf\"",
                ));
                let mut resp = req.into_response(200, None, &headers)?;
                let _ = resp.write(&buf);
                Ok(())
            },
        )
        .expect("Failed to register GET /api/system/coredump");

    let state_del = state.clone();
    server
        .fn_handler(
            "/api/system/coredump",
            Method::Delete,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_del) {
                    return auth::write_auth_failure(req, err);
                }
                let rc = unsafe { esp_idf_svc::sys::esp_core_dump_image_erase() };
                if rc == 0 {
                    info!("API: coredump erased");
                    let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"success\":true}");
                } else {
                    let mut resp = req.into_response(500, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"error\":\"erase failed\"}");
                }
                Ok(())
            },
        )
        .expect("Failed to register DELETE /api/system/coredump");
}

/// POST /api/system/self-test/run — kick off the factory self-test.
/// GET  /api/system/self-test/status — poll the most recent report.
///
/// The run happens in a background thread so the POST returns immediately.
/// The `mining_liveness` step waits up to 60 s for the first accepted share.
fn register_system_self_test(server: &mut EspHttpServer, state: SharedState) {
    let state_run = state.clone();
    server
        .fn_handler(
            "/api/system/self-test/run",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_run) {
                    return auth::write_auth_failure(req, err);
                }
                if state_run.self_test.is_running() {
                    let mut resp = req.into_response(409, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"error\":\"self-test already in progress\"}");
                    return Ok(());
                }
                let spawn_state = state_run.clone();
                std::thread::Builder::new()
                    .name("self_test".into())
                    .stack_size(8 * 1024)
                    .spawn(move || {
                        spawn_state.self_test.run_all(&spawn_state, 60);
                        // ESP-Miner PR #1602: Touch setups have no reachable
                        // RESET button once the case is closed, so we
                        // auto-reboot 10 s after a passing run to hand control
                        // back to the accessory. Non-BAP variants wait for the
                        // operator to trigger the reboot themselves.
                        #[cfg(feature = "bap")]
                        {
                            let snap = spawn_state.self_test.snapshot();
                            if snap.completed && snap.passed {
                                log::info!("Self-test PASS on BAP variant — auto-reboot in 10 s");
                                std::thread::sleep(std::time::Duration::from_secs(10));
                                unsafe {
                                    esp_idf_svc::sys::esp_restart();
                                }
                            }
                        }
                    })
                    .ok();
                let mut resp = req.into_response(202, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"{\"started\":true}");
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/self-test/run");

    let state_status = state.clone();
    server
        .fn_handler(
            "/api/system/self-test/status",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let snap = state_status.self_test.snapshot();
                let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/system/self-test/status");

    // POST /api/system/self-test/cancel — request a clean abort between steps.
    let state_cancel = state;
    server
        .fn_handler(
            "/api/system/self-test/cancel",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_cancel) {
                    return auth::write_auth_failure(req, err);
                }
                if !state_cancel.self_test.is_running() {
                    let mut resp = req.into_response(409, None, &COMPAT_HEADERS)?;
                    let _ = resp.write(b"{\"error\":\"no self-test running\"}");
                    return Ok(());
                }
                state_cancel.self_test.request_cancel();
                let mut resp = req.into_response(202, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"{\"cancelled\":true}");
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/self-test/cancel");
}

/// POST /api/system/clear-safe-mode — zero the task-WDT counter + reboot.
/// Used after the user addresses whatever wedge caused repeated task-WDT resets.
fn register_system_clear_safe_mode(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/clear-safe-mode",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                // Reset persisted WDT counters so the next boot runs normally.
                if let Ok(mut guard) = state.nvs.lock() {
                    if let Some(nvs) = guard.as_mut() {
                        crate::nvs_config::save_wdt_counters(nvs, 0, 0);
                    }
                }
                info!("API: safe mode cleared — rebooting");
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"{\"message\":\"Safe mode cleared. Rebooting.\"}");
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    unsafe {
                        esp_idf_svc::sys::esp_restart();
                    }
                });
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/clear-safe-mode");
}

fn register_block_found_dismiss(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/system/blockFound/dismiss",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"{\"success\":true}");
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/blockFound/dismiss");
}

/// OTA firmware update endpoints.
///
/// GET  /api/system/OTA — current firmware version and partition info
/// POST /api/system/OTA — receive firmware binary and flash to next OTA partition
///
/// The POST handler streams the binary in 1KB chunks directly to flash,
/// matching ESP-Miner's /api/system/OTA behavior for tool compatibility.
/// Binary is sent as a raw POST body (Content-Type: application/octet-stream),
/// NOT multipart — this matches AxeOS conventions.
fn register_ota(server: &mut EspHttpServer, state: SharedState) {
    // ── GET /api/system/OTA — firmware version info ─────────────────────
    let register_ota_get = |path: &str, state: SharedState, server: &mut EspHttpServer| {
        server
            .fn_handler(
                path,
                Method::Get,
                move |req| -> Result<(), Box<dyn std::error::Error>> {
                    let version_info = get_ota_info(&state);
                    let body = serde_json::to_string(&version_info).unwrap_or_default();
                    let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                    let _ = resp.write(body.as_bytes());
                    Ok(())
                },
            )
            .expect("Failed to register OTA info endpoint");
    };

    register_ota_get("/api/system/OTA", state.clone(), server);
    register_ota_get("/api/system/OTAWWW", state.clone(), server);

    // ── POST /api/system/OTA — receive and flash firmware binary ────────
    let state_post = state.clone();
    server.fn_handler("/api/system/OTA", Method::Post, move |mut req| -> Result<(), Box<dyn std::error::Error>> {
        use esp_idf_svc::sys::*;
        use std::io::Read;

        if let Err(err) = auth::authorize_rest_write(&req, &state_post) {
            return auth::write_auth_failure(req, err);
        }

        info!("API: OTA update starting...");

        let (runtime_target, runtime_device_model, allow_unsigned_ota, rollback_floor) = {
            let config = state_post.config.lock().unwrap_or_else(|e| e.into_inner());
            let rollback_floor = state_post
                .nvs
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().and_then(|nvs| nvs_config::load_ota_floor(nvs)))
                .unwrap_or_else(|| DCENTAXE_VERSION.to_string());
            (
                config.board_target().to_string(),
                config.bitaxe_model().canonical_key().to_string(),
                config.allow_unsigned_ota,
                rollback_floor,
            )
        };
        // AOTA-1 fail-closed: `allow_unsigned_ota` only waives signature
        // verification for an authenticated owner running in developer mode.
        // A passwordless/unauthenticated caller can never disable verification,
        // even if the persisted flag was flipped on, so an unsigned image is
        // rejected (ota_signature_metadata forced Some below -> missing headers
        // -> 400). The existing authorize_rest_write call above preserves the
        // legitimate passwordless SIGNED-OTA fleet-push path.
        let owner_password_set = auth::password_is_set(&state_post);
        let owner_session = auth::request_has_owner_session(&req, &state_post);
        let ota_signature_required = crate::ota_signature::ota_signature_enforced(
            crate::ota_signature::signature_required(),
            allow_unsigned_ota,
            owner_password_set,
            owner_session,
        );
        let uploaded_target = req
            .header("X-DCENT-Board-Target")
            .unwrap_or("")
            .trim()
            .to_string();
        let uploaded_device_model = req
            .header("X-DCENT-Device-Model")
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if uploaded_target.is_empty() {
            let msg = serde_json::json!({
                "success": false,
                "message": format!(
                    "Missing X-DCENT-Board-Target header. Expected build target '{}'.",
                    BUILD_BOARD_TARGET
                ),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Missing board target"), &JSON_HEADERS)?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }
        if uploaded_device_model.is_empty() {
            let msg = serde_json::json!({
                "success": false,
                "message": format!(
                    "Missing X-DCENT-Device-Model header. Expected exact model '{}'.",
                    runtime_device_model
                ),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Missing device model"), &JSON_HEADERS)?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }
        if uploaded_target != BUILD_BOARD_TARGET {
            let msg = serde_json::json!({
                "success": false,
                "message": format!(
                    "Wrong firmware target '{}'. This device expects build '{}' (runtime '{}').",
                    uploaded_target, BUILD_BOARD_TARGET, runtime_target
                ),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Wrong board target"), &JSON_HEADERS)?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }
        if uploaded_device_model != runtime_device_model {
            let msg = serde_json::json!({
                "success": false,
                "message": format!(
                    "Wrong firmware model '{}'. This device expects '{}' within build target '{}' (runtime '{}').",
                    uploaded_device_model, runtime_device_model, BUILD_BOARD_TARGET, runtime_target
                ),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Wrong device model"), &JSON_HEADERS)?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }

        // ── Size validation ─────────────────────────────────────────────
        let content_len: usize = req.header("Content-Length")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        const MIN_FIRMWARE_SIZE: usize = 64 * 1024;      // 64 KB
        const MAX_FIRMWARE_SIZE: usize = 3 * 1024 * 1024; // 3 MB (OTA partition size)

        if content_len > 0 && content_len < MIN_FIRMWARE_SIZE {
            warn!("API: OTA rejected — firmware too small ({} bytes, min {})", content_len, MIN_FIRMWARE_SIZE);
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Firmware too small: {} bytes (minimum {} bytes)", content_len, MIN_FIRMWARE_SIZE),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Firmware too small"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }

        if content_len > MAX_FIRMWARE_SIZE {
            warn!("API: OTA rejected — firmware too large ({} bytes, max {})", content_len, MAX_FIRMWARE_SIZE);
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Firmware too large: {} bytes (max {} bytes = 3 MB OTA partition)", content_len, MAX_FIRMWARE_SIZE),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Firmware too large"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }

        let signed_sha256 = req.header("X-DCENT-Payload-SHA256").unwrap_or("").trim().to_string();
        let signed_size = req
            .header("X-DCENT-Payload-Size")
            .and_then(|value| value.trim().parse::<usize>().ok());
        let signed_version = req.header("X-DCENT-Version").unwrap_or("").trim().to_string();
        let signed_key_id = req.header("X-DCENT-Key-Id").unwrap_or("").trim().to_string();
        let signed_signature = req.header("X-DCENT-Signature").unwrap_or("").trim().to_string();
        // AOTA-6: OPTIONAL out-of-band integrity pin for UNSIGNED OTA. Distinct
        // header from the signed X-DCENT-Payload-SHA256 so it does NOT trip the
        // signed-metadata gate below. When present in unsigned mode, the streamed
        // SHA-256 must match this value (integrity without authenticity). This is
        // informational only and does NOT weaken the signed path or AOTA-1.
        let unsigned_sha256_pin = req
            .header("X-DCENT-Unsigned-SHA256")
            .unwrap_or("")
            .trim()
            .to_string();
        let ota_signature_metadata = if !signed_sha256.is_empty()
            || signed_size.is_some()
            || !signed_version.is_empty()
            || !signed_key_id.is_empty()
            || !signed_signature.is_empty()
            || ota_signature_required
        {
            if signed_sha256.is_empty()
                || signed_size.is_none()
                || signed_version.is_empty()
                || signed_key_id.is_empty()
                || signed_signature.is_empty()
            {
                let msg = serde_json::json!({
                    "success": false,
                    "message": "Signed OTA requires X-DCENT-Payload-SHA256, X-DCENT-Payload-Size, X-DCENT-Version, X-DCENT-Key-Id, and X-DCENT-Signature headers.",
                });
                let body = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Incomplete OTA signature headers"), &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                return Ok(());
            }
            Some((
                signed_sha256,
                signed_size.unwrap_or_default(),
                signed_version,
                signed_key_id,
                signed_signature,
            ))
        } else {
            None
        };

        if let Some((expected_sha256, expected_size, version, key_id, signature)) =
            ota_signature_metadata.as_ref()
        {
            let effective_floor =
                if crate::ota_signature::version_is_newer(&rollback_floor, DCENTAXE_VERSION) {
                    rollback_floor.clone()
                } else {
                    DCENTAXE_VERSION.to_string()
                };
            if ota_signature_required
                && !crate::ota_signature::version_is_newer(version, &effective_floor)
            {
                error!(
                    "API: OTA rejected before flash write - signed version '{}' is not newer than rollback floor '{}'",
                    version, effective_floor
                );
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!(
                        "Signed OTA version '{}' must be newer than rollback floor '{}'.",
                        version, effective_floor
                    ),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp =
                    req.into_response(400, Some("Signed OTA rollback blocked"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if *expected_size > MAX_FIRMWARE_SIZE {
                error!(
                    "API: OTA rejected before flash write - signed size {} exceeds max {}",
                    expected_size, MAX_FIRMWARE_SIZE
                );
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!("Signed OTA size {} exceeds max {} bytes.", expected_size, MAX_FIRMWARE_SIZE),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp =
                    req.into_response(400, Some("Signed OTA too large"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if content_len > 0 && *expected_size != content_len {
                error!(
                    "API: OTA rejected before flash write - signed size {} does not match Content-Length {}",
                    expected_size, content_len
                );
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!("Signed OTA size {} does not match Content-Length {}.", expected_size, content_len),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp =
                    req.into_response(400, Some("Signed OTA size mismatch"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if expected_sha256.len() != 64
                || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                error!("API: OTA rejected before flash write - signed SHA256 is not 64 hex chars");
                let msg = serde_json::json!({
                    "success": false,
                    "message": "Signed OTA SHA256 must be 64 hex characters.",
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp =
                    req.into_response(400, Some("Signed OTA hash invalid"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if let Err(e) = crate::ota_signature::verify_signed_metadata(
                &uploaded_target,
                &uploaded_device_model,
                version,
                *expected_size,
                &expected_sha256.to_ascii_lowercase(),
                key_id,
                signature,
            ) {
                error!("API: OTA signature preflight failed before flash write: {}", e);
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!("Signed OTA verification failed before flash write: {}", e),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(
                    400,
                    Some("Signed OTA verification failed"),
                    &JSON_HEADERS,
                )?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
        }

        // ── Get next OTA partition ──────────────────────────────────────
        let ota_partition = unsafe { esp_ota_get_next_update_partition(std::ptr::null()) };
        if ota_partition.is_null() {
            error!("API: OTA — no update partition found");
            let msg = serde_json::json!({
                "success": false,
                "message": "No OTA update partition found. Check partition table.",
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(500, Some("No OTA partition"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }

        // ── Begin OTA ───────────────────────────────────────────────────
        let mut ota_handle: esp_ota_handle_t = 0;
        let err = unsafe { esp_ota_begin(ota_partition, OTA_SIZE_UNKNOWN as usize, &mut ota_handle) };
        if err != ESP_OK {
            error!("API: esp_ota_begin failed: {}", err);
            let msg = serde_json::json!({
                "success": false,
                "message": format!("esp_ota_begin failed (err {}). Flash may be busy or partition invalid.", err),
            });
            let body = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(500, Some("OTA begin failed"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body.as_bytes());
            return Ok(());
        }

        info!("API: OTA partition ready, receiving firmware ({} bytes expected)...",
              if content_len > 0 { content_len.to_string() } else { "unknown".to_string() });

        // ── Stream chunks and write to flash ────────────────────────────
        const CHUNK_SIZE: usize = 1024;
        let mut buf = [0u8; CHUNK_SIZE];
        let mut total_received: usize = 0;
        let mut first_chunk = true;
        let mut ota_failed = false;
        let mut hasher = Sha256::new();

        loop {
            let recv_len = match req.read(&mut buf) {
                Ok(0) => break,   // EOF
                Ok(n) => n,
                Err(e) => {
                    // esp-idf-svc maps HTTPD_SOCK_ERR_TIMEOUT to a WouldBlock-style error;
                    // retry like ESP-Miner does.
                    if format!("{e}").contains("Timeout") {
                        continue;
                    }
                    error!("API: OTA receive error: {}", e);
                    unsafe { esp_ota_abort(ota_handle); }
                    let msg = serde_json::json!({
                        "success": false,
                        "message": format!("Network receive error: {}", e),
                    });
                    let body_str = serde_json::to_string(&msg).unwrap_or_default();
                    let mut resp = req.into_response(500, Some("Receive error"), &[("Content-Type", "application/json")])?;
                    let _ = resp.write(body_str.as_bytes());
                    return Ok(());
                }
            };

            // ── Validate ESP32 magic byte on first chunk ────────────────
            if first_chunk {
                first_chunk = false;
                if recv_len == 0 || buf[0] != 0xE9 {
                    error!("API: OTA rejected — invalid ESP32 image (magic byte 0x{:02X}, expected 0xE9)",
                           if recv_len > 0 { buf[0] } else { 0 });
                    unsafe { esp_ota_abort(ota_handle); }
                    let msg = serde_json::json!({
                        "success": false,
                        "message": format!(
                            "Invalid firmware image: magic byte 0x{:02X} (expected 0xE9 for ESP32). \
                             Make sure you are uploading a .bin file, not a .elf or .zip.",
                            if recv_len > 0 { buf[0] } else { 0 }
                        ),
                    });
                    let body_str = serde_json::to_string(&msg).unwrap_or_default();
                    let mut resp = req.into_response(400, Some("Invalid firmware image"), &[("Content-Type", "application/json")])?;
                    let _ = resp.write(body_str.as_bytes());
                    return Ok(());
                }
            }

            // ── Write chunk to flash ────────────────────────────────────
            hasher.update(&buf[..recv_len]);
            let write_err = unsafe {
                esp_ota_write(ota_handle, buf.as_ptr() as *const _, recv_len)
            };
            if write_err != ESP_OK {
                error!("API: esp_ota_write failed at offset {}: {}", total_received, write_err);
                unsafe { esp_ota_abort(ota_handle); }
                ota_failed = true;
                break;
            }

            total_received += recv_len;

            // Reject if we exceed the OTA partition even without Content-Length
            if total_received > MAX_FIRMWARE_SIZE {
                error!("API: OTA firmware exceeds 3MB partition limit");
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": "Firmware exceeds 3 MB OTA partition limit. Upload aborted.",
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Firmware too large"), &[("Content-Type", "application/json")])?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }

            // Log progress every ~100 KB
            if total_received % (100 * 1024) < CHUNK_SIZE {
                if content_len > 0 {
                    let pct = (total_received * 100) / content_len;
                    info!("API: OTA progress: {} / {} bytes ({}%)", total_received, content_len, pct);
                } else {
                    info!("API: OTA progress: {} bytes received", total_received);
                }
            }
        }

        if ota_failed {
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Flash write error at offset {}. OTA aborted.", total_received),
            });
            let body_str = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(500, Some("Flash write error"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body_str.as_bytes());
            return Ok(());
        }

        // ── Final size check ────────────────────────────────────────────
        if total_received < MIN_FIRMWARE_SIZE {
            error!("API: OTA received only {} bytes — too small for valid firmware", total_received);
            unsafe { esp_ota_abort(ota_handle); }
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Received only {} bytes — too small for valid ESP32 firmware (min {} bytes)", total_received, MIN_FIRMWARE_SIZE),
            });
            let body_str = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("Firmware too small"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body_str.as_bytes());
            return Ok(());
        }

        if content_len > 0 && total_received != content_len {
            error!(
                "API: OTA Content-Length mismatch (declared {}, received {})",
                content_len, total_received
            );
            unsafe { esp_ota_abort(ota_handle); }
            let msg = serde_json::json!({
                "success": false,
                "message": format!("OTA upload length mismatch: declared {} bytes, received {} bytes", content_len, total_received),
            });
            let body_str = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(400, Some("OTA length mismatch"), &JSON_HEADERS)?;
            let _ = resp.write(body_str.as_bytes());
            return Ok(());
        }

        let payload_sha256 = format!("{:x}", hasher.finalize());
        let signed_version_for_floor = ota_signature_metadata
            .as_ref()
            .map(|(_, _, version, _, _)| version.clone());
        if let Some((expected_sha256, expected_size, version, key_id, signature)) = ota_signature_metadata {
            let effective_floor = if crate::ota_signature::version_is_newer(&rollback_floor, DCENTAXE_VERSION) {
                rollback_floor.clone()
            } else {
                DCENTAXE_VERSION.to_string()
            };
            if ota_signature_required
                && !crate::ota_signature::version_is_newer(&version, &effective_floor)
            {
                error!(
                    "API: OTA rejected — signed version '{}' is not newer than rollback floor '{}'",
                    version, effective_floor
                );
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!(
                        "Signed OTA version '{}' must be newer than rollback floor '{}'.",
                        version, effective_floor
                    ),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Signed OTA rollback blocked"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if expected_size != total_received {
                error!(
                    "API: OTA rejected — signed size mismatch (expected {}, got {})",
                    expected_size, total_received
                );
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!("Signed OTA size mismatch: expected {} bytes, received {} bytes", expected_size, total_received),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Signed OTA size mismatch"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if expected_sha256.to_ascii_lowercase() != payload_sha256 {
                error!("API: OTA rejected — signed sha256 mismatch");
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": "Signed OTA SHA256 mismatch. Upload aborted.",
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Signed OTA hash mismatch"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            if let Err(e) = crate::ota_signature::verify_signed_metadata(
                &uploaded_target,
                &uploaded_device_model,
                &version,
                total_received,
                &payload_sha256,
                &key_id,
                &signature,
            ) {
                error!("API: OTA signature verification failed: {}", e);
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": format!("Signed OTA verification failed: {}", e),
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Signed OTA verification failed"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
        } else {
            // AOTA-6: UNSIGNED OTA mode (no signed metadata present). Surface the
            // computed payload SHA-256 for out-of-band auditing and, if the caller
            // supplied the optional X-DCENT-Unsigned-SHA256 pin, verify it here —
            // integrity WITHOUT authenticity. This runs ONLY when no signed
            // metadata is present, so it does not weaken the signed path or the
            // AOTA-1 passwordless-flip protection.
            if !unsigned_sha256_pin.is_empty()
                && unsigned_sha256_pin.to_ascii_lowercase() != payload_sha256
            {
                error!(
                    "API: OTA rejected — unsigned SHA256 pin mismatch (pinned {}, got {})",
                    unsigned_sha256_pin, payload_sha256
                );
                unsafe { esp_ota_abort(ota_handle); }
                let msg = serde_json::json!({
                    "success": false,
                    "message": "Unsigned OTA SHA256 mismatch. Upload aborted.",
                });
                let body_str = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(400, Some("Unsigned OTA SHA256 mismatch"), &JSON_HEADERS)?;
                let _ = resp.write(body_str.as_bytes());
                return Ok(());
            }
            warn!(
                "API: OTA accepted in UNSIGNED mode — no authenticity check; payload sha256={}",
                payload_sha256
            );
        }

        // ── Finalize OTA: validate image + set boot partition ───────────
        let end_err = unsafe { esp_ota_end(ota_handle) };
        if end_err != ESP_OK {
            error!("API: esp_ota_end failed: {} — image validation failed", end_err);
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Firmware validation failed (esp_ota_end err {}). The image may be corrupt or incompatible.", end_err),
            });
            let body_str = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(500, Some("Validation failed"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body_str.as_bytes());
            return Ok(());
        }

        let boot_err = unsafe { esp_ota_set_boot_partition(ota_partition) };
        if boot_err != ESP_OK {
            error!("API: esp_ota_set_boot_partition failed: {}", boot_err);
            let msg = serde_json::json!({
                "success": false,
                "message": format!("Failed to set boot partition (err {}). OTA written but not activated.", boot_err),
            });
            let body_str = serde_json::to_string(&msg).unwrap_or_default();
            let mut resp = req.into_response(500, Some("Boot partition error"), &[("Content-Type", "application/json")])?;
            let _ = resp.write(body_str.as_bytes());
            return Ok(());
        }

        if let Some(version) = signed_version_for_floor {
            if let Ok(mut nvs_guard) = state_post.nvs.lock() {
                if let Some(nvs) = nvs_guard.as_mut() {
                    if let Err(e) = nvs_config::update_ota_floor_if_newer(nvs, &version) {
                        warn!("Failed to persist OTA rollback floor: {}", e);
                    }
                }
            }
        }

        // ── Success — respond and schedule reboot ───────────────────────
        // Success here means the image was accepted and the next boot slot was
        // scheduled. Reboot, rollback validation, and mining proof are later evidence.
        info!(
            "API: OTA accepted and boot partition scheduled. {} bytes written; rebooting...",
            total_received
        );
        if let Ok(config) = state_post.config.lock() {
            crate::notifications::spawn_event(
                config.notifications.clone(),
                crate::notifications::NotificationKind::Ota,
                "Firmware update accepted",
                format!("{} bytes written; reboot pending", total_received),
            );
        }

        let msg = serde_json::json!({
            "success": true,
            "message": "OTA accepted and boot partition scheduled. Rebooting; version and rollback proof pending.",
            "bytesWritten": total_received,
            // AOTA-6: always surface the computed image SHA-256 so an operator can
            // compare it out-of-band (auditability; not an authenticity claim).
            "payloadSha256": payload_sha256,
        });
        let body_str = serde_json::to_string(&msg).unwrap_or_default();
        let mut resp = req.into_response(200, None, &[("Content-Type", "application/json")])?;
        let _ = resp.write(body_str.as_bytes());

        // Delay to let HTTP response flush, then reboot
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            info!("Restarting system after OTA firmware update");
            unsafe { esp_idf_svc::sys::esp_restart(); }
        });

        Ok(())
    }).expect("Failed to register POST /api/system/OTA");

    server
        .fn_handler(
            "/api/system/OTAWWW",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                let msg = serde_json::json!({
                    "success": true,
                    "message": "DCENT_axe embeds its web UI inside the main firmware image. OTAWWW is a compatibility no-op.",
                });
                let body = serde_json::to_string(&msg).unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register POST /api/system/OTAWWW");
}

/// Gather current firmware version and partition information for the GET endpoint.
fn get_ota_info(state: &SharedState) -> serde_json::Value {
    use esp_idf_svc::sys::*;

    let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
    let board_cfg = config.board_config();
    let runtime_device_model = config.bitaxe_model().canonical_key();
    let build_device_model = crate::config::default_model_for_build().canonical_key();
    let build_board_version = crate::config::default_profile_for_build().board_version;
    let ota_signature_capable = crate::ota_signature::signature_required();
    let ota_signature_required = crate::ota_signature::ota_signature_required_for_display(
        ota_signature_capable,
        config.allow_unsigned_ota,
        auth::password_is_set(state),
    );
    let rollback_floor = state
        .nvs
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .as_ref()
                .and_then(|nvs| nvs_config::load_ota_floor(nvs))
        })
        .unwrap_or_else(|| DCENTAXE_VERSION.to_string());

    let running_partition = unsafe { esp_ota_get_running_partition() };
    let (running_label, next_label) = partition_labels();

    // Read the app description from the running partition
    let mut app_desc: esp_app_desc_t = unsafe { std::mem::zeroed() };
    let has_desc = if !running_partition.is_null() {
        unsafe { esp_ota_get_partition_description(running_partition, &mut app_desc) == ESP_OK }
    } else {
        false
    };

    let (app_version, idf_version, compile_date, compile_time) = if has_desc {
        let ver = unsafe { std::ffi::CStr::from_ptr(app_desc.version.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let idf = unsafe { std::ffi::CStr::from_ptr(app_desc.idf_ver.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let date = unsafe { std::ffi::CStr::from_ptr(app_desc.date.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let time = unsafe { std::ffi::CStr::from_ptr(app_desc.time.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        (ver, idf, date, time)
    } else {
        (
            "unknown".into(),
            "unknown".into(),
            "unknown".into(),
            "unknown".into(),
        )
    };

    serde_json::json!({
        "firmware": {
            "version": app_version,
            "type": "DCENT_axe",
            "boardTarget": BUILD_BOARD_TARGET,
            "boardVersion": board_cfg.board_version.clone(),
            "deviceModel": runtime_device_model,
            "buildDeviceModel": build_device_model,
            "buildBoardVersion": build_board_version,
            "idfVersion": idf_version,
            "compileDate": compile_date,
            "compileTime": compile_time,
        },
        "partition": {
            "running": running_label,
            "nextUpdate": next_label,
        },
        "ota": {
            "maxSize": 3 * 1024 * 1024,
            "minSize": 64 * 1024,
            "magicByte": "0xE9",
            "signatureCapable": ota_signature_capable,
            "signatureRequired": ota_signature_required,
            "allowUnsigned": config.allow_unsigned_ota,
            "keyId": crate::ota_signature::compiled_key_id().unwrap_or(""),
            "rollbackFloor": rollback_floor,
        },
        "toolbox": update_metadata(state),
    })
}

fn register_update_metadata(server: &mut EspHttpServer, state: SharedState) {
    let state_get = state.clone();
    server
        .fn_handler(
            "/api/system/update/metadata",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let body = serde_json::to_string(&update_metadata(&state_get)).unwrap_or_default();
                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/system/update/metadata");

    let register_manifest = |path: &str, _state: SharedState, server: &mut EspHttpServer| {
        server
            .fn_handler(
                path,
                Method::Get,
                move |req| -> Result<(), Box<dyn std::error::Error>> {
                    let body = serde_json::to_string(&serde_json::json!({
                        "success": false,
                        "message": "Device-served package manifests are deprecated. Use packaged release manifests generated by the build/package scripts.",
                        "authoritativeEndpoint": null,
                        "authoritativeSource": "Packaged release manifest generated beside the firmware artifacts",
                    }))
                    .unwrap_or_default();
                    let mut resp = req.into_response(410, Some("Deprecated manifest endpoint"), &JSON_HEADERS)?;
                    resp.write(body.as_bytes())?;
                    Ok(())
                },
            )
            .expect("Failed to register update package manifest endpoint");
    };

    register_manifest("/api/system/update/package-manifest", state.clone(), server);
    register_manifest("/api/system/update/manifest", state.clone(), server);
}

// ─── Mining Presets ──────────────────────────────────────────────────────

fn register_presets(server: &mut EspHttpServer, state: SharedState) {
    let state_get = state.clone();
    server
        .fn_handler(
            "/api/presets",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let model = {
                    let cfg = state_get.config.lock().unwrap_or_else(|e| e.into_inner());
                    cfg.bitaxe_model()
                };
                let presets = crate::config::mining_presets(model);
                let recommended_name = recommended_preset_name(model);
                let presets_json: Vec<serde_json::Value> = presets
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "frequency": p.frequency,
                            "voltage": p.voltage_mv,
                            "expectedHashrate": p.expected_hashrate_ghs,
                            "expectedPower": p.expected_power_w,
                            "requiresOverclock": p.requires_overclock,
                            "recommended": p.name == recommended_name,
                            "description": preset_description(p.name),
                        })
                    })
                    .collect();
                let body = serde_json::json!({
                    "model": format!("{:?}", model),
                    "recommendedPreset": recommended_name,
                    "presets": presets_json,
                })
                .to_string();
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "application/json"),
                        ("Access-Control-Allow-Origin", "*"),
                    ],
                )?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/presets");
}

// ─── Mining Start/Stop Toggle ─────────────────────────────────────────────

/// POST /api/mining/stop — pause mining (sets flag; mining thread checks it)
/// POST /api/mining/start — resume mining (sets flag; triggers reboot to reinit ASIC)
fn register_mining_toggle(server: &mut EspHttpServer, state: SharedState) {
    let state_stop = state.clone();
    server
        .fn_handler(
            "/api/mining/stop",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_stop) {
                    return auth::write_auth_failure(req, err);
                }
                {
                    let mut telem = state_stop
                        .telemetry
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    telem.mining_enabled = false;
                }
                let mut resp =
                    req.into_response(200, None, &[("Content-Type", "application/json")])?;
                resp.write(b"{\"status\":\"stopped\"}")?;
                Ok(())
            },
        )
        .expect("Failed to register POST /api/mining/stop");

    let state_start = state.clone();
    server
        .fn_handler(
            "/api/mining/start",
            Method::Post,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_start) {
                    return auth::write_auth_failure(req, err);
                }
                {
                    let mut telem = state_start
                        .telemetry
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    telem.mining_enabled = true;
                }
                let mut resp =
                    req.into_response(200, None, &[("Content-Type", "application/json")])?;
                resp.write(b"{\"status\":\"started\"}")?;
                Ok(())
            },
        )
        .expect("Failed to register POST /api/mining/start");
}

/// GET /api/achievements — drift-proof source of truth for achievement labels.
/// The dashboard fetches this once at boot so its ACH array can never fall out
/// of sync with `nvs_config::achievement_name()` (the source of truth).
/// Public: no auth required, read-only.
fn register_achievements(server: &mut EspHttpServer) {
    server
        .fn_handler(
            "/api/achievements",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut entries = Vec::with_capacity(nvs_config::ACHIEVEMENT_TOTAL as usize);
                for i in 0..nvs_config::ACHIEVEMENT_TOTAL {
                    let bit = 1u32 << i;
                    let name = nvs_config::achievement_name(bit);
                    // Rough rarity tiers — deterministic, not persisted. Tier
                    // bumps every six bits roughly match the difficulty ladder
                    // (early = common, late = legendary).
                    let rarity = match i {
                        0..=5 => "common",
                        6..=11 => "uncommon",
                        12..=17 => "rare",
                        18..=22 => "epic",
                        _ => "legendary",
                    };
                    entries.push(serde_json::json!({
                        "bit": i,
                        "name": name,
                        "rarity": rarity,
                    }));
                }
                let body = serde_json::to_string(&serde_json::json!({
                    "total": nvs_config::ACHIEVEMENT_TOTAL,
                    "entries": entries,
                }))
                .unwrap_or_else(|_| "{}".into());
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/achievements");
}

/// GET /api/autotuner/modes — drift-proof source of truth for autotuner modes.
/// Public: no auth required, read-only.
fn register_autotuner_modes(server: &mut EspHttpServer) {
    server
        .fn_handler(
            "/api/autotuner/modes",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let entries: Vec<_> = crate::autotuner::MODE_DESCRIPTIONS
                    .iter()
                    .map(|(id, name, desc)| {
                        serde_json::json!({
                            "id": id,
                            "name": name,
                            "description": desc,
                        })
                    })
                    .collect();
                let body = serde_json::to_string(&serde_json::json!({ "modes": entries }))
                    .unwrap_or_else(|_| "{}".into());
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/autotuner/modes");
}

/// GET /api/swarm — lightweight shared swarm status for future queen/worker logic.
fn register_swarm_status(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/swarm",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_read(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                let config = state.config.lock().unwrap_or_else(|e| e.into_inner());
                let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
                let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
                let swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
                let snap = stats.snapshot();
                let body = serde_json::to_string(&swarm_status_response(
                    &config,
                    &telem,
                    &swarm,
                    snap.hashrate_5m_ghs,
                ))
                .unwrap_or_default();

                let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /api/swarm");
}

/// POST /api/swarm/room-temp — accept external room temperature for automation.
fn register_swarm_room_temp(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/swarm/room-temp",
            Method::Options,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(b"");
                Ok(())
            },
        )
        .expect("Failed to register OPTIONS /api/swarm/room-temp");

    server
        .fn_handler(
            "/api/swarm/room-temp",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 512) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                let parsed = serde_json::from_slice::<SwarmRoomTempRequest>(&body);

                match parsed {
                    Ok(payload) if payload.temp_c.is_finite() && (-40.0..=80.0).contains(&payload.temp_c) => {
                        let ttl = payload.ttl_sec.unwrap_or(300).clamp(30, 3600);
                        let expires = unix_time_s() + ttl;
                        {
                            let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
                            swarm.observed_room_temp_c = Some(payload.temp_c);
                            swarm.room_temp_source = payload.source.clone();
                            swarm.room_temp_expires_epoch_s = Some(expires);
                        }
                        let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                        let _ = resp.write(
                            serde_json::json!({
                                "status": "ok",
                                "acceptedTempC": payload.temp_c,
                                "source": payload.source,
                                "expiresInSec": ttl,
                            })
                            .to_string()
                            .as_bytes(),
                        );
                    }
                    _ => {
                        let mut resp = req.into_response(400, None, &COMPAT_HEADERS)?;
                        let _ = resp.write(
                            b"{\"status\":\"error\",\"message\":\"temp_c must be a finite number in range [-40, 80]\"}",
                        );
                    }
                }
                Ok(())
            },
        )
        .expect("Failed to register POST /api/swarm/room-temp");
}

/// POST /api/swarm/config — change the room-temp source for Space Heater mode.
/// Body: `{"roomTempSource": "local" | "swarm_average" | "external"}`.
fn register_swarm_config(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/swarm/config",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 256) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                let parsed: serde_json::Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => {
                        let mut resp = req.into_response(400, None, &COMPAT_HEADERS)?;
                        let _ = resp.write(b"{\"error\":\"invalid JSON\"}");
                        return Ok(());
                    }
                };
                let src_str = parsed
                    .get("roomTempSource")
                    .or_else(|| parsed.get("room_temp_source"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("local");
                let src = match src_str {
                    "swarm_average" | "swarmAverage" => crate::config::RoomTempSource::SwarmAverage,
                    "external" => crate::config::RoomTempSource::External,
                    _ => crate::config::RoomTempSource::Local,
                };
                if let Ok(mut cfg) = state.config.lock() {
                    cfg.room_temp_source = src;
                    let to_save = cfg.clone();
                    drop(cfg);
                    if let Ok(mut guard) = state.nvs.lock() {
                        if let Some(nvs) = guard.as_mut() {
                            let _ = crate::nvs_config::save_config(nvs, &to_save);
                        }
                    }
                }
                let mut resp = req.into_response(200, None, &COMPAT_HEADERS)?;
                let _ = resp.write(
                    serde_json::json!({"status": "ok", "roomTempSource": src_str})
                        .to_string()
                        .as_bytes(),
                );
                Ok(())
            },
        )
        .expect("Failed to register POST /api/swarm/config");
}

fn register_swarm_report(server: &mut EspHttpServer, state: SharedState) {
    server
        .fn_handler(
            "/api/swarm/report",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state) {
                    return auth::write_auth_failure(req, err);
                }

                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 1024) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                match serde_json::from_slice::<SwarmPeerReport>(&body) {
                    Ok(report) => {
                        let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
                        let node_id = report
                            .id
                            .unwrap_or_else(|| format!("{}@{}", report.hostname, report.ip));
                        let node = crate::shared::SwarmNode {
                            id: node_id.clone(),
                            hostname: report.hostname,
                            display_name: node_id.clone(),
                            ip: report.ip,
                            board_model: report.board_model,
                            board_version: report.board_version.unwrap_or_default(),
                            board_target: report.board_target.unwrap_or_default(),
                            asic_model: report.asic_model,
                            firmware_version: report.firmware_version,
                            mining_enabled: report.mining_enabled,
                            pool_connected: report.pool_connected,
                            hashrate_ghs: report.hashrate_ghs,
                            last_seen_unix_ms: crate::shared::unix_time_ms(),
                            source: crate::shared::SwarmSource::Reported,
                        };

                        if let Some(existing) = swarm
                            .peers
                            .iter_mut()
                            .find(|peer| peer.id == node.id || peer.ip == node.ip)
                        {
                            *existing = node;
                        } else {
                            swarm.peers.push(node);
                            if swarm.peers.len() > swarm.max_peers {
                                let excess = swarm.peers.len() - swarm.max_peers;
                                swarm.peers.drain(0..excess);
                            }
                        }

                        let mut resp = req.into_response(200, None, &JSON_HEADERS)?;
                        let _ = resp.write(b"{\"status\":\"ok\"}");
                    }
                    Err(e) => {
                        let mut resp =
                            req.into_response(400, Some("Invalid peer report"), &JSON_HEADERS)?;
                        let _ = resp.write(
                            format!("{{\"status\":\"error\",\"message\":\"{}\"}}", e).as_bytes(),
                        );
                    }
                }
                Ok(())
            },
        )
        .expect("Failed to register POST /api/swarm/report");
}

// ─── PWA Manifest ─────────────────────────────────────────────────────────

fn register_pwa_manifest(server: &mut EspHttpServer) {
    server.fn_handler("/manifest.json", Method::Get, |req| -> Result<(), Box<dyn std::error::Error>> {
        let manifest = r##"{"name":"DCENT_axe","short_name":"DCENTaxe","start_url":"/","display":"standalone","background_color":"#0f1923","theme_color":"#f97316","icons":[]}"##;
        let mut resp = req.into_response(200, None, &[
            ("Content-Type", "application/manifest+json"),
            ("Cache-Control", "max-age=86400"),
        ])?;
        resp.write(manifest.as_bytes())?;
        Ok(())
    }).expect("Failed to register GET /manifest.json");
}

// ─── Prometheus Metrics ───────────────────────────────────────────────────

/// GET /metrics — Prometheus-compatible metrics for Grafana monitoring.
fn register_prometheus(server: &mut EspHttpServer, state: SharedState) {
    let state_prom = state.clone();
    server
        .fn_handler(
            "/metrics",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_metrics(&req, &state_prom) {
                    return auth::write_auth_failure(req, err);
                }
                let stats = state_prom.stats.lock().unwrap_or_else(|e| e.into_inner());
                let snap = stats.snapshot();
                drop(stats);
                let stratum_metrics = stratum_metric_snapshots(&state_prom);
                let stratum_shares_pending: u64 = stratum_metrics
                    .iter()
                    .map(|status| status.shares_pending as u64)
                    .sum();
                let stratum_shares_unresolved: u64 = stratum_metrics
                    .iter()
                    .map(|status| status.shares_unresolved)
                    .sum();
                let oldest_pending_submit_age_ms = stratum_metrics
                    .iter()
                    .map(|status| status.oldest_pending_submit_age_ms)
                    .max()
                    .unwrap_or(0);
                // B2/B3: gather the active pool's reject-reason breakdown + share
                // freshness timestamps (same active-status selection as
                // /api/system/info and MCP get_status). We don't need event
                // records here, so trim them with limit 0. This acquires and
                // releases the stratum_status lock (no nesting with telemetry/
                // config/pool_stats, matching the stratum_metric_snapshots call
                // above).
                let stratum_statuses =
                    stratum_status_snapshots_with_recent_event_limit(&state_prom, 0);
                let active_status = stratum_statuses
                    .iter()
                    .find(|status| status.connected)
                    .or_else(|| stratum_statuses.first());
                let share_truth = crate::metrics_render::ShareTruthView {
                    reject_reason_counts: active_status
                        .map(|status| {
                            status
                                .reject_reason_counts
                                .iter()
                                .map(|reason| (reason.key.clone(), reason.count as u64))
                                .collect()
                        })
                        .unwrap_or_default(),
                    last_share_response_unix_ms: active_status
                        .map(|status| status.last_share_response_unix_ms)
                        .unwrap_or(0),
                    last_share_accepted_unix_ms: active_status
                        .map(|status| status.last_share_accepted_unix_ms)
                        .unwrap_or(0),
                    last_share_rejected_unix_ms: active_status
                        .map(|status| status.last_share_rejected_unix_ms)
                        .unwrap_or(0),
                };
                let telem = state_prom
                    .telemetry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let config = state_prom.config.lock().unwrap_or_else(|e| e.into_inner());
                let (chip_temp_min_c, chip_temp_max_c, chip_temp_spread_c) = {
                    let mut min_temp = f32::MAX;
                    let mut max_temp = 0.0_f32;
                    let mut count = 0_u32;
                    for chip in telem.chip_data.iter() {
                        if let Some(temp) = chip.temp_c.filter(|temp| *temp > 0.0) {
                            min_temp = min_temp.min(temp);
                            max_temp = max_temp.max(temp);
                            count += 1;
                        }
                    }
                    if count == 0 {
                        (0.0, 0.0, 0.0)
                    } else {
                        (min_temp, max_temp, max_temp - min_temp)
                    }
                };
                let max_temperature_c = telem
                    .chip_temp_c
                    .max(telem.board_temp_c)
                    .max(telem.vreg_temp_c)
                    .max(telem.inlet_temp_c)
                    .max(telem.outlet_temp_c)
                    .max(chip_temp_max_c);
                let air_delta_c = if telem.inlet_temp_c > 0.0 && telem.outlet_temp_c > 0.0 {
                    telem.outlet_temp_c - telem.inlet_temp_c
                } else {
                    0.0
                };
                let power_limits = config.power_limits();

                // Thin caller: gather the live values into the host-pure plain
                // structs and let `crate::metrics_render::render_metrics_body` build the
                // entire exposition text (single source of truth, host-tested).
                let snapshot = crate::metrics_render::MetricsSnapshot {
                    hashrate_1m_ghs: snap.hashrate_1m_ghs,
                    hashrate_5m_ghs: snap.hashrate_5m_ghs,
                    hashrate_15m_ghs: snap.hashrate_15m_ghs,
                    accepted_shares: snap.accepted_shares,
                    rejected_shares: snap.rejected_shares,
                    stratum_shares_pending,
                    stratum_shares_unresolved,
                    oldest_pending_submit_age_ms,
                    stale_nonces: snap.stale_nonces,
                    slot_recoveries: snap.slot_recoveries,
                    filtered_shares: snap.filtered_shares,
                    ticket_difficulty: snap.ticket_difficulty,
                    best_difficulty: snap.best_difficulty,
                    chip_temp_c: telem.chip_temp_c,
                    board_temp_c: telem.board_temp_c,
                    vreg_temp_c: telem.vreg_temp_c,
                    inlet_temp_c: telem.inlet_temp_c,
                    outlet_temp_c: telem.outlet_temp_c,
                    chip_temp_min_c,
                    chip_temp_max_c,
                    chip_temp_spread_c,
                    max_temperature_c,
                    air_delta_c,
                    power_w: telem.power_w,
                    current_ma: telem.current_ma,
                    voltage_mv: telem.voltage_mv,
                    input_voltage_mv: telem.input_voltage_mv,
                    max_power_w: power_limits.max_power_w,
                    max_current_a: power_limits.max_current_a,
                    target_frequency: config.target_frequency,
                    fan_speed_pct: telem.fan_speed_pct,
                    fan_rpm: telem.fan_rpm,
                    fan2_rpm: telem.fan2_rpm,
                    sensors_ok: telem.sensors_ok,
                    mining_enabled: telem.mining_enabled,
                    uptime_secs: snap.uptime_secs,
                    free_heap: telem.free_heap,
                    achievement_count: telem.achievement_count,
                    lifetime_shares: telem.lifetime_shares,
                };
                let pool_pending: Vec<crate::metrics_render::PoolPendingRow> = stratum_metrics
                    .iter()
                    .map(|status| crate::metrics_render::PoolPendingRow {
                        pool_index: status.pool_index,
                        shares_pending: status.shares_pending,
                        shares_unresolved: status.shares_unresolved,
                    })
                    .collect();
                drop(telem);
                drop(config);

                // Per-pool split-mining rows (render gates on len > 1, exactly
                // like the former inline body).
                let pool_split: Vec<crate::metrics_render::PoolSplitRow> = {
                    let pool_stats = state_prom
                        .pool_stats
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    pool_stats
                        .iter()
                        .map(|p| crate::metrics_render::PoolSplitRow {
                            index: p.index,
                            target_pct: p.target_pct,
                            dispatched_count: p.dispatched_count,
                            shares_accepted: p.shares_accepted,
                            shares_rejected: p.shares_rejected,
                            connected: p.connected,
                        })
                        .collect()
                };

                let body = crate::metrics_render::render_metrics_body(
                    &snapshot,
                    &pool_pending,
                    &pool_split,
                    &share_truth,
                );

                let mut resp = req.into_response(
                    200,
                    None,
                    &[("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
                )?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /metrics");
}

// ─── Power Schedule API ───────────────────────────────────────────────────

/// GET /api/schedule — read current schedule
/// POST /api/schedule — set schedule
fn register_schedule(server: &mut EspHttpServer, state: SharedState) {
    // GET /api/schedule — read current schedule
    let state_get = state.clone();
    server
        .fn_handler(
            "/api/schedule",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let cfg = state_get.config.lock().unwrap_or_else(|e| e.into_inner());
                let uptime_secs = state_get
                    .telemetry
                    .lock()
                    .map(|telem| telem.uptime_secs)
                    .unwrap_or(0);
                let body = serde_json::to_string(&schedule_status_json(&cfg, uptime_secs))
                    .unwrap_or_else(|_| "{}".into());
                drop(cfg);
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "application/json"),
                        ("Access-Control-Allow-Origin", "*"),
                    ],
                )?;
                resp.write(body.as_bytes())?;
                Ok(())
            },
        )
        .expect("Failed to register GET /api/schedule");

    // POST /api/schedule — set schedule
    let state_post = state.clone();
    server
        .fn_handler(
            "/api/schedule",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                if let Err(err) = auth::authorize_rest_write(&req, &state_post) {
                    return auth::write_auth_failure(req, err);
                }
                // MAINAPI-3: accumulate the full body until EOF.
                let body = match read_full_body(&mut req, 4096) {
                    Ok(b) => b,
                    Err(e) => return api_body_error_response(req, e),
                };
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                    let parsed_schedule: Result<Vec<crate::config::PowerSchedule>, _> = if value.is_array() {
                        serde_json::from_value(value.clone())
                    } else {
                        serde_json::from_value(
                            value
                                .get("entries")
                                .cloned()
                                .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
                        )
                    };
                    let mut schedule = match parsed_schedule {
                        Ok(schedule) => schedule,
                        Err(e) => {
                            let mut resp = req.into_response(
                                400,
                                Some("Bad Request"),
                                &[("Content-Type", "application/json")],
                            )?;
                            resp.write(
                                json!({"error":"invalid schedule entries", "detail": e.to_string()})
                                    .to_string()
                                    .as_bytes(),
                            )?;
                            return Ok(());
                        }
                    };

                    if schedule.len() > 8 {
                        schedule.truncate(8);
                    }

                    let schedule_enabled = value
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let timezone_offset_minutes = value
                        .get("timezoneOffsetMinutes")
                        .or_else(|| value.get("timezone_offset_minutes"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        .clamp(-720, 840) as i16;

                    let mut save_error: Option<String> = None;
                    {
                        let mut cfg = state_post.config.lock().unwrap_or_else(|e| e.into_inner());
                        let mut sanitized = Vec::with_capacity(schedule.len());
                        for mut entry in schedule {
                            entry.hour = entry.hour.min(23);
                            entry.minute = entry.minute.min(59);
                            entry.label = sanitize_schedule_label(&entry.label);
                            entry.autotune_target =
                                entry.autotune_target.map(|v| v.clamp(0.0, 10_000.0));
                            if entry.autotune_enabled == Some(true) {
                                entry.autotune_mode = entry
                                    .autotune_mode
                                    .as_deref()
                                    .and_then(normalize_schedule_autotune_mode)
                                    .or_else(|| Some("best_efficiency".to_string()));
                            } else if let Some(mode) = entry.autotune_mode.clone() {
                                entry.autotune_mode = normalize_schedule_autotune_mode(&mode);
                            }
                            let qualified = cfg.qualify_operating_point(
                                entry.frequency,
                                entry.voltage_mv,
                                crate::config::ControlSurface::Schedule,
                            );
                            if qualified.clamped {
                                info!(
                                    "API: clamped scheduled point '{}' {}MHz/{}mV -> {:.0}MHz/{}mV",
                                    entry.label,
                                    entry.frequency,
                                    entry.voltage_mv,
                                    qualified.frequency_mhz,
                                    qualified.voltage_mv
                                );
                            }
                            entry.frequency = qualified.frequency_mhz;
                            entry.voltage_mv = qualified.voltage_mv;
                            sanitized.push(entry);
                        }
                        cfg.schedule_enabled = schedule_enabled;
                        cfg.schedule_timezone_offset_minutes = timezone_offset_minutes;
                        cfg.power_schedule = sanitized;
                    }
                    // Persist to NVS
                    if let Ok(mut nvs_guard) = state_post.nvs.lock() {
                        if let Some(ref mut nvs) = *nvs_guard {
                            let cfg = state_post.config.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = nvs_config::save_config(nvs, &cfg) {
                                error!("API: NVS save failed for schedule: {}", e);
                                save_error = Some(e);
                            } else {
                                info!("API: power schedule saved to NVS");
                            }
                        }
                    }
                    if let Some(error) = save_error {
                        let mut resp = req.into_response(
                            500,
                            Some("Internal Server Error"),
                            &[("Content-Type", "application/json")],
                        )?;
                        resp.write(
                            json!({"error":"schedule not persisted", "detail": error})
                                .to_string()
                                .as_bytes(),
                        )?;
                    } else {
                        let uptime_secs = state_post
                            .telemetry
                            .lock()
                            .map(|telem| telem.uptime_secs)
                            .unwrap_or(0);
                        let cfg = state_post.config.lock().unwrap_or_else(|e| e.into_inner());
                        let mut resp =
                            req.into_response(200, None, &[("Content-Type", "application/json")])?;
                        resp.write(
                            json!({"status":"schedule updated", "schedule": schedule_status_json(&cfg, uptime_secs)})
                                .to_string()
                                .as_bytes(),
                        )?;
                    }
                } else {
                    let mut resp = req.into_response(
                        400,
                        Some("Bad Request"),
                        &[("Content-Type", "application/json")],
                    )?;
                    resp.write(b"{\"error\":\"invalid schedule JSON\"}")?;
                }
                Ok(())
            },
        )
        .expect("Failed to register POST /api/schedule");
}

// ─── Pool Splitting API ──────────────────────────────────────────────────

/// GET /api/pools — per-pool statistics for hashrate splitting.
///
/// Returns an array of pool stats, one per configured pool.
/// If only a single pool is configured, returns an array with one element.
fn register_pools(server: &mut EspHttpServer, state: SharedState) {
    let state_pools = state.clone();
    server.fn_handler("/api/pools", Method::Get, move |req| -> Result<(), Box<dyn std::error::Error>> {
        if let Err(err) = auth::authorize_rest_read(&req, &state_pools) {
            return auth::write_auth_failure(req, err);
        }

        let config = state_pools.config.lock().unwrap_or_else(|e| e.into_inner());
        let pool_stats = state_pools.pool_stats.lock().unwrap_or_else(|e| e.into_inner());
        let stratum_statuses = stratum_status_snapshots(&state_pools);

        let total_dispatched: u64 = pool_stats.iter().map(|p| p.dispatched_count).sum();

        let mut pools_json = Vec::new();

        // Primary pool (always present)
        let primary_stats = pool_stats.iter().find(|p| p.index == 0);
        let primary_runtime = stratum_statuses.first();
        pools_json.push(serde_json::json!({
            "index": 0,
            // B-ESP-10: sanitize URL (strip user:pass@) + mask worker (BTC payout addr).
            "url": primary_runtime
                .map(|status| format!("{}:{}", crate::shared::sanitize_pool_url(&status.active_url), status.active_port))
                .unwrap_or_else(|| format!("{}:{}", crate::shared::sanitize_pool_url(&config.stratum.url), config.stratum.port)),
            "worker": crate::shared::mask_wallet(&config.stratum.worker_name),
            "target_pct": primary_stats.map(|s| s.target_pct).unwrap_or(100),
            "actual_pct": primary_stats.map(|s| s.actual_pct(total_dispatched)).unwrap_or(100.0),
            "dispatched": primary_stats.map(|s| s.dispatched_count).unwrap_or(0),
            "shares_submitted": primary_runtime.map(|s| s.shares_submitted).or_else(|| primary_stats.map(|s| s.shares_submitted)).unwrap_or(0),
            "shares_accepted": primary_runtime.map(|s| s.shares_accepted).or_else(|| primary_stats.map(|s| s.shares_accepted)).unwrap_or(0),
            "shares_rejected": primary_runtime.map(|s| s.shares_rejected).or_else(|| primary_stats.map(|s| s.shares_rejected)).unwrap_or(0),
            "shares_pending": primary_runtime.map(|s| s.shares_pending).unwrap_or(0),
            "shares_unresolved": primary_runtime.map(|s| s.shares_unresolved).unwrap_or(0),
            "oldest_pending_submit_age_ms": primary_runtime.map(|s| s.oldest_pending_submit_age_ms).unwrap_or(0),
            "connected": primary_runtime.map(|s| s.connected).or_else(|| primary_stats.map(|s| s.connected)).unwrap_or(false),
            "difficulty": primary_runtime.map(|s| s.difficulty).filter(|v| *v > 0.0).or_else(|| primary_stats.map(|s| s.difficulty)).unwrap_or(0.0),
            "failover_active": primary_runtime.map(|s| s.failover_active).unwrap_or(false),
            "primary_failback_state": primary_runtime.map(|s| s.primary_failback_state).unwrap_or_default(),
            "primary_failback_detail": primary_runtime.map(|s| s.primary_failback_detail.clone()).unwrap_or_default(),
            "last_primary_reprobe_unix_ms": primary_runtime.map(|s| s.last_primary_reprobe_unix_ms).unwrap_or(0),
            "last_primary_failback_unix_ms": primary_runtime.map(|s| s.last_primary_failback_unix_ms).unwrap_or(0),
            "authorized": primary_runtime.map(|s| s.authorized).unwrap_or(false),
            "response_time_ms": primary_runtime.map(|s| s.last_share_response_ms).unwrap_or(0.0),
            "last_share_submit_unix_ms": primary_runtime.map(|s| s.last_share_submit_unix_ms).unwrap_or(0),
            "last_share_response_unix_ms": primary_runtime.map(|s| s.last_share_response_unix_ms).unwrap_or(0),
            "last_share_accepted_unix_ms": primary_runtime.map(|s| s.last_share_accepted_unix_ms).unwrap_or(0),
            "last_share_rejected_unix_ms": primary_runtime.map(|s| s.last_share_rejected_unix_ms).unwrap_or(0),
            "last_reject_reason": primary_runtime.map(|s| s.last_reject_reason.clone()).unwrap_or_default(),
            "reject_reason_counts": primary_runtime.map(|s| s.reject_reason_counts.clone()).unwrap_or_default(),
            "recent_events": primary_runtime.map(|s| s.recent_events.clone()).unwrap_or_default(),
        }));

        // Secondary pool (if split configured)
        if let Some(ref split) = config.split_pool {
            let secondary_stats = pool_stats.iter().find(|p| p.index == 1);
            let secondary_runtime = stratum_statuses.get(1);
            pools_json.push(serde_json::json!({
                "index": 1,
                // B-ESP-10: sanitize URL + mask worker.
                "url": secondary_runtime
                    .map(|status| format!("{}:{}", crate::shared::sanitize_pool_url(&status.active_url), status.active_port))
                    .unwrap_or_else(|| format!("{}:{}", crate::shared::sanitize_pool_url(&split.pool.url), split.pool.port)),
                "worker": crate::shared::mask_wallet(&split.pool.worker_name),
                "target_pct": secondary_stats.map(|s| s.target_pct).unwrap_or(split.hashrate_pct),
                "actual_pct": secondary_stats.map(|s| s.actual_pct(total_dispatched)).unwrap_or(0.0),
                "dispatched": secondary_stats.map(|s| s.dispatched_count).unwrap_or(0),
                "shares_submitted": secondary_runtime.map(|s| s.shares_submitted).or_else(|| secondary_stats.map(|s| s.shares_submitted)).unwrap_or(0),
                "shares_accepted": secondary_runtime.map(|s| s.shares_accepted).or_else(|| secondary_stats.map(|s| s.shares_accepted)).unwrap_or(0),
                "shares_rejected": secondary_runtime.map(|s| s.shares_rejected).or_else(|| secondary_stats.map(|s| s.shares_rejected)).unwrap_or(0),
                "shares_pending": secondary_runtime.map(|s| s.shares_pending).unwrap_or(0),
                "shares_unresolved": secondary_runtime.map(|s| s.shares_unresolved).unwrap_or(0),
                "oldest_pending_submit_age_ms": secondary_runtime.map(|s| s.oldest_pending_submit_age_ms).unwrap_or(0),
                "connected": secondary_runtime.map(|s| s.connected).or_else(|| secondary_stats.map(|s| s.connected)).unwrap_or(false),
                "difficulty": secondary_runtime.map(|s| s.difficulty).filter(|v| *v > 0.0).or_else(|| secondary_stats.map(|s| s.difficulty)).unwrap_or(0.0),
                "authorized": secondary_runtime.map(|s| s.authorized).unwrap_or(false),
                "response_time_ms": secondary_runtime.map(|s| s.last_share_response_ms).unwrap_or(0.0),
                "last_share_submit_unix_ms": secondary_runtime.map(|s| s.last_share_submit_unix_ms).unwrap_or(0),
                "last_share_response_unix_ms": secondary_runtime.map(|s| s.last_share_response_unix_ms).unwrap_or(0),
                "last_share_accepted_unix_ms": secondary_runtime.map(|s| s.last_share_accepted_unix_ms).unwrap_or(0),
                "last_share_rejected_unix_ms": secondary_runtime.map(|s| s.last_share_rejected_unix_ms).unwrap_or(0),
                "last_reject_reason": secondary_runtime.map(|s| s.last_reject_reason.clone()).unwrap_or_default(),
                "reject_reason_counts": secondary_runtime.map(|s| s.reject_reason_counts.clone()).unwrap_or_default(),
                "recent_events": secondary_runtime.map(|s| s.recent_events.clone()).unwrap_or_default(),
            }));
        }

        let body = serde_json::to_string(&pools_json).unwrap_or_else(|_| "[]".into());
        drop(config);
        drop(pool_stats);

        let mut resp = req.into_response(200, None, &[
            ("Content-Type", "application/json"),
            ("Access-Control-Allow-Origin", "*"),
        ])?;
        resp.write(body.as_bytes())?;
        Ok(())
    }).expect("Failed to register GET /api/pools");
}
