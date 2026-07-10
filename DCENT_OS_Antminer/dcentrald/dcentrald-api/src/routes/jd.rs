//! W5.2 — SV2 Job Declaration REST endpoints.
//!
//! Configures and monitors the SV2 Job Declaration supervisor: status,
//! config persistence (TOML), and a connection-test handler that probes
//! `bitcoind` JSON-RPC + Template Provider/Job Declarator TCP endpoints.
//!
//! Routes:
//!   - `GET  /api/jd/status`           — supervisor status + redacted config
//!   - `POST /api/jd/config`           — persist Job Declaration config
//!   - `POST /api/jd/test-connection`  — probe bitcoind RPC + SV2 endpoints
//!
//! `read_jd_config_request` is also consumed by the mining-work-posture
//! handler in `rest.rs`, so it is exposed as `pub(crate)` rather than
//! private.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use crate::atomic_io::atomic_write;
use crate::AppState;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct JdConfigRequest {
    pub(crate) enabled: bool,
    pub(crate) mode: String,
    pub(crate) bitcoind_rpc_url: String,
    pub(crate) bitcoind_rpc_user: String,
    pub(crate) bitcoind_rpc_password: String,
    pub(crate) bitcoind_rpc_cookie: String,
    pub(crate) template_provider_url: String,
    pub(crate) job_declarator_url: String,
    pub(crate) coinbase_output_address: String,
    pub(crate) template_refresh_interval_s: u32,
    pub(crate) fallback_to_pool_templates: bool,
    pub(crate) declare_tx_data: bool,
    pub(crate) coinbase_output_max_additional_size: u32,
    pub(crate) coinbase_output_max_additional_sigops: u16,
}

impl Default for JdConfigRequest {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "coinbase_only".to_string(),
            bitcoind_rpc_url: "http://127.0.0.1:8332".to_string(),
            bitcoind_rpc_user: String::new(),
            bitcoind_rpc_password: String::new(),
            bitcoind_rpc_cookie: String::new(),
            template_provider_url: "sv2+tcp://127.0.0.1:8442".to_string(),
            job_declarator_url: String::new(),
            coinbase_output_address: String::new(),
            template_refresh_interval_s: 30,
            fallback_to_pool_templates: true,
            declare_tx_data: false,
            coinbase_output_max_additional_size: 512,
            coinbase_output_max_additional_sigops: 0,
        }
    }
}

impl JdConfigRequest {
    fn normalize(mut self) -> std::result::Result<Self, String> {
        self.mode = normalize_jd_mode(&self.mode)?;
        self.bitcoind_rpc_url = self.bitcoind_rpc_url.trim().to_string();
        self.bitcoind_rpc_user = self.bitcoind_rpc_user.trim().to_string();
        self.bitcoind_rpc_cookie = self.bitcoind_rpc_cookie.trim().to_string();
        self.template_provider_url = self.template_provider_url.trim().to_string();
        self.job_declarator_url = self.job_declarator_url.trim().to_string();
        self.coinbase_output_address = self.coinbase_output_address.trim().to_string();
        self.template_refresh_interval_s = self.template_refresh_interval_s.clamp(5, 300);
        self.declare_tx_data = self.declare_tx_data || self.mode == "full_template";
        if self.mode == "coinbase_only" {
            self.declare_tx_data = false;
        }

        if self.enabled {
            if self.template_provider_url.is_empty() {
                return Err(
                    "template_provider_url is required when Job Declaration is enabled".to_string(),
                );
            }
            if self.job_declarator_url.is_empty() {
                return Err(
                    "job_declarator_url is required when Job Declaration is enabled".to_string(),
                );
            }
        }

        Ok(self)
    }

