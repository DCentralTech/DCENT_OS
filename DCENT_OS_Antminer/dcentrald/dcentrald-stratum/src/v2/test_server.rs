//! Mock SV2 pool server for the W9.3 broad-pool harness.
//!
//! This module is compiled only when both `sv2` and `mock-pool` features
//! are enabled. The default DCENT_OS production sysupgrade tarball never
//! sets `mock-pool`, so none of these helpers ship to a real miner.
//!
//! # Why this exists
//!
//! Before W9.3, `StratumV2Client` was only end-to-end exercised against
//! a single real pool: Braiins Pool's SV2 endpoint (`v2.stratum.braiins.com`,
//! 2026-03-20). W5.3 then made V2 the auto-default protocol when an
//! `sv2_url` is configured, so a broken handshake against a non-Braiins
//! pool would silently disconnect every cold-boot retry until V1 fallback
//! kicks in 2 attempts later.
//!
//! This module ships two mock pool flavors that mimic the SV2 surface
//! the dcentrald client is expected to land on in 2026:
//!
//! - **OCEAN-style** ([`MockPoolStyle::Ocean`]): Standard mining channels
//!   (`OpenStandardMiningChannel`), pool-supplied merkle root, no
//!   version rolling required, deliberate `future_job=false +
//!   SetNewPrevHash already-known` ordering.
//! - **DEMAND/SRI-style** ([`MockPoolStyle::DemandSri`]): Extended mining
//!   channels (`OpenExtendedMiningChannel`), version-rolling allowed,
//!   coinbase-split + merkle-path job delivery, pool-driven extranonce
//!   prefix.
//!
//! Both flavors run the **real** Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256
//! handshake against the production [`super::noise::NoiseSession`] code,
//! so a broken initiator-side ECDH or HKDF derivation surfaces in CI on
//! every PR — not on the next operator who points dcentrald at a non-
//! Braiins pool.
//!
//! # NOT a security boundary
//!
//! `MockPool` is a deterministic test harness. It uses fixed RNG seeds
//! for reproducibility, accepts any client identity (no pool-side auth),
//! and returns a self-signed (TOFU-mode-friendly) SIGNATURE_NOISE_MESSAGE.
//! Never wire a real ASIC to a `MockPool` and expect mining credit.

use std::time::Duration;

use chacha20poly1305::aead::Payload;
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
use secp256k1::ellswift::{ElligatorSwift, ElligatorSwiftParty};
use secp256k1::{Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::v1::difficulty::difficulty_to_target;

use super::framing::{Sv2Frame, FRAME_HEADER_SIZE};
use super::noise::NoiseSession;
use super::types::{mining, EXTENSION_TYPE_MINING};

/// Encrypted SV2 header size matching the production client.
const ENCRYPTED_HEADER_SIZE: usize = 6 + 16;

/// Protocol-name identical to `noise.rs` PROTOCOL_NAME — duplicated here
/// so the test harness tolerates a future refactor that makes the
/// production constant `pub(crate)`.
const PROTOCOL_NAME: &[u8] = b"Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";

/// SIGNATURE_NOISE_MESSAGE size: version(2) + valid_from(4) +
/// not_valid_after(4) + signature(64) = 74 bytes.
const SIGNATURE_NOISE_MESSAGE_SIZE: usize = 74;

/// Which broad-pool style this mock pool emulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockPoolStyle {
    /// OCEAN-style: Standard mining channels, no version rolling, pool
    /// pushes pre-computed merkle root.
    Ocean,
    /// DEMAND/SRI-style: Extended mining channels with `version_rolling
    /// _allowed=true`, coinbase-split job delivery.
    DemandSri,
}

impl MockPoolStyle {
    /// Deterministic ephemeral secp256k1 keypair seed for this style.
    /// Pinning the seed keeps the test reproducible — the operator can
    /// re-run the test offline and get the exact same wire bytes.
    fn server_e_seed(self) -> [u8; 64] {
        match self {
            MockPoolStyle::Ocean => [0xCAu8; 64],
            MockPoolStyle::DemandSri => [0xD3u8; 64],
        }
    }

    /// Deterministic static secp256k1 keypair seed for this style.
    fn server_s_seed(self) -> [u8; 64] {
        match self {
            MockPoolStyle::Ocean => [0x71u8; 64],
            MockPoolStyle::DemandSri => [0x5Au8; 64],
        }
    }

    /// Human-readable label for log lines.
    pub fn label(self) -> &'static str {
        match self {
            MockPoolStyle::Ocean => "ocean-style",
            MockPoolStyle::DemandSri => "demand-sri-style",
        }
    }
}

