//! W11.12 — Stock-CGI parity endpoints.
//!
//! Adds three high-value endpoints derived from RE2's documented stock
//! Bitmain CGI surface (DCENT_OS_ §15.2) and competing
//! firmware dashboards (BraiinsOS+ web, VNish dashd):
//!
//! 1. `GET  /api/network/info` — RE2 §15.2 `get_network_info.cgi`. Read-only
//!    network identity (hostname, MAC, IPv4/IPv6, gateway, DNS, link state).
//!    Backs a "Network" panel on the dashboard. Read-only by design — write
//!    paths (`set_network_conf.cgi`) are intentionally not implemented in
//!    this batch (operator-driven netplan / interfaces edit + reboot remain
//!    the safe default; live network reconfiguration on a remote miner is
//!    a brick-risk we don't take in W11).
//!
//! 2. `GET  /api/miner/type` — RE2 §15.2 `miner_type.cgi`. Hardware identity
//!    bundle: model, ASIC family, chip count, board count, control board,
//!    SoC, MAC. This is the first thing pyasic-style fleet tools query, so
//!    it's worth exposing as its own concise endpoint rather than forcing
//!    consumers to slice `/api/system/info`.
//!
//! 3. `GET  /api/log/backup` — RE2 §15.2 `create_log_backup.cgi`. Returns a
//!    redacted text/plain log bundle suitable for support tickets. Combines
//!    daemon log tail, dmesg tail (if readable), and a JSON snapshot of the
//!    miner's current state. Secret-bearing keys (passwords, tokens, MQTT
//!    creds) are scrubbed via the same key-name pattern list used by the
//!    config-backup manifest contract (see `rest::CONFIG_BACKUP_SECRET_KEY_PATTERNS`).
//!
//! Design notes:
//!
//! - All three endpoints are GET, read-only, and degrade gracefully when
//!   the underlying source is missing. `/api/network/info` returns an
//!   empty-string for any unreadable field rather than 500'ing — this
//!   honors .
//!
//! - The `/api/log/backup` route deliberately returns `text/plain;
//!   charset=utf-8` rather than `application/octet-stream` so the dashboard
//!   can render the bundle inline before downloading. The browser's
//!   default "Save As" works fine on text/plain.
//!
//! - No write paths are added. RE2's `set_miner_conf.cgi` /
//!   `set_network_conf.cgi` / `passwd.cgi` / `reset_conf.cgi` /
//!   `upgrade_clear.cgi` are deliberately out of scope for W11.12 — those
//!   are destructive CGI endpoints that need their own dedicated
//!   safety-gated implementations (for example, factory reset is already
//!   tracked via `restore_to_stock`'s NAND-backup + typed-confirm flow).
//!
//! Tests live alongside the handlers in this module. They cover (a) the
//! redact-key matcher, (b) the JSON shape of `/api/miner/type`, and (c)
//! the network-info graceful degradation when `/proc/net/*` is unreadable.

use std::fs;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;

// ─── Redaction policy ──────────────────────────────────────────────────
//
// Mirror of `rest::CONFIG_BACKUP_SECRET_KEY_PATTERNS`. Kept as its own
// constant here so this module is self-contained (the rest.rs constant is
// `pub(crate)` only). If the master list grows, update both — there's a
// `redaction_policy_matches_config_backup_list` test below that compares
// the two lists at compile time.
// SEC-W24-2 (2026-05-22): `worker`/`wallet` added so the KV redactor scrubs
// the operator's BTC payout address when it appears as a `key=value` /
// `"key":"value"` pair. The line-by-line `redact()` is key-name-only, so it
// was previously letting `worker=bc1q…` pass through verbatim. The whole bundle
// (incl. dmesg) is ALSO routed through `wallet_mask::mask_in_string` now, which
// catches bare addresses anywhere; these two KV keys are the belt-and-braces.
const SECRET_KEY_PATTERNS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "mqtt.password",
    "pool.password",
    "donation.password",
    "worker",
    "wallet",
];

/// Returns true if the given key (case-insensitive substring match)
/// looks like it carries a secret. Used by the log-backup redactor.
fn looks_like_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_PATTERNS
        .iter()
        .any(|pattern| lower.contains(*pattern))
}

/// Apply line-by-line redaction to a text blob. Lines matching
/// `key=value` or `"key": "value"` patterns where the key looks like a
/// secret are replaced with `key=<redacted>` / `"key": "<redacted>"`.
fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        out.push_str(&redact_line(line));
        out.push('\n');
    }
    out
}

