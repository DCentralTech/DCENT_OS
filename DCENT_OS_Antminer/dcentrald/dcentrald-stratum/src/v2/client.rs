//! Stratum V2 async client for dcentrald.
//!
//! Connects to an SV2 pool, performs the Noise_NX handshake, opens a standard
//! mining channel, and processes jobs/shares. Ported from the proven DCENT_axe
//! (ESP32) blocking client to async tokio.
//!
//! # Interface
//! Matches the V1 client's channel interface exactly:
//! - `job_tx`: Sends new `JobTemplate` when NewMiningJob + SetNewPrevHash arrive
//! - `share_rx`: Receives `ValidShare` from the work dispatcher for submission
//! - `status_tx`: Sends `StratumStatus` updates (state changes, share results)
//!
//! # Noise Transport Framing
//! After the Noise_NX handshake, all messages are encrypted. Each SV2 frame is
//! sent as two encrypted blocks:
//!   1. Encrypted header (6 bytes plaintext -> 22 bytes ciphertext + MAC)
//!   2. Encrypted payload (N bytes plaintext -> N+16 bytes ciphertext + MAC)
//!
//! # Pending Header Fix (from ESP32 client)
//! When the encrypted header arrives in one TCP read but the payload hasn't
//! arrived yet, we stash the decrypted header and wait. This prevents Noise
//! nonce desynchronization, which would cause all subsequent decryptions to
//! fail with authentication errors.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
#[cfg(feature = "jd")]
use tokio::sync::watch;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use super::adapter;
use super::adapter::ExtendedJobAssembly;
use super::channel::{Sv2Event, Sv2MiningChannel};
#[cfg(feature = "jd")]
use super::jd::{CustomJobCandidate, CustomJobCandidateKey, JdStatus};
use crate::types::*;
use crate::StratumV1Client;

/// Operational over-cap predicate for an inbound SV2 mining-client frame.
///
/// `cap == 0` ⇒ disabled (the 16 MiB wire-format protocol max still
/// applies in `framing.rs`; this returns `false`). Otherwise an
/// announced `payload_len` strictly greater than `cap` is rejected.
/// Pure + deterministic so the boundary is unit-pinned independently of
/// any socket/Noise/hardware. SV2 inbound-frame cap (strat-09 hardening);
///.
pub(crate) fn sv2_inbound_frame_too_large(payload_len: u32, cap: u32) -> bool {
    cap != 0 && payload_len > cap
}

/// TCP connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Noise handshake read timeout.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Steady-state mining read timeout. If no SV2 frame arrives within this window
/// the TCP flow is treated as half-open (peer crash without FIN/RST, or an idle
/// flow silently dropped by a stateful NAT/firewall between blocks) and the
/// session returns Err so the existing reconnect/backoff/failover machinery
/// fires — instead of `reader.read()` blocking forever while the daemon reports
/// `Mining` and produces zero shares. Mirrors the V1 path's 300 s read timeout
/// (v1/connection.rs); >> normal inter-job cadence so a quiet-but-healthy pool
/// never false-fails.
const STEADY_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Encrypted SV2 header size: 6 bytes plaintext + 16 bytes MAC = 22 bytes.
const ENCRYPTED_HEADER_SIZE: usize = 6 + 16;

/// Maximum exponential backoff delay.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Initial backoff delay.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// After this many consecutive pre-mining SV2 failures in Auto mode, fall back to V1.
const AUTO_V2_TO_V1_FALLBACK_THRESHOLD: u32 = 2;

/// While Auto mode is running on V1 fallback, wait this long before re-attempting SV2.
const AUTO_V1_TO_V2_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Repeated Auto-mode SV2->V1 oscillations stretch the next V1 holdoff up to this cap.
const AUTO_V1_TO_V2_MAX_RETRY_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// Add a small deterministic offset so repeated retries do not always line up exactly.
const AUTO_V1_TO_V2_RETRY_JITTER_STEP: Duration = Duration::from_secs(45);
const AUTO_V1_TO_V2_RETRY_JITTER_BUCKETS: u64 = 5;

#[derive(Debug)]
enum Sv2SessionExit {
    Clean { reached_mining: bool },
    Error { reached_mining: bool, error: String },
}

#[derive(Debug, Default)]
struct AutoFallbackState {
    consecutive_fallbacks: u32,
}

impl AutoFallbackState {
    fn consecutive_fallbacks(&self) -> u32 {
        self.consecutive_fallbacks
    }

    fn note_sv2_mining_success(&mut self) {
        self.consecutive_fallbacks = 0;
    }

    fn next_retry_interval(&mut self, sv2_url: &str) -> Duration {
        let penalty_level = self.consecutive_fallbacks.min(3);
        let multiplier = 1u64 << penalty_level;
        let base_secs = AUTO_V1_TO_V2_RETRY_INTERVAL
            .as_secs()
            .saturating_mul(multiplier);
        let jitter_secs = auto_retry_jitter_secs(sv2_url, penalty_level);
        self.consecutive_fallbacks = self.consecutive_fallbacks.saturating_add(1);

        Duration::from_secs(base_secs)
            .saturating_add(Duration::from_secs(jitter_secs))
            .min(AUTO_V1_TO_V2_MAX_RETRY_INTERVAL)
    }
}

fn auto_retry_jitter_secs(sv2_url: &str, penalty_level: u32) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    sv2_url.hash(&mut hasher);
    penalty_level.hash(&mut hasher);

    (hasher.finish() % AUTO_V1_TO_V2_RETRY_JITTER_BUCKETS)
        * AUTO_V1_TO_V2_RETRY_JITTER_STEP.as_secs()
}

/// Apply +/-25% jitter to an exponential reconnect backoff, mirroring the V1
/// path's `Backoff::next_delay` (v1/connection.rs) byte-for-byte: same
/// `rand::random::<u64>()` source (NOT a new RNG), same `delay/4` range, same
/// 100 ms floor. Without this the SV2 reconnect loops slept on the bare
/// exponential value, so a whole fleet that lost a shared pool reconnected in
/// lockstep — a synchronized reconnect storm. The caller keeps the clean
/// exponential `backoff` for the next doubling, so jitter never compounds
/// (same discipline as V1, which stores only `attempt` and re-jitters fresh
/// each call).
fn jittered_backoff(backoff: Duration) -> Duration {
    let delay_ms = backoff.as_millis() as u64;

    // Add +/- 25% jitter (identical to v1/connection.rs Backoff::next_delay).
    let jitter_range = delay_ms / 4;
    let jitter = if jitter_range > 0 {
        let r = rand::random::<u64>() % (jitter_range * 2);
        r as i64 - jitter_range as i64
    } else {
        0
    };

    let final_ms = (delay_ms as i64 + jitter).max(100) as u64;
    Duration::from_millis(final_ms)
}

#[cfg(feature = "jd")]
#[derive(Debug, Clone)]
struct PendingCustomJob {
    candidate: CustomJobCandidate,
    extranonce: Vec<u8>,
}

#[cfg(feature = "jd")]
#[derive(Debug, Clone)]
struct ActiveCustomJob {
    channel_id: u32,
    extranonce: Vec<u8>,
}

/// The async Stratum V2 client for dcentrald.
///
/// Manages the full SV2 lifecycle: TCP connection, Noise_NX handshake,
/// channel setup, job reception, and share submission. Runs as an autonomous
/// tokio task with automatic reconnection and exponential backoff.
///
/// ## Channel Interface
/// Matches `StratumV1Client` exactly — drop-in replacement at the daemon level.
pub struct StratumV2Client {
    config: StratumConfig,

    /// Nominal device hashrate in GH/s. Used in OpenStandardMiningChannel
    /// so the pool can set appropriate initial difficulty.
    nominal_hashrate_ghs: f32,

    /// Send new jobs to the mining pipeline.
    job_tx: mpsc::Sender<JobTemplate>,

    /// Receive valid shares for submission to pool.
    share_rx: mpsc::Receiver<ValidShare>,

    /// Send status updates to the main daemon.
    status_tx: mpsc::Sender<StratumStatus>,

    /// Local rolling pool-acceptance tracker for SV2 share result events.
    acceptance_tracker: crate::acceptance_tracker::AcceptanceTracker,

    /// Optional JD/TDP supervisor feed used for custom job injection.
    #[cfg(feature = "jd")]
    jd_status_rx: Option<watch::Receiver<JdStatus>>,
}

impl StratumV2Client {
    fn active_sv2_pool(&self) -> PoolConfig {
        let mut pool = self.config.pool1.clone();
        if let Some(sv2_url) = &pool.sv2_url {
            pool.url = sv2_url.clone();
        }
        pool
    }

    /// Create a new Stratum V2 client.
    ///
    /// # Arguments
    /// - `config`: Pool URLs, worker credentials, donation settings
    /// - `job_tx`: Send new job templates to the job dispatcher
    /// - `share_rx`: Receive valid shares from the share validator
    /// - `status_tx`: Send status updates (state changes, share results)
    pub fn new(
        config: StratumConfig,
        nominal_hashrate_ghs: f32,
        job_tx: mpsc::Sender<JobTemplate>,
        share_rx: mpsc::Receiver<ValidShare>,
        status_tx: mpsc::Sender<StratumStatus>,
    ) -> Self {
        Self {
            config,
            nominal_hashrate_ghs,
            job_tx,
            share_rx,
            status_tx,
            acceptance_tracker: crate::acceptance_tracker::AcceptanceTracker::new(),
            #[cfg(feature = "jd")]
            jd_status_rx: None,
        }
    }