    pub(crate) fn from_table(table: &toml::Table) -> Self {
        let mut config = Self::default();
        if let Some(jd) = table
            .get("job_declaration")
            .and_then(|value| value.as_table())
        {
            config.enabled = jd
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(config.enabled);
            config.mode = jd
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or(&config.mode)
                .to_string();
            config.bitcoind_rpc_url = jd
                .get("bitcoind_rpc_url")
                .and_then(|v| v.as_str())
                .unwrap_or(&config.bitcoind_rpc_url)
                .to_string();
            config.bitcoind_rpc_user = jd
                .get("bitcoind_rpc_user")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            config.bitcoind_rpc_password = jd
                .get("bitcoind_rpc_password")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            config.bitcoind_rpc_cookie = jd
                .get("bitcoind_rpc_cookie")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            config.template_provider_url = jd
                .get("template_provider_url")
                .and_then(|v| v.as_str())
                .unwrap_or(&config.template_provider_url)
                .to_string();
            config.job_declarator_url = jd
                .get("job_declarator_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            config.coinbase_output_address = jd
                .get("coinbase_output_address")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            config.template_refresh_interval_s = jd
                .get("template_refresh_interval_s")
                .and_then(|v| v.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(config.template_refresh_interval_s);
            config.fallback_to_pool_templates = jd
                .get("fallback_to_pool_templates")
                .and_then(|v| v.as_bool())
                .unwrap_or(config.fallback_to_pool_templates);
            config.declare_tx_data = jd
                .get("declare_tx_data")
                .and_then(|v| v.as_bool())
                .unwrap_or(config.declare_tx_data);
            config.coinbase_output_max_additional_size = jd
                .get("coinbase_output_max_additional_size")
                .and_then(|v| v.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(config.coinbase_output_max_additional_size);
            config.coinbase_output_max_additional_sigops = jd
                .get("coinbase_output_max_additional_sigops")
                .and_then(|v| v.as_integer())
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(config.coinbase_output_max_additional_sigops);
        }
        config.normalize().unwrap_or_default()
    }

    pub(crate) fn redacted_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "configured": !self.template_provider_url.is_empty() && !self.job_declarator_url.is_empty(),
            "mode": &self.mode,
            "bitcoind_rpc_url": &self.bitcoind_rpc_url,
            "bitcoind_rpc_user": &self.bitcoind_rpc_user,
            "bitcoind_rpc_password_set": !self.bitcoind_rpc_password.is_empty(),
            "bitcoind_rpc_cookie": &self.bitcoind_rpc_cookie,
            "template_provider_url": &self.template_provider_url,
            "job_declarator_url": &self.job_declarator_url,
            "coinbase_output_address": &self.coinbase_output_address,
            "template_refresh_interval_s": self.template_refresh_interval_s,
            "fallback_to_pool_templates": self.fallback_to_pool_templates,
            "declare_tx_data": self.declare_tx_data,
            "coinbase_output_max_additional_size": self.coinbase_output_max_additional_size,
            "coinbase_output_max_additional_sigops": self.coinbase_output_max_additional_sigops,
        })
    }
}

pub(crate) fn normalize_jd_mode(mode: &str) -> std::result::Result<String, String> {
    match mode.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "" | "coinbase_only" | "coinbase" => Ok("coinbase_only".to_string()),
        "full_template" | "full" | "declare_tx_data" => Ok("full_template".to_string()),
        other => Err(format!("unsupported Job Declaration mode '{}'", other)),
    }
}

pub(crate) fn read_jd_config_request() -> JdConfigRequest {
    let Ok(contents) = std::fs::read_to_string(crate::rest::get_config_path()) else {
        return JdConfigRequest::default();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&contents) else {
        return JdConfigRequest::default();
    };
    JdConfigRequest::from_table(&table)
}