/// Outcome a single mock-pool session reaches before disconnecting.
///
/// The integration test asserts on this so a regression that, e.g., gets
/// the handshake right but stalls before opening a channel is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockPoolOutcome {
    /// The client completed the Noise handshake but the first SV2 frame
    /// the mock pool received was NOT a `SetupConnection`. Surfaces a
    /// regression that breaks the post-handshake message sequence.
    HandshakeOnly,
    /// Channel opened; first job delivered to the client. This is the
    /// success state for the [`MockPoolBehavior::JobThenStop`] behavior.
    JobDelivered,
    /// Channel opened, first job delivered, AND the mock pool decrypted a
    /// well-formed `SubmitSharesStandard`/`SubmitSharesExtended` frame from
    /// the client and replied `SubmitSharesSuccess`. Reached only by the
    /// [`MockPoolBehavior::JobThenShare`] behavior.
    ///
    /// Carries the fields the pool decoded off the encrypted transport so
    /// the integration test can prove the exact share bytes survived the
    /// Noise round-trip (not just that *some* frame arrived).
    ShareAccepted {
        /// `0x1a` (`SUBMIT_SHARES_STANDARD`) or `0x1b` (`SUBMIT_SHARES_EXTENDED`).
        msg_type: u8,
        /// The channel_id the client echoed back — must equal the id the
        /// pool assigned in `Open*MiningChannelSuccess`.
        channel_id: u32,
        /// Monotonic per-channel submit sequence number from the client.
        sequence_number: u32,
        /// The 32-bit job id the share references.
        job_id: u32,
        /// The 32-bit nonce the client submitted.
        nonce: u32,
        /// The ntime the client submitted.
        ntime: u32,
        /// The full block-header version (post-BIP320-roll) the client submitted.
        version: u32,
    },
    /// Channel opened, first job delivered, and the mock pool rejected a
    /// well-formed `SubmitShares*` frame with `SubmitSharesError`.
    ShareRejected {
        /// `0x1a` (`SUBMIT_SHARES_STANDARD`) or `0x1b` (`SUBMIT_SHARES_EXTENDED`).
        msg_type: u8,
        /// The channel_id the client echoed back.
        channel_id: u32,
        /// Monotonic per-channel submit sequence number from the client.
        sequence_number: u32,
        /// The 32-bit job id the share references.
        job_id: u32,
        /// The 32-bit nonce the client submitted.
        nonce: u32,
        /// The ntime the client submitted.
        ntime: u32,
        /// The full block-header version the client submitted.
        version: u32,
        /// The rejection reason sent over `SubmitSharesError`.
        reason: String,
    },
    /// The mock pool replied `SetupConnectionError` and the client tore the
    /// session down WITHOUT opening a mining channel — the success state for
    /// the [`MockPoolBehavior::AuthReject`] behavior.
    AuthRejected,
}

/// How far through the lifecycle a single mock-pool session drives the
/// client, and where it deliberately diverges.
///
/// Split out from [`MockPoolStyle`] (which only selects the channel flavor)
/// so a test can pick `Ocean`/`DemandSri` *and* independently pick how the
/// session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockPoolBehavior {
    /// Stop after the first job is delivered. This is the legacy behavior
    /// the connect→first-job tests rely on (the pool never waits for a
    /// share, so a test that does not push one still reaches a terminal
    /// outcome instead of blocking).
    JobThenStop,
    /// After the first job, read exactly one `SubmitShares*` frame from the
    /// client, validate it, and reply `SubmitSharesSuccess`. Drives the
    /// back half of the lifecycle the JobThenStop behavior stopped short of.
    JobThenShare,
    /// After the first job, send a mid-session `SetTarget`, then accept one
    /// submitted share. Pins target changes while the channel is already mining.
    JobThenSetTargetThenShare,
    /// After the first job, read one submitted share and reply
    /// `SubmitSharesError`. Pins rejected-share status handling e2e.
    JobThenShareReject,
    /// Reply `SetupConnectionError` instead of `SetupConnectionSuccess` and
    /// never open a channel — proves the client refuses to open a channel on
    /// an auth/setup rejection and exits cleanly.
    AuthReject,
}

/// One run of a mock pool against a live `StratumV2Client` connection.
pub struct MockPool {
    style: MockPoolStyle,
    /// Local TCP address the client should connect to.
    pub addr: std::net::SocketAddr,
    /// Receives the session-end outcome.
    outcome_rx: mpsc::Receiver<Result<MockPoolOutcome, String>>,
}

impl MockPool {
    /// Bind a fresh mock pool on `127.0.0.1:0` and spawn the accept loop
    /// as a background tokio task.
    ///
    /// The accept loop only handles a single inbound connection — once
    /// the client connects, the server runs the chosen style's protocol
    /// state machine to first-job delivery, then closes the socket so
    /// the integration test can move on to its assertions.
    ///
    /// This is shorthand for `spawn_with_behavior(style,
    /// MockPoolBehavior::JobThenStop)` — the connect→first-job behavior the
    /// original W9.3 tests assert on.
    pub async fn spawn(style: MockPoolStyle) -> Result<Self, String> {
        Self::spawn_with_behavior(style, MockPoolBehavior::JobThenStop).await
    }

