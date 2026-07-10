//! Stratum V2 Job Declaration and Template Distribution primitives.
//!
//! Own-template mining is a miner-side proxy/OS responsibility, not an end
//! ASIC-device responsibility. The Job Declarator Client receives templates
//! from a Template Provider, asks the pool-side JDS for mining-job tokens,
//! declares or advertises the custom job, then serves standard SV2 work to
//! downstream devices.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use super::auth::{
    parse_authority_key_from_sv2_url, sv2_insecure_no_noise, sv2_tp_require_noise,
    AuthorityKeyError,
};
use super::framing::{Sv2Frame, Sv2FrameHeader, FRAME_HEADER_SIZE};
use super::noise::NoiseSession;
use super::types::{
    common, job_declaration, mining, template_distribution, SetupConnection, DECLARE_TX_DATA,
    EXTENSION_TYPE_CORE, PROTOCOL_JOB_DECLARATION, PROTOCOL_TEMPLATE_DISTRIBUTION,
};

type CodecResult<T> = Result<T, String>;

fn default_bitcoind_url() -> String {
    "http://127.0.0.1:8332".to_string()
}

fn default_template_provider_url() -> String {
    "sv2+tcp://127.0.0.1:8442".to_string()
}

fn default_template_refresh() -> u32 {
    30
}

fn default_coinbase_output_max_additional_size() -> u32 {
    512
}

/// Job Declaration mode negotiated with a pool/JDS pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JdMode {
    /// Pool sees payout/fee commitment but not the selected transaction set.
    CoinbaseOnly,
    /// JDC declares the template txids and can provide missing tx data.
    FullTemplate,
}

impl Default for JdMode {
    fn default() -> Self {
        Self::CoinbaseOnly
    }
}

/// Job Declaration Client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JdConfig {
    pub enabled: bool,
    pub mode: JdMode,
    pub bitcoind_rpc_url: String,
    pub bitcoind_rpc_user: String,
    pub bitcoind_rpc_password: String,
    pub bitcoind_rpc_cookie: String,
    pub template_provider_url: String,
    pub job_declarator_url: String,
    pub coinbase_output_address: String,
    pub template_refresh_interval_s: u32,
    pub fallback_to_pool_templates: bool,
    pub declare_tx_data: bool,
    pub coinbase_output_max_additional_size: u32,
    pub coinbase_output_max_additional_sigops: u16,
}

impl Default for JdConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: JdMode::CoinbaseOnly,
            bitcoind_rpc_url: default_bitcoind_url(),
            bitcoind_rpc_user: String::new(),
            bitcoind_rpc_password: String::new(),
            bitcoind_rpc_cookie: String::new(),
            template_provider_url: default_template_provider_url(),
            job_declarator_url: String::new(),
            coinbase_output_address: String::new(),
            template_refresh_interval_s: default_template_refresh(),
            fallback_to_pool_templates: true,
            declare_tx_data: false,
            coinbase_output_max_additional_size: default_coinbase_output_max_additional_size(),
            coinbase_output_max_additional_sigops: 0,
        }
    }
}

/// Job Declaration Client status.
#[derive(Debug, Clone, Serialize)]
pub struct JdStatus {
    pub enabled: bool,
    pub configured: bool,
    pub protocol_ready: bool,
    pub connected: bool,
    pub template_provider_connected: bool,
    pub job_declarator_connected: bool,
    pub mining_job_token_available: bool,
    pub template_prev_hash_ready: bool,
    pub custom_job_candidate_ready: bool,
    pub mode: JdMode,
    pub runtime_state: String,
    pub bitcoind_url: String,
    pub template_provider_url: String,
    pub job_declarator_url: String,
    pub templates_constructed: u64,
    pub last_template_age_s: Option<u64>,
    pub current_template_id: Option<u64>,
    pub current_tx_count: Option<u32>,
    pub coinbase_value_remaining_sats: Option<u64>,
    pub coinbase_output_count: Option<u32>,
    pub last_declared_job_id: Option<u32>,
    pub custom_job_injection_ready: bool,
    pub custom_job_injection_active: bool,
    pub custom_job_last_request_id: Option<u32>,
    pub custom_job_last_template_id: Option<u64>,
    pub last_error: Option<String>,
    pub last_connection_attempt_s: Option<u64>,
    pub last_update_s: Option<u64>,
    #[serde(skip_serializing)]
    pub custom_job_candidate: Option<CustomJobCandidate>,
}

impl Default for JdStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            configured: false,
            protocol_ready: true,
            connected: false,
            template_provider_connected: false,
            job_declarator_connected: false,
            mining_job_token_available: false,
            template_prev_hash_ready: false,
            custom_job_candidate_ready: false,
            mode: JdMode::CoinbaseOnly,
            runtime_state: "disabled".to_string(),
            bitcoind_url: String::new(),
            template_provider_url: String::new(),
            job_declarator_url: String::new(),
            templates_constructed: 0,
            last_template_age_s: None,
            current_template_id: None,
            current_tx_count: None,
            coinbase_value_remaining_sats: None,
            coinbase_output_count: None,
            last_declared_job_id: None,
            custom_job_injection_ready: false,
            custom_job_injection_active: false,
            custom_job_last_request_id: None,
            custom_job_last_template_id: None,
            last_error: None,
            last_connection_attempt_s: None,
            last_update_s: None,
            custom_job_candidate: None,
        }
    }
}

/// Private payload passed from the JD/TDP supervisor into the mining proxy.
///
/// The token and coinbase outputs must not be exposed through REST. They are
/// carried in `JdStatus` with `skip_serializing` so the mining client can build
/// `SetCustomMiningJob` without creating a second control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomJobCandidate {
    pub template_id: u64,
    pub mining_job_token: Vec<u8>,
    pub version: u32,
    pub prev_hash: [u8; 32],
    pub min_ntime: u32,
    pub nbits: u32,
    pub target: [u8; 32],
    pub coinbase_tx_version: u32,
    pub coinbase_prefix: Vec<u8>,
    pub coinbase_tx_input_nsequence: u32,
    pub coinbase_tx_outputs: Vec<u8>,
    pub coinbase_tx_locktime: u32,
    pub merkle_path: Vec<[u8; 32]>,
    pub tx_count: u32,
    pub coinbase_value_remaining_sats: u64,
}

impl CustomJobCandidate {
    pub fn to_set_custom_mining_job(&self, channel_id: u32, request_id: u32) -> SetCustomMiningJob {
        SetCustomMiningJob {
            channel_id,
            request_id,
            mining_job_token: self.mining_job_token.clone(),
            version: self.version,
            prev_hash: self.prev_hash,
            min_ntime: self.min_ntime,
            nbits: self.nbits,
            coinbase_tx_version: self.coinbase_tx_version,
            coinbase_prefix: self.coinbase_prefix.clone(),
            coinbase_tx_input_nsequence: self.coinbase_tx_input_nsequence,
            coinbase_tx_outputs: self.coinbase_tx_outputs.clone(),
            coinbase_tx_locktime: self.coinbase_tx_locktime,
            merkle_path: self.merkle_path.clone(),
        }
    }