fn write_jd_config_table(
    table: &mut toml::Table,
    config: &JdConfigRequest,
    preserve_password: Option<String>,
) {
    let jd = table
        .entry("job_declaration".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(jd_table) = jd {
        jd_table.insert("enabled".into(), toml::Value::Boolean(config.enabled));
        jd_table.insert("mode".into(), toml::Value::String(config.mode.clone()));
        jd_table.insert(
            "bitcoind_rpc_url".into(),
            toml::Value::String(config.bitcoind_rpc_url.clone()),
        );
        jd_table.insert(
            "bitcoind_rpc_user".into(),
            toml::Value::String(config.bitcoind_rpc_user.clone()),
        );
        let password = if config.bitcoind_rpc_password.is_empty() {
            preserve_password.unwrap_or_default()
        } else {
            config.bitcoind_rpc_password.clone()
        };
        jd_table.insert(
            "bitcoind_rpc_password".into(),
            toml::Value::String(password),
        );
        jd_table.insert(
            "bitcoind_rpc_cookie".into(),
            toml::Value::String(config.bitcoind_rpc_cookie.clone()),
        );
        jd_table.insert(
            "template_provider_url".into(),
            toml::Value::String(config.template_provider_url.clone()),
        );
        jd_table.insert(
            "job_declarator_url".into(),
            toml::Value::String(config.job_declarator_url.clone()),
        );
        jd_table.insert(
            "coinbase_output_address".into(),
            toml::Value::String(config.coinbase_output_address.clone()),
        );
        jd_table.insert(
            "template_refresh_interval_s".into(),
            toml::Value::Integer(config.template_refresh_interval_s as i64),
        );
        jd_table.insert(
            "fallback_to_pool_templates".into(),
            toml::Value::Boolean(config.fallback_to_pool_templates),
        );
        jd_table.insert(
            "declare_tx_data".into(),
            toml::Value::Boolean(config.declare_tx_data),
        );
        jd_table.insert(
            "coinbase_output_max_additional_size".into(),
            toml::Value::Integer(config.coinbase_output_max_additional_size as i64),
        );
        jd_table.insert(
            "coinbase_output_max_additional_sigops".into(),
            toml::Value::Integer(i64::from(config.coinbase_output_max_additional_sigops)),
        );
    }
}

async fn get_jd_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = read_jd_config_request();
    let redacted_config = config.redacted_json();
    let live = state.jd_status_rx.borrow().clone();
    let miner = state.state_rx.borrow().clone();
    let custom_job_bridge = miner.pool.sv2_custom_job.clone();
    let custom_job_injection_active = custom_job_bridge
        .as_ref()
        .map(|job| job.status == "accepted")
        .unwrap_or(false);
    let custom_job_injection_ready = live.custom_job_candidate_ready && miner.pool.encrypted;
    let runtime_state = live.runtime_state.clone();
    let live_mode = match live.mode {
        dcentrald_stratum::v2::jd::JdMode::CoinbaseOnly => "coinbase_only",
        dcentrald_stratum::v2::jd::JdMode::FullTemplate => "full_template",
    };
    let restart_required = config.enabled != live.enabled
        || config.template_provider_url != live.template_provider_url
        || config.job_declarator_url != live.job_declarator_url
        || normalize_jd_mode(&config.mode).unwrap_or_else(|_| config.mode.clone()) != live_mode;
    let bridge_last_declared_job_id = custom_job_bridge
        .as_ref()
        .and_then(|job| job.job_id)
        .or(live.last_declared_job_id);
    let bridge_last_request_id = custom_job_bridge
        .as_ref()
        .and_then(|job| job.request_id)
        .or(live.custom_job_last_request_id);
    let bridge_last_template_id = custom_job_bridge
        .as_ref()
        .and_then(|job| job.template_id)
        .or(live.custom_job_last_template_id);
    let reason = if restart_required {
        "Saved SV2 Job Declaration config differs from the running supervisor. Restart dcentrald to apply the new runtime endpoints."
    } else if !config.enabled {
        "SV2 Job Declaration is disabled."
    } else if !live.configured {
        "SV2 Job Declaration is enabled but missing Template Provider or Job Declarator endpoint configuration."
    } else if custom_job_injection_active {
        "SV2 custom work was accepted by the upstream pool and is active in the mining pipeline."
    } else if custom_job_bridge
        .as_ref()
        .map(|job| job.status == "declared")
        .unwrap_or(false)
    {
        "SV2 custom work was sent to the upstream pool and is waiting for SetCustomMiningJob.Success."
    } else if live.custom_job_candidate_ready {
        "SV2 Job Declaration supervisor has a Template Provider candidate and JDS mining-job token. The mining bridge will inject it after SV2 work-selection channel setup."
    } else if live.connected && live.mining_job_token_available {
        "SV2 Job Declaration supervisor completed setup handshakes and allocated a mining-job token. Waiting for a complete Template Provider candidate."
    } else if live.connected {
        "SV2 Job Declaration supervisor completed Template Provider and Job Declarator setup handshakes. Custom-job injection remains gated until the full declaration pipeline is enabled."
    } else if live.template_provider_connected || live.job_declarator_connected {
        "SV2 Job Declaration supervisor has a partial protocol connection. Custom jobs are not being injected."
    } else {
        "SV2 Job Declaration supervisor is active but has not completed both setup handshakes."
    };
    axum::Json(serde_json::json!({
        "status": runtime_state,
        "enabled": config.enabled,
        "configured": live.configured,
        "connected": live.connected,
        "template_provider_connected": live.template_provider_connected,
        "job_declarator_connected": live.job_declarator_connected,
        "mining_job_token_available": live.mining_job_token_available,
        "template_prev_hash_ready": live.template_prev_hash_ready,
        "custom_job_candidate_ready": live.custom_job_candidate_ready,
        "custom_job_injection_ready": custom_job_injection_ready,
        "custom_job_injection_active": custom_job_injection_active,
        "custom_job_bridge": custom_job_bridge,
        "protocol_ready": live.protocol_ready,
        "live_jdc_runtime": live.enabled,
        "restart_required": restart_required,
        "mode": live.mode,
        "bitcoind_url": &config.bitcoind_rpc_url,
        "template_provider_url": live.template_provider_url,
        "job_declarator_url": live.job_declarator_url,
        "templates_constructed": live.templates_constructed,
        "last_template_age_s": live.last_template_age_s,
        "current_template_id": live.current_template_id,
        "current_tx_count": live.current_tx_count,
        "coinbase_value_remaining_sats": live.coinbase_value_remaining_sats,
        "coinbase_output_count": live.coinbase_output_count,
        "last_declared_job_id": bridge_last_declared_job_id,
        "custom_job_last_request_id": bridge_last_request_id,
        "custom_job_last_template_id": bridge_last_template_id,
        "last_connection_attempt_s": live.last_connection_attempt_s,
        "last_update_s": live.last_update_s,
        "last_error": live.last_error,
        "current_fees_btc": serde_json::Value::Null,
        "config": redacted_config,
        "runtime_state": runtime_state,
        "reason": reason,
    }))
}