    /// Bind a fresh mock pool and run the chosen [`MockPoolBehavior`].
    ///
    /// Identical to [`spawn`](Self::spawn) except the caller selects how far
    /// through the lifecycle the session drives (and where it diverges):
    /// stop at first job, continue through a share submit, or reject the
    /// setup.
    pub async fn spawn_with_behavior(
        style: MockPoolStyle,
        behavior: MockPoolBehavior,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("MockPool[{}]: bind failed: {}", style.label(), e))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("MockPool[{}]: local_addr failed: {}", style.label(), e))?;

        let (outcome_tx, outcome_rx) = mpsc::channel::<Result<MockPoolOutcome, String>>(1);

        tokio::spawn(async move {
            let result = Self::run_session(style, behavior, listener).await;
            let _ = outcome_tx.send(result).await;
        });

        Ok(MockPool {
            style,
            addr,
            outcome_rx,
        })
    }

    /// Wait up to `timeout` for the mock pool's session to terminate.
    pub async fn await_outcome(&mut self, timeout: Duration) -> Result<MockPoolOutcome, String> {
        match tokio::time::timeout(timeout, self.outcome_rx.recv()).await {
            Ok(Some(Ok(out))) => Ok(out),
            Ok(Some(Err(e))) => Err(e),
            Ok(None) => Err(format!(
                "MockPool[{}]: outcome channel closed without result",
                self.style.label()
            )),
            Err(_) => Err(format!(
                "MockPool[{}]: timed out after {:?} waiting for outcome",
                self.style.label(),
                timeout
            )),
        }
    }

    async fn run_session(
        style: MockPoolStyle,
        behavior: MockPoolBehavior,
        listener: TcpListener,
    ) -> Result<MockPoolOutcome, String> {
        let (mut stream, peer) = listener
            .accept()
            .await
            .map_err(|e| format!("MockPool[{}]: accept failed: {}", style.label(), e))?;
        tracing::info!(
            "MockPool[{}]: accepted connection from {}",
            style.label(),
            peer
        );

        // ── Step 1: read the client's `-> e` (64-byte EllSwift) ──
        let mut client_e = [0u8; 64];
        stream
            .read_exact(&mut client_e)
            .await
            .map_err(|e| format!("MockPool[{}]: read client_e failed: {}", style.label(), e))?;

        // Reproduce the client-side h evolution so we can derive the
        // same chaining key without sharing process memory.
        let mut h = sha256(PROTOCOL_NAME);
        let mut chaining_key = h;
        // mix_hash(prologue=""):
        h = sha256_concat(&h, b"");
        // mix_hash(client_e):
        h = sha256_concat(&h, &client_e);
        // EncryptAndHash(empty payload) with k=None ⇒ mix_hash("").
        h = sha256_concat(&h, b"");

        // ── Step 2: build the server response (re || enc_static || enc_cert) ──
        let secp = Secp256k1::new();

        // Server ephemeral keypair + EllSwift
        let re_seed = style.server_e_seed();
        let re_sk = SecretKey::from_slice(&re_seed[..32])
            .map_err(|e| format!("MockPool: bad re_sk: {}", e))?;
        let mut re_aux = [0u8; 32];
        re_aux.copy_from_slice(&re_seed[32..64]);
        let re_es = ElligatorSwift::from_seckey(&secp, re_sk, Some(re_aux));
        let re_pub_arr = re_es.to_array();

        // Server static keypair + EllSwift
        let rs_seed = style.server_s_seed();
        let rs_sk = SecretKey::from_slice(&rs_seed[..32])
            .map_err(|e| format!("MockPool: bad rs_sk: {}", e))?;
        let mut rs_aux = [0u8; 32];
        rs_aux.copy_from_slice(&rs_seed[32..64]);
        let rs_es = ElligatorSwift::from_seckey(&secp, rs_sk, Some(rs_aux));
        let rs_pub_arr = rs_es.to_array();

        // mix_hash(re_pub):
        h = sha256_concat(&h, &re_pub_arr);

        // DH(ee): EllSwift ECDH (BIP324 tagged) — server is Party::B
        let client_es = ElligatorSwift::from_array(client_e);
        let ee_shared =
            ElligatorSwift::shared_secret(client_es, re_es, re_sk, ElligatorSwiftParty::B, None);
        let ee_bytes: [u8; 32] = ee_shared.to_secret_bytes();

        // MixKey(ee): (ck, k_ee) = HKDF(ck, ee)
        let (ck_after_ee, k_ee) = NoiseSession::hkdf_sha256(&chaining_key, &ee_bytes);
        chaining_key = ck_after_ee;

        // EncryptAndHash(rs_pub) under k_ee, nonce=0, AAD=current h
        let encrypted_static = aead_encrypt(&k_ee, 0, &h, &rs_pub_arr)
            .map_err(|e| format!("MockPool: encrypt_static failed: {}", e))?;

        // mix_hash(encrypted_static):
        h = sha256_concat(&h, &encrypted_static);

        // DH(es): EllSwift ECDH (BIP324 tagged) — server is Party::B
        let es_shared =
            ElligatorSwift::shared_secret(client_es, rs_es, rs_sk, ElligatorSwiftParty::B, None);
        let es_bytes: [u8; 32] = es_shared.to_secret_bytes();

        // MixKey(es)
        let (ck_after_es, k_es) = NoiseSession::hkdf_sha256(&chaining_key, &es_bytes);
        chaining_key = ck_after_es;

        // EncryptAndHash(SIGNATURE_NOISE_MESSAGE) — fake but well-formed
        let mut cert_payload = Vec::with_capacity(SIGNATURE_NOISE_MESSAGE_SIZE);
        cert_payload.extend_from_slice(&0u16.to_le_bytes()); // version
        cert_payload.extend_from_slice(&0u32.to_le_bytes()); // valid_from
        cert_payload.extend_from_slice(&u32::MAX.to_le_bytes()); // not_valid_after
        cert_payload.extend_from_slice(&[0xAAu8; 64]); // dummy signature

        let encrypted_cert = aead_encrypt(&k_es, 0, &h, &cert_payload)
            .map_err(|e| format!("MockPool: encrypt_cert failed: {}", e))?;

        // The production client mixes encrypted_cert into its handshake
        // hash for channel-binding (see `noise.rs::initiator_handshake_finish`).
        // The mock server doesn't expose its own channel-binding hash so
        // we deliberately skip the final `mix_hash(encrypted_cert)` here —
        // every transport-phase frame uses `aad=b""` (matching the
        // client's `aead.encrypt`), so the hash isn't load-bearing past
        // this point.

        // Split: (k1, k2) = HKDF(ck, empty)
        let (k1, k2) = NoiseSession::hkdf_sha256(&chaining_key, &[]);
        // Initiator (client) sending=k1, receiving=k2.
        // Server is the mirror: server receives with k1, server sends with k2.
        let server_recv_key = k1;
        let server_send_key = k2;

        // Send the response: re_pub || encrypted_static || encrypted_cert
        let mut server_msg = Vec::with_capacity(64 + encrypted_static.len() + encrypted_cert.len());
        server_msg.extend_from_slice(&re_pub_arr);
        server_msg.extend_from_slice(&encrypted_static);
        server_msg.extend_from_slice(&encrypted_cert);
        stream
            .write_all(&server_msg)
            .await
            .map_err(|e| format!("MockPool[{}]: write handshake failed: {}", style.label(), e))?;

        tracing::info!(
            "MockPool[{}]: Noise_NX handshake complete, transport active",
            style.label()
        );

        // ── Step 3: post-handshake protocol — wire-encrypted SV2 frames on
        // both directions now. The chosen `behavior` selects how far through
        // the lifecycle this drives.
        let mut server = ServerSession {
            stream,
            send_key: server_send_key,
            recv_key: server_recv_key,
            send_nonce: 0,
            recv_nonce: 0,
            style,
        };

        match behavior {
            MockPoolBehavior::AuthReject => server.run_auth_reject().await,
            MockPoolBehavior::JobThenStop
            | MockPoolBehavior::JobThenShare
            | MockPoolBehavior::JobThenSetTargetThenShare
            | MockPoolBehavior::JobThenShareReject => {
                // Setup + channel-open + first-job is shared by both
                // job-delivering behaviors. `None` ⇒ the client's first
                // post-handshake frame was not a SetupConnection.
                let channel_id = match server.setup_and_deliver_first_job().await? {
                    Some(id) => id,
                    None => return Ok(MockPoolOutcome::HandshakeOnly),
                };
                match behavior {
                    MockPoolBehavior::JobThenStop => Ok(MockPoolOutcome::JobDelivered),
                    MockPoolBehavior::JobThenShare => server.read_submit_and_ack(channel_id).await,
                    MockPoolBehavior::JobThenSetTargetThenShare => {
                        server.send_set_target(channel_id, 256.0).await?;
                        server.read_submit_and_ack(channel_id).await
                    }
                    MockPoolBehavior::JobThenShareReject => {
                        server
                            .read_submit_and_reject(channel_id, "mock-share-rejected")
                            .await
                    }
                    MockPoolBehavior::AuthReject => unreachable!("handled above"),
                }
            }
        }
    }
}