    pub fn stable_key(&self) -> CustomJobCandidateKey {
        CustomJobCandidateKey {
            template_id: self.template_id,
            prev_hash: self.prev_hash,
            min_ntime: self.min_ntime,
            nbits: self.nbits,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomJobCandidateKey {
    pub template_id: u64,
    pub prev_hash: [u8; 32],
    pub min_ntime: u32,
    pub nbits: u32,
}

/// Job Declaration Client state holder.
pub struct JdClient {
    config: JdConfig,
}

impl JdClient {
    pub fn new(config: JdConfig) -> Self {
        // Redact any embedded `user:pass@` credentials before logging. The
        // bitcoind RPC URL commonly carries rpcuser:rpcpassword@host (Bitcoin
        // Core's URL-auth form), and the SV2 TP/JD endpoints may carry an auth
        // token — a raw log would leak them into the daemon log / support bundle.
        info!(
            bitcoind = %crate::pool_api::sanitize_pool_url(&config.bitcoind_rpc_url),
            template_provider = %crate::pool_api::sanitize_pool_url(&config.template_provider_url),
            job_declarator = %crate::pool_api::sanitize_pool_url(&config.job_declarator_url),
            mode = ?config.mode,
            "Job Declaration client configured"
        );
        Self { config }
    }

    pub fn status(&self) -> JdStatus {
        let configured = !self.config.template_provider_url.trim().is_empty()
            && !self.config.job_declarator_url.trim().is_empty();
        JdStatus {
            enabled: self.config.enabled,
            configured,
            protocol_ready: true,
            connected: false,
            template_provider_connected: false,
            job_declarator_connected: false,
            mining_job_token_available: false,
            template_prev_hash_ready: false,
            custom_job_candidate_ready: false,
            mode: self.config.mode,
            runtime_state: if self.config.enabled {
                "configured".to_string()
            } else {
                "disabled".to_string()
            },
            bitcoind_url: self.config.bitcoind_rpc_url.clone(),
            template_provider_url: self.config.template_provider_url.clone(),
            job_declarator_url: self.config.job_declarator_url.clone(),
            templates_constructed: 0,
            last_template_age_s: None,
            current_template_id: None,
            current_tx_count: None,
            coinbase_value_remaining_sats: None,
            coinbase_output_count: None,
            last_declared_job_id: None,
            custom_job_injection_ready: false,
            custom_job_injection_active: false,
            custom_job_last_request_id: None,
            custom_job_last_template_id: None,
            last_error: None,
            last_connection_attempt_s: None,
            last_update_s: Some(unix_now_s()),
            custom_job_candidate: None,
        }
    }

    /// Run one bounded live JD/TDP session probe.
    ///
    /// This is intentionally a supervisor proof, not yet mining-work injection:
    /// it connects to the configured Template Provider and Job Declarator,
    /// sends Common `SetupConnection`, and for the Template Provider sends
    /// `CoinbaseOutputConstraints`. The downstream custom-job bridge is kept
    /// gated until both sessions are healthy and a full template/job-declare
    /// pipeline is merged.
    pub async fn probe_once(&self) -> JdStatus {
        let mut status = self.status();
        let now = unix_now_s();
        status.last_connection_attempt_s = Some(now);
        status.last_update_s = Some(now);

        if !self.config.enabled {
            status.runtime_state = "disabled".to_string();
            return status;
        }
        if !status.configured {
            status.runtime_state = "missing_endpoints".to_string();
            status.last_error =
                Some("Template Provider and Job Declarator endpoints are required".to_string());
            return status;
        }

        let tp = self.probe_template_provider().await;
        status.template_provider_connected = tp.connected;
        if let Some(template) = tp.template.as_ref() {
            status.current_template_id = template.template_id;
            status.template_prev_hash_ready = template.prev_hash_matches_template();
            status.current_tx_count = template.current_tx_count;
            status.coinbase_value_remaining_sats = template.coinbase_value_remaining_sats;
            status.coinbase_output_count = template.coinbase_output_count;
            if template.new_template_seen {
                status.templates_constructed = 1;
                status.last_template_age_s = Some(0);
            }
        }

        let jd = self.probe_job_declarator().await;
        status.job_declarator_connected = jd.connected;
        status.mining_job_token_available = jd
            .mining_job_token
            .as_ref()
            .map(|token| !token.mining_job_token.is_empty())
            .unwrap_or(false);
        status.connected = status.template_provider_connected && status.job_declarator_connected;
        status.custom_job_candidate_ready = status.connected
            && status.mining_job_token_available
            && status.templates_constructed > 0
            && status.template_prev_hash_ready;
        if status.custom_job_candidate_ready {
            match (
                tp.template.as_ref(),
                jd.mining_job_token.as_ref(),
                status.coinbase_value_remaining_sats,
            ) {
                (Some(template), Some(token), Some(coinbase_value)) => {
                    match template.to_custom_job_candidate(token, coinbase_value) {
                        Ok(candidate) => {
                            status.custom_job_last_template_id = Some(candidate.template_id);
                            status.custom_job_candidate = Some(candidate);
                        }
                        Err(error) => {
                            status.custom_job_candidate_ready = false;
                            status.last_error = Some(error);
                        }
                    }
                }
                _ => {
                    status.custom_job_candidate_ready = false;
                }
            }
        }
        status.runtime_state = if status.custom_job_candidate_ready {
            "custom_job_candidate_ready".to_string()
        } else if status.connected && status.mining_job_token_available {
            "connected_token_ready".to_string()
        } else if status.connected {
            "connected_setup_complete".to_string()
        } else if status.template_provider_connected || status.job_declarator_connected {
            "partial_connection".to_string()
        } else {
            "connection_failed".to_string()
        };

        let errors = [tp.error, jd.error]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            status.last_error = Some(errors.join("; "));
        }
        status
    }

    pub async fn run_probe_loop(
        self,
        status_tx: tokio::sync::watch::Sender<JdStatus>,
        interval_s: u32,
    ) {
        let interval_s = interval_s.max(5);
        loop {
            let status = self.probe_once().await;
            let _ = status_tx.send(status);
            tokio::time::sleep(std::time::Duration::from_secs(interval_s as u64)).await;
        }
    }

    async fn probe_template_provider(&self) -> EndpointProbe {
        let constraints = CoinbaseOutputConstraints {
            max_additional_size: self.config.coinbase_output_max_additional_size,
            max_additional_sigops: self.config.coinbase_output_max_additional_sigops,
        };
        match self
            .setup_endpoint(
                "template_provider",
                &self.config.template_provider_url,
                PROTOCOL_TEMPLATE_DISTRIBUTION,
                0,
            )
            .await
        {
            Ok(mut conn) => {
                let frame = constraints.to_frame();
                if let Err(error) = conn.write_frame(&frame).await {
                    return EndpointProbe::err(format!(
                        "template_provider: failed to send CoinbaseOutputConstraints: {}",
                        error
                    ));
                }
                let mut template = TemplateProbe::default();
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(750);
                loop {
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    match tokio::time::timeout(remaining, conn.read_frame()).await {
                        Ok(Ok(frame)) => match frame.header.msg_type {
                            template_distribution::NEW_TEMPLATE => {
                                match NewTemplate::from_bytes(&frame.payload) {
                                    Ok(new_template) => template.note_new_template(&new_template),
                                    Err(error) => {
                                        return EndpointProbe {
                                            connected: true,
                                            template: template.into_option(),
                                            mining_job_token: None,
                                            error: Some(format!(
                                            "template_provider: failed to parse NewTemplate: {}",
                                            error
                                        )),
                                        }
                                    }
                                }
                            }
                            template_distribution::SET_NEW_PREV_HASH => {
                                match TemplateSetNewPrevHash::from_bytes(&frame.payload) {
                                    Ok(prev_hash) => template.note_prev_hash(&prev_hash),
                                    Err(error) => {
                                        return EndpointProbe {
                                            connected: true,
                                            template: template.into_option(),
                                            mining_job_token: None,
                                            error: Some(format!(
                                            "template_provider: failed to parse SetNewPrevHash: {}",
                                            error
                                        )),
                                        }
                                    }
                                }
                            }
                            _ => {}
                        },
                        Ok(Err(error)) => {
                            return EndpointProbe {
                                connected: true,
                                template: template.into_option(),
                                mining_job_token: None,
                                error: Some(format!(
                                    "template_provider: frame read failed: {}",
                                    error
                                )),
                            }
                        }
                        Err(_) => break,
                    }
                    if template.new_template_seen && template.prev_hash_seen {
                        break;
                    }
                }
                EndpointProbe {
                    connected: true,
                    template: template.into_option(),
                    mining_job_token: None,
                    error: None,
                }
            }
            Err(error) => EndpointProbe::err(format!("template_provider: {}", error)),
        }
    }

    async fn probe_job_declarator(&self) -> EndpointProbe {
        let flags = if self.config.declare_tx_data || self.config.mode == JdMode::FullTemplate {
            DECLARE_TX_DATA
        } else {
            0
        };
        match self
            .setup_endpoint(
                "job_declarator",
                &self.config.job_declarator_url,
                PROTOCOL_JOB_DECLARATION,
                flags,
            )
            .await
        {
            Ok(mut conn) => {
                let request = AllocateMiningJobToken {
                    user_identifier: self.token_user_identifier(),
                    request_id: 1,
                };
                let frame = match request.to_frame() {
                    Ok(frame) => frame,
                    Err(error) => {
                        return EndpointProbe {
                            connected: true,
                            template: None,
                            mining_job_token: None,
                            error: Some(format!(
                                "job_declarator: failed to encode AllocateMiningJobToken: {}",
                                error
                            )),
                        }
                    }
                };
                if let Err(error) = conn.write_frame(&frame).await {
                    return EndpointProbe {
                        connected: true,
                        template: None,
                        mining_job_token: None,
                        error: Some(format!(
                            "job_declarator: failed to send AllocateMiningJobToken: {}",
                            error
                        )),
                    };
                }
                match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    conn.read_frame(),
                )
                .await
                {
                    Ok(Ok(response)) => match response.header.msg_type {
                        job_declaration::ALLOCATE_MINING_JOB_TOKEN_SUCCESS => {
                            match AllocateMiningJobTokenSuccess::from_bytes(&response.payload) {
                                Ok(success) if success.request_id == request.request_id => {
                                    EndpointProbe {
                                        connected: true,
                                        template: None,
                                        mining_job_token: Some(JdTokenProbe {
                                            mining_job_token: success.mining_job_token,
                                            coinbase_tx_outputs: success.coinbase_tx_outputs,
                                        }),
                                        error: None,
                                    }
                                }
                                Ok(success) => EndpointProbe {
                                    connected: true,
                                    template: None,
                                    mining_job_token: None,
                                    error: Some(format!(
                                        "job_declarator: token response request_id {} did not match {}",
                                        success.request_id, request.request_id
                                    )),
                                },
                                Err(error) => EndpointProbe {
                                    connected: true,
                                    template: None,
                                    mining_job_token: None,
                                    error: Some(format!(
                                        "job_declarator: failed to parse token response: {}",
                                        error
                                    )),
                                },
                            }
                        }
                        job_declaration::DECLARE_MINING_JOB_ERROR => {
                            let message = DeclareMiningJobError::from_bytes(&response.payload)
                                .map(|error| error.error_code)
                                .unwrap_or_else(|_| "unparseable_error".to_string());
                            EndpointProbe {
                                connected: true,
                                template: None,
                                mining_job_token: None,
                                error: Some(format!(
                                    "job_declarator: token allocation returned error {}",
                                    message
                                )),
                            }
                        }
                        other => EndpointProbe {
                            connected: true,
                            template: None,
                            mining_job_token: None,
                            error: Some(format!(
                                "job_declarator: expected AllocateMiningJobTokenSuccess, got 0x{:02x}",
                                other
                            )),
                        },
                    },
                    Ok(Err(error)) => EndpointProbe {
                        connected: true,
                        template: None,
                        mining_job_token: None,
                        error: Some(format!("job_declarator: token response failed: {}", error)),
                    },
                    Err(_) => EndpointProbe {
                        connected: true,
                        template: None,
                        mining_job_token: None,
                        error: Some("job_declarator: token response timed out".to_string()),
                    },
                }
            }
            Err(error) => EndpointProbe::err(format!("job_declarator: {}", error)),
        }
    }

    async fn setup_endpoint(
        &self,
        label: &str,
        url: &str,
        protocol: u8,
        flags: u32,
    ) -> Result<JdConn, String> {
        let endpoint = parse_tcp_endpoint(url)?;
        let addr = endpoint.socket_addr();

        // Noise_NX handshake runs here (secure by default). The only
        // cleartext exceptions are a loopback Template Provider and the
        // explicit DCENT_SV2_INSECURE_NO_NOISE opt-out (both logged).
        let is_template_provider = protocol == PROTOCOL_TEMPLATE_DISTRIBUTION;
        let mut conn =
            JdConn::connect(label, url, &endpoint.host, &addr, is_template_provider).await?;

        let setup = SetupConnection {
            protocol,
            min_version: 2,
            max_version: 2,
            flags,
            endpoint_host: endpoint.host,
            endpoint_port: endpoint.port,
            vendor: "D-Central Technologies".to_string(),
            hardware_version: "DCENT_OS".to_string(),
            firmware: "dcentrald".to_string(),
            device_id: "dcentos-jdc".to_string(),
        };
        let payload = setup.to_bytes();
        let frame = Sv2Frame::new(EXTENSION_TYPE_CORE, common::SETUP_CONNECTION, payload);
        conn.write_frame(&frame.to_bytes())
            .await
            .map_err(|error| format!("{} setup write failed: {}", label, error))?;

        let response = tokio::time::timeout(std::time::Duration::from_secs(2), conn.read_frame())
            .await
            .map_err(|_| format!("{} setup response timed out", label))?
            .map_err(|error| format!("{} setup response failed: {}", label, error))?;
        match response.header.msg_type {
            common::SETUP_CONNECTION_SUCCESS => {
                info!(
                    endpoint = label,
                    encrypted = conn.is_encrypted(),
                    "JD: SetupConnection.Success ({})",
                    if conn.is_encrypted() {
                        "Noise-encrypted"
                    } else {
                        "cleartext (trusted-local / insecure opt-out)"
                    }
                );
                Ok(conn)
            }
            common::SETUP_CONNECTION_ERROR => {
                Err(format!("{} returned SetupConnectionError", label))
            }
            other => Err(format!(
                "{} returned unexpected setup message type 0x{:02x}",
                label, other
            )),
        }
    }

    fn token_user_identifier(&self) -> String {
        let user = self.config.coinbase_output_address.trim();
        if user.is_empty() {
            "dcentos-jdc".to_string()
        } else {
            user.to_string()
        }
    }
}

#[derive(Debug)]
struct EndpointProbe {
    connected: bool,
    template: Option<TemplateProbe>,
    mining_job_token: Option<JdTokenProbe>,
    error: Option<String>,
}

impl EndpointProbe {
    fn err(error: String) -> Self {
        Self {
            connected: false,
            template: None,
            mining_job_token: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone)]
struct JdTokenProbe {
    mining_job_token: Vec<u8>,
    coinbase_tx_outputs: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct TemplateProbe {
    template_id: Option<u64>,
    new_template_id: Option<u64>,
    prev_hash_template_id: Option<u64>,
    new_template_seen: bool,
    prev_hash_seen: bool,
    current_tx_count: Option<u32>,
    coinbase_value_remaining_sats: Option<u64>,
    coinbase_output_count: Option<u32>,
    new_template: Option<NewTemplate>,
    prev_hash: Option<TemplateSetNewPrevHash>,
}

impl TemplateProbe {
    fn note_new_template(&mut self, template: &NewTemplate) {
        self.template_id = Some(template.template_id);
        self.new_template_id = Some(template.template_id);
        self.new_template_seen = true;
        self.current_tx_count = Some(
            template
                .merkle_path
                .len()
                .saturating_add(1)
                .min(u32::MAX as usize) as u32,
        );
        self.coinbase_value_remaining_sats = Some(template.coinbase_tx_value_remaining);
        self.coinbase_output_count = Some(template.coinbase_tx_outputs_count);
        self.new_template = Some(template.clone());
    }

    fn note_prev_hash(&mut self, prev_hash: &TemplateSetNewPrevHash) {
        if self.template_id.is_none() {
            self.template_id = Some(prev_hash.template_id);
        }
        self.prev_hash_template_id = Some(prev_hash.template_id);
        self.prev_hash_seen = true;
        self.prev_hash = Some(prev_hash.clone());
    }

    fn prev_hash_matches_template(&self) -> bool {
        matches!(
            (self.new_template_id, self.prev_hash_template_id),
            (Some(new_template_id), Some(prev_hash_template_id))
                if new_template_id == prev_hash_template_id
        )
    }

    fn into_option(self) -> Option<Self> {
        if self.new_template_seen || self.prev_hash_seen {
            Some(self)
        } else {
            None
        }
    }