async fn post_jd_config(Json(body): Json<JdConfigRequest>) -> impl IntoResponse {
    let config = match body.normalize() {
        Ok(config) => config,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "status": "error",
                    "message": message,
                })),
            );
        }
    };

    let config_path = crate::rest::get_writable_config_path();
    let write_result = (|| -> std::result::Result<(), String> {
        // RELIAB-2b: serialize load→modify→write (lost-update guard). Scoped to
        // this synchronous closure so it drops before any `.await`.
        let _cfg_write_guard = crate::atomic_io::config_write_lock();
        let mut table = crate::rest::load_config_table_for_write()?;
        let saved = JdConfigRequest::from_table(&table);
        if let Some(parent) = std::path::Path::new(config_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }
        write_jd_config_table(
            &mut table,
            &config,
            if saved.bitcoind_rpc_password.is_empty() {
                None
            } else {
                Some(saved.bitcoind_rpc_password)
            },
        );
        let output = toml::to_string_pretty(&table)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
        Ok(())
    })();

    match write_result {
        Ok(()) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "ok",
                "message": "Job Declaration configuration saved; restart dcentrald to apply the live supervisor endpoints",
                "restart_required": true,
                "config_path": config_path,
                "config": config.redacted_json(),
            })),
        ),
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        ),
    }
}

async fn post_jd_test_connection(Json(body): Json<JdConfigRequest>) -> impl IntoResponse {
    let config = match body.normalize() {
        Ok(config) => config,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "status": "error",
                    "message": message,
                    "checks": [],
                })),
            );
        }
    };

    let mut checks = Vec::new();
    checks.push(probe_bitcoind_rpc(&config).await);
    checks.push(probe_tcp_endpoint("template_provider", &config.template_provider_url).await);
    if !config.job_declarator_url.is_empty() {
        checks.push(probe_tcp_endpoint("job_declarator", &config.job_declarator_url).await);
    }

    let ok = checks
        .iter()
        .all(|check| check.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        axum::Json(serde_json::json!({
            "status": if ok { "ok" } else { "error" },
            "message": if ok { "Bitcoin Core template RPC and configured SV2 endpoints are ready" } else { "Bitcoin Core template RPC or one of the configured SV2 endpoints is not ready" },
            "checks": checks,
        })),
    )
}

#[derive(Debug, Deserialize)]
struct BitcoinRpcEnvelope {
    result: Option<serde_json::Value>,
    error: Option<BitcoinRpcError>,
}