/// Per-direction encrypted SV2 transport for the mock pool.
struct ServerSession {
    stream: TcpStream,
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_nonce: u64,
    recv_nonce: u64,
    style: MockPoolStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedShare {
    msg_type: u8,
    channel_id: u32,
    sequence_number: u32,
    job_id: u32,
    nonce: u32,
    ntime: u32,
    version: u32,
}

impl ServerSession {
    /// Read one decrypted SV2 frame (handles header + payload Noise blocks).
    async fn read_frame(&mut self) -> Result<Sv2Frame, String> {
        // Read encrypted header
        let mut header_cipher = [0u8; ENCRYPTED_HEADER_SIZE];
        self.stream
            .read_exact(&mut header_cipher)
            .await
            .map_err(|e| format!("read encrypted header: {}", e))?;
        let header_plain = aead_decrypt(&self.recv_key, self.recv_nonce, b"", &header_cipher)
            .map_err(|e| format!("decrypt header: {}", e))?;
        self.recv_nonce += 1;
        if header_plain.len() != FRAME_HEADER_SIZE {
            return Err(format!(
                "decrypted header wrong size: {}",
                header_plain.len()
            ));
        }
        let payload_len =
            u32::from_le_bytes([header_plain[3], header_plain[4], header_plain[5], 0]) as usize;
        let mut payload_cipher = vec![0u8; payload_len + 16];
        self.stream
            .read_exact(&mut payload_cipher)
            .await
            .map_err(|e| format!("read encrypted payload: {}", e))?;
        let payload_plain = aead_decrypt(&self.recv_key, self.recv_nonce, b"", &payload_cipher)
            .map_err(|e| format!("decrypt payload: {}", e))?;
        self.recv_nonce += 1;

        let mut header_arr = [0u8; FRAME_HEADER_SIZE];
        header_arr.copy_from_slice(&header_plain);
        let mut combined = Vec::with_capacity(FRAME_HEADER_SIZE + payload_plain.len());
        combined.extend_from_slice(&header_arr);
        combined.extend_from_slice(&payload_plain);
        let (frame, _) =
            Sv2Frame::from_bytes(&combined).map_err(|e| format!("parse decrypted frame: {}", e))?;
        Ok(frame)
    }