    #[cfg(feature = "jd")]
    pub fn with_job_declaration_status_rx(mut self, rx: watch::Receiver<JdStatus>) -> Self {
        self.jd_status_rx = Some(rx);
        self
    }

    async fn run_auto_v1_fallback(
        self,
        retry_interval: Duration,
        consecutive_fallbacks: u32,
    ) -> Self {
        self.send_status(StratumStatus::AutoFallbackStateChanged {
            active: true,
            retry_after_s: retry_interval.as_secs(),
            reason: "sv2_early_failures".to_string(),
        })
        .await;

        warn!(
            sv2_url = %self.active_sv2_pool().url,
            v1_url = %self.config.pool1.url,
            retry_sv2_after_s = retry_interval.as_secs(),
            consecutive_fallbacks,
            "Auto mode: falling back from SV2 to V1 after repeated early SV2 failures"
        );

        let Self {
            config,
            nominal_hashrate_ghs,
            job_tx,
            share_rx,
            status_tx,
            acceptance_tracker,
            #[cfg(feature = "jd")]
            jd_status_rx,
        } = self;

        let v1_client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
        let v1_client = v1_client.run_until_sv2_retry(retry_interval).await;
        let (config, job_tx, share_rx, status_tx) = v1_client.into_parts();

        let client = Self {
            config,
            nominal_hashrate_ghs,
            job_tx,
            share_rx,
            status_tx,
            acceptance_tracker,
            #[cfg(feature = "jd")]
            jd_status_rx,
        };

        client
            .send_status(StratumStatus::AutoFallbackStateChanged {
                active: false,
                retry_after_s: 0,
                reason: "retrying_sv2".to_string(),
            })
            .await;

        info!(
            sv2_url = %client.active_sv2_pool().url,
            retry_interval_s = retry_interval.as_secs(),
            consecutive_fallbacks,
            "Auto mode: retrying preferred SV2 endpoint after temporary V1 fallback"
        );

        client
    }