#[derive(Debug, Deserialize)]
struct BitcoinRpcError {
    code: i64,
    message: String,
}

#[derive(Debug)]
struct BitcoinRpcCallError {
    code: Option<i64>,
    message: String,
}

async fn probe_bitcoind_rpc(config: &JdConfigRequest) -> serde_json::Value {
    let url = config.bitcoind_rpc_url.trim();
    if url.is_empty() {
        return serde_json::json!({
            "label": "bitcoind_rpc",
            "url": url,
            "ok": false,
            "status": "invalid",
            "message": "Bitcoin Core RPC URL is empty",
        });
    }

    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return serde_json::json!({
            "label": "bitcoind_rpc",
            "url": url,
            "ok": false,
            "status": "invalid",
            "message": "Bitcoin Core RPC URL must start with http:// or https://",
        });
    }

    let credentials = match bitcoind_rpc_credentials(config) {
        Ok(credentials) => credentials,
        Err(message) => {
            return serde_json::json!({
                "label": "bitcoind_rpc",
                "url": url,
                "ok": false,
                "status": "auth_unavailable",
                "message": message,
            });
        }
    };

    let started = std::time::Instant::now();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return serde_json::json!({
                "label": "bitcoind_rpc",
                "url": url,
                "ok": false,
                "status": "client_error",
                "message": format!("Failed to create HTTP client: {}", error),
            });
        }
    };

    let chain = match bitcoind_rpc_call(
        &client,
        url,
        credentials.as_ref(),
        "getblockchaininfo",
        serde_json::json!([]),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return serde_json::json!({
                "label": "bitcoind_rpc",
                "url": url,
                "ok": false,
                "status": "rpc_error",
                "rpc_reachable": false,
                "message": error.message,
                "rpc_error_code": error.code,
                "latency_ms": started.elapsed().as_millis() as u64,
            });
        }
    };

    let initial_sync = chain
        .get("initialblockdownload")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if initial_sync {
        return serde_json::json!({
            "label": "bitcoind_rpc",
            "url": url,
            "ok": false,
            "status": "initial_sync",
            "rpc_reachable": true,
            "template_ready": false,
            "chain": chain.get("chain").cloned().unwrap_or(serde_json::Value::Null),
            "blocks": chain.get("blocks").cloned().unwrap_or(serde_json::Value::Null),
            "headers": chain.get("headers").cloned().unwrap_or(serde_json::Value::Null),
            "verification_progress": chain.get("verificationprogress").cloned().unwrap_or(serde_json::Value::Null),
            "pruned": chain.get("pruned").cloned().unwrap_or(serde_json::Value::Null),
            "prune_target_size": chain.get("prune_target_size").cloned().unwrap_or(serde_json::Value::Null),
            "size_on_disk": chain.get("size_on_disk").cloned().unwrap_or(serde_json::Value::Null),
            "message": "Bitcoin Core RPC is reachable, but the node is still in initial block download and cannot provide mining templates yet.",
            "latency_ms": started.elapsed().as_millis() as u64,
        });
    }

    match bitcoind_rpc_call(
        &client,
        url,
        credentials.as_ref(),
        "getblocktemplate",
        serde_json::json!([{ "rules": ["segwit"] }]),
    )
    .await
    {
        Ok(template) => serde_json::json!({
            "label": "bitcoind_rpc",
            "url": url,
            "ok": true,
            "status": "template_ready",
            "rpc_reachable": true,
            "template_ready": true,
            "chain": chain.get("chain").cloned().unwrap_or(serde_json::Value::Null),
            "blocks": chain.get("blocks").cloned().unwrap_or(serde_json::Value::Null),
            "headers": chain.get("headers").cloned().unwrap_or(serde_json::Value::Null),
            "verification_progress": chain.get("verificationprogress").cloned().unwrap_or(serde_json::Value::Null),
            "pruned": chain.get("pruned").cloned().unwrap_or(serde_json::Value::Null),
            "prune_target_size": chain.get("prune_target_size").cloned().unwrap_or(serde_json::Value::Null),
            "size_on_disk": chain.get("size_on_disk").cloned().unwrap_or(serde_json::Value::Null),
            "template_height": template.get("height").cloned().unwrap_or(serde_json::Value::Null),
            "template_previous_block_hash": template.get("previousblockhash").cloned().unwrap_or(serde_json::Value::Null),
            "template_transaction_count": template
                .get("transactions")
                .and_then(|value| value.as_array())
                .map(|transactions| transactions.len())
                .unwrap_or(0),
            "message": "Bitcoin Core RPC is reachable and getblocktemplate returned a candidate template.",
            "latency_ms": started.elapsed().as_millis() as u64,
        }),
        Err(error) => serde_json::json!({
            "label": "bitcoind_rpc",
            "url": url,
            "ok": false,
            "status": "template_unavailable",
            "rpc_reachable": true,
            "template_ready": false,
            "chain": chain.get("chain").cloned().unwrap_or(serde_json::Value::Null),
            "blocks": chain.get("blocks").cloned().unwrap_or(serde_json::Value::Null),
            "headers": chain.get("headers").cloned().unwrap_or(serde_json::Value::Null),
            "verification_progress": chain.get("verificationprogress").cloned().unwrap_or(serde_json::Value::Null),
            "pruned": chain.get("pruned").cloned().unwrap_or(serde_json::Value::Null),
            "prune_target_size": chain.get("prune_target_size").cloned().unwrap_or(serde_json::Value::Null),
            "size_on_disk": chain.get("size_on_disk").cloned().unwrap_or(serde_json::Value::Null),
            "rpc_error_code": error.code,
            "message": error.message,
            "latency_ms": started.elapsed().as_millis() as u64,
        }),
    }
}