    fn to_custom_job_candidate(
        &self,
        token: &JdTokenProbe,
        coinbase_value_remaining: u64,
    ) -> CodecResult<CustomJobCandidate> {
        if !self.prev_hash_matches_template() {
            return Err(
                "template_provider: NewTemplate and SetNewPrevHash template IDs differ".to_string(),
            );
        }
        let template = self
            .new_template
            .as_ref()
            .ok_or_else(|| "template_provider: missing NewTemplate payload".to_string())?;
        let prev_hash = self
            .prev_hash
            .as_ref()
            .ok_or_else(|| "template_provider: missing SetNewPrevHash payload".to_string())?;
        let coinbase_tx_outputs = build_custom_coinbase_outputs(
            &token.coinbase_tx_outputs,
            coinbase_value_remaining,
            template.coinbase_tx_outputs_count,
            &template.coinbase_tx_outputs,
        )?;

        Ok(CustomJobCandidate {
            template_id: template.template_id,
            mining_job_token: token.mining_job_token.clone(),
            version: template.version,
            prev_hash: prev_hash.prev_hash,
            min_ntime: prev_hash.header_timestamp,
            nbits: prev_hash.nbits,
            target: prev_hash.target,
            coinbase_tx_version: template.coinbase_tx_version,
            coinbase_prefix: template.coinbase_prefix.clone(),
            coinbase_tx_input_nsequence: template.coinbase_tx_input_sequence,
            coinbase_tx_outputs,
            coinbase_tx_locktime: template.coinbase_tx_locktime,
            merkle_path: template.merkle_path.clone(),
            tx_count: template
                .merkle_path
                .len()
                .saturating_add(1)
                .min(u32::MAX as usize) as u32,
            coinbase_value_remaining_sats: coinbase_value_remaining,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TcpEndpoint {
    host: String,
    port: u16,
}

impl TcpEndpoint {
    fn socket_addr(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn parse_tcp_endpoint(url: &str) -> Result<TcpEndpoint, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("endpoint URL is empty".to_string());
    }

    let mut scheme = "tcp";
    let mut host_port = trimmed;
    for prefix in [
        "sv2+tcp://",
        "stratum2+tcp://",
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

    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return Err(format!(
                "endpoint URL '{}' has an invalid IPv6 host",
                trimmed
            ));
        };
        let port = suffix
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .or_else(|| jd_default_port(scheme))
            .ok_or_else(|| {
                format!(
                    "endpoint URL '{}' is missing a port for scheme '{}'",
                    trimmed, scheme
                )
            })?;
        (host.to_string(), port)
    } else if let Some((host, port_text)) = host_port.rsplit_once(':') {
        if host.contains(':') {
            return Err(format!(
                "endpoint URL '{}' has an IPv6 host without brackets",
                trimmed
            ));
        }
        let port = port_text
            .parse::<u16>()
            .map_err(|_| format!("endpoint URL '{}' has an invalid port", trimmed))?;
        (host.to_string(), port)
    } else {
        let port = jd_default_port(scheme).ok_or_else(|| {
            format!(
                "endpoint URL '{}' is missing a port for scheme '{}'",
                trimmed, scheme
            )
        })?;
        (host_port.to_string(), port)
    };

    if host.trim().is_empty() {
        return Err(format!("endpoint URL '{}' is missing a host", trimmed));
    }
    Ok(TcpEndpoint { host, port })
}

fn jd_default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "tcp" | "sv2" | "sv2+tcp" | "stratum2+tcp" => Some(34255),
        "tp+tcp" => Some(8442),
        "jd+tcp" => Some(8443),
        _ => None,
    }
}

fn build_custom_coinbase_outputs(
    pool_outputs_compact: &[u8],
    coinbase_value_remaining: u64,
    template_outputs_count: u32,
    template_outputs: &[u8],
) -> CodecResult<Vec<u8>> {
    let (pool_count, pool_outputs_offset) = if pool_outputs_compact.is_empty() {
        (0u64, 0usize)
    } else {
        read_compact_size(pool_outputs_compact, 0)?
    };
    let mut pool_outputs = pool_outputs_compact
        .get(pool_outputs_offset..)
        .ok_or_else(|| "JDS coinbase outputs compact-size offset is out of bounds".to_string())?
        .to_vec();

    if pool_count > 0 {
        if pool_outputs.len() < 8 {
            return Err("JDS coinbase output is too short for value allocation".to_string());
        }
        pool_outputs[0..8].copy_from_slice(&coinbase_value_remaining.to_le_bytes());
    }

    let total_count = pool_count
        .checked_add(u64::from(template_outputs_count))
        .ok_or_else(|| "coinbase output count overflow".to_string())?;
    let mut out = Vec::new();
    write_compact_size(&mut out, total_count)?;
    out.extend_from_slice(&pool_outputs);
    out.extend_from_slice(template_outputs);
    if out.len() > u16::MAX as usize {
        return Err("SetCustomMiningJob coinbase_tx_outputs exceeds B0_64K".to_string());
    }
    Ok(out)
}

fn read_compact_size(data: &[u8], offset: usize) -> CodecResult<(u64, usize)> {
    let Some(first) = data.get(offset).copied() else {
        return Err("missing CompactSize value".to_string());
    };
    match first {
        n @ 0x00..=0xfc => Ok((u64::from(n), offset + 1)),
        0xfd => {
            let end = offset + 3;
            if data.len() < end {
                return Err("truncated CompactSize u16".to_string());
            }
            Ok((
                u64::from(u16::from_le_bytes([data[offset + 1], data[offset + 2]])),
                end,
            ))
        }
        0xfe => {
            let end = offset + 5;
            if data.len() < end {
                return Err("truncated CompactSize u32".to_string());
            }
            Ok((
                u64::from(u32::from_le_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                ])),
                end,
            ))
        }
        0xff => {
            let end = offset + 9;
            if data.len() < end {
                return Err("truncated CompactSize u64".to_string());
            }
            Ok((
                u64::from_le_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                    data[offset + 8],
                ]),
                end,
            ))
        }
    }
}

fn write_compact_size(buf: &mut Vec<u8>, value: u64) -> CodecResult<()> {
    if value <= 0xfc {
        buf.push(value as u8);
    } else if value <= u16::MAX as u64 {
        buf.push(0xfd);
        buf.extend_from_slice(&(value as u16).to_le_bytes());
    } else if value <= u32::MAX as u64 {
        buf.push(0xfe);
        buf.extend_from_slice(&(value as u32).to_le_bytes());
    } else {
        buf.push(0xff);
        buf.extend_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

async fn read_frame(stream: &mut tokio::net::TcpStream) -> Result<Sv2Frame, String> {
    let mut header_buf = [0u8; FRAME_HEADER_SIZE];
    stream
        .read_exact(&mut header_buf)
        .await
        .map_err(|error| error.to_string())?;
    let header = Sv2FrameHeader::from_bytes(&header_buf).map_err(|error| format!("{:?}", error))?;
    let mut payload = vec![0u8; header.payload_len as usize];
    if !payload.is_empty() {
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(Sv2Frame { header, payload })
}

fn unix_now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Encrypted SV2 header size: 6-byte plaintext header + 16-byte ChaChaPoly
/// tag. Identical to the mining client's `ENCRYPTED_HEADER_SIZE`.
const JD_ENCRYPTED_HEADER_SIZE: usize = FRAME_HEADER_SIZE + 16;

/// A JD/TDP connection that transparently applies SV2 Noise transport.
///
/// # Why JD must be encrypted too
///
/// The Job Declaration and Template Distribution protocols are SV2
/// sub-protocols and the SV2 spec runs them over the *same* `Noise_NX`
/// transport as the mining channel. Before this type, `JdClient` connected
/// to the (potentially remote) Job Declarator and Template Provider in
/// **cleartext** — a JD token / declared-job stream that an active
/// attacker could read or rewrite. `JdConn` closes that gap: it performs
/// the initiator Noise_NX handshake before the first SV2 frame and
/// AEAD-frames every subsequent message (header block + payload block,
/// exactly like `client.rs::send_noise_frame`).
///
/// # Local Template Provider exception
///
/// A Template Provider that is the operator's own `bitcoind`/SRI-TP on
/// loopback is a trusted local socket; SRI commonly exposes the TP without
/// Noise there. `JdConn::connect` therefore allows **plaintext only for a
/// loopback Template Provider** (and only the TP), and logs that decision.
/// Every other endpoint (remote TP, any Job Declarator) gets Noise. The
/// explicit `DCENT_SV2_INSECURE_NO_NOISE` operator opt-out forces cleartext
/// everywhere (loud `tracing::error!`), for lab/mock use only.
struct JdConn {
    stream: tokio::net::TcpStream,
    /// `Some` ⇒ Noise transport active; `None` ⇒ trusted-local cleartext.
    noise: Option<NoiseSession>,
    recv_buf: Vec<u8>,
}

impl JdConn {
    /// Connect, then run the SV2 Noise_NX handshake unless this is the
    /// documented trusted-local-TP exception or the explicit insecure
    /// opt-out is set.
    ///
    /// * `is_template_provider` — the local-plaintext exception only ever
    ///   applies to the TP, never to a Job Declarator.
    async fn connect(
        label: &str,
        url: &str,
        host: &str,
        addr: &str,
        is_template_provider: bool,
    ) -> Result<Self, String> {
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        .map_err(|_| format!("{} connect timed out", label))?
        .map_err(|error| format!("{} connect failed: {}", label, error))?;
        let _ = stream.set_nodelay(true);

        let host_is_loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false);

        if sv2_insecure_no_noise() {
            warn!(
                endpoint = label,
                "JD: DCENT_SV2_INSECURE_NO_NOISE set — connecting in CLEARTEXT (lab/mock only)"
            );
            return Ok(Self {
                stream,
                noise: None,
                recv_buf: Vec::new(),
            });
        }

        if is_template_provider && host_is_loopback && !sv2_tp_require_noise() {
            info!(
                endpoint = label,
                %host,
                "JD: local loopback Template Provider — trusted cleartext (documented SV2 exception; set DCENT_SV2_TP_REQUIRE_NOISE=1 to force Noise)"
            );
            return Ok(Self {
                stream,
                noise: None,
                recv_buf: Vec::new(),
            });
        }

        // Secure path: Noise_NX handshake, with authority-key pinning from
        // the URL when present (TOFU + WARN otherwise — same posture as the
        // mining client).
        let mut noise = NoiseSession::new();
        match parse_authority_key_from_sv2_url(url) {
            Ok(key) => {
                noise.pool_authority_key = Some(*key.as_bytes());
                info!(
                    endpoint = label,
                    "JD: pinned authority key from URL — server certificate will be verified"
                );
            }
            Err(AuthorityKeyError::NotPresent) => {
                warn!(
                    endpoint = label,
                    "JD: no authority key in URL — Noise handshake in TOFU mode (MITM possible)"
                );
            }
            Err(e) => {
                return Err(format!(
                    "{}: URL carries an invalid authority key ({}). Refusing to connect.",
                    label, e
                ));
            }
        }

        let mut rng_seed = [0u8; 64];
        {
            use rand_core::RngCore;
            rand_core::OsRng.fill_bytes(&mut rng_seed);
        }
        let act1 = noise
            .initiator_handshake_start(rng_seed)
            .map_err(|e| format!("{}: Noise handshake start failed: {}", label, e))?;
        stream
            .write_all(&act1)
            .await
            .map_err(|e| format!("{}: send Noise act1 failed: {}", label, e))?;

        let mut hs_buf = vec![0u8; 512];
        let hs_len =
            tokio::time::timeout(std::time::Duration::from_secs(15), stream.read(&mut hs_buf))
                .await
                .map_err(|_| format!("{}: Noise handshake timeout", label))?
                .map_err(|e| format!("{}: read Noise response failed: {}", label, e))?;
        if hs_len == 0 {
            return Err(format!(
                "{}: server closed connection during Noise handshake",
                label
            ));
        }
        noise
            .initiator_handshake_finish(&hs_buf[..hs_len])
            .map_err(|e| format!("{}: Noise handshake finish failed: {}", label, e))?;
        info!(
            endpoint = label,
            "JD: Noise_NX handshake COMPLETE — encrypted transport active"
        );

        Ok(Self {
            stream,
            noise: Some(noise),
            recv_buf: Vec::new(),
        })
    }

    /// Whether this connection is encrypted (Noise transport active).
    fn is_encrypted(&self) -> bool {
        self.noise
            .as_ref()
            .map(|n| !n.is_plaintext_passthrough())
            .unwrap_or(false)
    }

    /// Write one SV2 frame, applying Noise framing (separate header +
    /// payload AEAD blocks) when the transport is encrypted.
    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        if frame.len() < FRAME_HEADER_SIZE {
            return Err("JD: frame too short for header".into());
        }
        match self.noise.as_mut() {
            Some(noise) => {
                let header = &frame[..FRAME_HEADER_SIZE];
                let payload = &frame[FRAME_HEADER_SIZE..];
                let enc_h = noise
                    .encrypt(header)
                    .map_err(|e| format!("JD: encrypt header: {}", e))?;
                let enc_p = noise
                    .encrypt(payload)
                    .map_err(|e| format!("JD: encrypt payload: {}", e))?;
                self.stream
                    .write_all(&enc_h)
                    .await
                    .map_err(|e| e.to_string())?;
                self.stream
                    .write_all(&enc_p)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            None => {
                self.stream
                    .write_all(frame)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Read one SV2 frame, transparently decrypting when Noise is active.
    async fn read_frame(&mut self) -> Result<Sv2Frame, String> {
        match self.noise.is_some() {
            false => read_frame(&mut self.stream).await,
            true => self.read_encrypted_frame().await,
        }
    }

    async fn read_encrypted_frame(&mut self) -> Result<Sv2Frame, String> {
        // Decrypt the 6-byte header block (22 bytes on the wire).
        let header = self.read_decrypt_block(JD_ENCRYPTED_HEADER_SIZE).await?;
        let fh = Sv2FrameHeader::from_bytes(&header).map_err(|e| format!("{:?}", e))?;
        let enc_payload_len = fh.payload_len as usize + 16;
        let payload = if fh.payload_len == 0 {
            // A zero-length payload still arrives as a 16-byte AEAD tag.
            self.read_decrypt_block(16).await?
        } else {
            self.read_decrypt_block(enc_payload_len).await?
        };
        Ok(Sv2Frame {
            header: fh,
            payload,
        })
    }

    /// Read exactly `enc_len` ciphertext bytes (buffering across TCP reads)
    /// and return the decrypted plaintext.
    async fn read_decrypt_block(&mut self, enc_len: usize) -> Result<Vec<u8>, String> {
        let mut tmp = [0u8; 4096];
        while self.recv_buf.len() < enc_len {
            let n = self
                .stream
                .read(&mut tmp)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("JD: connection closed mid-frame".into());
            }
            self.recv_buf.extend_from_slice(&tmp[..n]);
        }
        let block: Vec<u8> = self.recv_buf.drain(..enc_len).collect();
        self.noise
            .as_mut()
            .ok_or("JD: read_decrypt_block without noise")?
            .decrypt(&block)
            .map_err(|e| format!("JD: decrypt block: {}", e))
    }
}

/// Template Provider coinbase-reservation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinbaseOutputConstraints {
    pub max_additional_size: u32,
    pub max_additional_sigops: u16,
}

impl CoinbaseOutputConstraints {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(6);
        buf.extend_from_slice(&self.max_additional_size.to_le_bytes());
        buf.extend_from_slice(&self.max_additional_sigops.to_le_bytes());
        buf
    }

    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            max_additional_size: r.u32()?,
            max_additional_sigops: r.u16()?,
        })
    }

    pub fn to_frame(&self) -> Vec<u8> {
        core_frame(
            template_distribution::COINBASE_OUTPUT_CONSTRAINTS,
            self.to_bytes(),
        )
    }
}