    /// Write one encrypted SV2 frame (header block + payload block).
    async fn write_frame(&mut self, msg_type: u8, payload: &[u8]) -> Result<(), String> {
        let frame = Sv2Frame::new(EXTENSION_TYPE_MINING, msg_type, payload.to_vec());
        let bytes = frame.to_bytes();
        let header = &bytes[..FRAME_HEADER_SIZE];
        let body = &bytes[FRAME_HEADER_SIZE..];
        let enc_header = aead_encrypt(&self.send_key, self.send_nonce, b"", header)
            .map_err(|e| format!("encrypt header: {}", e))?;
        self.send_nonce += 1;
        let enc_body = aead_encrypt(&self.send_key, self.send_nonce, b"", body)
            .map_err(|e| format!("encrypt body: {}", e))?;
        self.send_nonce += 1;
        self.stream
            .write_all(&enc_header)
            .await
            .map_err(|e| format!("write enc_header: {}", e))?;
        self.stream
            .write_all(&enc_body)
            .await
            .map_err(|e| format!("write enc_body: {}", e))?;
        Ok(())
    }

    /// Run SetupConnection → SetupConnectionSuccess → Open*MiningChannel →
    /// first job. Shared by the JobThenStop and JobThenShare behaviors.
    ///
    /// Returns `Some(channel_id)` (the id the pool assigned in the
    /// `Open*MiningChannelSuccess`) on success, or `None` when the client's
    /// first post-handshake frame was not a `SetupConnection` (caller maps
    /// that to [`MockPoolOutcome::HandshakeOnly`]).
    async fn setup_and_deliver_first_job(&mut self) -> Result<Option<u32>, String> {
        // Read SetupConnection (we don't strictly validate its contents — the
        // Noise auth-tag check during the handshake is the real proof that the
        // client is wire-compatible with this pool style).
        let setup_frame = self.read_frame().await.map_err(|e| {
            format!(
                "MockPool[{}]: read SetupConnection: {}",
                self.style.label(),
                e
            )
        })?;
        if setup_frame.header.msg_type != mining::SETUP_CONNECTION {
            return Ok(None);
        }

        // Reply with SetupConnectionSuccess (used_version + flags).
        let mut setup_success_payload = Vec::with_capacity(6);
        setup_success_payload.extend_from_slice(&2u16.to_le_bytes()); // used_version=2
        setup_success_payload.extend_from_slice(&0u32.to_le_bytes()); // flags=0
        self.write_frame(mining::SETUP_CONNECTION_SUCCESS, &setup_success_payload)
            .await
            .map_err(|e| {
                format!(
                    "MockPool[{}]: write SetupConnectionSuccess: {}",
                    self.style.label(),
                    e
                )
            })?;

        // Read OpenStandardMiningChannel or OpenExtendedMiningChannel.
        let open_frame = self
            .read_frame()
            .await
            .map_err(|e| format!("MockPool[{}]: read Open*: {}", self.style.label(), e))?;

        // The channel_id the pool assigns in each `*Success` below. Returned so
        // the share-submit follow-up can assert the client echoes it back.
        let channel_id = match (self.style, open_frame.header.msg_type) {
            (MockPoolStyle::Ocean, mining::OPEN_STANDARD_MINING_CHANNEL) => {
                self.send_open_standard_success_and_first_job().await?;
                7
            }
            (MockPoolStyle::DemandSri, mining::OPEN_EXTENDED_MINING_CHANNEL) => {
                self.send_open_extended_success_and_first_job().await?;
                13
            }
            (style, mt) => {
                return Err(format!(
                    "MockPool[{}]: client sent unexpected open msg_type=0x{:02x}",
                    style.label(),
                    mt
                ));
            }
        };

        Ok(Some(channel_id))
    }

    /// Send a pool-driven `SetTarget` while the mining channel is already open.
    async fn send_set_target(&mut self, channel_id: u32, difficulty: f64) -> Result<(), String> {
        let target = difficulty_to_target(difficulty);
        let mut payload = Vec::with_capacity(36);
        payload.extend_from_slice(&channel_id.to_le_bytes());
        payload.extend_from_slice(&target);
        self.write_frame(mining::SET_TARGET, &payload)
            .await
            .map_err(|e| format!("MockPool[{}]: write SetTarget: {}", self.style.label(), e))?;
        tracing::info!(
            "MockPool[{}]: SetTarget dispatched (channel_id={}, difficulty={})",
            self.style.label(),
            channel_id,
            difficulty
        );
        Ok(())
    }