fn bitcoind_rpc_credentials(
    config: &JdConfigRequest,
) -> std::result::Result<Option<(String, String)>, String> {
    let cookie_path = config.bitcoind_rpc_cookie.trim();
    if !cookie_path.is_empty() {
        let cookie = std::fs::read_to_string(cookie_path)
            .map_err(|error| format!("Failed to read Bitcoin Core RPC cookie: {}", error))?;
        let (user, password) = cookie
            .trim()
            .split_once(':')
            .ok_or_else(|| "Bitcoin Core RPC cookie did not contain user:password".to_string())?;
        return Ok(Some((user.to_string(), password.to_string())));
    }

    if !config.bitcoind_rpc_user.trim().is_empty() || !config.bitcoind_rpc_password.is_empty() {
        return Ok(Some((
            config.bitcoind_rpc_user.trim().to_string(),
            config.bitcoind_rpc_password.clone(),
        )));
    }

    Ok(None)
}

async fn bitcoind_rpc_call(
    client: &reqwest::Client,
    url: &str,
    credentials: Option<&(String, String)>,
    method: &str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, BitcoinRpcCallError> {
    let body = serde_json::json!({
        "jsonrpc": "1.0",
        "id": "dcentrald-jd-test",
        "method": method,
        "params": params,
    });
    let mut request = client.post(url).json(&body);
    if let Some((user, password)) = credentials {
        request = request.basic_auth(user, Some(password));
    }

    let response = request.send().await.map_err(|error| BitcoinRpcCallError {
        code: None,
        message: format!("Bitcoin Core RPC request failed: {}", error),
    })?;
    let status = response.status();
    let text = response.text().await.map_err(|error| BitcoinRpcCallError {
        code: None,
        message: format!("Bitcoin Core RPC response read failed: {}", error),
    })?;

    if !status.is_success() {
        return Err(BitcoinRpcCallError {
            code: None,
            message: format!("Bitcoin Core RPC returned HTTP status {}", status),
        });
    }

    let envelope =
        serde_json::from_str::<BitcoinRpcEnvelope>(&text).map_err(|error| BitcoinRpcCallError {
            code: None,
            message: format!("Bitcoin Core RPC response was not valid JSON: {}", error),
        })?;
    if let Some(error) = envelope.error {
        return Err(BitcoinRpcCallError {
            code: Some(error.code),
            message: error.message,
        });
    }
    envelope.result.ok_or_else(|| BitcoinRpcCallError {
        code: None,
        message: "Bitcoin Core RPC response did not include a result".to_string(),
    })
}

async fn probe_tcp_endpoint(label: &str, url: &str) -> serde_json::Value {
    let parsed = parse_jd_tcp_endpoint(url);
    let (host, port) = match parsed {
        Ok(parts) => parts,
        Err(message) => {
            return serde_json::json!({
                "label": label,
                "url": url,
                "ok": false,
                "status": "invalid",
                "message": message,
            });
        }
    };

    let addr = format!("{}:{}", host, port);
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(&addr),
    )
    .await;
    match result {
        Ok(Ok(_stream)) => serde_json::json!({
            "label": label,
            "url": url,
            "host": host,
            "port": port,
            "ok": true,
            "status": "reachable",
            "latency_ms": started.elapsed().as_millis() as u64,
        }),
        Ok(Err(error)) => serde_json::json!({
            "label": label,
            "url": url,
            "host": host,
            "port": port,
            "ok": false,
            "status": "connection_failed",
            "message": error.to_string(),
        }),
        Err(_) => serde_json::json!({
            "label": label,
            "url": url,
            "host": host,
            "port": port,
            "ok": false,
            "status": "timeout",
            "message": "TCP connection timed out after 2s",
        }),
    }
}