/// JDC -> JDS request for a token that can identify future custom work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocateMiningJobToken {
    pub user_identifier: String,
    pub request_id: u32,
}

impl AllocateMiningJobToken {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        write_str_255(&mut buf, &self.user_identifier)?;
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        Ok(buf)
    }

    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            user_identifier: r.str_255()?,
            request_id: r.u32()?,
        })
    }

    pub fn to_frame(&self) -> CodecResult<Vec<u8>> {
        Ok(core_frame(
            job_declaration::ALLOCATE_MINING_JOB_TOKEN,
            self.to_bytes()?,
        ))
    }
}

/// JDS -> JDC token allocation response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocateMiningJobTokenSuccess {
    pub request_id: u32,
    pub mining_job_token: Vec<u8>,
    pub coinbase_tx_outputs: Vec<u8>,
}

impl AllocateMiningJobTokenSuccess {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        write_b0_255(&mut buf, &self.mining_job_token)?;
        write_b0_64k(&mut buf, &self.coinbase_tx_outputs)?;
        Ok(buf)
    }

    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            request_id: r.u32()?,
            mining_job_token: r.b0_255()?,
            coinbase_tx_outputs: r.b0_64k()?,
        })
    }
}

/// Full-template JD declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareMiningJob {
    pub request_id: u32,
    pub mining_job_token: Vec<u8>,
    pub version: u32,
    pub coinbase_tx_prefix: Vec<u8>,
    pub coinbase_tx_suffix: Vec<u8>,
    pub wtxid_list: Vec<[u8; 32]>,
    pub excess_data: Vec<u8>,
}