    /// Read and validate one submitted share without replying.
    async fn read_submit_fields(&mut self, channel_id: u32) -> Result<DecodedShare, String> {
        let submit = self.read_frame().await.map_err(|e| {
            format!(
                "MockPool[{}]: read SubmitShares*: {}",
                self.style.label(),
                e
            )
        })?;

        let msg_type = submit.header.msg_type;
        if msg_type != mining::SUBMIT_SHARES_STANDARD && msg_type != mining::SUBMIT_SHARES_EXTENDED
        {
            return Err(format!(
                "MockPool[{}]: expected SubmitSharesStandard(0x1a)/Extended(0x1b), got 0x{:02x}",
                self.style.label(),
                msg_type
            ));
        }

        let p = &submit.payload;
        if p.len() < 24 {
            return Err(format!(
                "MockPool[{}]: SubmitShares* payload too short ({} < 24)",
                self.style.label(),
                p.len()
            ));
        }
        let got_channel_id = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        let sequence_number = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
        let job_id = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
        let nonce = u32::from_le_bytes([p[12], p[13], p[14], p[15]]);
        let ntime = u32::from_le_bytes([p[16], p[17], p[18], p[19]]);
        let version = u32::from_le_bytes([p[20], p[21], p[22], p[23]]);

        if got_channel_id != channel_id {
            return Err(format!(
                "MockPool[{}]: SubmitShares* channel_id mismatch: got {} want {}",
                self.style.label(),
                got_channel_id,
                channel_id
            ));
        }

        if msg_type == mining::SUBMIT_SHARES_EXTENDED {
            let ext_len = *p.get(24).unwrap_or(&0) as usize;
            if 25 + ext_len > p.len() {
                return Err(format!(
                    "MockPool[{}]: SubmitSharesExtended extranonce truncated (len byte {} but only {} payload bytes)",
                    self.style.label(),
                    ext_len,
                    p.len()
                ));
            }
        }

        Ok(DecodedShare {
            msg_type,
            channel_id: got_channel_id,
            sequence_number,
            job_id,
            nonce,
            ntime,
            version,
        })
    }

    /// Read one `SubmitSharesStandard`(0x1a)/`SubmitSharesExtended`(0x1b)
    /// frame, validate it, reply `SubmitSharesSuccess`(0x1c), and return the
    /// decoded share fields in [`MockPoolOutcome::ShareAccepted`].
    ///
    /// The decode runs on the *decrypted* plaintext, so a returned
    /// `ShareAccepted` proves the share bytes survived the Noise transport
    /// intact — the test asserts on `nonce`/`ntime`/`version` to make that an
    /// end-to-end fact rather than "some frame showed up".
    async fn read_submit_and_ack(&mut self, channel_id: u32) -> Result<MockPoolOutcome, String> {
        let submit = self.read_frame().await.map_err(|e| {
            format!(
                "MockPool[{}]: read SubmitShares*: {}",
                self.style.label(),
                e
            )
        })?;

        let msg_type = submit.header.msg_type;
        if msg_type != mining::SUBMIT_SHARES_STANDARD && msg_type != mining::SUBMIT_SHARES_EXTENDED
        {
            return Err(format!(
                "MockPool[{}]: expected SubmitSharesStandard(0x1a)/Extended(0x1b), got 0x{:02x}",
                self.style.label(),
                msg_type
            ));
        }

        // Both share types share a fixed 24-byte prefix:
        //   channel_id u32 + sequence_number u32 + job_id u32
        //   + nonce u32 + ntime u32 + version u32   (all little-endian)
        let p = &submit.payload;
        if p.len() < 24 {
            return Err(format!(
                "MockPool[{}]: SubmitShares* payload too short ({} < 24)",
                self.style.label(),
                p.len()
            ));
        }
        let got_channel_id = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        let sequence_number = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
        let job_id = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
        let nonce = u32::from_le_bytes([p[12], p[13], p[14], p[15]]);
        let ntime = u32::from_le_bytes([p[16], p[17], p[18], p[19]]);
        let version = u32::from_le_bytes([p[20], p[21], p[22], p[23]]);

        if got_channel_id != channel_id {
            return Err(format!(
                "MockPool[{}]: SubmitShares* channel_id mismatch: got {} want {}",
                self.style.label(),
                got_channel_id,
                channel_id
            ));
        }

        // Extended submits append a u8-length-prefixed extranonce. Bounds-check
        // it so a truncated frame is a hard failure, not a silent accept.
        if msg_type == mining::SUBMIT_SHARES_EXTENDED {
            let ext_len = *p.get(24).unwrap_or(&0) as usize;
            if 25 + ext_len > p.len() {
                return Err(format!(
                    "MockPool[{}]: SubmitSharesExtended extranonce truncated (len byte {} but only {} payload bytes)",
                    self.style.label(),
                    ext_len,
                    p.len()
                ));
            }
        }

        // SubmitSharesSuccess wire format (20 bytes):
        //   channel_id u32 + last_sequence_number u32
        //   + new_submits_accepted_count u32 + new_shares_sum u64
        let mut ack = Vec::with_capacity(20);
        ack.extend_from_slice(&channel_id.to_le_bytes());
        ack.extend_from_slice(&sequence_number.to_le_bytes()); // last_sequence_number
        ack.extend_from_slice(&1u32.to_le_bytes()); // new_submits_accepted_count
        ack.extend_from_slice(&1u64.to_le_bytes()); // new_shares_sum
        self.write_frame(mining::SUBMIT_SHARES_SUCCESS, &ack)
            .await
            .map_err(|e| {
                format!(
                    "MockPool[{}]: write SubmitSharesSuccess: {}",
                    self.style.label(),
                    e
                )
            })?;

        tracing::info!(
            "MockPool[{}]: SubmitShares* accepted (channel_id={}, seq={}, nonce=0x{:08x})",
            self.style.label(),
            got_channel_id,
            sequence_number,
            nonce
        );

        Ok(MockPoolOutcome::ShareAccepted {
            msg_type,
            channel_id: got_channel_id,
            sequence_number,
            job_id,
            nonce,
            ntime,
            version,
        })
    }