fn parse_jd_tcp_endpoint(url: &str) -> std::result::Result<(String, u16), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("endpoint URL is empty".to_string());
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let parsed = reqwest::Url::parse(trimmed)
            .map_err(|e| format!("invalid endpoint URL '{}': {}", trimmed, e))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("endpoint URL '{}' is missing a host", trimmed))?
            .to_string();
        let port = parsed
            .port()
            .or_else(|| jd_default_port(parsed.scheme()))
            .ok_or_else(|| {
                format!(
                    "endpoint URL '{}' is missing a port for scheme '{}'",
                    trimmed,
                    parsed.scheme()
                )
            })?;
        return Ok((host, port));
    }

    let mut scheme = "tcp";
    let mut host_port = trimmed;
    for prefix in [
        "sv2+tcp://",
        "stratum2+tcp://",
        "stratum+tcp://",
        "tp+tcp://",
        "jd+tcp://",
        "tcp://",
        "sv2://",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            scheme = prefix.trim_end_matches("://");
            host_port = rest;
            break;
        }
    }

    if host_port.is_empty() {
        return Err(format!("endpoint URL '{}' is missing a host", trimmed));
    }

    if host_port.starts_with('[') {
        let Some(end) = host_port.find(']') else {
            return Err(format!(
                "endpoint URL '{}' has an invalid IPv6 host",
                trimmed
            ));
        };
        let host = host_port[1..end].to_string();
        let port = if host_port.len() > end + 1 {
            let rest = &host_port[end + 1..];
            let Some(port_text) = rest.strip_prefix(':') else {
                return Err(format!("endpoint URL '{}' has an invalid port", trimmed));
            };
            port_text
                .parse::<u16>()
                .map_err(|_| format!("endpoint URL '{}' has an invalid port", trimmed))?
        } else {
            jd_default_port(scheme).ok_or_else(|| {
                format!(
                    "endpoint URL '{}' is missing a port for scheme '{}'",
                    trimmed, scheme
                )
            })?
        };
        return Ok((host, port));
    }

    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port_text)) if !host.contains(':') => {
            let port = port_text
                .parse::<u16>()
                .map_err(|_| format!("endpoint URL '{}' has an invalid port", trimmed))?;
            (host.to_string(), port)
        }
        Some((_host, _port_text)) => {
            return Err(format!(
                "endpoint URL '{}' has an IPv6 host without brackets",
                trimmed
            ));
        }
        None => {
            let port = jd_default_port(scheme).ok_or_else(|| {
                format!(
                    "endpoint URL '{}' is missing a port for scheme '{}'",
                    trimmed, scheme
                )
            })?;
            (host_port.to_string(), port)
        }
    };

    if host.is_empty() {
        return Err(format!("endpoint URL '{}' is missing a host", trimmed));
    }
    Ok((host, port))
}

fn jd_default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "tcp" | "sv2" | "sv2+tcp" | "stratum2+tcp" => Some(34255),
        "stratum+tcp" => Some(3333),
        "tp+tcp" => Some(8442),
        "jd+tcp" => Some(8443),
        _ => None,
    }
}

/// Build the JD sub-router. Merged into the top-level router by
/// `rest::build_router()`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/jd/status", get(get_jd_status))
        .route("/api/jd/config", post(post_jd_config))
        .route("/api/jd/test-connection", post(post_jd_test_connection))
}
