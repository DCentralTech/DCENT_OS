//! Stratum V2 Job Declaration helpers shared by every mining mode.
//!
//! W2.1 follow-up extraction (2026-05-07): the SV2 Job Declaration glue used
//! to live inline at the top of `daemon.rs`. Both the S9 / am2-passthrough
//! `Daemon` path and the BM1362 / BM1368 `SerialMiner` path call these
//! helpers, so they belong in `runtime::*` rather than the daemon-specific
//! orchestration file.
//!
//! Pure, no hardware. The supervisor task probes the configured Job
//! Declarator URL on a 5-300 s interval (clamped) and broadcasts the
//! latest `JdStatus` over a `tokio::sync::watch` channel. It never touches
//! `/dev/i2c-0`, `/dev/uio*`, or `/dev/ttyS*`.

use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::config::JobDeclarationConfig;

/// Map a `[job_declaration]` TOML block onto the SV2 client's `JdConfig`.
///
/// The `mode` string is forgiving (case-insensitive, accepts `full`,
/// `full_template`, `declare-tx-data`, etc.) so existing operator config
/// files don't need verbatim-canonical mode strings.
pub(crate) fn job_declaration_config_to_sv2(
    config: &JobDeclarationConfig,
) -> dcentrald_stratum::v2::jd::JdConfig {
    let mode = match config
        .mode
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "full" | "full_template" | "declare_tx_data" => {
            dcentrald_stratum::v2::jd::JdMode::FullTemplate
        }
        _ => dcentrald_stratum::v2::jd::JdMode::CoinbaseOnly,
    };
    dcentrald_stratum::v2::jd::JdConfig {
        enabled: config.enabled,
        mode,
        bitcoind_rpc_url: config.bitcoind_rpc_url.clone(),
        bitcoind_rpc_user: config.bitcoind_rpc_user.clone(),
        bitcoind_rpc_password: config.bitcoind_rpc_password.clone(),
        bitcoind_rpc_cookie: config.bitcoind_rpc_cookie.clone(),
        template_provider_url: config.template_provider_url.clone(),
        job_declarator_url: config.job_declarator_url.clone(),
        coinbase_output_address: config.coinbase_output_address.clone(),
        template_refresh_interval_s: config.template_refresh_interval_s,
        fallback_to_pool_templates: config.fallback_to_pool_templates,
        declare_tx_data: config.declare_tx_data
            || matches!(mode, dcentrald_stratum::v2::jd::JdMode::FullTemplate),
        coinbase_output_max_additional_size: config.coinbase_output_max_additional_size,
        coinbase_output_max_additional_sigops: config.coinbase_output_max_additional_sigops,
    }
}

/// Snapshot the initial Job Declaration status BEFORE the supervisor task
/// is spawned, so the API can serve a non-empty `/api/jd/status` from the
/// first request onwards.
pub(crate) fn initial_job_declaration_status(
    config: &JobDeclarationConfig,
) -> dcentrald_stratum::v2::jd::JdStatus {
    dcentrald_stratum::v2::jd::JdClient::new(job_declaration_config_to_sv2(config)).status()
}

/// Spawn the SV2 Job Declaration supervisor task.
///
/// Probes the JD endpoint every `template_refresh_interval_s` (clamped to
/// 5-300 s) and broadcasts a fresh `JdStatus` over `status_tx`. Returns
/// when `shutdown` cancels.
pub(crate) fn spawn_job_declaration_supervisor(
    config: JobDeclarationConfig,
    status_tx: watch::Sender<dcentrald_stratum::v2::jd::JdStatus>,
    shutdown: CancellationToken,
) {
    let interval_s = config.template_refresh_interval_s.clamp(5, 300);
    let client = dcentrald_stratum::v2::jd::JdClient::new(job_declaration_config_to_sv2(&config));
    tokio::spawn(async move {
        loop {
            let status = client.probe_once().await;
            let enabled = status.enabled;
            let runtime_state = status.runtime_state.clone();
            let connected = status.connected;
            let _ = status_tx.send(status);
            if enabled {
                tracing::debug!(
                    connected,
                    runtime_state = %runtime_state,
                    "SV2 Job Declaration supervisor probe completed"
                );
            }

            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("SV2 Job Declaration supervisor stopping");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(interval_s as u64)) => {}
            }
        }
    });
}