    /// Read one submitted share and reply with `SubmitSharesError`.
    async fn read_submit_and_reject(
        &mut self,
        channel_id: u32,
        reason: &str,
    ) -> Result<MockPoolOutcome, String> {
        let share = self.read_submit_fields(channel_id).await?;

        // SubmitSharesError wire format:
        //   channel_id u32 + sequence_number u32 + error STR0_255
        let mut payload = Vec::new();
        payload.extend_from_slice(&channel_id.to_le_bytes());
        payload.extend_from_slice(&share.sequence_number.to_le_bytes());
        write_sv2_str(&mut payload, reason);
        self.write_frame(mining::SUBMIT_SHARES_ERROR, &payload)
            .await
            .map_err(|e| {
                format!(
                    "MockPool[{}]: write SubmitSharesError: {}",
                    self.style.label(),
                    e
                )
            })?;

        tracing::info!(
            "MockPool[{}]: SubmitShares* rejected (channel_id={}, seq={}, nonce=0x{:08x})",
            self.style.label(),
            share.channel_id,
            share.sequence_number,
            share.nonce
        );

        Ok(MockPoolOutcome::ShareRejected {
            msg_type: share.msg_type,
            channel_id: share.channel_id,
            sequence_number: share.sequence_number,
            job_id: share.job_id,
            nonce: share.nonce,
            ntime: share.ntime,
            version: share.version,
            reason: reason.to_string(),
        })
    }

    /// Reply `SetupConnectionError`(0x02) and assert the client tears the
    /// session down WITHOUT opening a mining channel.
    ///
    /// The client must, on a setup rejection, exit cleanly (its
    /// `reached_mining` stays false) rather than charge ahead and send an
    /// `Open*MiningChannel`. We prove that by reading one more frame after the
    /// error: a transport close (EOF) is the success signal; an actual
    /// `Open*MiningChannel` frame is a contract violation.
    async fn run_auth_reject(&mut self) -> Result<MockPoolOutcome, String> {
        let setup_frame = self.read_frame().await.map_err(|e| {
            format!(
                "MockPool[{}]: read SetupConnection (auth-reject): {}",
                self.style.label(),
                e
            )
        })?;
        if setup_frame.header.msg_type != mining::SETUP_CONNECTION {
            return Ok(MockPoolOutcome::HandshakeOnly);
        }

        // SetupConnectionError payload: error_code STR0_255 + error_msg STR0_255.
        let mut payload = Vec::new();
        write_sv2_str(&mut payload, "unsupported-feature-flags");
        write_sv2_str(&mut payload, "mock pool rejected SetupConnection");
        self.write_frame(mining::SETUP_CONNECTION_ERROR, &payload)
            .await
            .map_err(|e| {
                format!(
                    "MockPool[{}]: write SetupConnectionError: {}",
                    self.style.label(),
                    e
                )
            })?;

        // Expect the client to drop the connection (read hits EOF). If it
        // instead sends an Open*MiningChannel — or any other frame — it ignored
        // the rejection, which is the bug this test guards against.
        match self.read_frame().await {
            Err(_eof) => Ok(MockPoolOutcome::AuthRejected),
            Ok(frame) => Err(format!(
                "MockPool[{}]: client sent frame 0x{:02x} after SetupConnectionError (expected clean disconnect, no channel open)",
                self.style.label(),
                frame.header.msg_type
            )),
        }
    }

    async fn send_open_standard_success_and_first_job(&mut self) -> Result<(), String> {
        // OpenStandardMiningChannelSuccess wire format:
        //   request_id u32 + channel_id u32 + target [u8; 32]
        //   + extranonce_prefix B0_32 (1B len + bytes)
        //   + group_channel_id u32
        let mut payload = Vec::with_capacity(45);
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&7u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&[0xFFu8; 32]); // target (effectively diff=1)
        payload.push(0); // extranonce_prefix length = 0
        payload.extend_from_slice(&0u32.to_le_bytes()); // group_channel_id = 0
        self.write_frame(mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS, &payload)
            .await?;

        // SetNewPrevHash first — channel.rs requires a prev_hash before it
        // can emit a NewJob from a non-future_job NewMiningJob. Per SV2
        // spec the standard ordering is NewMiningJob (future) +
        // SetNewPrevHash. We use future_job=false + prev_hash-first so
        // the NewJob event fires immediately on the NewMiningJob arrival.
        let mut prev_hash = Vec::with_capacity(44);
        prev_hash.extend_from_slice(&7u32.to_le_bytes()); // channel_id
        prev_hash.extend_from_slice(&0u32.to_le_bytes()); // job_id (matched at NewMiningJob)
        prev_hash.extend_from_slice(&[0x42u8; 32]); // prev_hash
        prev_hash.extend_from_slice(&1_730_000_000u32.to_le_bytes()); // min_ntime
        prev_hash.extend_from_slice(&0x1d00_ffffu32.to_le_bytes()); // nbits
        self.write_frame(mining::SET_NEW_PREV_HASH, &prev_hash)
            .await?;