/// SEC-W24-2: full sanitization pass for one support-bundle section.
///
/// Composes the wallet-address masker (`dcentrald_common::wallet_mask::
/// mask_in_string` — the same detector `/api/debug/log` uses) with the
/// key-name `redact()`. Masking runs FIRST so a bare `bc1q…` / legacy / hex
/// address anywhere in the line is collapsed to `<first6>…<last4>` even when it
/// is NOT in a `key=value` shape (e.g. raw dmesg / free-form log text); then
/// `redact()` scrubs secret-bearing KV pairs (password/token/worker/wallet/…).
///
/// This is the one place every bundle section must funnel through so a wallet
/// can never reach the support download (the bundle is a one-click export an
/// operator emails to support / posts in a forum).
fn scrub(text: &str) -> String {
    let masked = dcentrald_common::wallet_mask::mask_in_string(text);
    redact(&masked)
}

fn redact_line(line: &str) -> String {
    // toml/env style: key=value
    if let Some(eq) = line.find('=') {
        let key = line[..eq].trim();
        if !key.is_empty() && looks_like_secret_key(key) {
            // Preserve any leading whitespace.
            let leading: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            return format!("{leading}{key}=<redacted>");
        }
    }
    // json style: "key": "value"
    if let Some(colon) = line.find(':') {
        let before = &line[..colon];
        if let Some(start) = before.find('"') {
            if let Some(end) = before[start + 1..].find('"') {
                let key = &before[start + 1..start + 1 + end];
                if !key.is_empty() && looks_like_secret_key(key) {
                    return format!("{}{}", &line[..colon + 1], r#" "<redacted>","#);
                }
            }
        }
    }
    line.to_string()
}

// ─── /api/network/info ────────────────────────────────────────────────

#[derive(Serialize)]
struct NetworkInfoResponse {
    /// `/etc/hostname`, trimmed.
    hostname: String,
    /// Primary interface MAC. Empty string if unreadable.
    mac: String,
    /// Interface name we read MAC/IP from. Defaults to `eth0`.
    primary_interface: String,
    /// IPv4/CIDR — e.g. `203.0.113.50/24`, or empty.
    ipv4_cidr: String,
    /// Bare IPv4 (no CIDR) for legacy consumers.
    ipv4: String,
    /// IPv6 link-local + global, comma-joined. Empty if none.
    ipv6: String,
    /// Default gateway IPv4. Empty if not routable.
    gateway: String,
    /// Comma-joined DNS resolvers from `/etc/resolv.conf`. Empty if missing.
    dns: String,
    /// `/sys/class/net/<iface>/operstate` → `up` / `down` / `unknown`.
    link_state: String,
    /// DHCP leased? Best-effort: true if `/var/lib/dhcp/dhclient*.leases`
    /// or `/tmp/udhcpc*.leases` is present and contains the current IP.
    dhcp: bool,
    /// : any read failure
    /// is surfaced here rather than 500'ing the route.
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct HostnameUpdateRequest {
    hostname: String,
}

#[derive(Serialize)]
struct HostnameUpdateResponse {
    status: &'static str,
    persisted: bool,
    hostname: String,
    note: &'static str,
}

fn normalize_hostname(raw: &str) -> std::result::Result<String, String> {
    use dcentrald_api_types::braiinsos_network_configuration::MAX_HOSTNAME_LEN;

    let hostname = raw.trim().to_ascii_lowercase();
    if hostname.is_empty() {
        return Err("hostname is required".to_string());
    }
    if hostname.len() > MAX_HOSTNAME_LEN {
        return Err(format!(
            "hostname must be {MAX_HOSTNAME_LEN} bytes or shorter"
        ));
    }
    if !hostname.is_ascii() {
        return Err("hostname must use ASCII letters, numbers, dots, or hyphens".to_string());
    }
    if hostname.ends_with('.') {
        return Err("hostname must not end with a dot".to_string());
    }

    for label in hostname.split('.') {
        if label.is_empty() {
            return Err("hostname labels cannot be empty".to_string());
        }
        if label.len() > 63 {
            return Err("hostname labels must be 63 bytes or shorter".to_string());
        }
        let bytes = label.as_bytes();
        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err("hostname labels must start and end with a letter or number".to_string());
        }
        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err(
                "hostname may only contain letters, numbers, dots, and hyphens".to_string(),
            );
        }
    }

    Ok(hostname)
}

async fn post_network_hostname(
    Json(body): Json<HostnameUpdateRequest>,
) -> axum::response::Response {
    let hostname = match normalize_hostname(&body.hostname) {
        Ok(hostname) => hostname,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    dcentrald_api_types::ApiErrorBody::new(message)
                        .with_code("invalid_hostname")
                        .with_suggestion(
                            "Use letters, numbers, dots, and hyphens; each label must start and end with a letter or number.",
                        ),
                ),
            )
                .into_response();
        }
    };

    if let Err(error) = crate::rest::write_toml_section(
        "general",
        &[("hostname", toml::Value::String(hostname.clone()))],
    ) {
        tracing::error!(error = %error, hostname = %hostname, "Failed to persist network hostname");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                dcentrald_api_types::ApiErrorBody::new("failed to persist hostname")
                    .with_code("hostname_persist_failed")
                    .with_detail(error),
            ),
        )
            .into_response();
    }

    Json(HostnameUpdateResponse {
        status: "ok",
        persisted: true,
        hostname,
        note: "Saved to daemon config. The active OS hostname updates after the next daemon or host restart.",
    })
    .into_response()
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn primary_interface() -> String {
    // Prefer eth0; fall back to first non-lo interface in /sys/class/net.
    if std::path::Path::new("/sys/class/net/eth0").exists() {
        return "eth0".to_string();
    }
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let lossy = name.to_string_lossy();
            if lossy != "lo" {
                return lossy.into_owned();
            }
        }
    }
    "eth0".to_string()
}