impl DeclareMiningJob {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        write_b0_255(&mut buf, &self.mining_job_token)?;
        buf.extend_from_slice(&self.version.to_le_bytes());
        write_b0_64k(&mut buf, &self.coinbase_tx_prefix)?;
        write_b0_64k(&mut buf, &self.coinbase_tx_suffix)?;
        write_seq_u256_64k(&mut buf, &self.wtxid_list)?;
        write_b0_64k(&mut buf, &self.excess_data)?;
        Ok(buf)
    }

    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            request_id: r.u32()?,
            mining_job_token: r.b0_255()?,
            version: r.u32()?,
            coinbase_tx_prefix: r.b0_64k()?,
            coinbase_tx_suffix: r.b0_64k()?,
            wtxid_list: r.seq_u256_64k()?,
            excess_data: r.b0_64k()?,
        })
    }

    pub fn to_frame(&self) -> CodecResult<Vec<u8>> {
        Ok(core_frame(
            job_declaration::DECLARE_MINING_JOB,
            self.to_bytes()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareMiningJobSuccess {
    pub request_id: u32,
    pub new_mining_job_token: Vec<u8>,
}

impl DeclareMiningJobSuccess {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            request_id: r.u32()?,
            new_mining_job_token: r.b0_255()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareMiningJobError {
    pub request_id: u32,
    pub error_code: String,
    pub error_details: Vec<u8>,
}

impl DeclareMiningJobError {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            request_id: r.u32()?,
            error_code: r.str_255()?,
            error_details: r.b0_64k()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvideMissingTransactions {
    pub request_id: u32,
    pub unknown_tx_position_list: Vec<u16>,
}

impl ProvideMissingTransactions {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            request_id: r.u32()?,
            unknown_tx_position_list: r.seq_u16_64k()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvideMissingTransactionsSuccess {
    pub request_id: u32,
    pub transaction_list: Vec<Vec<u8>>,
}

impl ProvideMissingTransactionsSuccess {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        write_seq_b0_16m_64k(&mut buf, &self.transaction_list)?;
        Ok(buf)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSolution {
    pub extranonce: Vec<u8>,
    pub prev_hash: [u8; 32],
    pub nonce: u32,
    pub ntime: u32,
    pub nbits: u32,
    pub version: u32,
}

impl PushSolution {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        write_b0_32(&mut buf, &self.extranonce)?;
        buf.extend_from_slice(&self.prev_hash);
        buf.extend_from_slice(&self.nonce.to_le_bytes());
        buf.extend_from_slice(&self.ntime.to_le_bytes());
        buf.extend_from_slice(&self.nbits.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        Ok(buf)
    }
}

/// Template Distribution `NewTemplate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTemplate {
    pub template_id: u64,
    pub future_template: bool,
    pub version: u32,
    pub coinbase_tx_version: u32,
    pub coinbase_prefix: Vec<u8>,
    pub coinbase_tx_input_sequence: u32,
    pub coinbase_tx_value_remaining: u64,
    pub coinbase_tx_outputs_count: u32,
    pub coinbase_tx_outputs: Vec<u8>,
    pub coinbase_tx_locktime: u32,
    pub merkle_path: Vec<[u8; 32]>,
}

impl NewTemplate {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            template_id: r.u64()?,
            future_template: r.bool()?,
            version: r.u32()?,
            coinbase_tx_version: r.u32()?,
            coinbase_prefix: r.b0_255()?,
            coinbase_tx_input_sequence: r.u32()?,
            coinbase_tx_value_remaining: r.u64()?,
            coinbase_tx_outputs_count: r.u32()?,
            coinbase_tx_outputs: r.b0_64k()?,
            coinbase_tx_locktime: r.u32()?,
            merkle_path: r.seq_u256_255()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSetNewPrevHash {
    pub template_id: u64,
    pub prev_hash: [u8; 32],
    pub header_timestamp: u32,
    pub nbits: u32,
    pub target: [u8; 32],
}

impl TemplateSetNewPrevHash {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            template_id: r.u64()?,
            prev_hash: r.u256()?,
            header_timestamp: r.u32()?,
            nbits: r.u32()?,
            target: r.u256()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTransactionData {
    pub template_id: u64,
}

impl RequestTransactionData {
    pub fn to_bytes(&self) -> Vec<u8> {
        self.template_id.to_le_bytes().to_vec()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTransactionDataSuccess {
    pub template_id: u64,
    pub excess_data: Vec<u8>,
    pub transaction_list: Vec<Vec<u8>>,
}

impl RequestTransactionDataSuccess {
    pub fn from_bytes(data: &[u8]) -> CodecResult<Self> {
        let mut r = Reader::new(data);
        Ok(Self {
            template_id: r.u64()?,
            excess_data: r.b0_64k()?,
            transaction_list: r.seq_b0_16m_64k()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitSolution {
    pub template_id: u64,
    pub version: u32,
    pub header_timestamp: u32,
    pub header_nonce: u32,
    pub coinbase_tx: Vec<u8>,
}

impl SubmitSolution {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.template_id.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.header_timestamp.to_le_bytes());
        buf.extend_from_slice(&self.header_nonce.to_le_bytes());
        write_b0_64k(&mut buf, &self.coinbase_tx)?;
        Ok(buf)
    }
}

/// Mining Protocol `SetCustomMiningJob`; emitted by a JDC/proxy after it has a
/// token and template it wants the pool to reward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetCustomMiningJob {
    pub channel_id: u32,
    pub request_id: u32,
    pub mining_job_token: Vec<u8>,
    pub version: u32,
    pub prev_hash: [u8; 32],
    pub min_ntime: u32,
    pub nbits: u32,
    pub coinbase_tx_version: u32,
    pub coinbase_prefix: Vec<u8>,
    pub coinbase_tx_input_nsequence: u32,
    pub coinbase_tx_outputs: Vec<u8>,
    pub coinbase_tx_locktime: u32,
    pub merkle_path: Vec<[u8; 32]>,
}

impl SetCustomMiningJob {
    pub fn to_bytes(&self) -> CodecResult<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.channel_id.to_le_bytes());
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        write_b0_255(&mut buf, &self.mining_job_token)?;
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.prev_hash);
        buf.extend_from_slice(&self.min_ntime.to_le_bytes());
        buf.extend_from_slice(&self.nbits.to_le_bytes());
        buf.extend_from_slice(&self.coinbase_tx_version.to_le_bytes());
        write_b0_255(&mut buf, &self.coinbase_prefix)?;
        buf.extend_from_slice(&self.coinbase_tx_input_nsequence.to_le_bytes());
        write_b0_64k(&mut buf, &self.coinbase_tx_outputs)?;
        buf.extend_from_slice(&self.coinbase_tx_locktime.to_le_bytes());
        write_seq_u256_255(&mut buf, &self.merkle_path)?;
        Ok(buf)
    }

    pub fn to_frame(&self) -> CodecResult<Vec<u8>> {
        Ok(core_frame(mining::SET_CUSTOM_MINING_JOB, self.to_bytes()?))
    }
}

pub fn core_frame(msg_type: u8, payload: Vec<u8>) -> Vec<u8> {
    Sv2Frame::new(EXTENSION_TYPE_CORE, msg_type, payload).to_bytes()
}

fn write_str_255(buf: &mut Vec<u8>, value: &str) -> CodecResult<()> {
    let bytes = value.as_bytes();
    if bytes.len() > u8::MAX as usize {
        return Err("STR0_255 value too long".to_string());
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

fn write_b0_32(buf: &mut Vec<u8>, value: &[u8]) -> CodecResult<()> {
    if value.len() > 32 {
        return Err("B0_32 value too long".to_string());
    }
    buf.push(value.len() as u8);
    buf.extend_from_slice(value);
    Ok(())
}

fn write_b0_255(buf: &mut Vec<u8>, value: &[u8]) -> CodecResult<()> {
    if value.len() > u8::MAX as usize {
        return Err("B0_255 value too long".to_string());
    }
    buf.push(value.len() as u8);
    buf.extend_from_slice(value);
    Ok(())
}

fn write_b0_64k(buf: &mut Vec<u8>, value: &[u8]) -> CodecResult<()> {
    if value.len() > u16::MAX as usize {
        return Err("B0_64K value too long".to_string());
    }
    buf.extend_from_slice(&(value.len() as u16).to_le_bytes());
    buf.extend_from_slice(value);
    Ok(())
}

fn write_b0_16m(buf: &mut Vec<u8>, value: &[u8]) -> CodecResult<()> {
    if value.len() > 0xFF_FFFF {
        return Err("B0_16M value too long".to_string());
    }
    let len = value.len() as u32;
    let len_bytes = len.to_le_bytes();
    buf.extend_from_slice(&len_bytes[..3]);
    buf.extend_from_slice(value);
    Ok(())
}

fn write_seq_u256_255(buf: &mut Vec<u8>, values: &[[u8; 32]]) -> CodecResult<()> {
    if values.len() > u8::MAX as usize {
        return Err("SEQ0_255[U256] too long".to_string());
    }
    buf.push(values.len() as u8);
    for value in values {
        buf.extend_from_slice(value);
    }
    Ok(())
}

fn write_seq_u256_64k(buf: &mut Vec<u8>, values: &[[u8; 32]]) -> CodecResult<()> {
    if values.len() > u16::MAX as usize {
        return Err("SEQ0_64K[U256] too long".to_string());
    }
    buf.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for value in values {
        buf.extend_from_slice(value);
    }
    Ok(())
}

fn write_seq_b0_16m_64k(buf: &mut Vec<u8>, values: &[Vec<u8>]) -> CodecResult<()> {
    if values.len() > u16::MAX as usize {
        return Err("SEQ0_64K[B0_16M] too long".to_string());
    }
    buf.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for value in values {
        write_b0_16m(buf, value)?;
    }
    Ok(())
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn take(&mut self, len: usize) -> CodecResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| "SV2 buffer offset overflow".to_string())?;
        if end > self.data.len() {
            return Err(format!(
                "SV2 buffer too short: need {} bytes at {}, have {}",
                len,
                self.offset,
                self.data.len().saturating_sub(self.offset)
            ));
        }
        let out = &self.data[self.offset..end];
        self.offset = end;
        Ok(out)
    }

    fn u16(&mut self) -> CodecResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> CodecResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> CodecResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn bool(&mut self) -> CodecResult<bool> {
        let b = self.take(1)?[0];
        match b {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("invalid SV2 bool value {}", other)),
        }
    }

    fn u256(&mut self) -> CodecResult<[u8; 32]> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    fn str_255(&mut self) -> CodecResult<String> {
        let bytes = self.b0_255()?;
        String::from_utf8(bytes).map_err(|_| "STR0_255 is not valid UTF-8".to_string())
    }

    fn b0_255(&mut self) -> CodecResult<Vec<u8>> {
        let len = self.take(1)?[0] as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn b0_64k(&mut self) -> CodecResult<Vec<u8>> {
        let len = self.u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn b0_16m(&mut self) -> CodecResult<Vec<u8>> {
        let b = self.take(3)?;
        let len = u32::from_le_bytes([b[0], b[1], b[2], 0]) as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn seq_u16_64k(&mut self) -> CodecResult<Vec<u16>> {
        let len = self.u16()? as usize;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.u16()?);
        }
        Ok(out)
    }

    fn seq_u256_255(&mut self) -> CodecResult<Vec<[u8; 32]>> {
        let len = self.take(1)?[0] as usize;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.u256()?);
        }
        Ok(out)
    }

    fn seq_u256_64k(&mut self) -> CodecResult<Vec<[u8; 32]>> {
        let len = self.u16()? as usize;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.u256()?);
        }
        Ok(out)
    }

    fn seq_b0_16m_64k(&mut self) -> CodecResult<Vec<Vec<u8>>> {
        let len = self.u16()? as usize;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.b0_16m()?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{PROTOCOL_JOB_DECLARATION, PROTOCOL_TEMPLATE_DISTRIBUTION};
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn jd_message_decoders_never_panic_on_arbitrary_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..2048)
        ) {
            let _ = CoinbaseOutputConstraints::from_bytes(&data);
            let _ = AllocateMiningJobToken::from_bytes(&data);
            let _ = AllocateMiningJobTokenSuccess::from_bytes(&data);
            let _ = DeclareMiningJob::from_bytes(&data);
            let _ = DeclareMiningJobSuccess::from_bytes(&data);
            let _ = DeclareMiningJobError::from_bytes(&data);
            let _ = ProvideMissingTransactions::from_bytes(&data);
            let _ = NewTemplate::from_bytes(&data);
            let _ = TemplateSetNewPrevHash::from_bytes(&data);
            let _ = RequestTransactionDataSuccess::from_bytes(&data);
        }

        #[test]
        fn jd_endpoint_parser_never_panics_on_arbitrary_text(url in ".{0,512}") {
            let _ = parse_tcp_endpoint(&url);
        }
    }

    #[test]
    fn protocol_ids_match_spec() {
        assert_eq!(PROTOCOL_JOB_DECLARATION, 1);
        assert_eq!(PROTOCOL_TEMPLATE_DISTRIBUTION, 2);
        assert_eq!(job_declaration::DECLARE_MINING_JOB, 0x57);
        assert_eq!(template_distribution::NEW_TEMPLATE, 0x71);
    }

    #[test]
    fn coinbase_constraints_round_trip() {
        let msg = CoinbaseOutputConstraints {
            max_additional_size: 900,
            max_additional_sigops: 2,
        };
        let parsed = CoinbaseOutputConstraints::from_bytes(&msg.to_bytes()).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn allocate_token_round_trip() {
        let msg = AllocateMiningJobToken {
            user_identifier: "bc1qminer.worker".to_string(),
            request_id: 7,
        };
        let parsed = AllocateMiningJobToken::from_bytes(&msg.to_bytes().unwrap()).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn token_success_round_trip() {
        let msg = AllocateMiningJobTokenSuccess {
            request_id: 9,
            mining_job_token: vec![1, 2, 3, 4],
            coinbase_tx_outputs: vec![0x01, 0x00, 0x00],
        };
        let parsed = AllocateMiningJobTokenSuccess::from_bytes(&msg.to_bytes().unwrap()).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn declare_job_round_trip() {
        let msg = DeclareMiningJob {
            request_id: 11,
            mining_job_token: vec![0xaa; 8],
            version: 0x2000_0000,
            coinbase_tx_prefix: vec![0x01, 0x02],
            coinbase_tx_suffix: vec![0x03, 0x04],
            wtxid_list: vec![[0x10; 32], [0x20; 32]],
            excess_data: vec![0x99],
        };
        let parsed = DeclareMiningJob::from_bytes(&msg.to_bytes().unwrap()).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn template_transaction_data_success_parses_b0_16m_sequence() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&42u64.to_le_bytes());
        write_b0_64k(&mut payload, &[0xaa, 0xbb]).unwrap();
        write_seq_b0_16m_64k(&mut payload, &[vec![0x01; 12], vec![0x02; 3]]).unwrap();

        let parsed = RequestTransactionDataSuccess::from_bytes(&payload).unwrap();
        assert_eq!(parsed.template_id, 42);
        assert_eq!(parsed.excess_data, vec![0xaa, 0xbb]);
        assert_eq!(parsed.transaction_list.len(), 2);
        assert_eq!(parsed.transaction_list[0], vec![0x01; 12]);
    }

    #[test]
    fn custom_mining_job_frame_uses_core_extension_type() {
        let msg = SetCustomMiningJob {
            channel_id: 5,
            request_id: 6,
            mining_job_token: vec![0x22],
            version: 0x2000_0000,
            prev_hash: [0x11; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
            coinbase_tx_version: 2,
            coinbase_prefix: vec![0x01],
            coinbase_tx_input_nsequence: 0xffff_fffe,
            coinbase_tx_outputs: vec![0x02, 0x03],
            coinbase_tx_locktime: 0,
            merkle_path: vec![[0x44; 32]],
        };
        let frame = msg.to_frame().unwrap();
        assert_eq!(&frame[0..2], &EXTENSION_TYPE_CORE.to_le_bytes());
        assert_eq!(frame[2], mining::SET_CUSTOM_MINING_JOB);
    }

    #[test]
    fn parses_sv2_tcp_endpoints() {
        assert_eq!(
            parse_tcp_endpoint("sv2+tcp://127.0.0.1:8442").unwrap(),
            TcpEndpoint {
                host: "127.0.0.1".to_string(),
                port: 8442,
            }
        );
        assert_eq!(
            parse_tcp_endpoint("jd+tcp://[::1]").unwrap(),
            TcpEndpoint {
                host: "::1".to_string(),
                port: 8443,
            }
        );
    }

    /// JD/TDP live-proof harness (encrypted path).
    ///
    /// Drives the real `JdClient::probe_once` against two in-process mock
    /// servers (Template Provider + Job Declarator) that each run the
    /// **server-side Noise_NX handshake** and frame every SV2 message
    /// through ChaChaPoly. This proves end-to-end that:
    ///   * the handshake is encrypted (a plaintext server would fail to
    ///     read the client's `-> e` / produce a parseable response — the
    ///     mock only succeeds because it speaks real Noise),
    ///   * `AllocateMiningJobToken` → `…Success` round-trips over Noise,
    ///   * the TP `CoinbaseOutputConstraints` → `NewTemplate` /
    ///     `SetNewPrevHash` exchange round-trips over Noise,
    ///   * the JD custom-job candidate is assembled from the encrypted feed.
    ///
    /// `DCENT_SV2_TP_REQUIRE_NOISE=1` is set so the loopback-TP cleartext
    /// convenience is bypassed and the TP path is also encrypted. The env
    /// var is restored before the test returns; a process-wide guard
    /// serializes against the only other env-touching test.
    #[tokio::test]
    async fn jd_probe_round_trips_over_real_noise_transport() {
        let _guard = crate::v2::auth::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        std::env::set_var(crate::v2::auth::ENV_TP_REQUIRE_NOISE, "1");

        let tp = spawn_setup_server(PROTOCOL_TEMPLATE_DISTRIBUTION, true, false).await;
        let jd = spawn_setup_server(PROTOCOL_JOB_DECLARATION, false, true).await;

        let client = JdClient::new(JdConfig {
            enabled: true,
            template_provider_url: format!("sv2+tcp://{}", tp),
            job_declarator_url: format!("sv2+tcp://{}", jd),
            ..JdConfig::default()
        });

        let status = client.probe_once().await;

        std::env::remove_var(crate::v2::auth::ENV_TP_REQUIRE_NOISE);

        // Encrypted handshake completed on BOTH endpoints (the mock
        // servers would have errored if the client sent plaintext).
        assert!(
            status.template_provider_connected,
            "TP Noise handshake + setup must succeed"
        );
        assert!(
            status.job_declarator_connected,
            "JDS Noise handshake + setup must succeed"
        );
        assert!(status.connected);
        // Token round-trips over the encrypted JDS session.
        assert!(status.mining_job_token_available);
        // Template + prev-hash round-trip over the encrypted TP session.
        assert!(status.template_prev_hash_ready);
        assert!(status.custom_job_candidate_ready);
        assert_eq!(status.runtime_state, "custom_job_candidate_ready");
        assert_eq!(status.current_template_id, Some(42));
        assert_eq!(status.templates_constructed, 1);
        assert_eq!(status.current_tx_count, Some(1));
        assert_eq!(status.coinbase_value_remaining_sats, Some(625_000_000));
        assert_eq!(status.coinbase_output_count, Some(1));
        let candidate = status.custom_job_candidate.as_ref().unwrap();
        assert_eq!(candidate.template_id, 42);
        assert_eq!(candidate.coinbase_tx_outputs[0], 2);
        assert_eq!(
            &candidate.coinbase_tx_outputs[1..9],
            &625_000_000u64.to_le_bytes()
        );
    }

    /// Server-side Noise_NX responder for the mock JD/TP harness.
    ///
    /// Mirrors `client.rs`'s initiator handshake: reads the client `-> e`
    /// (64-byte EllSwift), runs the EllSwift `ee`/`es` ECDH against the
    /// **production** [`NoiseSession::hkdf_sha256`], sends
    /// `re || enc(static) || enc(cert)`, then frames every SV2 message
    /// through the established ChaChaPoly transport — exactly the wire the
    /// real `JdConn` produces. Proves the JD path is genuinely encrypted.
    struct MockJdServer {
        stream: tokio::net::TcpStream,
        send_key: [u8; 32],
        recv_key: [u8; 32],
        send_nonce: u64,
        recv_nonce: u64,
        recv_buf: Vec<u8>,
    }

    impl MockJdServer {
        async fn handshake(mut stream: tokio::net::TcpStream) -> Self {
            use chacha20poly1305::aead::{Aead, KeyInit, Payload};
            use chacha20poly1305::{ChaCha20Poly1305, Nonce};
            use secp256k1::ellswift::{ElligatorSwift, ElligatorSwiftParty};
            use secp256k1::{Secp256k1, SecretKey};
            use sha2::{Digest, Sha256};

            const PROTOCOL_NAME: &[u8] = b"Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";
            let sha = |d: &[u8]| {
                let mut h = Sha256::new();
                h.update(d);
                let mut o = [0u8; 32];
                o.copy_from_slice(&h.finalize());
                o
            };
            let sha_cat = |a: &[u8; 32], b: &[u8]| {
                let mut h = Sha256::new();
                h.update(a);
                h.update(b);
                let mut o = [0u8; 32];
                o.copy_from_slice(&h.finalize());
                o
            };
            let nonce = |c: u64| {
                let mut nb = [0u8; 12];
                nb[4..12].copy_from_slice(&c.to_le_bytes());
                *Nonce::from_slice(&nb)
            };

            // Read client -> e
            let mut client_e = [0u8; 64];
            stream.read_exact(&mut client_e).await.unwrap();

            let mut h = sha(PROTOCOL_NAME);
            let chaining_key = h;
            h = sha_cat(&h, b""); // mix_hash(prologue)
            h = sha_cat(&h, &client_e); // mix_hash(client_e)
            h = sha_cat(&h, b""); // EncryptAndHash(empty), k=None

            let secp = Secp256k1::new();
            let re_sk = SecretKey::from_slice(&[0xE7u8; 32]).unwrap();
            let re_es = ElligatorSwift::from_seckey(&secp, re_sk, Some([0xE8u8; 32]));
            let re_pub = re_es.to_array();
            let rs_sk = SecretKey::from_slice(&[0x5Bu8; 32]).unwrap();
            let rs_es = ElligatorSwift::from_seckey(&secp, rs_sk, Some([0x5Cu8; 32]));
            let rs_pub = rs_es.to_array();

            h = sha_cat(&h, &re_pub); // mix_hash(re_pub)

            let client_es = ElligatorSwift::from_array(client_e);
            let ee = ElligatorSwift::shared_secret(
                client_es,
                re_es,
                re_sk,
                ElligatorSwiftParty::B,
                None,
            )
            .to_secret_bytes();
            let (ck1, k_ee) = NoiseSession::hkdf_sha256(&chaining_key, &ee);

            let enc_static = {
                let c = ChaCha20Poly1305::new_from_slice(&k_ee).unwrap();
                c.encrypt(
                    &nonce(0),
                    Payload {
                        msg: &rs_pub,
                        aad: &h,
                    },
                )
                .unwrap()
            };
            h = sha_cat(&h, &enc_static);

            let es = ElligatorSwift::shared_secret(
                client_es,
                rs_es,
                rs_sk,
                ElligatorSwiftParty::B,
                None,
            )
            .to_secret_bytes();
            let (ck2, k_es) = NoiseSession::hkdf_sha256(&ck1, &es);

            // Well-formed TOFU cert (no authority key pinned in the URL).
            let mut cert = Vec::new();
            cert.extend_from_slice(&0u16.to_le_bytes());
            cert.extend_from_slice(&0u32.to_le_bytes());
            cert.extend_from_slice(&u32::MAX.to_le_bytes());
            cert.extend_from_slice(&[0xAAu8; 64]);
            let enc_cert = {
                let c = ChaCha20Poly1305::new_from_slice(&k_es).unwrap();
                c.encrypt(
                    &nonce(0),
                    Payload {
                        msg: &cert,
                        aad: &h,
                    },
                )
                .unwrap()
            };

            let (k1, k2) = NoiseSession::hkdf_sha256(&ck2, &[]);
            // Initiator sends with k1 / receives with k2 → server mirror.
            let server_recv = k1;
            let server_send = k2;

            let mut msg = Vec::new();
            msg.extend_from_slice(&re_pub);
            msg.extend_from_slice(&enc_static);
            msg.extend_from_slice(&enc_cert);
            stream.write_all(&msg).await.unwrap();

            MockJdServer {
                stream,
                send_key: server_send,
                recv_key: server_recv,
                send_nonce: 0,
                recv_nonce: 0,
                recv_buf: Vec::new(),
            }
        }

        async fn read_block(&mut self, enc_len: usize) -> Vec<u8> {
            use chacha20poly1305::aead::{Aead, KeyInit};
            use chacha20poly1305::{ChaCha20Poly1305, Nonce};
            let mut tmp = [0u8; 4096];
            while self.recv_buf.len() < enc_len {
                let n = self.stream.read(&mut tmp).await.unwrap();
                assert!(n > 0, "client closed mid-frame");
                self.recv_buf.extend_from_slice(&tmp[..n]);
            }
            let block: Vec<u8> = self.recv_buf.drain(..enc_len).collect();
            let c = ChaCha20Poly1305::new_from_slice(&self.recv_key).unwrap();
            let mut nb = [0u8; 12];
            nb[4..12].copy_from_slice(&self.recv_nonce.to_le_bytes());
            self.recv_nonce += 1;
            c.decrypt(Nonce::from_slice(&nb), block.as_slice()).unwrap()
        }

        async fn read_frame(&mut self) -> Sv2Frame {
            let header = self.read_block(FRAME_HEADER_SIZE + 16).await;
            let fh = Sv2FrameHeader::from_bytes(&header).unwrap();
            let payload = if fh.payload_len == 0 {
                self.read_block(16).await
            } else {
                self.read_block(fh.payload_len as usize + 16).await
            };
            Sv2Frame {
                header: fh,
                payload,
            }
        }

        async fn write_frame(&mut self, frame: &[u8]) {
            use chacha20poly1305::aead::{Aead, KeyInit};
            use chacha20poly1305::{ChaCha20Poly1305, Nonce};
            let c = ChaCha20Poly1305::new_from_slice(&self.send_key).unwrap();
            for block in [&frame[..FRAME_HEADER_SIZE], &frame[FRAME_HEADER_SIZE..]] {
                let mut nb = [0u8; 12];
                nb[4..12].copy_from_slice(&self.send_nonce.to_le_bytes());
                self.send_nonce += 1;
                let ct = c.encrypt(Nonce::from_slice(&nb), block).unwrap();
                self.stream.write_all(&ct).await.unwrap();
            }
        }
    }

    async fn spawn_setup_server(
        expected_protocol: u8,
        send_template: bool,
        send_token: bool,
    ) -> std::net::SocketAddr {
        // Deterministic high-fidelity default template (the W1-D mock).
        let default_template = NewTemplate {
            template_id: 42,
            future_template: false,
            version: 0x2000_0000,
            coinbase_tx_version: 2,
            coinbase_prefix: vec![0x01],
            coinbase_tx_input_sequence: 0xffff_fffe,
            coinbase_tx_value_remaining: 625_000_000,
            coinbase_tx_outputs_count: 1,
            coinbase_tx_outputs: sample_txout(0, &[0x6a]),
            coinbase_tx_locktime: 0,
            merkle_path: vec![],
        };
        let default_prev = TemplateSetNewPrevHash {
            template_id: 42,
            prev_hash: [0x11; 32],
            header_timestamp: 1_700_000_000,
            nbits: 0x170a_bcd0,
            target: [0x22; 32],
        };
        spawn_setup_server_with(
            expected_protocol,
            send_template,
            send_token,
            default_template,
            default_prev,
        )
        .await
    }

    /// Encrypted-Noise mock JD/TP server that serves a CALLER-SUPPLIED
    /// `NewTemplate` + `TemplateSetNewPrevHash`.
    ///
    /// Both the W1-D deterministic harness and the new
    /// real-`bitcoind -regtest` integration test drive the IDENTICAL
    /// server-side Noise_NX path through this function — the only
    /// difference is whether the template carries hardcoded constants or
    /// real regtest-chain data parsed from a genuine `getblocktemplate`.
    /// This keeps the encryption proof byte-for-byte the same regardless
    /// of where the template originates.
    async fn spawn_setup_server_with(
        expected_protocol: u8,
        send_template: bool,
        send_token: bool,
        template: NewTemplate,
        prev_hash: TemplateSetNewPrevHash,
    ) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Real server-side Noise_NX handshake — every subsequent SV2
            // frame is ChaChaPoly-encrypted, proving the JD path is now
            // genuinely confidential (no plaintext JD token / template).
            let mut srv = MockJdServer::handshake(socket).await;

            let setup = srv.read_frame().await;
            assert_eq!(setup.header.msg_type, common::SETUP_CONNECTION);
            assert_eq!(setup.payload[0], expected_protocol);

            let success = Sv2Frame::new(
                EXTENSION_TYPE_CORE,
                common::SETUP_CONNECTION_SUCCESS,
                [2u16.to_le_bytes().as_slice(), 0u32.to_le_bytes().as_slice()].concat(),
            );
            srv.write_frame(&success.to_bytes()).await;

            if send_template {
                let constraints = srv.read_frame().await;
                assert_eq!(
                    constraints.header.msg_type,
                    template_distribution::COINBASE_OUTPUT_CONSTRAINTS
                );
                let frame = Sv2Frame::new(
                    EXTENSION_TYPE_CORE,
                    template_distribution::NEW_TEMPLATE,
                    template_to_bytes(&template),
                );
                srv.write_frame(&frame.to_bytes()).await;

                let frame = Sv2Frame::new(
                    EXTENSION_TYPE_CORE,
                    template_distribution::SET_NEW_PREV_HASH,
                    prev_hash_to_bytes(&prev_hash),
                );
                srv.write_frame(&frame.to_bytes()).await;
            }

            if send_token {
                let token = srv.read_frame().await;
                assert_eq!(
                    token.header.msg_type,
                    job_declaration::ALLOCATE_MINING_JOB_TOKEN
                );
                let request = AllocateMiningJobToken::from_bytes(&token.payload).unwrap();
                let success = AllocateMiningJobTokenSuccess {
                    request_id: request.request_id,
                    mining_job_token: vec![0xaa, 0xbb],
                    coinbase_tx_outputs: sample_compact_outputs(vec![sample_txout(0, &[0x51])]),
                };
                let frame = Sv2Frame::new(
                    EXTENSION_TYPE_CORE,
                    job_declaration::ALLOCATE_MINING_JOB_TOKEN_SUCCESS,
                    success.to_bytes().unwrap(),
                );
                srv.write_frame(&frame.to_bytes()).await;
            }
        });
        addr
    }

    fn template_to_bytes(template: &NewTemplate) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&template.template_id.to_le_bytes());
        buf.push(template.future_template as u8);
        buf.extend_from_slice(&template.version.to_le_bytes());
        buf.extend_from_slice(&template.coinbase_tx_version.to_le_bytes());
        write_b0_255(&mut buf, &template.coinbase_prefix).unwrap();
        buf.extend_from_slice(&template.coinbase_tx_input_sequence.to_le_bytes());
        buf.extend_from_slice(&template.coinbase_tx_value_remaining.to_le_bytes());
        buf.extend_from_slice(&template.coinbase_tx_outputs_count.to_le_bytes());
        write_b0_64k(&mut buf, &template.coinbase_tx_outputs).unwrap();
        buf.extend_from_slice(&template.coinbase_tx_locktime.to_le_bytes());
        write_seq_u256_255(&mut buf, &template.merkle_path).unwrap();
        buf
    }

    fn prev_hash_to_bytes(prev_hash: &TemplateSetNewPrevHash) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&prev_hash.template_id.to_le_bytes());
        buf.extend_from_slice(&prev_hash.prev_hash);
        buf.extend_from_slice(&prev_hash.header_timestamp.to_le_bytes());
        buf.extend_from_slice(&prev_hash.nbits.to_le_bytes());
        buf.extend_from_slice(&prev_hash.target);
        buf
    }

    fn sample_txout(value: u64, script: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&value.to_le_bytes());
        write_compact_size(&mut out, script.len() as u64).unwrap();
        out.extend_from_slice(script);
        out
    }

    fn sample_compact_outputs(outputs: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = Vec::new();
        write_compact_size(&mut out, outputs.len() as u64).unwrap();
        for txout in outputs {
            out.extend_from_slice(&txout);
        }
        out
    }

    // -----------------------------------------------------------------------
    // JD config + status defaults; credential boundary contracts.
    // -----------------------------------------------------------------------

    #[test]
    fn jd_config_default_is_disabled_with_no_credentials() {
        // CRITICAL: JD must require explicit operator opt-in. Default
        // `enabled=false` ensures a fresh dcentrald never auto-connects
        // to a Job Declarator. Pin every credential field as empty so a
        // refactor that pre-populated a recipient address is caught.
        let cfg = JdConfig::default();

        assert!(!cfg.enabled, "JD must be disabled by default (opt-in)");
        assert_eq!(cfg.mode, JdMode::CoinbaseOnly);

        // Credentials must default to empty so a fresh deployment doesn't
        // accidentally ship recipient address or RPC credentials.
        assert_eq!(cfg.bitcoind_rpc_user, "");
        assert_eq!(cfg.bitcoind_rpc_password, "");
        assert_eq!(cfg.bitcoind_rpc_cookie, "");
        assert_eq!(cfg.coinbase_output_address, "");
        assert_eq!(cfg.job_declarator_url, "");

        // Sane infrastructure defaults (localhost endpoints).
        assert_eq!(cfg.bitcoind_rpc_url, "http://127.0.0.1:8332");
        assert_eq!(cfg.template_provider_url, "sv2+tcp://127.0.0.1:8442");

        // Operational defaults.
        assert_eq!(cfg.template_refresh_interval_s, 30);
        assert!(
            cfg.fallback_to_pool_templates,
            "default must allow fallback to pool templates if JD is unreachable"
        );
        assert!(
            !cfg.declare_tx_data,
            "default must NOT declare full template tx data (privacy posture)"
        );
        assert_eq!(cfg.coinbase_output_max_additional_size, 512);
        assert_eq!(cfg.coinbase_output_max_additional_sigops, 0);
    }

    #[test]
    fn jd_config_serde_default_fills_missing_fields() {
        // Old config files without JD section must parse cleanly with
        // safe defaults (disabled, no credentials).
        let bare = "{}";
        let cfg: JdConfig = serde_json::from_str(bare).unwrap();

        assert!(!cfg.enabled);
        assert_eq!(cfg.mode, JdMode::CoinbaseOnly);
        assert_eq!(cfg.coinbase_output_address, "");
        assert_eq!(cfg.bitcoind_rpc_url, "http://127.0.0.1:8332");
    }

    #[test]
    fn jd_config_partial_override_keeps_remaining_defaults_disabled() {
        // Setting only the bitcoind URL must NOT silently flip `enabled=true`.
        let partial = r#"{"bitcoind_rpc_url":"http://203.0.113.1:8332"}"#;
        let cfg: JdConfig = serde_json::from_str(partial).unwrap();

        assert!(
            !cfg.enabled,
            "operator must opt-in with explicit enabled=true"
        );
        assert_eq!(cfg.bitcoind_rpc_url, "http://203.0.113.1:8332");
        assert_eq!(cfg.coinbase_output_address, "");
    }

    #[test]
    fn jd_status_default_reports_disabled_state() {
        let status = JdStatus::default();
        assert!(!status.enabled);
        assert!(!status.configured);
        assert!(
            status.protocol_ready,
            "protocol path is ready even when JD is disabled"
        );
        assert!(!status.connected);
        assert!(!status.template_provider_connected);
        assert!(!status.job_declarator_connected);
        assert!(!status.mining_job_token_available);
        assert!(!status.template_prev_hash_ready);
        assert!(!status.custom_job_candidate_ready);
        assert_eq!(status.mode, JdMode::CoinbaseOnly);
        assert_eq!(status.runtime_state, "disabled");
        assert_eq!(status.templates_constructed, 0);
        assert!(status.last_template_age_s.is_none());
        assert!(status.current_template_id.is_none());
        assert!(status.last_error.is_none());
        assert!(status.custom_job_candidate.is_none());
    }

    #[test]
    fn jd_status_skip_serializes_custom_job_candidate_credentials() {
        // CRITICAL: the CustomJobCandidate carries `mining_job_token` and
        // `coinbase_tx_outputs` — pool authorization material and the
        // operator's payout outputs. These MUST never be exposed through
        // REST. Pin the `#[serde(skip_serializing)]` attribute by checking
        // the JSON output omits the field entirely even when populated.
        let mut status = JdStatus::default();
        status.custom_job_candidate = Some(CustomJobCandidate {
            template_id: 1,
            mining_job_token: vec![0xAA, 0xBB, 0xCC, 0xDD], // sensitive
            version: 0x2000_0000,
            prev_hash: [0x11; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
            target: [0x22; 32],
            coinbase_tx_version: 2,
            coinbase_prefix: vec![0x03],
            coinbase_tx_input_nsequence: 0xffff_fffe,
            coinbase_tx_outputs: vec![0xDE, 0xAD, 0xBE, 0xEF], // sensitive
            coinbase_tx_locktime: 0,
            merkle_path: vec![[0x44; 32]],
            tx_count: 2,
            coinbase_value_remaining_sats: 5_000_000_000,
        });

        // Parse to a Value so we can check field presence precisely
        // (substring matching would catch `custom_job_candidate_ready`,
        // a different bool field that IS legitimately serialized).
        let json: serde_json::Value = serde_json::to_value(&status).unwrap();
        let obj = json.as_object().expect("JdStatus serializes as object");
        assert!(
            !obj.contains_key("custom_job_candidate"),
            "JdStatus must NOT serialize custom_job_candidate (carries token+outputs); fields: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
        // Sanity: the unrelated `_ready` flag IS legitimately serialized.
        assert!(obj.contains_key("custom_job_candidate_ready"));

        // Belt-and-suspenders: scan every value recursively for the
        // sensitive byte sequences. The token + outputs we set above
        // would render as JSON arrays of bytes if accidentally serialized.
        let raw = serde_json::to_string(&status).unwrap();
        // The mining_job_token bytes would appear as `[170,187,204,221]`
        // (decimal-encoded array) if leaked. Confirm none of those
        // sensitive byte arrays surface.
        assert!(
            !raw.contains("[170,187,204,221]"),
            "mining_job_token bytes leaked into JSON: {raw}"
        );
        assert!(
            !raw.contains("[222,173,190,239]"),
            "coinbase_tx_outputs bytes leaked into JSON: {raw}"
        );
    }

    #[test]
    fn jd_mode_serializes_in_snake_case_wire_form() {
        // Pin both variants. A refactor that flipped to PascalCase would
        // break every JD client.
        assert_eq!(
            serde_json::to_string(&JdMode::CoinbaseOnly).unwrap(),
            "\"coinbase_only\""
        );
        assert_eq!(
            serde_json::to_string(&JdMode::FullTemplate).unwrap(),
            "\"full_template\""
        );
    }

    #[test]
    fn jd_mode_round_trips_through_json() {
        for mode in [JdMode::CoinbaseOnly, JdMode::FullTemplate] {
            let json = serde_json::to_string(&mode).unwrap();
            let recovered: JdMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, recovered);
        }
    }

    #[test]
    fn jd_mode_default_is_coinbase_only() {
        // CoinbaseOnly is the privacy-preserving default — pool sees the
        // payout commitment but NOT the selected transactions. A refactor
        // that flipped the default to FullTemplate would silently leak
        // template txids to the pool for every operator who hasn't
        // explicitly opted into FullTemplate.
        assert_eq!(JdMode::default(), JdMode::CoinbaseOnly);
    }

    #[test]
    fn custom_job_candidate_stable_key_uses_template_and_block_state() {
        // The stable key is used to dedupe custom job candidates so we
        // don't re-declare the same job twice. Pin which fields contribute.
        let candidate = CustomJobCandidate {
            template_id: 42,
            mining_job_token: vec![1, 2, 3], // NOT in stable key
            version: 0x2000_0000,
            prev_hash: [0x11; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_4219,
            target: [0x22; 32], // NOT in stable key
            coinbase_tx_version: 2,
            coinbase_prefix: vec![],
            coinbase_tx_input_nsequence: 0,
            coinbase_tx_outputs: vec![],
            coinbase_tx_locktime: 0,
            merkle_path: vec![],
            tx_count: 0,
            coinbase_value_remaining_sats: 0,
        };

        let key = candidate.stable_key();
        assert_eq!(key.template_id, 42);
        assert_eq!(key.prev_hash, [0x11; 32]);
        assert_eq!(key.min_ntime, 1_700_000_000);
        assert_eq!(key.nbits, 0x1703_4219);

        // Modifying a field that's NOT in the stable key (token, target,
        // coinbase) must NOT change the key.
        let mut other = candidate.clone();
        other.mining_job_token = vec![9, 9, 9];
        other.target = [0xFF; 32];
        other.coinbase_tx_outputs = vec![0xDE; 100];
        assert_eq!(
            other.stable_key(),
            key,
            "stable_key must depend ONLY on template_id, prev_hash, min_ntime, nbits"
        );

        // Changing template_id MUST change the key.
        let mut diff_template = candidate.clone();
        diff_template.template_id = 43;
        assert_ne!(diff_template.stable_key(), key);

        // Changing prev_hash MUST change the key.
        let mut diff_prev = candidate.clone();
        diff_prev.prev_hash = [0x99; 32];
        assert_ne!(diff_prev.stable_key(), key);
    }

    #[test]
    fn custom_job_candidate_to_set_custom_mining_job_carries_through_fields() {
        // The candidate → SetCustomMiningJob conversion is the bridge
        // between the JD supervisor and the SV2 client. Pin every field
        // that must round-trip.
        let candidate = CustomJobCandidate {
            template_id: 5,
            mining_job_token: vec![0xAA, 0xBB],
            version: 0x2000_4000,
            prev_hash: [0x33; 32],
            min_ntime: 1_700_001_000,
            nbits: 0x1903_a30c,
            target: [0x44; 32], // not in SetCustomMiningJob
            coinbase_tx_version: 2,
            coinbase_prefix: vec![0x03, 0x04],
            coinbase_tx_input_nsequence: 0xffff_fffe,
            coinbase_tx_outputs: vec![0xCC, 0xDD],
            coinbase_tx_locktime: 0,
            merkle_path: vec![[0x55; 32]],
            tx_count: 3,
            coinbase_value_remaining_sats: 50_000,
        };

        let scmj = candidate.to_set_custom_mining_job(7, 11);
        assert_eq!(scmj.channel_id, 7);
        assert_eq!(scmj.request_id, 11);
        assert_eq!(scmj.mining_job_token, vec![0xAA, 0xBB]);
        assert_eq!(scmj.version, 0x2000_4000);
        assert_eq!(scmj.prev_hash, [0x33; 32]);
        assert_eq!(scmj.min_ntime, 1_700_001_000);
        assert_eq!(scmj.nbits, 0x1903_a30c);
        assert_eq!(scmj.coinbase_tx_version, 2);
        assert_eq!(scmj.coinbase_prefix, vec![0x03, 0x04]);
        assert_eq!(scmj.coinbase_tx_input_nsequence, 0xffff_fffe);
        assert_eq!(scmj.coinbase_tx_outputs, vec![0xCC, 0xDD]);
        assert_eq!(scmj.coinbase_tx_locktime, 0);
        assert_eq!(scmj.merkle_path, vec![[0x55; 32]]);
    }

    #[test]
    fn jd_protocol_url_defaults_use_localhost_endpoints() {
        // Default JD endpoints point to localhost so a fresh deployment
        // doesn't accidentally connect to a public Bitcoin node or
        // Template Provider. Pin both.
        assert_eq!(default_bitcoind_url(), "http://127.0.0.1:8332");
        assert_eq!(default_template_provider_url(), "sv2+tcp://127.0.0.1:8442");
    }

    #[test]
    fn jd_template_refresh_default_is_30_seconds() {
        // Refresh cadence drives Bitcoin RPC load and template freshness.
        // Pin the operational default.
        assert_eq!(default_template_refresh(), 30);
    }

    #[test]
    fn jd_coinbase_output_default_caps_at_512_bytes() {
        // The 512-byte cap on additional coinbase outputs prevents a
        // misconfigured operator from constructing oversized coinbases.
        assert_eq!(default_coinbase_output_max_additional_size(), 512);
    }

    // =======================================================================
    // JD live-proof hardening — real `bitcoind -regtest` as Template source.
    //
    // SCOPE STATEMENT (read this before trusting the proof):
    //
    //   PROVEN IN-PROCESS by this test (always, every CI run via the
    //   high-fidelity-mock fallback; ADDITIONALLY against real chain data
    //   when a regtest `bitcoind` is present):
    //     * the real `JdClient::probe_once` initiator runs the SPEC-CORRECT
    //       Noise_NX handshake against a server that speaks REAL EllSwift
    //       ECDH + ChaChaPoly — a plaintext client cannot complete it,
    //     * `CoinbaseOutputConstraints → NewTemplate → SetNewPrevHash`
    //       (the SV2 Template-Distribution equivalent of bitcoind's
    //       `getblocktemplate`) round-trips fully ENCRYPTED,
    //     * `AllocateMiningJobToken → …Success` round-trips fully ENCRYPTED,
    //     * the JD custom-job candidate is assembled from that encrypted
    //       feed and carries the template's prev_hash / nbits / ntime /
    //       coinbase value verbatim,
    //     * a self-declared-template share is mapped through the proxy
    //       adapter into a wire-shaped `SubmitSharesExtended` /
    //       `PushSolution` whose coinbase reconstruction is byte-identical
    //       to the declared template's,
    //     * when `bitcoind` is available: ALL of the above using a prev
    //       hash / nbits / ntime / coinbase value taken from a GENUINE
    //       101-block regtest chain via the real `getblocktemplate` RPC —
    //       i.e. the SV2 message flow is exercised with real Bitcoin
    //       consensus data, not hand-rolled constants.
    //
    //   NOT proven here (requires a live external SV2 stack — honest):
    //     * interop against a REAL SV2 Job Declarator Server (SRI `jds`)
    //       and a REAL SV2 Template Provider (bitcoind built with `-sv2`,
    //       which mainline Bitcoin Core release binaries do NOT ship —
    //       only the SRI `27.x-stratumv2` patchset / a `--with-sv2`
    //       build), authenticating with the pool's own authority-signed
    //       certificate,
    //     * a self-declared block actually accepted by a real pool +
    //       relayed to the Bitcoin network.
    //   Those need the SRI reference stack and are tracked as the
    //   standing "live interop vs a real SV2 pool" follow-up (W1-D).
    // =======================================================================

    /// Minimal blocking JSON-RPC call to a local `bitcoind` over a raw
    /// `std::net::TcpStream` (no reqwest/hyper dep — keeps the proof
    /// scoped to `dcentrald-stratum` with zero new dependencies).
    #[cfg(test)]
    fn bitcoind_rpc(
        addr: std::net::SocketAddr,
        auth: &str,
        method: &str,
        params: &str,
    ) -> Result<String, String> {
        use std::io::{Read, Write};
        let body = format!(
            r#"{{"jsonrpc":"1.0","id":"dcent","method":"{}","params":{}}}"#,
            method, params
        );
        let b64 = {
            // tiny base64 (RFC 4648) for the Basic auth header.
            const T: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let inp = auth.as_bytes();
            let mut o = String::new();
            for c in inp.chunks(3) {
                let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
                let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
                o.push(T[((n >> 18) & 63) as usize] as char);
                o.push(T[((n >> 12) & 63) as usize] as char);
                o.push(if c.len() > 1 {
                    T[((n >> 6) & 63) as usize] as char
                } else {
                    '='
                });
                o.push(if c.len() > 2 {
                    T[(n & 63) as usize] as char
                } else {
                    '='
                });
            }
            o
        };
        let req = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            b64,
            body.len(),
            body
        );
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
                .map_err(|e| format!("connect: {e}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        let mut resp = String::new();
        stream
            .read_to_string(&mut resp)
            .map_err(|e| format!("read: {e}"))?;
        let body = resp
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .ok_or("no http body")?;
        Ok(body)
    }

    /// Pull a JSON string/number field out of a flat-ish RPC result blob
    /// without a JSON dep. Good enough for the handful of scalar
    /// `getblocktemplate` fields this proof needs.
    fn json_field<'a>(blob: &'a str, key: &str) -> Option<&'a str> {
        let needle = format!("\"{}\"", key);
        let i = blob.find(&needle)? + needle.len();
        let rest = &blob[i..];
        let c = rest.find(':')? + 1;
        let rest = rest[c..].trim_start();
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(&stripped[..end])
        } else {
            let end = rest
                .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
                .unwrap_or(rest.len());
            Some(&rest[..end])
        }
    }

    fn hex32_be_to_le(h: &str) -> Option<[u8; 32]> {
        if h.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()?;
        }
        // bitcoind reports block hashes big-endian (display order); SV2
        // prev_hash is the internal byte order — reverse.
        out.reverse();
        Some(out)
    }

    /// Locate a usable `bitcoind`/`bitcoin-cli` pair. Honors
    /// `DCENT_SV2_JD_REGTEST_BITCOIND` (path to `bitcoind`), else probes
    /// the common Windows / *nix install locations. Returns `None` (test
    /// degrades to the high-fidelity-mock path) when not found.
    fn find_bitcoind() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let candidates: Vec<std::path::PathBuf> =
            if let Ok(p) = std::env::var("DCENT_SV2_JD_REGTEST_BITCOIND") {
                vec![std::path::PathBuf::from(p)]
            } else {
                vec![
                    "C:/Program Files/Bitcoin/daemon/bitcoind.exe".into(),
                    "/usr/bin/bitcoind".into(),
                    "/usr/local/bin/bitcoind".into(),
                    "bitcoind".into(),
                ]
            };
        for d in candidates {
            let cli = {
                let s = d.to_string_lossy();
                std::path::PathBuf::from(
                    s.replace("bitcoind.exe", "bitcoin-cli.exe")
                        .replace("bitcoind", "bitcoin-cli"),
                )
            };
            if d.exists() && cli.exists() {
                return Some((d, cli));
            }
            // PATH-resolved "bitcoind" with no extension: verify it is
            // actually runnable before trusting it. A clean checkout / CI
            // without bitcoind installed must degrade to the high-fidelity
            // mock template (the `None` arm below), never panic on spawn.
            if d.to_string_lossy() == "bitcoind"
                && std::process::Command::new(&d)
                    .arg("-version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok()
            {
                return Some((d.clone(), std::path::PathBuf::from("bitcoin-cli")));
            }
        }
        None
    }

    /// End-to-end JD/TDP proof. With a real regtest `bitcoind` it sources
    /// a GENUINE block template; without one it falls back to the
    /// deterministic high-fidelity mock — either way the encrypted Noise
    /// JD round-trip + the self-declared-template share mapping are
    /// proven in-process. See the SCOPE STATEMENT above.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn jd_regtest_or_high_fidelity_mock_template_round_trips_encrypted() {
        let _guard = crate::v2::auth::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        std::env::set_var(crate::v2::auth::ENV_TP_REQUIRE_NOISE, "1");

        // ---- 1. Source a template: real regtest if available --------------
        let mut used_regtest = false;
        let (template, prev) = match find_bitcoind() {
            Some((bitcoind, cli)) => {
                let dir = std::env::temp_dir().join(format!("dcent_jd_rt_{}", unix_now_s()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                let rpc_port = 19_111u16;
                let p2p_port = 19_112u16;
                let mut child = std::process::Command::new(&bitcoind)
                    .arg("-regtest")
                    .arg(format!("-datadir={}", dir.display()))
                    .arg("-fallbackfee=0.0002")
                    .arg("-rpcuser=dcentjd")
                    .arg("-rpcpassword=jdproof")
                    .arg(format!("-rpcport={rpc_port}"))
                    .arg(format!("-port={p2p_port}"))
                    .arg("-listen=0")
                    .arg("-server")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn bitcoind");

                let rpc_addr: std::net::SocketAddr =
                    format!("127.0.0.1:{rpc_port}").parse().unwrap();
                let auth = "dcentjd:jdproof";
                // Poll RPC up to ~25s (regtest cold-start on a busy CI box).
                let mut ready = false;
                for _ in 0..50 {
                    if bitcoind_rpc(rpc_addr, auth, "getblockchaininfo", "[]").is_ok() {
                        ready = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                assert!(ready, "regtest bitcoind RPC never came up");

                let run_cli = |args: &[&str]| {
                    std::process::Command::new(&cli)
                        .arg("-regtest")
                        .arg(format!("-datadir={}", dir.display()))
                        .arg(format!("-rpcport={rpc_port}"))
                        .arg("-rpcuser=dcentjd")
                        .arg("-rpcpassword=jdproof")
                        .args(args)
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default()
                };
                let _ = run_cli(&["createwallet", "jdproof"]);
                let addr = run_cli(&["getnewaddress"]);
                assert!(!addr.is_empty(), "regtest getnewaddress failed");
                let _ = run_cli(&["generatetoaddress", "101", &addr]);

                let gbt = bitcoind_rpc(
                    rpc_addr,
                    auth,
                    "getblocktemplate",
                    r#"[{"rules":["segwit"]}]"#,
                )
                .expect("getblocktemplate");

                let height: u64 = json_field(&gbt, "height").unwrap().parse().unwrap();
                let prevhash_hex = json_field(&gbt, "previousblockhash").unwrap().to_string();
                let bits_hex = json_field(&gbt, "bits").unwrap().to_string();
                let curtime: u32 = json_field(&gbt, "curtime").unwrap().parse().unwrap();
                let coinbasevalue: u64 =
                    json_field(&gbt, "coinbasevalue").unwrap().parse().unwrap();
                let version: u32 = json_field(&gbt, "version").unwrap().parse().unwrap();

                // Real regtest chain values prove the SV2 message flow
                // carries genuine consensus data end-to-end.
                assert!(height >= 102, "expected chain past 101 blocks");
                assert_eq!(coinbasevalue, 5_000_000_000, "regtest subsidy");
                let nbits = u32::from_str_radix(&bits_hex, 16).unwrap();
                let prev_hash = hex32_be_to_le(&prevhash_hex).expect("32-byte prev hash");

                // Best-effort clean shutdown of the regtest node.
                let _ = run_cli(&["stop"]);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);

                used_regtest = true;
                eprintln!(
                    "JD-proof: REAL regtest template h={height} \
                     prev={prevhash_hex} bits={bits_hex} \
                     cbval={coinbasevalue} ver={version}"
                );
                (
                    NewTemplate {
                        template_id: height,
                        future_template: false,
                        version,
                        coinbase_tx_version: 2,
                        // Real regtest coinbase scriptSig prefix: BIP34
                        // height push. Encoded as the SV2
                        // `coinbase_prefix` so the candidate carries the
                        // chain's true height.
                        coinbase_prefix: {
                            let mut p = vec![0x03];
                            p.extend_from_slice(&(height as u32).to_le_bytes()[..3]);
                            p
                        },
                        coinbase_tx_input_sequence: 0xffff_ffff,
                        coinbase_tx_value_remaining: coinbasevalue,
                        coinbase_tx_outputs_count: 1,
                        coinbase_tx_outputs: sample_txout(coinbasevalue, &[0x51]),
                        coinbase_tx_locktime: 0,
                        merkle_path: vec![],
                    },
                    TemplateSetNewPrevHash {
                        template_id: height,
                        prev_hash,
                        header_timestamp: curtime,
                        nbits,
                        target: [0x22; 32],
                    },
                )
            }
            None => {
                eprintln!(
                    "JD-proof: no bitcoind found — using high-fidelity \
                     deterministic mock template (set \
                     DCENT_SV2_JD_REGTEST_BITCOIND to force real regtest)"
                );
                (
                    NewTemplate {
                        template_id: 102,
                        future_template: false,
                        version: 0x2000_0000,
                        coinbase_tx_version: 2,
                        coinbase_prefix: vec![0x03, 0x66, 0x00, 0x00],
                        coinbase_tx_input_sequence: 0xffff_ffff,
                        coinbase_tx_value_remaining: 5_000_000_000,
                        coinbase_tx_outputs_count: 1,
                        coinbase_tx_outputs: sample_txout(5_000_000_000, &[0x51]),
                        coinbase_tx_locktime: 0,
                        merkle_path: vec![],
                    },
                    TemplateSetNewPrevHash {
                        template_id: 102,
                        prev_hash: [0x7a; 32],
                        header_timestamp: 1_779_000_000,
                        nbits: 0x207f_ffff,
                        target: [0x22; 32],
                    },
                )
            }
        };

        // ---- 2. Drive the REAL JdClient over ENCRYPTED Noise --------------
        let want_prev = prev.prev_hash;
        let want_nbits = prev.nbits;
        let want_ntime = prev.header_timestamp;
        let want_cbval = template.coinbase_tx_value_remaining;
        let want_tid = template.template_id;

        let tp = spawn_setup_server_with(
            PROTOCOL_TEMPLATE_DISTRIBUTION,
            true,
            false,
            template.clone(),
            prev.clone(),
        )
        .await;
        let jd =
            spawn_setup_server_with(PROTOCOL_JOB_DECLARATION, false, true, template, prev).await;

        let client = JdClient::new(JdConfig {
            enabled: true,
            template_provider_url: format!("sv2+tcp://{}", tp),
            job_declarator_url: format!("sv2+tcp://{}", jd),
            ..JdConfig::default()
        });
        let status = client.probe_once().await;
        std::env::remove_var(crate::v2::auth::ENV_TP_REQUIRE_NOISE);

        // Encrypted handshake completed on BOTH endpoints (a plaintext
        // client could not have produced a parseable Noise response).
        assert!(
            status.template_provider_connected,
            "encrypted TP handshake+template must succeed"
        );
        assert!(
            status.job_declarator_connected,
            "encrypted JDS handshake+token must succeed"
        );
        assert!(status.connected);
        assert!(status.custom_job_candidate_ready);
        assert_eq!(status.runtime_state, "custom_job_candidate_ready");

        // The custom-job candidate carries the (real-regtest-or-mock)
        // template's consensus fields verbatim, over the encrypted feed.
        let candidate = status.custom_job_candidate.as_ref().unwrap();
        assert_eq!(candidate.template_id, want_tid);
        assert_eq!(candidate.prev_hash, want_prev);
        assert_eq!(candidate.nbits, want_nbits);
        assert_eq!(candidate.min_ntime, want_ntime);
        assert_eq!(
            candidate.coinbase_value_remaining_sats, want_cbval,
            "coinbase value must survive the encrypted TP round-trip"
        );

        // ---- 3. Self-declared-template share → wire-shaped submit --------
        // Map a found nonce on this self-declared job through the proxy
        // adapter into a SubmitSharesExtended, and prove the coinbase the
        // pool would reconstruct == the coinbase the declared template
        // implies (byte-fidelity on the JD path too).
        use crate::v2::adapter::{
            reconstruct_v1_coinbase, to_hex, v1_submit_to_sv2_extended, Sv2ProxyExtranonce,
            V1SubmitParams,
        };
        let scmj = candidate.to_set_custom_mining_job(7, 1);
        // The proxy advertises the pool prefix as extranonce1; pick an
        // 8-byte miner extranonce2.
        let proxy_en = Sv2ProxyExtranonce::from_open_success(&[0xAB, 0xCD], 8);
        let miner_en2 = vec![0u8, 1, 2, 3, 4, 5, 6, 7];
        let submit = V1SubmitParams {
            worker: "jd.worker".to_string(),
            job_id: "1".to_string(),
            extranonce2_hex: to_hex(&miner_en2),
            ntime_hex: format!("{:08x}", want_ntime),
            nonce_hex: "0badf00d".to_string(),
            version_bits_hex: None,
        };
        let ext =
            v1_submit_to_sv2_extended(&submit, &proxy_en, 7, 1, scmj.request_id, scmj.version)
                .expect("self-declared share maps to SubmitSharesExtended");
        assert_eq!(ext.extranonce, miner_en2, "SV2 carries miner en2 ONLY");
        assert_eq!(ext.version, candidate.version);
        assert_eq!(ext.nonce, 0x0bad_f00d);

        // Coinbase byte-fidelity on the JD self-declared path:
        // declared coinbase (prefix||en1||en2||suffix) reconstructs the
        // same bytes the pool would from SubmitSharesExtended.
        let declared = reconstruct_v1_coinbase(
            &scmj.coinbase_prefix,
            &proxy_en.extranonce1,
            &miner_en2,
            &scmj.coinbase_tx_outputs,
        );
        let mut pool_side = Vec::new();
        pool_side.extend_from_slice(&scmj.coinbase_prefix);
        pool_side.extend_from_slice(&proxy_en.extranonce1);
        pool_side.extend_from_slice(&ext.extranonce);
        pool_side.extend_from_slice(&scmj.coinbase_tx_outputs);
        assert_eq!(
            declared, pool_side,
            "JD self-declared coinbase must be byte-identical to the \
             pool-side reconstruction"
        );

        // Also assert the PushSolution shape a real TP submission uses is
        // wire-encodable from this self-declared share.
        let push = PushSolution {
            extranonce: ext.extranonce.clone(),
            prev_hash: candidate.prev_hash,
            nonce: ext.nonce,
            ntime: ext.ntime,
            nbits: candidate.nbits,
            version: ext.version,
        };
        assert!(
            push.to_bytes().is_ok(),
            "self-declared solution must wire-encode as PushSolution"
        );

        if used_regtest {
            eprintln!(
                "JD-proof: PASSED against REAL regtest chain data \
                 (encrypted Noise TP+JDS round-trip + self-declared share)"
            );
        } else {
            eprintln!(
                "JD-proof: PASSED via high-fidelity mock (encrypted Noise \
                 TP+JDS round-trip + self-declared share)"
            );
        }
    }
}