        // NewMiningJob (standard channels): channel_id + job_id +
        // future_job(u8) + version(u32) + merkle_root(32).
        let mut job = Vec::with_capacity(45);
        job.extend_from_slice(&7u32.to_le_bytes()); // channel_id
        job.extend_from_slice(&101u32.to_le_bytes()); // job_id
        job.push(0); // future_job=false
        job.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        job.extend_from_slice(&[0x55u8; 32]); // merkle_root
        self.write_frame(mining::NEW_MINING_JOB, &job).await?;

        tracing::info!(
            "MockPool[{}]: first standard job dispatched",
            self.style.label()
        );
        Ok(())
    }

    async fn send_open_extended_success_and_first_job(&mut self) -> Result<(), String> {
        // OpenExtendedMiningChannelSuccess wire format:
        //   request_id u32 + channel_id u32 + target [u8; 32]
        //   + extranonce_size u16 + extranonce_prefix B0_32 (1B len + bytes)
        //   + group_channel_id u32
        let mut payload = Vec::with_capacity(50);
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&13u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&[0xFFu8; 32]); // target
        payload.extend_from_slice(&4u16.to_le_bytes()); // extranonce_size = 4
        payload.push(2); // extranonce_prefix length = 2
        payload.extend_from_slice(&[0xDE, 0xAD]); // extranonce_prefix
        payload.extend_from_slice(&0u32.to_le_bytes()); // group_channel_id = 0
        self.write_frame(mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS, &payload)
            .await?;

        // SetNewPrevHash first (same reason as standard path).
        let mut prev_hash = Vec::with_capacity(44);
        prev_hash.extend_from_slice(&13u32.to_le_bytes()); // channel_id
        prev_hash.extend_from_slice(&0u32.to_le_bytes()); // job_id
        prev_hash.extend_from_slice(&[0x99u8; 32]); // prev_hash
        prev_hash.extend_from_slice(&1_730_001_000u32.to_le_bytes()); // min_ntime
        prev_hash.extend_from_slice(&0x1d00_ffffu32.to_le_bytes()); // nbits
        self.write_frame(mining::SET_NEW_PREV_HASH, &prev_hash)
            .await?;

        // NewExtendedMiningJob wire format:
        //   channel_id u32 + job_id u32 + future_job u8 + version u32
        //   + version_rolling_allowed u8 + merkle_path_count u8 (count)
        //   + count*32 bytes merkle_path
        //   + coinbase_tx_prefix u16 len + bytes
        //   + coinbase_tx_suffix u16 len + bytes
        let mut job = Vec::with_capacity(64);
        job.extend_from_slice(&13u32.to_le_bytes()); // channel_id
        job.extend_from_slice(&202u32.to_le_bytes()); // job_id
        job.push(0); // future_job=false
        job.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        job.push(1); // version_rolling_allowed=true
        job.push(2); // merkle_path_count
        job.extend_from_slice(&[0x11u8; 32]); // merkle path[0]
        job.extend_from_slice(&[0x22u8; 32]); // merkle path[1]
                                              // coinbase_tx_prefix
        let cb_pre: [u8; 4] = [0xCA, 0xFE, 0xBA, 0xBE];
        job.extend_from_slice(&(cb_pre.len() as u16).to_le_bytes());
        job.extend_from_slice(&cb_pre);
        // coinbase_tx_suffix
        let cb_suf: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
        job.extend_from_slice(&(cb_suf.len() as u16).to_le_bytes());
        job.extend_from_slice(&cb_suf);
        self.write_frame(mining::NEW_EXTENDED_MINING_JOB, &job)
            .await?;

        tracing::info!(
            "MockPool[{}]: first extended job dispatched",
            self.style.label()
        );
        Ok(())
    }
}

// ─── helpers (intentionally duplicated from noise.rs to keep the test
// harness independent of the production module's private surface) ───

/// Encode an SV2 `STR0_255` (u8 length prefix + UTF-8 bytes) into `buf`.
///
/// Mirrors the production `SetupConnection::write_sv2_str` (private), kept
/// local so the harness stays independent of the production module's
/// internals. Used to build the `SetupConnectionError` payload.
fn write_sv2_str(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(255);
    buf.push(len as u8);
    buf.extend_from_slice(&bytes[..len]);
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn sha256_concat(a: &[u8; 32], b: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(a);
    hasher.update(b);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn make_nonce(counter: u64) -> chacha20poly1305::Nonce {
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[4..12].copy_from_slice(&counter.to_le_bytes());
    *chacha20poly1305::Nonce::from_slice(&nonce_bytes)
}

fn aead_encrypt(
    key: &[u8; 32],
    nonce_counter: u64,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|e| format!("key init: {}", e))?;
    let nonce = make_nonce(nonce_counter);
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| format!("AEAD encrypt: {}", e))
}

fn aead_decrypt(
    key: &[u8; 32],
    nonce_counter: u64,
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|e| format!("key init: {}", e))?;
    let nonce = make_nonce(nonce_counter);
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| format!("AEAD decrypt: {}", e))
}