    /// Run the client forever. This is the main entry point -- spawn as a tokio task.
    ///
    /// The client will:
    /// 1. Parse the SV2 pool URL
    /// 2. TCP connect with timeout
    /// 3. Noise_NX handshake (ECDH + encrypted transport)
    /// 4. Send SetupConnection + OpenStandardMiningChannel
    /// 5. Enter the mining loop (receive jobs, submit shares)
    /// 6. Reconnect with exponential backoff on disconnect
    pub async fn run(mut self) {
        let mut backoff = INITIAL_BACKOFF;
        let pool = self.active_sv2_pool();

        // W1.4: SV2 worker is the operator's wallet/identifier — mask it.
        info!(
            primary_pool = %pool.url,
            worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
            version_rolling = self.config.version_rolling,
            "Stratum V2 client starting"
        );

        loop {
            let pool = self.active_sv2_pool();
            let (host, port) = match parse_sv2_url(&pool.url) {
                Ok(hp) => hp,
                Err(e) => {
                    error!(%e, url = %pool.url, "Invalid SV2 pool URL");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // W1.4: mask wallet-shaped worker.
            info!(
                %host, port, worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
                "Connecting to SV2 pool {}:{}", host, port,
            );
            self.send_status(StratumStatus::StateChanged(StratumState::Connecting))
                .await;

            match self.run_session(&host, port).await {
                Sv2SessionExit::Clean { .. } => {
                    info!("SV2 session ended cleanly, reconnecting");
                    backoff = INITIAL_BACKOFF;
                }
                Sv2SessionExit::Error { error: e, .. } => {
                    warn!(%e, "SV2 session error, reconnecting with backoff");
                }
            }

            self.send_status(StratumStatus::StateChanged(StratumState::Disconnected))
                .await;

            let delay = jittered_backoff(backoff);
            info!(
                delay_ms = delay.as_millis() as u64,
                "Waiting {:.1}s before SV2 reconnect",
                delay.as_secs_f32()
            );
            tokio::time::sleep(delay).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Run SV2 in Auto mode, then fall back to V1 if SV2 repeatedly fails
    /// before delivering a mining job.
    pub async fn run_auto_with_v1_fallback(mut self) {
        let mut backoff = INITIAL_BACKOFF;
        let mut early_failures = 0u32;
        let mut fallback_state = AutoFallbackState::default();
        let pool = self.active_sv2_pool();

        // W1.4: mask wallet-shaped worker.
        info!(
            sv2_pool = %pool.url,
            v1_pool = %self.config.pool1.url,
            worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
            threshold = AUTO_V2_TO_V1_FALLBACK_THRESHOLD,
            v1_retry_interval_s = AUTO_V1_TO_V2_RETRY_INTERVAL.as_secs(),
            v1_retry_interval_max_s = AUTO_V1_TO_V2_MAX_RETRY_INTERVAL.as_secs(),
            "Stratum Auto mode starting with SV2 preference"
        );

        loop {
            let pool = self.active_sv2_pool();
            let sv2_endpoint = match parse_sv2_url(&pool.url) {
                Ok(hp) => Some(hp),
                Err(e) => {
                    early_failures += 1;
                    error!(
                        %e,
                        url = %pool.url,
                        consecutive_early_failures = early_failures,
                        "Invalid SV2 pool URL while Auto mode prefers SV2"
                    );
                    if early_failures < AUTO_V2_TO_V1_FALLBACK_THRESHOLD {
                        tokio::time::sleep(jittered_backoff(backoff)).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                    None
                }
            };

            if sv2_endpoint.is_none() {
                self.send_status(StratumStatus::StateChanged(StratumState::Disconnected))
                    .await;
            }

            let Some((host, port)) = sv2_endpoint else {
                // Use the normal fallback path below once the early-failure threshold is hit.
                if early_failures >= AUTO_V2_TO_V1_FALLBACK_THRESHOLD {
                    let retry_interval =
                        fallback_state.next_retry_interval(&self.active_sv2_pool().url);
                    let consecutive_fallbacks = fallback_state.consecutive_fallbacks();
                    self = self
                        .run_auto_v1_fallback(retry_interval, consecutive_fallbacks)
                        .await;
                    early_failures = 0;
                    backoff = INITIAL_BACKOFF;
                    continue;
                }

                continue;
            };

            // W1.4: mask wallet-shaped worker.
            info!(
                %host,
                port,
                worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
                "Auto mode: connecting to preferred SV2 endpoint {}:{}",
                host,
                port,
            );
            self.send_status(StratumStatus::StateChanged(StratumState::Connecting))
                .await;

            match self.run_session(&host, port).await {
                Sv2SessionExit::Clean { reached_mining } => {
                    if reached_mining {
                        info!("SV2 session ended after reaching mining state, reconnecting in SV2");
                        early_failures = 0;
                        fallback_state.note_sv2_mining_success();
                        backoff = INITIAL_BACKOFF;
                    } else {
                        early_failures += 1;
                        warn!(
                            consecutive_early_failures = early_failures,
                            threshold = AUTO_V2_TO_V1_FALLBACK_THRESHOLD,
                            "SV2 session ended before first job in Auto mode"
                        );
                    }
                }
                Sv2SessionExit::Error {
                    reached_mining,
                    error,
                } => {
                    if reached_mining {
                        warn!(%error, "SV2 session error after mining started, reconnecting in SV2");
                        early_failures = 0;
                        fallback_state.note_sv2_mining_success();
                    } else {
                        early_failures += 1;
                        warn!(
                            %error,
                            consecutive_early_failures = early_failures,
                            threshold = AUTO_V2_TO_V1_FALLBACK_THRESHOLD,
                            "SV2 session failed before first job in Auto mode"
                        );
                    }
                }
            }

            self.send_status(StratumStatus::StateChanged(StratumState::Disconnected))
                .await;

            if early_failures >= AUTO_V2_TO_V1_FALLBACK_THRESHOLD {
                let retry_interval =
                    fallback_state.next_retry_interval(&self.active_sv2_pool().url);
                let consecutive_fallbacks = fallback_state.consecutive_fallbacks();
                self = self
                    .run_auto_v1_fallback(retry_interval, consecutive_fallbacks)
                    .await;
                early_failures = 0;
                backoff = INITIAL_BACKOFF;

                continue;
            }

            let delay = jittered_backoff(backoff);
            info!(
                delay_ms = delay.as_millis() as u64,
                "Waiting {:.1}s before Auto-mode SV2 reconnect",
                delay.as_secs_f32()
            );
            tokio::time::sleep(delay).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Run a single SV2 session: connect -> handshake -> mine -> disconnect.
    async fn run_session(&mut self, host: &str, port: u16) -> Sv2SessionExit {
        let mut reached_mining = false;

        let result = self
            .run_session_inner(host, port, &mut reached_mining)
            .await;
        match result {
            Ok(()) => Sv2SessionExit::Clean { reached_mining },
            Err(error) => Sv2SessionExit::Error {
                reached_mining,
                error,
            },
        }
    }

    async fn run_session_inner(
        &mut self,
        host: &str,
        port: u16,
        reached_mining: &mut bool,
    ) -> Result<(), String> {
        // ── Step 1: TCP connect ────────────────────────────────────────
        let addr = format!("{}:{}", host, port);
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| format!("TCP connect timeout to {}", addr))?
            .map_err(|e| format!("TCP connect to {} failed: {}", addr, e))?;

        stream
            .set_nodelay(true)
            .map_err(|e| format!("set_nodelay failed: {}", e))?;

        info!("SV2: TCP connected to {}", addr);

        let (mut reader, mut writer) = tokio::io::split(stream);

        // ── Step 2: SV2 Mining Channel state machine ───────────────────
        let pool = self.active_sv2_pool();
        let worker = &pool.worker;
        // BUG FIX (2026-04-11): Was hardcoded 13500 GH/s (S9 only).
        // Now uses nominal_hashrate_ghs from daemon config so pool sets
        // correct initial difficulty for S19/S21/etc.
        let mut channel = Sv2MiningChannel::new(worker, self.nominal_hashrate_ghs);
        if self.config.sv2_extended_channel {
            channel.enable_work_selection();
            info!("SV2: extended-channel mode enabled by config");
        }
        #[cfg(feature = "jd")]
        let custom_jobs_enabled = self
            .jd_status_rx
            .as_ref()
            .map(|rx| rx.borrow().enabled)
            .unwrap_or(false);
        #[cfg(feature = "jd")]
        if custom_jobs_enabled {
            if !channel.work_selection_enabled() {
                channel.enable_work_selection();
            }
            info!(
                "SV2: work-selection mode enabled; will open an extended channel for JD custom jobs"
            );
        }

        // ── Step 3: Noise_NX handshake ─────────────────────────────────
        // Pin the pool authority key from the SV2 URL (SV2 spec §4.1: the
        // base58check authority key travels in the URL path). When present,
        // the Noise session verifies the server certificate against it and
        // aborts on mismatch — closing the MITM hole. When absent the client
        // proceeds in TOFU mode (NoiseSession logs the exposure at WARN).
        match super::auth::parse_authority_key_from_sv2_url(&pool.url) {
            Ok(key) => {
                info!(
                    "SV2: pinned pool authority key from URL — server certificate will be verified (BIP340)"
                );
                channel.noise_session_mut().pool_authority_key = Some(*key.as_bytes());
            }
            Err(super::auth::AuthorityKeyError::NotPresent) => {
                warn!(
                    "SV2: no authority key in pool URL — operating in TOFU mode (active MITM possible). Append the pool's base58check authority key to the SV2 URL to enable verification."
                );
            }
            Err(e) => {
                // A present-but-malformed key is a hard configuration error:
                // refuse rather than silently downgrade to TOFU.
                return Err(format!(
                    "SV2: pool URL carries an invalid authority key ({}). Refusing to connect — fix the SV2 URL or remove the key for explicit TOFU.",
                    e
                ));
            }
        }

        // Generate RNG seed for the ephemeral keypair
        let mut rng_seed = [0u8; 64];
        use rand_core::RngCore;
        rand_core::OsRng.fill_bytes(&mut rng_seed);

        // Act 1: -> e (send our ephemeral public key)
        let act1 = channel
            .noise_session_mut()
            .initiator_handshake_start(rng_seed)
            .map_err(|e| format!("Noise handshake start failed: {}", e))?;

        info!("SV2: Noise -> e sent ({} bytes)", act1.len());
        writer
            .write_all(&act1)
            .await
            .map_err(|e| format!("Failed to send Noise act1: {}", e))?;

        // Act 2: <- e, ee, s, es (read server response)
        let mut hs_buf = vec![0u8; 512];
        let hs_len = timeout(HANDSHAKE_TIMEOUT, reader.read(&mut hs_buf))
            .await
            .map_err(|_| "Noise handshake timeout (15s)".to_string())?
            .map_err(|e| format!("Failed to read Noise response: {}", e))?;

        if hs_len == 0 {
            return Err("Server closed connection during Noise handshake".into());
        }

        info!("SV2: Noise <- response ({} bytes)", hs_len);

        channel
            .noise_session_mut()
            .initiator_handshake_finish(&hs_buf[..hs_len])
            .map_err(|e| format!("Noise handshake finish failed: {}", e))?;

        info!("SV2: Noise_NX handshake COMPLETE -- encrypted transport active");

        // Send SV2 session metadata to daemon after handshake
        {
            let noise = channel.noise_session();
            let (cert_from, cert_after) = noise.certificate_info().unwrap_or((0, 0));
            self.send_status(StratumStatus::Sv2SessionUpdated {
                cipher_suite: "Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256".to_string(),
                handshake_latency_ms: noise.handshake_latency_ms().unwrap_or(0),
                pool_pubkey_fingerprint: noise.server_static_key_hex().unwrap_or_default(),
                certificate_valid_from: cert_from,
                certificate_not_after: cert_after,
                channel_id: channel.channel_id(),
                noise_nonce_tx: noise.nonce_tx(),
                noise_nonce_rx: noise.nonce_rx(),
                bytes_encrypted: noise.bytes_encrypted(),
                bytes_decrypted: noise.bytes_decrypted(),
                messages_sent: channel.messages_sent(),
                messages_received: channel.messages_received(),
            })
            .await;
        }

        // ── Step 4: SetupConnection ────────────────────────────────────
        // BUG FIX (2026-04-11): Set actual connected endpoint instead of hardcoded Braiins.
        channel.set_endpoint(host, port);
        let setup_msg = channel.make_setup_connection();
        send_noise_frame(&mut channel, &mut writer, &setup_msg).await?;
        info!(
            "SV2: SetupConnection sent ({} bytes plaintext)",
            setup_msg.len()
        );

        self.send_status(StratumStatus::StateChanged(StratumState::Connecting))
            .await;

        // ── Step 5: Mining loop ────────────────────────────────────────
        let mut noise_recv_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut read_buf = vec![0u8; 4096];
        let mut pending_header: Option<[u8; 6]> = None;
        let mut current_share_target = [0xFFu8; 32]; // Accept all until pool sets target
        let mut current_pool_difficulty = 0.0f64;
        let extranonce_prefix: Vec<u8> = Vec::new();
        let version_mask: u32 = if self.config.version_rolling {
            self.config.version_rolling_mask
        } else {
            0
        };
        let mut _last_job: Option<JobTemplate> = None;
        let session_start = Instant::now();
        let mut last_sv2_update = Instant::now();
        #[cfg(feature = "jd")]
        let mut jd_status_rx = self.jd_status_rx.clone();
        #[cfg(not(feature = "jd"))]
        let mut jd_status_rx: Option<()> = None;
        #[cfg(feature = "jd")]
        let mut last_custom_candidate_key: Option<CustomJobCandidateKey> = None;
        #[cfg(feature = "jd")]
        let mut pending_custom_jobs: HashMap<u32, PendingCustomJob> = HashMap::new();
        #[cfg(feature = "jd")]
        let mut active_custom_jobs: HashMap<u32, ActiveCustomJob> = HashMap::new();
        // Non-JD extended-channel jobs: (job_id -> chosen extranonce). The
        // miner picks one fixed extranonce per job (zero-filled) so the
        // header-only ASIC dispatch path stays unchanged. Shares for these
        // job_ids must round-trip through `SubmitSharesExtended` carrying the
        // same extranonce.
        let mut active_extended_jobs: HashMap<u32, Vec<u8>> = HashMap::new();

        loop {
            tokio::select! {
                // ── Branch A: Read encrypted data from pool ────────────
                read_result = timeout(STEADY_READ_TIMEOUT, reader.read(&mut read_buf)) => {
                    // A half-open flow makes reader.read() block forever; the
                    // timeout turns that into an Err so the outer reconnect loop
                    // fires (mirrors the V1 read timeout).
                    let read_result = match read_result {
                        Ok(r) => r,
                        Err(_elapsed) => {
                            return Err(format!(
                                "SV2: no data from pool for {}s — flow appears half-open, failing into reconnect",
                                STEADY_READ_TIMEOUT.as_secs()
                            ));
                        }
                    };
                    match read_result {
                        Ok(0) => {
                            let uptime = session_start.elapsed();
                            info!(
                                session_secs = uptime.as_secs(),
                                "SV2: pool closed connection after {:.0}s",
                                uptime.as_secs_f32()
                            );
                            return Ok(());
                        }
                        Ok(n) => {
                            noise_recv_buf.extend_from_slice(&read_buf[..n]);
                        }
                        Err(e) => {
                            return Err(format!("SV2: TCP read error: {}", e));
                        }
                    }

                    // Process complete Noise-encrypted SV2 frames.
                    // SV2 Noise transport: [encrypted_header (22 bytes)] [encrypted_payload (N+16 bytes)]
                    // The pending_header buffer prevents nonce desync when payload
                    // arrives in a later read.
                    loop {
                        // Step 1: Get the decrypted header (from pending or decrypt now)
                        let header_plain: [u8; 6] = if let Some(h) = pending_header {
                            h
                        } else {
                            if noise_recv_buf.len() < ENCRYPTED_HEADER_SIZE {
                                break;
                            }

                            let header_cipher = noise_recv_buf[..ENCRYPTED_HEADER_SIZE].to_vec();
                            let h = match channel.noise_session_mut().decrypt(&header_cipher) {
                                Ok(h) => h,
                                Err(e) => {
                                    error!("SV2: Noise header decrypt failed: {}", e);
                                    return Err(format!("Noise header decrypt: {}", e));
                                }
                            };

                            if h.len() != 6 {
                                return Err(format!("SV2: decrypted header wrong size: {}", h.len()));
                            }

                            // Drain encrypted header (nonce already incremented)
                            noise_recv_buf.drain(..ENCRYPTED_HEADER_SIZE);

                            let mut arr = [0u8; 6];
                            arr.copy_from_slice(&h);
                            arr
                        };

                        // Step 2: Parse payload length from decrypted SV2 header
                        let payload_len = u32::from_le_bytes([
                            header_plain[3], header_plain[4], header_plain[5], 0
                        ]) as usize;
                        let encrypted_payload_size = payload_len + 16; // payload + MAC

                        // Operational inbound-frame cap (strat-09): reject an
                        // over-cap announced frame HERE — before Step 3 keeps
                        // growing `noise_recv_buf` toward `payload_len` across
                        // reads — so a hostile/buggy pool can't amplify memory
                        // toward the 16 MiB wire max on a 228 MB-class miner.
                        // `payload_len` is a 24-bit on-wire field (≤ 16 MiB-1),
                        // always fits u32. Fail-closed into the EXISTING SV2
                        // reconnect/backoff (this `Err` propagates out of the
                        // read loop just like the Noise-decrypt siblings — no
                        // parallel failover path).
                        if sv2_inbound_frame_too_large(
                            payload_len as u32,
                            self.config.sv2_max_inbound_frame_bytes,
                        ) {
                            let msg = format!(
                                "SV2: pool announced oversized inbound frame: {} bytes > cap {} bytes (msg_type 0x{:02x})",
                                payload_len,
                                self.config.sv2_max_inbound_frame_bytes,
                                header_plain[2]
                            );
                            error!("{}", msg);
                            return Err(msg);
                        }

                        // Step 3: Check if we have the full payload
                        if noise_recv_buf.len() < encrypted_payload_size {
                            // Stash header -- nonce is safe because we already drained
                            // the encrypted header bytes and incremented the nonce
                            pending_header = Some(header_plain);
                            break;
                        }

                        // Clear pending header -- processing this frame now
                        pending_header = None;

                        // Step 4: Decrypt the payload
                        let payload_cipher = noise_recv_buf[..encrypted_payload_size].to_vec();
                        noise_recv_buf.drain(..encrypted_payload_size);

                        let payload_plain = match channel.noise_session_mut().decrypt(&payload_cipher) {
                            Ok(p) => p,
                            Err(e) => {
                                error!("SV2: Noise payload decrypt failed: {}", e);
                                return Err(format!("Noise payload decrypt: {}", e));
                            }
                        };

                        // Reconstruct full SV2 frame and feed to channel state machine
                        let mut sv2_frame = Vec::with_capacity(6 + payload_plain.len());
                        sv2_frame.extend_from_slice(&header_plain);
                        sv2_frame.extend_from_slice(&payload_plain);

                        debug!(
                            msg_type = format_args!("0x{:02x}", header_plain[2]),
                            payload_len = payload_plain.len(),
                            "SV2: decrypted frame"
                        );

                        let events = channel.feed_data(&sv2_frame);

                        // Process channel events
                        for event in events {
                            match event {
                                Sv2Event::Connected => {
                                    info!("SV2: SetupConnection accepted, opening mining channel");
                                    let open_msg = channel.make_open_channel();
                                    send_noise_frame(&mut channel, &mut writer, &open_msg).await?;
                                    if channel.work_selection_enabled() {
                                        info!("SV2: OpenExtendedMiningChannel sent");
                                    } else {
                                        info!("SV2: OpenStandardMiningChannel sent");
                                    }
                                }

                                Sv2Event::NewJob {
                                    job_id,
                                    version,
                                    prev_hash,
                                    merkle_root,
                                    nbits,
                                    ntime,
                                    clean_jobs,
                                } => {
                                    info!(
                                        job_id,
                                        version = format_args!("0x{:08x}", version),
                                        nbits = format_args!("0x{:08x}", nbits),
                                        clean_jobs,
                                        "SV2: new mining job"
                                    );

                                    // Build a synthetic NewMiningJob + SetNewPrevHash for the adapter
                                    let sv2_job = super::types::NewMiningJob {
                                        channel_id: 0,
                                        job_id,
                                        future_job: false,
                                        version,
                                        merkle_root,
                                    };
                                    let sv2_prev = super::types::SetNewPrevHash {
                                        channel_id: 0,
                                        job_id,
                                        prev_hash,
                                        min_ntime: ntime,
                                        nbits,
                                    };

                                    let template = adapter::sv2_to_job_template(
                                        &sv2_job,
                                        &sv2_prev,
                                        channel.channel_id().unwrap_or(0),
                                        &extranonce_prefix,
                                        version_mask,
                                        current_share_target,
                                    );

                                    _last_job = Some(template.clone());

                                    if let Err(e) = self.job_tx.send(template).await {
                                        error!("SV2: failed to send job to dispatcher: {}", e);
                                    }

                                    // Report mining state
                                    *reached_mining = true;

                                    self.send_status(
                                        StratumStatus::StateChanged(StratumState::Mining),
                                    )
                                    .await;

                                    // Periodic SV2 session metadata update (~every 30s)
                                    if last_sv2_update.elapsed() >= Duration::from_secs(30) {
                                        let noise = channel.noise_session();
                                        let (cert_from, cert_after) = noise.certificate_info().unwrap_or((0, 0));
                                        self.send_status(StratumStatus::Sv2SessionUpdated {
                                            cipher_suite: "Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256".to_string(),
                                            handshake_latency_ms: noise.handshake_latency_ms().unwrap_or(0),
                                            pool_pubkey_fingerprint: noise.server_static_key_hex().unwrap_or_default(),
                                            certificate_valid_from: cert_from,
                                            certificate_not_after: cert_after,
                                            channel_id: channel.channel_id(),
                                            noise_nonce_tx: noise.nonce_tx(),
                                            noise_nonce_rx: noise.nonce_rx(),
                                            bytes_encrypted: noise.bytes_encrypted(),
                                            bytes_decrypted: noise.bytes_decrypted(),
                                            messages_sent: channel.messages_sent(),
                                            messages_received: channel.messages_received(),
                                        }).await;
                                        last_sv2_update = Instant::now();
                                    }
                                }

                                Sv2Event::NewExtendedJob {
                                    job_id,
                                    version,
                                    version_rolling_allowed,
                                    prev_hash,
                                    nbits,
                                    ntime,
                                    merkle_path,
                                    coinbase_tx_prefix,
                                    coinbase_tx_suffix,
                                    clean_jobs,
                                } => {
                                    let extranonce_size =
                                        usize::from(channel.channel_extranonce_size());
                                    if extranonce_size > 32 {
                                        warn!(
                                            job_id,
                                            extranonce_size,
                                            "SV2: refusing NewExtendedMiningJob with oversized extranonce_size"
                                        );
                                        continue;
                                    }
                                    // Pick one fixed extranonce per job. DCENT_OS
                                    // dispatches header-only work, so a single
                                    // zero-filled extranonce is sufficient — the
                                    // pool sees a stable coinbase per job_id and
                                    // shares round-trip the same extranonce.
                                    let extranonce = vec![0u8; extranonce_size];
                                    let extranonce_prefix_owned =
                                        channel.channel_extranonce_prefix().to_vec();

                                    info!(
                                        job_id,
                                        version = format_args!("0x{:08x}", version),
                                        version_rolling_allowed,
                                        nbits = format_args!("0x{:08x}", nbits),
                                        merkle_path_len = merkle_path.len(),
                                        prefix_bytes = coinbase_tx_prefix.len(),
                                        suffix_bytes = coinbase_tx_suffix.len(),
                                        extranonce_prefix_len = extranonce_prefix_owned.len(),
                                        extranonce_size,
                                        clean_jobs,
                                        "SV2: new extended mining job"
                                    );

                                    let template = adapter::extended_job_to_job_template(
                                        ExtendedJobAssembly {
                                            job_id,
                                            version,
                                            version_rolling_allowed,
                                            prev_hash,
                                            nbits,
                                            ntime,
                                            coinbase_tx_prefix: &coinbase_tx_prefix,
                                            coinbase_tx_suffix: &coinbase_tx_suffix,
                                            merkle_path: &merkle_path,
                                            extranonce_prefix: &extranonce_prefix_owned,
                                            extranonce: &extranonce,
                                            version_mask,
                                            share_target: current_share_target,
                                        },
                                    );
                                    _last_job = Some(template.clone());

                                    // Bound memory: a clean-jobs flag drops everything
                                    // older than this job_id, otherwise keep the last 16.
                                    if clean_jobs {
                                        active_extended_jobs.retain(|id, _| *id >= job_id);
                                    }
                                    active_extended_jobs.insert(job_id, extranonce);
                                    if active_extended_jobs.len() > 16 {
                                        let oldest = active_extended_jobs
                                            .keys()
                                            .min()
                                            .copied();
                                        if let Some(k) = oldest {
                                            active_extended_jobs.remove(&k);
                                        }
                                    }

                                    if let Err(e) = self.job_tx.send(template).await {
                                        error!(
                                            "SV2: failed to send extended job to dispatcher: {}",
                                            e
                                        );
                                    }

                                    *reached_mining = true;
                                    self.send_status(StratumStatus::StateChanged(
                                        StratumState::Mining,
                                    ))
                                    .await;
                                }

                                Sv2Event::DifficultyChanged(diff) => {
                                    info!(difficulty = diff, "SV2: difficulty changed");
                                    // Update share target from difficulty
                                    current_pool_difficulty = diff;
                                    current_share_target =
                                        crate::work::difficulty_to_target(diff);
                                    self.send_status(StratumStatus::DifficultyChanged(diff))
                                        .await;
                                    #[cfg(feature = "jd")]
                                    if channel.work_selection_enabled() && channel.is_mining() {
                                        if let Some(jd_status) = jd_status_rx
                                            .as_ref()
                                            .map(|rx| rx.borrow().clone())
                                        {
                                            match try_send_custom_job_candidate(
                                                &mut channel,
                                                &mut writer,
                                                &jd_status,
                                                &mut last_custom_candidate_key,
                                                &mut pending_custom_jobs,
                                            )
                                            .await
                                            {
                                                Ok(Some((channel_id, request_id, template_id))) => {
                                                    info!(
                                                        channel_id,
                                                        request_id,
                                                        template_id,
                                                        "SV2: declared custom mining job to upstream pool"
                                                    );
                                                    self.send_status(
                                                        StratumStatus::Sv2CustomJobDeclared {
                                                            channel_id,
                                                            request_id,
                                                            template_id,
                                                        },
                                                    )
                                                    .await;
                                                }
                                                Ok(None) => {}
                                                Err(error) => {
                                                    warn!(%error, "SV2: failed to declare custom mining job");
                                                    self.send_status(
                                                        StratumStatus::Sv2CustomJobRejected {
                                                            channel_id: channel
                                                                .channel_id()
                                                                .unwrap_or(0),
                                                            request_id: 0,
                                                            template_id: jd_status
                                                                .current_template_id,
                                                            reason: error,
                                                        },
                                                    )
                                                    .await;
                                                }
                                            }
                                        }
                                    }
                                }

                                Sv2Event::ShareAccepted {
                                    job_id,
                                    last_sequence_number,
                                    new_submits_accepted_count,
                                    new_shares_sum,
                                } => {
                                    // Per-share pool target uses the batch
                                    // average (sum/count) when the pool reports
                                    // both. This stays accurate even if a
                                    // SetTarget difficulty change straddled
                                    // the batch; falls back to
                                    // current_pool_difficulty for legacy
                                    // 8-byte truncated payloads (count=0,
                                    // sum=0).
                                    let per_share_difficulty =
                                        super::difficulty_autotune::batch_average_share_difficulty(
                                            new_shares_sum,
                                            new_submits_accepted_count,
                                            current_pool_difficulty,
                                        );
                                    info!(
                                        job_id,
                                        last_sequence_number,
                                        new_submits_accepted_count,
                                        new_shares_sum,
                                        per_share_difficulty,
                                        "SV2: shares accepted batch"
                                    );
                                    // Emit one StratumStatus::ShareAccepted per
                                    // share the pool credited in this batch so
                                    // downstream rate counters stay accurate
                                    // (V1 was per-share; SV2 batches the ack).
                                    // When the pool reports zero shares accepted
                                    // we still emit one for backwards-compat
                                    // observability but use the batch sentinel
                                    // job_id=0.
                                    let emit_count = new_submits_accepted_count.max(1);
                                    for _ in 0..emit_count {
                                        self.acceptance_tracker.record_share(true);
                                        self.send_status(StratumStatus::ShareAccepted {
                                            job_id: job_id.to_string(),
                                            pool_target_difficulty: per_share_difficulty,
                                            achieved_difficulty: None,
                                            meta: None,
                                        })
                                        .await;
                                    }
                                    let rolling_pct =
                                        self.acceptance_tracker.rolling_acceptance_pct();
                                    let rolling_count = self.acceptance_tracker.rolling_count();
                                    self.send_status(StratumStatus::RollingAcceptanceUpdated {
                                        pct: rolling_pct,
                                        accepted: rolling_count.0,
                                        total: rolling_count.1,
                                    })
                                    .await;
                                }

                                Sv2Event::ShareRejected { job_id, reason } => {
                                    warn!(job_id, %reason, "SV2: share rejected");
                                    self.send_status(StratumStatus::ShareRejected {
                                        job_id: job_id.to_string(),
                                        error_code: -1,
                                        error_msg: reason,
                                        meta: None,
                                    })
                                    .await;
                                    self.acceptance_tracker.record_share(false);
                                    let rolling_pct =
                                        self.acceptance_tracker.rolling_acceptance_pct();
                                    let rolling_count = self.acceptance_tracker.rolling_count();
                                    self.send_status(StratumStatus::RollingAcceptanceUpdated {
                                        pct: rolling_pct,
                                        accepted: rolling_count.0,
                                        total: rolling_count.1,
                                    })
                                    .await;
                                }

                                Sv2Event::CustomJobAccepted {
                                    channel_id,
                                    request_id,
                                    job_id,
                                } => {
                                    #[cfg(feature = "jd")]
                                    if let Some(pending) = pending_custom_jobs.remove(&request_id) {
                                        let template_id = pending.candidate.template_id;
                                        let template = adapter::custom_job_to_job_template(
                                            &pending.candidate,
                                            job_id,
                                            channel.channel_extranonce_prefix(),
                                            &pending.extranonce,
                                            version_mask,
                                            current_share_target,
                                        );
                                        active_custom_jobs.insert(
                                            job_id,
                                            ActiveCustomJob {
                                                channel_id,
                                                extranonce: pending.extranonce,
                                            },
                                        );
                                        _last_job = Some(template.clone());
                                        if let Err(e) = self.job_tx.send(template).await {
                                            error!(
                                                "SV2: failed to send accepted custom job to dispatcher: {}",
                                                e
                                            );
                                        }
                                        *reached_mining = true;
                                        self.send_status(StratumStatus::StateChanged(
                                            StratumState::Mining,
                                        ))
                                        .await;
                                        self.send_status(StratumStatus::Sv2CustomJobAccepted {
                                            channel_id,
                                            request_id,
                                            template_id,
                                            job_id,
                                        })
                                        .await;
                                    }
                                    #[cfg(not(feature = "jd"))]
                                    {
                                        let _ = (channel_id, request_id, job_id);
                                    }
                                }

                                Sv2Event::CustomJobRejected {
                                    channel_id,
                                    request_id,
                                    reason,
                                } => {
                                    #[cfg(feature = "jd")]
                                    {
                                        let template_id = pending_custom_jobs
                                            .remove(&request_id)
                                            .map(|pending| pending.candidate.template_id);
                                        self.send_status(StratumStatus::Sv2CustomJobRejected {
                                            channel_id,
                                            request_id,
                                            template_id,
                                            reason,
                                        })
                                        .await;
                                    }
                                    #[cfg(not(feature = "jd"))]
                                    {
                                        let _ = (channel_id, request_id, reason);
                                    }
                                }

                                Sv2Event::GroupChannelAssigned {
                                    group_channel_id,
                                    channel_ids,
                                } => {
                                    info!(
                                        group_channel_id,
                                        channel_count = channel_ids.len(),
                                        current_channel_id = ?channel.channel_id(),
                                        "SV2: group channel assigned"
                                    );
                                }

                                Sv2Event::ExtranoncePrefixChanged { channel_id, prefix } => {
                                    info!(
                                        channel_id,
                                        prefix_len = prefix.len(),
                                        current_channel_id = ?channel.channel_id(),
                                        "SV2: extranonce prefix changed"
                                    );
                                }

                                Sv2Event::Disconnected(reason) => {
                                    warn!(%reason, "SV2: channel disconnected");
                                    return Err(format!("SV2 channel: {}", reason));
                                }

                                Sv2Event::Reconnect { host, port } => {
                                    info!(%host, port, "SV2: pool requested reconnect");
                                    self.send_status(StratumStatus::ReconnectRequested {
                                        host,
                                        port,
                                        wait_seconds: 0,
                                    })
                                    .await;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }

                // ── Branch B: Submit shares ────────────────────────────
                jd_status = recv_jd_status(&mut jd_status_rx), if jd_status_rx.is_some() => {
                    #[cfg(feature = "jd")]
                    {
                    let Some(jd_status) = jd_status else {
                        jd_status_rx = None;
                        continue;
                    };
                    match try_send_custom_job_candidate(
                        &mut channel,
                        &mut writer,
                        &jd_status,
                        &mut last_custom_candidate_key,
                        &mut pending_custom_jobs,
                    )
                    .await
                    {
                        Ok(Some((channel_id, request_id, template_id))) => {
                            info!(
                                channel_id,
                                request_id,
                                template_id,
                                "SV2: declared custom mining job to upstream pool"
                            );
                            self.send_status(StratumStatus::Sv2CustomJobDeclared {
                                channel_id,
                                request_id,
                                template_id,
                            })
                            .await;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%error, "SV2: failed to declare custom mining job");
                            self.send_status(StratumStatus::Sv2CustomJobRejected {
                                channel_id: channel.channel_id().unwrap_or(0),
                                request_id: 0,
                                template_id: jd_status.current_template_id,
                                reason: error,
                            })
                            .await;
                        }
                    }
                    }
                    #[cfg(not(feature = "jd"))]
                    {
                        let _ = jd_status;
                    }
                },

                share = self.share_rx.recv() => {
                    match share {
                        Some(share) => {
                            let channel_id = match channel.channel_id() {
                                Some(id) => id,
                                None => {
                                    warn!("SV2: share received but no channel open, dropping");
                                    continue;
                                }
                            };

                            let (ch, _seq, job_id, nonce, ntime, version) =
                                adapter::valid_share_to_sv2_submit(
                                    &share,
                                    channel_id,
                                    channel.sequence_number() + 1,
                                );

                            debug!(
                                job_id,
                                nonce = format_args!("0x{:08x}", nonce),
                                ntime = format_args!("0x{:08x}", ntime),
                                version = format_args!("0x{:08x}", version),
                                "SV2: submitting share"
                            );

                            // Route order:
                            //   1. non-JD extended-channel jobs picked up via
                            //      `Sv2Event::NewExtendedJob` (must use
                            //      SubmitSharesExtended with the per-job extranonce)
                            //   2. JD-feature custom jobs (also extended)
                            //   3. standard channel jobs
                            #[cfg(feature = "jd")]
                            let submit_frame =
                                if let Some(extranonce) = active_extended_jobs.get(&job_id) {
                                    channel.make_submit_share_extended(
                                        ch, job_id, nonce, ntime, version, extranonce,
                                    )
                                } else if let Some(active) = active_custom_jobs.get(&job_id) {
                                    channel.make_submit_share_extended(
                                        active.channel_id,
                                        job_id,
                                        nonce,
                                        ntime,
                                        version,
                                        &active.extranonce,
                                    )
                                } else {
                                    channel.make_submit_share(ch, job_id, nonce, ntime, version)
                                };
                            #[cfg(not(feature = "jd"))]
                            let submit_frame =
                                if let Some(extranonce) = active_extended_jobs.get(&job_id) {
                                    channel.make_submit_share_extended(
                                        ch, job_id, nonce, ntime, version, extranonce,
                                    )
                                } else {
                                    channel.make_submit_share(ch, job_id, nonce, ntime, version)
                                };
                            if let Err(e) = send_noise_frame(
                                &mut channel,
                                &mut writer,
                                &submit_frame,
                            )
                            .await
                            {
                                error!(%e, "SV2: failed to send share");
                                return Err(format!("SV2 share send: {}", e));
                            }
                        }
                        None => {
                            info!("SV2: share channel closed, shutting down");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    /// Send a status update to the daemon.
    async fn send_status(&self, status: StratumStatus) {
        if let Err(e) = self.status_tx.send(status).await {
            warn!("SV2: failed to send status: {}", e);
        }
    }
}

#[cfg(feature = "jd")]
async fn recv_jd_status(rx: &mut Option<watch::Receiver<JdStatus>>) -> Option<JdStatus> {
    let Some(rx) = rx.as_mut() else {
        std::future::pending::<()>().await;
        return None;
    };
    match rx.changed().await {
        Ok(()) => Some(rx.borrow().clone()),
        Err(_) => None,
    }
}

#[cfg(not(feature = "jd"))]
async fn recv_jd_status(_rx: &mut Option<()>) -> Option<()> {
    std::future::pending::<()>().await;
    None
}

#[cfg(feature = "jd")]
async fn try_send_custom_job_candidate(
    channel: &mut Sv2MiningChannel,
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    status: &JdStatus,
    last_candidate_key: &mut Option<CustomJobCandidateKey>,
    pending_custom_jobs: &mut HashMap<u32, PendingCustomJob>,
) -> Result<Option<(u32, u32, u64)>, String> {
    if !channel.work_selection_enabled()
        || !channel.is_mining()
        || !status.custom_job_candidate_ready
    {
        return Ok(None);
    }
    let Some(candidate) = status.custom_job_candidate.as_ref() else {
        return Ok(None);
    };
    let key = candidate.stable_key();
    if last_candidate_key.as_ref() == Some(&key) {
        return Ok(None);
    }
    let Some(extranonce) = channel.fixed_custom_job_extranonce() else {
        return Err(format!(
            "SV2: extended channel assigned unsupported extranonce_size={}",
            channel.channel_extranonce_size()
        ));
    };
    let channel_id = channel
        .channel_id()
        .ok_or_else(|| "SV2: no channel open for custom job".to_string())?;
    let (request_id, frame) = channel.make_set_custom_mining_job(candidate)?;
    pending_custom_jobs.insert(
        request_id,
        PendingCustomJob {
            candidate: candidate.clone(),
            extranonce,
        },
    );
    send_noise_frame(channel, writer, &frame).await?;
    *last_candidate_key = Some(key);
    Ok(Some((channel_id, request_id, candidate.template_id)))
}

/// Send an SV2 frame through the Noise encrypted transport.
///
/// SV2 Noise transport encrypts header and payload separately:
///   1. EncryptWithAd([], 6-byte SV2 header) -> 22 bytes (6 + 16 MAC)
///   2. EncryptWithAd([], payload)            -> payload_len + 16 MAC
///
/// Each encrypted block uses a separate nonce (auto-incremented by ChaChaPoly).
async fn send_noise_frame(
    channel: &mut Sv2MiningChannel,
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    sv2_frame: &[u8],
) -> Result<(), String> {
    if sv2_frame.len() < 6 {
        return Err("SV2: frame too short for header".into());
    }

    let header = &sv2_frame[..6];
    let payload = &sv2_frame[6..];

    // Encrypt SV2 header (6 bytes -> 22 bytes with MAC)
    let encrypted_header = channel
        .noise_session_mut()
        .encrypt(header)
        .map_err(|e| format!("Noise encrypt header failed: {}", e))?;

    // Encrypt SV2 payload (variable -> payload_len + 16 with MAC)
    let encrypted_payload = channel
        .noise_session_mut()
        .encrypt(payload)
        .map_err(|e| format!("Noise encrypt payload failed: {}", e))?;

    // Write both encrypted blocks
    writer
        .write_all(&encrypted_header)
        .await
        .map_err(|e| format!("Failed to write encrypted header: {}", e))?;
    writer
        .write_all(&encrypted_payload)
        .await
        .map_err(|e| format!("Failed to write encrypted payload: {}", e))?;

    debug!(
        header_bytes = encrypted_header.len(),
        payload_bytes = encrypted_payload.len(),
        plaintext_bytes = sv2_frame.len(),
        "SV2: sent encrypted frame"
    );

    Ok(())
}

/// Strip the optional SV2 authority-key path component from a pool URL,
/// leaving just `scheme://host:port` for the host:port validator.
///
/// Per the SV2 spec the pinned authority key is appended as a URL path
/// (`stratum2+tcp://host:port/<base58check_key>`). The shared
/// `url_validator::validate_sv2_pool_url` deliberately rejects any path
/// component (it only validates host:port and is used by other call
/// sites). The authority key itself is parsed separately by
/// [`super::auth::parse_authority_key_from_sv2_url`] from the *full* URL,
/// so here we only need the base endpoint for the TCP connect.
fn strip_authority_key_path(url: &str) -> &str {
    // Find the first '/', '?' or '#' that appears *after* the "://"
    // scheme separator and truncate there. Schemes + host:port are ASCII
    // so byte indexing is char-boundary-safe.
    let Some(sep) = url.find("://") else {
        return url;
    };
    let after_scheme = sep + "://".len();
    match url[after_scheme..].find(['/', '?', '#']) {
        Some(rel) => &url[..after_scheme + rel],
        None => url,
    }
}

/// Parse an SV2 pool URL.
///
/// Accepted formats:
/// - `stratum2+tcp://host:port`
/// - `sv2+tcp://host:port`
/// - `stratum2+tcp://host:port/<base58check_authority_key>` (key stripped
///   here; parsed by [`super::auth`])
///
/// Returns (host, port).
fn parse_sv2_url(url: &str) -> Result<(String, u16), String> {
    let endpoint = strip_authority_key_path(url);
    let parsed = crate::url_validator::validate_sv2_pool_url(endpoint)
        .map_err(|e| format!("Invalid SV2 pool URL: {}", e))?;
    Ok((parsed.host, parsed.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // SV2 inbound-frame operational cap (strat-09 hardening).
    //
    // Pins the over-cap predicate's boundary independently of any
    // socket / Noise / hardware so a future refactor that weakens it
    // (e.g. flips the cap off, makes `0` mean "reject everything", or
    // off-by-ones the boundary) lights up the suite. Plan/review:
    //
    // ---------------------------------------------------------------

    /// The shipped serde default is the 1 MiB operational cap (≈8× the
    /// largest realistic pool→miner mining message).
    const DEFAULT_CAP: u32 = 1_048_576;
    /// The 24-bit SV2 wire-format protocol max (`framing::MAX_PAYLOAD_SIZE`).
    const PROTOCOL_MAX: u32 = (1 << 24) - 1;

    #[test]
    fn sv2_cap_disabled_passes_everything_including_protocol_max() {
        // cap == 0 ⇒ disabled: the 16 MiB wire max still bounds it in
        // framing.rs, but the operational policy must NOT reject here.
        assert!(!sv2_inbound_frame_too_large(0, 0));
        assert!(!sv2_inbound_frame_too_large(1, 0));
        assert!(!sv2_inbound_frame_too_large(DEFAULT_CAP, 0));
        assert!(!sv2_inbound_frame_too_large(PROTOCOL_MAX, 0));
        assert!(!sv2_inbound_frame_too_large(u32::MAX, 0));
    }

    #[test]
    fn sv2_cap_allows_at_and_below_cap() {
        assert!(!sv2_inbound_frame_too_large(0, DEFAULT_CAP));
        assert!(!sv2_inbound_frame_too_large(1, DEFAULT_CAP));
        assert!(!sv2_inbound_frame_too_large(DEFAULT_CAP - 1, DEFAULT_CAP));
        // Exactly at the cap is allowed (cap is the inclusive max).
        assert!(!sv2_inbound_frame_too_large(DEFAULT_CAP, DEFAULT_CAP));
    }

    #[test]
    fn sv2_cap_rejects_one_byte_over_and_protocol_max() {
        assert!(sv2_inbound_frame_too_large(DEFAULT_CAP + 1, DEFAULT_CAP));
        // The exact attack: a pool announcing the 16 MiB wire max while
        // the operational default is 1 MiB → rejected before any
        // 16 MiB-class allocation.
        assert!(sv2_inbound_frame_too_large(PROTOCOL_MAX, DEFAULT_CAP));
        assert!(sv2_inbound_frame_too_large(u32::MAX, DEFAULT_CAP));
    }

    #[test]
    fn sv2_cap_passes_realistic_largest_mining_message() {
        // Largest realistic pool→miner mining message: a full-coinbase
        // NewExtendedMiningJob ≈ B0_64K prefix + B0_64K suffix +
        // SEQ0_255[U256] merkle path ≈ 65535 + 65535 + 255*32 ≈ 139 KB.
        // Must pass under the 1 MiB default with generous headroom.
        let realistic_max: u32 = 65_535 + 65_535 + 255 * 32;
        assert!(realistic_max < DEFAULT_CAP);
        assert!(!sv2_inbound_frame_too_large(realistic_max, DEFAULT_CAP));
    }

    #[test]
    fn sv2_cap_default_is_secure_by_default_against_the_attack() {
        // The on-by-default contract: with the shipped 1 MiB cap, the
        // exact memory-amplification attack (announce the 16 MiB wire
        // max) is rejected with NO operator action required. The
        // serde-default ↔ DEFAULT_CAP linkage is pinned authoritatively
        // in `types.rs` (private default fn in scope there).
        assert!(sv2_inbound_frame_too_large(PROTOCOL_MAX, DEFAULT_CAP));
        assert!(!sv2_inbound_frame_too_large(DEFAULT_CAP, DEFAULT_CAP));
    }

    #[test]
    fn test_parse_sv2_url_stratum2() {
        let (host, port) = parse_sv2_url("stratum2+tcp://v2.braiins.com:3336").unwrap();
        assert_eq!(host, "v2.braiins.com");
        assert_eq!(port, 3336);
    }

    #[test]
    fn test_parse_sv2_url_sv2_prefix() {
        let (host, port) = parse_sv2_url("sv2+tcp://pool.example.com:34255").unwrap();
        assert_eq!(host, "pool.example.com");
        assert_eq!(port, 34255);
    }

    #[test]
    fn test_parse_sv2_url_ipv6_literal() {
        let (host, port) = parse_sv2_url("sv2+tcp://[2001:db8::1]:34255").unwrap();
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, 34255);
    }

    #[test]
    fn test_parse_sv2_url_rejects_legacy_shortcuts() {
        for bad in [
            "203.0.113.100:3336",
            "sv2://pool.example.com:34255",
            "sv2://pool.example.com",
            "stratum+tcp://pool.com:3336",
            "stratum2+tcp://pool.com",
        ] {
            assert!(parse_sv2_url(bad).is_err(), "{bad} should fail");
        }
    }

    /// SV2 spec §4.1: the base58check authority key is appended as a URL
    /// path. `parse_sv2_url` must extract a valid host:port from such a
    /// URL (the key is parsed separately by `v2::auth`). A bare trailing
    /// `/` is the explicit no-pinned-key (TOFU) form and is now valid.
    #[test]
    fn test_parse_sv2_url_accepts_authority_key_path() {
        // Trailing slash, no key → host:port resolves (TOFU posture).
        let (h, p) = parse_sv2_url("stratum2+tcp://pool.com:3336/").unwrap();
        assert_eq!((h.as_str(), p), ("pool.com", 3336));

        // With a base58check authority-key path component.
        let (h, p) = parse_sv2_url("stratum2+tcp://v2.braiins.com:3336/9p6t...keyblob").unwrap();
        assert_eq!((h.as_str(), p), ("v2.braiins.com", 3336));

        // IPv6 + key path still resolves the bracketed host.
        let (h, p) = parse_sv2_url("sv2+tcp://[2001:db8::1]:34255/abcdef").unwrap();
        assert_eq!((h.as_str(), p), ("2001:db8::1", 34255));
    }

    #[test]
    fn test_strip_authority_key_path_is_exact() {
        assert_eq!(
            strip_authority_key_path("stratum2+tcp://h:1/key?x#y"),
            "stratum2+tcp://h:1"
        );
        assert_eq!(strip_authority_key_path("sv2+tcp://h:1"), "sv2+tcp://h:1");
        // No scheme separator → returned unchanged.
        assert_eq!(strip_authority_key_path("h:1/key"), "h:1/key");
    }

    #[test]
    fn test_auto_retry_interval_grows_and_caps() {
        let mut state = AutoFallbackState::default();
        let url = "stratum2+tcp://pool.example.com:3336";

        let first = state.next_retry_interval(url);
        let second = state.next_retry_interval(url);
        let third = state.next_retry_interval(url);
        let fourth = state.next_retry_interval(url);
        let fifth = state.next_retry_interval(url);

        assert!(first >= AUTO_V1_TO_V2_RETRY_INTERVAL);
        assert!(second > first);
        assert!(third > second);
        assert!(fourth >= third);
        assert!(fourth <= AUTO_V1_TO_V2_MAX_RETRY_INTERVAL);
        assert_eq!(fifth, AUTO_V1_TO_V2_MAX_RETRY_INTERVAL);
    }

    #[test]
    fn test_auto_retry_interval_resets_after_sv2_mining_success() {
        let mut state = AutoFallbackState::default();
        let url = "stratum2+tcp://pool.example.com:3336";

        let first = state.next_retry_interval(url);
        let second = state.next_retry_interval(url);
        assert!(second > first);

        state.note_sv2_mining_success();
        let reset = state.next_retry_interval(url);

        assert_eq!(reset, first);
    }

    // -----------------------------------------------------------------------
    // AutoFallbackState invariants + jitter determinism.
    //
    // The Auto-mode SV2→V1 fallback retry calculator gates how aggressively
    // dcentrald retries SV2 after V1 fallback. Pin the saturation, reset,
    // accessor, and jitter determinism so a refactor cannot silently flip
    // the back-off curve.
    // -----------------------------------------------------------------------

    #[test]
    fn auto_fallback_state_starts_at_zero_consecutive_fallbacks() {
        let state = AutoFallbackState::default();
        assert_eq!(state.consecutive_fallbacks(), 0);
    }

    #[test]
    fn auto_fallback_state_consecutive_fallbacks_increments_each_retry() {
        let mut state = AutoFallbackState::default();
        let url = "stratum2+tcp://pool.example.com:3336";

        let _ = state.next_retry_interval(url);
        assert_eq!(state.consecutive_fallbacks(), 1);
        let _ = state.next_retry_interval(url);
        assert_eq!(state.consecutive_fallbacks(), 2);
    }

    #[test]
    fn auto_fallback_state_note_sv2_mining_success_resets_counter() {
        let mut state = AutoFallbackState::default();
        let url = "stratum2+tcp://pool.example.com:3336";
        let _ = state.next_retry_interval(url);
        let _ = state.next_retry_interval(url);
        let _ = state.next_retry_interval(url);
        assert_eq!(state.consecutive_fallbacks(), 3);

        state.note_sv2_mining_success();
        assert_eq!(state.consecutive_fallbacks(), 0);
    }

    #[test]
    fn auto_fallback_state_next_retry_interval_saturates_consecutive_fallbacks() {
        // The counter uses saturating_add — at u32::MAX it must NOT wrap
        // back to 0 (which would silently restart the gentle back-off).
        let mut state = AutoFallbackState::default();
        state.consecutive_fallbacks = u32::MAX;
        let url = "stratum2+tcp://pool.example.com:3336";

        let _ = state.next_retry_interval(url);
        assert_eq!(state.consecutive_fallbacks(), u32::MAX);

        // Multiple calls at saturation must keep returning the cap, not
        // panic and not wrap.
        for _ in 0..5 {
            let interval = state.next_retry_interval(url);
            assert!(interval <= AUTO_V1_TO_V2_MAX_RETRY_INTERVAL);
        }
    }

    #[test]
    fn auto_retry_jitter_is_deterministic_for_same_inputs() {
        // Same URL + same penalty_level must always produce the same
        // jitter so the back-off curve is reproducible across miners.
        let url = "stratum2+tcp://pool.example.com:3336";
        let a = auto_retry_jitter_secs(url, 0);
        let b = auto_retry_jitter_secs(url, 0);
        assert_eq!(a, b);

        let c = auto_retry_jitter_secs(url, 3);
        let d = auto_retry_jitter_secs(url, 3);
        assert_eq!(c, d);
    }

    #[test]
    fn auto_retry_jitter_stays_within_bucket_range() {
        // Jitter must be in [0, BUCKETS * STEP) to keep the upper bound
        // on retry interval finite.
        let url = "stratum2+tcp://pool.example.com:3336";
        let max = AUTO_V1_TO_V2_RETRY_JITTER_BUCKETS * AUTO_V1_TO_V2_RETRY_JITTER_STEP.as_secs();
        for level in 0..10 {
            let jitter = auto_retry_jitter_secs(url, level);
            assert!(jitter < max, "level {level}: jitter {jitter} >= max {max}");
        }
    }

    #[test]
    fn auto_retry_jitter_uses_step_aligned_buckets() {
        // The bucket math is `bucket * STEP`; jitter must be a multiple
        // of STEP regardless of URL or level.
        let url = "stratum2+tcp://different-pool.example.com:34255";
        let step = AUTO_V1_TO_V2_RETRY_JITTER_STEP.as_secs();
        for level in 0..10 {
            let jitter = auto_retry_jitter_secs(url, level);
            assert_eq!(
                jitter % step,
                0,
                "level {level}: jitter {jitter} not aligned to step {step}"
            );
        }
    }

    #[test]
    fn auto_retry_jitter_varies_across_urls_at_same_level() {
        // Different URLs at the same level should produce a mix of jitter
        // values — pin that the function actually uses the URL as part
        // of the hash. (Probabilistic: across many URLs at level 0, we
        // should see at least 2 distinct jitter values.)
        let urls = [
            "stratum2+tcp://a.pool.com:3336",
            "stratum2+tcp://b.pool.com:3336",
            "stratum2+tcp://c.pool.com:3336",
            "stratum2+tcp://d.pool.com:3336",
            "stratum2+tcp://e.pool.com:3336",
            "stratum2+tcp://f.pool.com:3336",
            "stratum2+tcp://g.pool.com:3336",
            "stratum2+tcp://h.pool.com:3336",
        ];
        let mut seen = std::collections::HashSet::new();
        for url in urls {
            seen.insert(auto_retry_jitter_secs(url, 0));
        }
        assert!(
            seen.len() >= 2,
            "expected URL hashing to produce >=2 distinct jitter values, got {}",
            seen.len()
        );
    }

    #[test]
    fn auto_retry_interval_first_call_meets_minimum_base_interval() {
        // First retry interval must be >= the configured base interval
        // (15 minutes per AUTO_V1_TO_V2_RETRY_INTERVAL). Pin so a refactor
        // that lowered the base to seconds would burn pool bandwidth.
        let mut state = AutoFallbackState::default();
        let interval = state.next_retry_interval("stratum2+tcp://x:1");
        assert!(
            interval >= AUTO_V1_TO_V2_RETRY_INTERVAL,
            "first retry {:?} below base {:?}",
            interval,
            AUTO_V1_TO_V2_RETRY_INTERVAL
        );
    }

    #[test]
    fn auto_retry_interval_caps_strictly_at_max() {
        // Upper cap is AUTO_V1_TO_V2_MAX_RETRY_INTERVAL (4 hours). Even
        // with maximum penalty + maximum jitter, the result must NOT
        // exceed the cap.
        let mut state = AutoFallbackState::default();
        let url = "stratum2+tcp://pool.example.com:3336";
        // Burn enough attempts to force saturation.
        for _ in 0..20 {
            let interval = state.next_retry_interval(url);
            assert!(
                interval <= AUTO_V1_TO_V2_MAX_RETRY_INTERVAL,
                "interval {:?} exceeds cap {:?}",
                interval,
                AUTO_V1_TO_V2_MAX_RETRY_INTERVAL
            );
        }
    }

    #[test]
    fn auto_retry_interval_constants_are_pinned() {
        // Drift in any of these silently changes the back-off curve.
        // Pin the load-bearing values: 2 fallbacks before V1 takeover,
        // 30-minute base retry, 4-hour cap.
        assert_eq!(AUTO_V2_TO_V1_FALLBACK_THRESHOLD, 2);
        assert_eq!(AUTO_V1_TO_V2_RETRY_INTERVAL.as_secs(), 30 * 60);
        assert_eq!(AUTO_V1_TO_V2_MAX_RETRY_INTERVAL.as_secs(), 4 * 60 * 60);
        assert_eq!(AUTO_V1_TO_V2_RETRY_JITTER_STEP.as_secs(), 45);
        assert_eq!(AUTO_V1_TO_V2_RETRY_JITTER_BUCKETS, 5);
    }

    #[test]
    fn handshake_and_connect_timeout_constants_are_pinned() {
        // Pin the connection establishment timing constants.
        assert_eq!(CONNECT_TIMEOUT.as_secs(), 10);
        assert_eq!(HANDSHAKE_TIMEOUT.as_secs(), 15);
        assert_eq!(MAX_BACKOFF.as_secs(), 60);
        assert_eq!(INITIAL_BACKOFF.as_secs(), 1);
    }

    #[test]
    fn encrypted_header_size_constant_is_22_bytes() {
        // SV2 Noise transport: 6-byte plaintext header + 16-byte ChaChaPoly
        // MAC = 22 bytes encrypted. A drift here silently corrupts every
        // frame the decoder reads.
        assert_eq!(ENCRYPTED_HEADER_SIZE, 22);
    }

    // -----------------------------------------------------------------------
    // SV2 reconnect-backoff jitter.
    //
    // The SV2 reconnect loops slept on the bare exponential backoff while the
    // V1 path applied +/-25% jitter, so a synchronized fleet that lost a shared
    // pool reconnected in lockstep (reconnect storm). `jittered_backoff` must
    // mirror v1/connection.rs `Backoff::next_delay` exactly: +/-25% range,
    // 100 ms floor, and never exceed the (already MAX_BACKOFF-capped) base by
    // more than +25%.
    // -----------------------------------------------------------------------

    #[test]
    fn jittered_backoff_stays_within_plus_minus_25_percent() {
        // Mirror V1: base +/- 25% (range = base/4). Probe many samples since
        // the jitter is random.
        let base = Duration::from_secs(4); // 4000 ms, range +/- 1000 ms
        let lo = Duration::from_millis(3000); // base * 0.75
        let hi = Duration::from_millis(5000); // base * 1.25
        for _ in 0..10_000 {
            let d = jittered_backoff(base);
            assert!(
                d >= lo && d <= hi,
                "jittered {d:?} outside [{lo:?}, {hi:?}] for base {base:?}"
            );
        }
    }

    #[test]
    fn jittered_backoff_honors_100ms_floor() {
        // Identical to V1's `.max(100)` floor: even with maximum negative
        // jitter the delay must never drop below 100 ms.
        let base = Duration::from_millis(200); // range +/- 50 ms -> min 150 ms
        for _ in 0..10_000 {
            let d = jittered_backoff(base);
            assert!(
                d >= Duration::from_millis(100),
                "jittered {d:?} below 100ms floor"
            );
        }
    }

    #[test]
    fn jittered_backoff_capped_base_never_exceeds_max_plus_jitter() {
        // The loops cap the exponential progression at MAX_BACKOFF *before*
        // jittering, so the slept value is bounded by MAX_BACKOFF * 1.25.
        let capped = MAX_BACKOFF; // 60s — the post-cap base fed to jitter
        let upper = MAX_BACKOFF + MAX_BACKOFF / 4; // 75s
        for _ in 0..10_000 {
            let d = jittered_backoff(capped);
            assert!(
                d <= upper,
                "jittered {d:?} exceeds MAX_BACKOFF+25% ({upper:?})"
            );
            assert!(
                d >= MAX_BACKOFF - MAX_BACKOFF / 4,
                "jittered {d:?} below MAX_BACKOFF-25%"
            );
        }
    }

    #[test]
    fn jittered_backoff_zero_base_returns_floor() {
        // jitter_range == 0 path: returns the 100 ms floor, no panic, no
        // divide-by-zero in the `% (jitter_range * 2)`.
        assert_eq!(
            jittered_backoff(Duration::from_secs(0)),
            Duration::from_millis(100)
        );
    }
}