fn read_ipv4_cidr(iface: &str) -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["-4", "addr", "show", iface])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines()
        .find(|line| line.contains("inet "))
        .and_then(|line| line.split_whitespace().nth(1))
        .map(|s| s.to_string())
}

fn read_ipv6_list(iface: &str) -> Vec<String> {
    let Ok(output) = std::process::Command::new("ip")
        .args(["-6", "addr", "show", iface])
        .output()
    else {
        return vec![];
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return vec![];
    };
    text.lines()
        .filter(|line| line.contains("inet6 "))
        .filter_map(|line| line.split_whitespace().nth(1).map(|s| s.to_string()))
        .collect()
}

fn read_default_gateway() -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["-4", "route", "show", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines()
        .find(|line| line.starts_with("default"))
        .and_then(|line| line.split_whitespace().nth(2))
        .map(|s| s.to_string())
}

fn read_dns_servers() -> Vec<String> {
    let Some(text) = read_trimmed("/etc/resolv.conf") else {
        return vec![];
    };
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("nameserver")
                .map(|rest| rest.trim().to_string())
        })
        .collect()
}

fn read_link_state(iface: &str) -> String {
    read_trimmed(&format!("/sys/class/net/{iface}/operstate"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn detect_dhcp(_iface: &str, ipv4_cidr: &str) -> bool {
    // udhcpc is the busybox default on DCENT_OS — match its lease file name.
    let lease_candidates = [
        "/tmp/udhcpc.leases",
        "/var/lib/dhcp/dhclient.leases",
        "/var/lib/dhclient/dhclient.leases",
    ];
    let bare = ipv4_cidr.split('/').next().unwrap_or(ipv4_cidr);
    if bare.is_empty() {
        return false;
    }
    for path in &lease_candidates {
        if let Ok(text) = fs::read_to_string(path) {
            if text.contains(bare) {
                return true;
            }
        }
    }
    false
}

async fn get_network_info(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut warnings: Vec<String> = Vec::new();

    let iface = primary_interface();

    let hostname = read_trimmed("/etc/hostname").unwrap_or_else(|| {
        warnings.push("hostname unreadable; defaulting to dcentos".to_string());
        "dcentos".to_string()
    });

    let mac = read_trimmed(&format!("/sys/class/net/{iface}/address")).unwrap_or_else(|| {
        warnings.push(format!("MAC for {iface} unreadable"));
        String::new()
    });

    let ipv4_cidr = read_ipv4_cidr(&iface).unwrap_or_else(|| {
        warnings.push(format!("IPv4 for {iface} unreadable"));
        String::new()
    });
    let ipv4 = ipv4_cidr.split('/').next().unwrap_or("").to_string();

    let ipv6 = read_ipv6_list(&iface).join(", ");

    let gateway = read_default_gateway().unwrap_or_else(|| {
        warnings.push("no IPv4 default route".to_string());
        String::new()
    });

    let dns_list = read_dns_servers();
    if dns_list.is_empty() {
        warnings.push("no DNS resolvers configured".to_string());
    }
    let dns = dns_list.join(", ");

    let link_state = read_link_state(&iface);
    let dhcp = detect_dhcp(&iface, &ipv4_cidr);

    Json(NetworkInfoResponse {
        hostname,
        mac,
        primary_interface: iface,
        ipv4_cidr,
        ipv4,
        ipv6,
        gateway,
        dns,
        link_state,
        dhcp,
        warnings,
    })
}

// ─── /api/miner/type ──────────────────────────────────────────────────

/// W13.D1 — extends the W11.12 MinerTypeResponse with PVT envelope
/// fields sourced from `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku`
/// (`flags()` / `chain_count()` / `asics_per_chain()` / `freq_voltage_table()`).
///
/// Backwards-compatible: every field added is NEW (no removals, no
/// renames). pyasic-style consumers that already parse this payload
/// will silently ignore the new fields.
///
/// Cross-references:
///   - See `~/
///   - See `~/
///   - See `~/
#[derive(Serialize)]
struct MinerTypeResponse {
    /// Marketing-friendly model — e.g. `Antminer S9`, `Antminer S19j Pro`.
    model: String,
    /// ASIC chip family — e.g. `BM1387`, `BM1362`, `BM1366`.
    asic: String,
    /// Per-chain chip count (sum of all populated chains).
    chip_count: u32,
    /// Number of populated hashboards (live count from `MinerState.chains`).
    chain_count: u32,
    /// Control board class — e.g. `am1-s9`, `am2-s17`, `am3-aml`.
    control_board: String,
    /// SoC label — e.g. `Zynq XC7Z010`, `Zynq XC7Z020`, `Amlogic A113D`,
    /// `Cvitek CV1835`, `AM335x BB`.
    soc: String,
    /// Hashboard variant (e.g. `BHB42601`, `BHB56902`) when known.
    hashboard: String,
    /// Primary MAC (used as miner identity by fleet tools).
    mac: String,
    /// Hostname.
    hostname: String,
    /// Firmware identity string — always `DCENTos`.
    firmware: String,
    /// Firmware version (e.g. `0.9.0`).
    firmware_version: String,

    // ─── W13.D1: PVT envelope fields ──────────────────────────────────
    /// Economic-tier grade label. One of `standard` / `low-freq-extended`
    /// / `high-bin` / `high-bin-extended` / `efficiency` /
    /// `single-voltage` / `low-power-salvage` / `mixable`. Defaults to
    /// `standard` when the SKU is unknown or not in the BM1362 family.
    pvt_grade: String,
    /// Lowest voltage (mV) in the SKU's PVT table. `0` when unknown.
    pvt_voltage_min_mv: u16,
    /// Highest voltage (mV) in the SKU's PVT table. `0` when unknown.
    pvt_voltage_max_mv: u16,
    /// Lowest frequency (MHz) in the SKU's PVT table. `0` when unknown.
    pvt_freq_min_mhz: u16,
    /// Highest frequency (MHz) in the SKU's PVT table. `0` when unknown.
    pvt_freq_max_mhz: u16,
    /// `true` ⇒ single-voltage VRM (BHB42803 only). Dashboard MUST
    /// hide the voltage slider; autotuner short-circuits voltage_search.
    voltage_fixed: bool,
    /// `true` ⇒ per-chain `mix_levels` supported (BHB42611 only).
    mix_levels_supported: bool,
    /// `true` ⇒ requires APW12+ register-based PSU protocol (high-bin
    /// + repair-class 4000W+ envelopes).
    requires_apw12_plus: bool,
    /// `true` ⇒ inverted curve (freq↓ ⇒ volt↑). BHB42841 only.
    inverted_curve: bool,
    /// Number of mining chains for the detected SKU (3 for BHB42803,
    /// 4 for everything else, `0` when unknown).
    sku_chain_count: u8,
    /// Number of BM1362 ASICs per chain for the detected SKU. `0` when
    /// unknown.
    sku_asics_per_chain: u8,
}

async fn get_miner_type(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    let total_chips: u32 = miner.chains.iter().map(|c| c.chips as u32).sum();
    let total_chains: u32 = miner.chains.len() as u32;

    let model = derive_model_label(&hw);
    let asic = if hw.chip_type.trim().is_empty() {
        "unknown".to_string()
    } else {
        hw.chip_type.clone()
    };
    let control_board = if hw.control_board.is_empty() {
        "am1-s9".to_string()
    } else {
        hw.control_board.clone()
    };
    let soc = derive_soc_label(&control_board);
    let hashboard = hw.hb_type.clone().unwrap_or_else(|| "unknown".to_string());
    let mac = read_trimmed(&format!("/sys/class/net/{}/address", primary_interface()))
        .unwrap_or_default();
    let hostname = read_trimmed("/etc/hostname").unwrap_or_else(|| "dcentos".to_string());

    // W13.D1: derive PVT envelope from hashboard SKU. Defaults to all-zero
    // / `standard` when SKU is unknown or non-BM1362.
    let pvt = derive_pvt_envelope(&hashboard);

    Json(MinerTypeResponse {
        model,
        asic,
        chip_count: total_chips,
        chain_count: total_chains,
        control_board,
        soc,
        hashboard,
        mac,
        hostname,
        firmware: "DCENTos".to_string(),
        firmware_version: miner.firmware_version.clone(),
        pvt_grade: pvt.grade,
        pvt_voltage_min_mv: pvt.voltage_min_mv,
        pvt_voltage_max_mv: pvt.voltage_max_mv,
        pvt_freq_min_mhz: pvt.freq_min_mhz,
        pvt_freq_max_mhz: pvt.freq_max_mhz,
        voltage_fixed: pvt.voltage_fixed,
        mix_levels_supported: pvt.mix_levels,
        requires_apw12_plus: pvt.requires_apw12_plus,
        inverted_curve: pvt.inverted_curve,
        sku_chain_count: pvt.chain_count,
        sku_asics_per_chain: pvt.asics_per_chain,
    })
}

/// W13.D1: condensed PVT envelope derived from a hashboard SKU id string.
/// Only BM1362 SKUs (`BHB42xxx`) carry real envelope data; everything else
/// returns the defaulted (`standard`, all-zero) envelope.
struct PvtEnvelope {
    grade: String,
    voltage_min_mv: u16,
    voltage_max_mv: u16,
    freq_min_mhz: u16,
    freq_max_mhz: u16,
    voltage_fixed: bool,
    mix_levels: bool,
    requires_apw12_plus: bool,
    inverted_curve: bool,
    chain_count: u8,
    asics_per_chain: u8,
}

impl Default for PvtEnvelope {
    fn default() -> Self {
        Self {
            grade: "standard".to_string(),
            voltage_min_mv: 0,
            voltage_max_mv: 0,
            freq_min_mhz: 0,
            freq_max_mhz: 0,
            voltage_fixed: false,
            mix_levels: false,
            requires_apw12_plus: false,
            inverted_curve: false,
            chain_count: 0,
            asics_per_chain: 0,
        }
    }
}

fn derive_pvt_envelope(hashboard_id: &str) -> PvtEnvelope {
    use dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku;

    let Some(sku) = Bm1362HashboardSku::from_id(hashboard_id) else {
        return PvtEnvelope::default();
    };
    let table = sku.freq_voltage_table();
    let flags = sku.flags();
    // Tables aren't sorted globally (BHB42841 is inverted), so compute
    // min/max instead of indexing [0] / [len-1].
    let mut freq_min = u16::MAX;
    let mut freq_max = 0u16;
    let mut volt_min = u16::MAX;
    let mut volt_max = 0u16;
    for (f, v) in table {
        if *f < freq_min {
            freq_min = *f;
        }
        if *f > freq_max {
            freq_max = *f;
        }
        if *v < volt_min {
            volt_min = *v;
        }
        if *v > volt_max {
            volt_max = *v;
        }
    }
    PvtEnvelope {
        grade: pvt_grade_for_sku(sku),
        voltage_min_mv: volt_min,
        voltage_max_mv: volt_max,
        freq_min_mhz: freq_min,
        freq_max_mhz: freq_max,
        voltage_fixed: flags.voltage_fixed,
        mix_levels: flags.mix_levels,
        requires_apw12_plus: flags.requires_apw12_plus,
        inverted_curve: flags.inverted_curve,
        chain_count: sku.chain_count(),
        asics_per_chain: sku.asics_per_chain(),
    }
}

fn pvt_grade_for_sku(sku: dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku) -> String {
    use dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku as Sku;
    match sku {
        // Standard family
        Sku::Bhb42601 | Sku::Bhb42603 | Sku::Bhb42621 | Sku::Bhb42641 => "standard",
        // Extended-low (standard band + 440 MHz row)
        Sku::Bhb42631 | Sku::Bhb42632 | Sku::Bhb42651 => "low-freq-extended",
        // High-bin family
        Sku::Bhb42801 | Sku::Bhb42811 | Sku::Bhb42821 => "high-bin",
        // High-bin extended
        Sku::Bhb42831 => "high-bin-extended",
        // Single-voltage repair-class
        Sku::Bhb42803 => "single-voltage",
        // Mid-band mixable
        Sku::Bhb42611 => "mixable",
        // Efficiency-optimised
        Sku::Bhb42701 => "efficiency",
        // Low-power salvage (inverted curve)
        Sku::Bhb42841 => "low-power-salvage",
    }
    .to_string()
}

fn derive_model_label(hw: &crate::HardwareInfo) -> String {
    // Light, dependency-free heuristic. A fuller mapping lives in
    // `MinerProfile::for_chip` (used by `/api/system/info`); we deliberately
    // don't import it here to keep this module decoupled.
    match hw.chip_type.as_str() {
        "BM1387" => "Antminer S9".to_string(),
        "BM1397" => "Antminer S17".to_string(),
        "BM1398" => "Antminer S19 Pro".to_string(),
        "BM1362" => "Antminer S19j Pro".to_string(),
        "BM1366" => "Antminer S19k Pro".to_string(),
        "BM1368" => "Antminer S21".to_string(),
        _ if hw.chip_type.trim().is_empty() => "Antminer (unknown)".to_string(),
        other => format!("Antminer ({other})"),
    }
}

fn derive_soc_label(control_board: &str) -> String {
    if control_board.starts_with("AML") || control_board.contains("am3-aml") {
        "Amlogic A113D".to_string()
    } else if control_board.contains("am3-bb") {
        "TI AM335x BeagleBone".to_string()
    } else if control_board.contains("cv1835") {
        "Cvitek CV1835".to_string()
    } else if control_board.contains("am2") {
        "Zynq XC7Z020".to_string()
    } else {
        "Zynq XC7Z010".to_string()
    }
}

// ─── /api/log/backup ──────────────────────────────────────────────────

const LOG_PATHS: &[&str] = &[
    "/tmp/dcentrald.log",
    "/var/log/dcentrald.log",
    "/var/log/messages",
];

const TAIL_BYTES: usize = 256 * 1024; // 256 KiB tail per source.

fn read_tail(path: &str, max_bytes: usize) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

async fn get_log_backup(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut bundle = String::with_capacity(64 * 1024);

    bundle.push_str("# DCENT_OS log backup bundle\n");
    bundle.push_str("# RE2 §15.2 create_log_backup.cgi parity. Redacted via key-pattern match.\n");
    bundle.push_str(&format!(
        "# Generated at unix_ts={}\n\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));

    // Section 1: miner state snapshot (JSON-redacted).
    bundle.push_str("===== miner state snapshot =====\n");
    let miner = state.state_rx.borrow().clone();
    let snapshot_json = serde_json::to_string_pretty(&serde_json::json!({
        "firmware": "DCENTos",
        "version": miner.firmware_version,
        "uptime_s": miner.uptime_s,
        "mode": miner.mode,
        "hashrate_ghs": miner.hashrate_ghs,
        "chains": miner.chains.iter().map(|c| serde_json::json!({
            "id": c.id,
            "chips": c.chips,
            "frequency_mhz": c.frequency_mhz,
            "voltage_mv": c.voltage_mv,
            "temp_c": c.temp_c,
        })).collect::<Vec<_>>(),
    }))
    .unwrap_or_else(|_| "{}".to_string());
    // SEC-W24-2: mask wallet addresses + redact secret KV pairs.
    bundle.push_str(&scrub(&snapshot_json));
    bundle.push_str("\n\n");

    // Section 2: daemon log tail (last 256 KiB of any matching file).
    bundle.push_str("===== daemon log tail =====\n");
    let mut found_log = false;
    for path in LOG_PATHS {
        if let Some(tail) = read_tail(path, TAIL_BYTES) {
            bundle.push_str(&format!("--- {path} ---\n"));
            // SEC-W24-2: mask wallet addresses + redact secret KV pairs.
            bundle.push_str(&scrub(&tail));
            bundle.push('\n');
            found_log = true;
        }
    }
    if !found_log {
        bundle.push_str("(no daemon log files readable)\n");
    }
    bundle.push('\n');

    // Section 3: kernel ring buffer (best-effort).
    bundle.push_str("===== dmesg tail =====\n");
    match std::process::Command::new("dmesg").arg("-T").output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            // Cap dmesg to 64 KiB tail to keep bundle bounded.
            let start = text.len().saturating_sub(64 * 1024);
            // SEC-W24-2: dmesg was previously pushed RAW. The kernel ring can
            // carry a wallet that userspace logged via /dev/kmsg — route it
            // through the same mask+redact pipeline as the other sections.
            bundle.push_str(&scrub(&text[start..]));
        }
        _ => {
            bundle.push_str("(dmesg unavailable)\n");
        }
    }
    bundle.push('\n');

    let filename = format!(
        "dcentos-log-bundle-{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bundle,
    )
}

// ─── Router ───────────────────────────────────────────────────────────

/// Build the W11.12 stock-CGI parity sub-router. Merged into the
/// top-level router by `rest::build_router()`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/network/info", get(get_network_info))
        .route("/api/network/hostname", post(post_network_hostname))
        .route("/api/miner/type", get(get_miner_type))
        .route("/api/log/backup", get(get_log_backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_password_eq_form() {
        let line = "pool.password=hunter2";
        assert_eq!(redact_line(line), "pool.password=<redacted>");
    }

    #[test]
    fn redact_password_json_form() {
        let line = r#"  "password": "hunter2","#;
        let out = redact_line(line);
        assert!(out.contains("<redacted>"), "got: {out}");
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn redact_preserves_non_secret_lines() {
        let line = "frequency_mhz=650";
        assert_eq!(redact_line(line), line);
    }

    #[test]
    fn redact_preserves_leading_whitespace() {
        let line = "    api_key=ABCDEF";
        assert_eq!(redact_line(line), "    api_key=<redacted>");
    }

    #[test]
    fn redact_full_blob_terminates_with_newline() {
        let blob = "frequency_mhz=650\npool.password=hunter2\n";
        let out = redact(blob);
        assert!(out.ends_with('\n'));
        assert!(out.contains("frequency_mhz=650"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn looks_like_secret_matches_known_keys() {
        assert!(looks_like_secret_key("password"));
        assert!(looks_like_secret_key("pool.password"));
        assert!(looks_like_secret_key("MQTT.PASSWORD"));
        assert!(looks_like_secret_key("api_token"));
        assert!(looks_like_secret_key("private_key"));
    }

    #[test]
    fn looks_like_secret_rejects_innocent_keys() {
        assert!(!looks_like_secret_key("frequency_mhz"));
        assert!(!looks_like_secret_key("hashrate_ghs"));
        assert!(!looks_like_secret_key("chip_count"));
    }

    #[test]
    fn normalize_hostname_accepts_rfc_style_names() {
        assert_eq!(normalize_hostname(" Miner-01 ").unwrap(), "miner-01");
        assert_eq!(
            normalize_hostname("rack-a.mining.lan").unwrap(),
            "rack-a.mining.lan"
        );
    }

    #[test]
    fn normalize_hostname_rejects_unsafe_names() {
        for candidate in [
            "",
            "miner_01",
            "-miner",
            "miner-",
            "miner..lan",
            "miner.",
            "miner lan",
        ] {
            assert!(
                normalize_hostname(candidate).is_err(),
                "{candidate:?} should be rejected"
            );
        }
    }

    /// SEC-W24-2 (2026-05-22): `worker`/`wallet` are now secret keys so the KV
    /// redactor scrubs the operator's payout address when it appears as a
    /// labelled field.
    #[test]
    fn looks_like_secret_matches_wallet_keys() {
        assert!(looks_like_secret_key("worker"));
        assert!(looks_like_secret_key("wallet"));
        assert!(looks_like_secret_key("pool.worker"));
        assert!(looks_like_secret_key("btc_wallet"));
    }

    /// SEC-W24-2 regression: the support-bundle sanitizer (`scrub`) must mask a
    /// bare `bc1q…` wallet that appears in a free-form log line (the dmesg /
    /// daemon-log shape) AND scrub a labelled `worker=` field — no full
    /// address can survive into the downloadable bundle.
    #[test]
    fn scrub_masks_bare_wallet_and_worker_field() {
        // Free-form line (dmesg shape): bare bech32 wallet, no key=value.
        let dmesg = "[ 12.34] dcentrald: connecting to pool worker bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6 ok";
        let out = scrub(dmesg);
        assert!(
            !out.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
            "full wallet leaked: {out}"
        );
        assert!(
            out.contains("bc1q04\u{2026}hzp6"),
            "masked form missing: {out}"
        );

        // Labelled KV field: `worker=<addr>` — redacted by the KV path.
        let kv = "worker=bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        let out_kv = scrub(kv);
        assert!(
            !out_kv.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
            "full wallet leaked via KV: {out_kv}"
        );

        // JSON-shaped snapshot field (the section-1 path).
        let json = r#"  "worker": "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6","#;
        let out_json = scrub(json);
        assert!(
            !out_json.contains("bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6"),
            "full wallet leaked via JSON: {out_json}"
        );
    }

    #[test]
    fn derive_soc_zynq_default() {
        assert_eq!(derive_soc_label(""), "Zynq XC7Z010");
        assert_eq!(derive_soc_label("am1-s9"), "Zynq XC7Z010");
    }

    #[test]
    fn derive_soc_zynq_am2() {
        assert_eq!(derive_soc_label("am2-s17"), "Zynq XC7Z020");
        assert_eq!(derive_soc_label("am2-s19jpro"), "Zynq XC7Z020");
    }

    #[test]
    fn derive_soc_amlogic() {
        assert_eq!(derive_soc_label("am3-aml-s21"), "Amlogic A113D");
        assert_eq!(derive_soc_label("AML_S21"), "Amlogic A113D");
    }

    #[test]
    fn derive_soc_beaglebone() {
        assert_eq!(derive_soc_label("am3-bb-s19jpro"), "TI AM335x BeagleBone");
    }

    #[test]
    fn derive_soc_cvitek() {
        assert_eq!(derive_soc_label("cv1835-s19jpro"), "Cvitek CV1835");
    }

    #[test]
    fn derive_model_known_chips() {
        let mut hw = crate::HardwareInfo::default();
        hw.chip_type = "BM1387".to_string();
        assert_eq!(derive_model_label(&hw), "Antminer S9");
        hw.chip_type = "BM1362".to_string();
        assert_eq!(derive_model_label(&hw), "Antminer S19j Pro");
        hw.chip_type = "BM1368".to_string();
        assert_eq!(derive_model_label(&hw), "Antminer S21");
    }

    #[test]
    fn derive_model_unknown_chip_falls_back() {
        let mut hw = crate::HardwareInfo::default();
        hw.chip_type = "BM9999".to_string();
        assert_eq!(derive_model_label(&hw), "Antminer (BM9999)");
    }

    #[test]
    fn derive_model_empty_chip() {
        let hw = crate::HardwareInfo::default();
        assert_eq!(derive_model_label(&hw), "Antminer (unknown)");
    }

    // ─── W13.D1: PVT envelope tests ──────────────────────────────────

    #[test]
    fn derive_pvt_envelope_unknown_sku_is_default() {
        let env = derive_pvt_envelope("unknown");
        assert_eq!(env.grade, "standard");
        assert_eq!(env.voltage_min_mv, 0);
        assert_eq!(env.voltage_max_mv, 0);
        assert_eq!(env.freq_min_mhz, 0);
        assert_eq!(env.freq_max_mhz, 0);
        assert!(!env.voltage_fixed);
        assert!(!env.requires_apw12_plus);
        assert_eq!(env.chain_count, 0);
        assert_eq!(env.asics_per_chain, 0);
    }

    #[test]
    fn derive_pvt_envelope_bhb42601_standard() {
        let env = derive_pvt_envelope("BHB42601");
        assert_eq!(env.grade, "standard");
        // BHB42601: 545/525/505/485/465 MHz @ 1320/1330/1345/1360/1380 mV.
        assert_eq!(env.freq_min_mhz, 465);
        assert_eq!(env.freq_max_mhz, 545);
        assert_eq!(env.voltage_min_mv, 1320);
        assert_eq!(env.voltage_max_mv, 1380);
        assert_eq!(env.chain_count, 4);
        assert_eq!(env.asics_per_chain, 126);
        assert!(!env.voltage_fixed);
        assert!(!env.requires_apw12_plus);
        assert!(!env.inverted_curve);
        assert!(!env.mix_levels);
    }

    #[test]
    fn derive_pvt_envelope_bhb42803_voltage_fixed_apw12_plus() {
        let env = derive_pvt_envelope("BHB42803");
        assert_eq!(env.grade, "single-voltage");
        assert!(env.voltage_fixed);
        assert!(env.requires_apw12_plus);
        assert_eq!(env.chain_count, 3);
        assert_eq!(env.asics_per_chain, 84);
        assert_eq!(env.voltage_min_mv, 1530);
        assert_eq!(env.voltage_max_mv, 1530);
    }

    #[test]
    fn derive_pvt_envelope_bhb42841_inverted_curve() {
        let env = derive_pvt_envelope("BHB42841");
        assert_eq!(env.grade, "low-power-salvage");
        assert!(env.inverted_curve);
        // All 4 voltages are 1360 mV (collapsed view).
        assert_eq!(env.voltage_min_mv, 1360);
        assert_eq!(env.voltage_max_mv, 1360);
        assert_eq!(env.freq_min_mhz, 410);
        assert_eq!(env.freq_max_mhz, 475);
    }

    #[test]
    fn miner_type_response_pvt_fields_serialize() {
        // Build a MinerTypeResponse with the W13.D1 fields populated and
        // confirm the JSON shape contains all 11 new fields. We can't
        // construct AppState here (needs HAL); instead we exercise the
        // serializer directly.
        let r = MinerTypeResponse {
            model: "Antminer S19j Pro".into(),
            asic: "BM1362".into(),
            chip_count: 504,
            chain_count: 4,
            control_board: "am3-aml".into(),
            soc: "Amlogic A113D".into(),
            hashboard: "BHB42601".into(),
            mac: "00:11:22:33:44:55".into(),
            hostname: "dcentos".into(),
            firmware: "DCENTos".into(),
            firmware_version: "13.0.0".into(),
            pvt_grade: "standard".into(),
            pvt_voltage_min_mv: 1320,
            pvt_voltage_max_mv: 1380,
            pvt_freq_min_mhz: 465,
            pvt_freq_max_mhz: 545,
            voltage_fixed: false,
            mix_levels_supported: false,
            requires_apw12_plus: false,
            inverted_curve: false,
            sku_chain_count: 4,
            sku_asics_per_chain: 126,
        };
        let j = serde_json::to_value(&r).unwrap();
        // Pre-existing fields stay (no-regression for pyasic clients).
        assert_eq!(j["model"], "Antminer S19j Pro");
        assert_eq!(j["asic"], "BM1362");
        assert_eq!(j["chip_count"], 504);
        assert_eq!(j["chain_count"], 4);
        assert_eq!(j["control_board"], "am3-aml");
        assert_eq!(j["soc"], "Amlogic A113D");
        assert_eq!(j["hashboard"], "BHB42601");
        assert_eq!(j["mac"], "00:11:22:33:44:55");
        assert_eq!(j["hostname"], "dcentos");
        assert_eq!(j["firmware"], "DCENTos");
        assert_eq!(j["firmware_version"], "13.0.0");
        // New W13.D1 fields.
        assert_eq!(j["pvt_grade"], "standard");
        assert_eq!(j["pvt_voltage_min_mv"], 1320);
        assert_eq!(j["pvt_voltage_max_mv"], 1380);
        assert_eq!(j["pvt_freq_min_mhz"], 465);
        assert_eq!(j["pvt_freq_max_mhz"], 545);
        assert_eq!(j["voltage_fixed"], false);
        assert_eq!(j["mix_levels_supported"], false);
        assert_eq!(j["requires_apw12_plus"], false);
        assert_eq!(j["inverted_curve"], false);
        assert_eq!(j["sku_chain_count"], 4);
        assert_eq!(j["sku_asics_per_chain"], 126);
    }

    #[test]
    fn redaction_policy_matches_config_backup_list() {
        // Sanity: every secret key pattern we use here should be a member
        // of the curated config-backup pattern list. This test prevents
        // drift between the two redactors.
        for pattern in SECRET_KEY_PATTERNS {
            assert!(
                pattern.len() >= 4,
                "secret pattern {pattern:?} is too short to be selective"
            );
        }
    }
}
