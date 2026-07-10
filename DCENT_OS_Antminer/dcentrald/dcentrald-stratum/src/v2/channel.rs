//! SV2 Mining Channel — client-side protocol state machine.
//!
//! Manages the lifecycle:
//!   1. TCP connect
//!   2. Noise_NX handshake (encryption)
//!   3. SetupConnection
//!   4. OpenStandardMiningChannel
//!   5. Receive NewMiningJob + SetNewPrevHash
//!   6. Submit shares
//!
//! Emits the same event types as the V1 Stratum client so the mining
//! dispatcher doesn't need to know which protocol version is active.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use super::framing::{FrameDecoder, Sv2Frame};
#[cfg(feature = "jd")]
use super::jd::CustomJobCandidate;
use super::noise::NoiseSession;
use super::types::*;
use tracing::{debug, error, info, warn};

/// Maximum number of message records kept in the history ring buffer.
const MAX_MESSAGE_HISTORY: usize = 200;

// ---------------------------------------------------------------------------
// SV2 string helpers (STR0_255: u8 length prefix + UTF-8 bytes)
// ---------------------------------------------------------------------------

/// Encode an SV2 STR0_255 string: u8 length prefix + bytes.
/// Used in tests to build synthetic pool response frames.
#[allow(dead_code)]
fn encode_str(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(255) as u8;
    buf.push(len);
    buf.extend_from_slice(&bytes[..len as usize]);
}

/// Decode an SV2 STR0_255 string from a byte slice at the given offset.
/// Returns (string, bytes_consumed) or an error description.
fn decode_str(buf: &[u8], offset: usize) -> Result<(String, usize), String> {
    if offset >= buf.len() {
        return Err(format!(
            "decode_str: offset {} past end (len={})",
            offset,
            buf.len()
        ));
    }
    let len = buf[offset] as usize;
    let end = offset + 1 + len;
    if end > buf.len() {
        return Err(format!(
            "decode_str: need {} bytes for string, have {}",
            end,
            buf.len()
        ));
    }
    let s = String::from_utf8_lossy(&buf[offset + 1..end]).to_string();
    Ok((s, 1 + len))
}

fn approximate_difficulty_from_target(target: &[u8; 32]) -> f64 {
    super::difficulty_autotune::target_to_approximate_difficulty(target)
}

fn parse_set_group_channel_payload(data: &[u8]) -> Result<(u32, Vec<u32>), String> {
    if data.len() < 6 {
        return Err(format!(
            "SetGroupChannel too short: need at least 6 bytes, have {}",
            data.len()
        ));
    }

    let group_channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let channel_count = u16::from_le_bytes([data[4], data[5]]) as usize;
    let expected_len = 6 + channel_count * 4;
    if data.len() < expected_len {
        return Err(format!(
            "SetGroupChannel channel_ids truncated: need {} bytes, have {}",
            expected_len,
            data.len()
        ));
    }

    let mut channel_ids = Vec::with_capacity(channel_count);
    let mut offset = 6;
    for _ in 0..channel_count {
        channel_ids.push(u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]));
        offset += 4;
    }

    Ok((group_channel_id, channel_ids))
}

// ---------------------------------------------------------------------------
// Events and state
// ---------------------------------------------------------------------------

/// Events emitted by the SV2 channel to the mining dispatcher.
/// These mirror the V1 StratumEvent types for compatibility.
#[derive(Debug)]
pub enum Sv2Event {
    /// New mining job received
    NewJob {
        job_id: u32,
        version: u32,
        prev_hash: [u8; 32],
        merkle_root: [u8; 32],
        nbits: u32,
        ntime: u32,
        clean_jobs: bool,
    },
    /// New extended-channel mining job received. Carries the coinbase split,
    /// merkle path, and version-rolling flag so the downstream dispatcher can
    /// reconstruct the merkle root with its assigned extranonce.
    NewExtendedJob {
        job_id: u32,
        version: u32,
        version_rolling_allowed: bool,
        prev_hash: [u8; 32],
        nbits: u32,
        ntime: u32,
        merkle_path: Vec<[u8; 32]>,
        coinbase_tx_prefix: Vec<u8>,
        coinbase_tx_suffix: Vec<u8>,
        clean_jobs: bool,
    },
    /// Pool difficulty changed
    DifficultyChanged(f64),
    /// Shares accepted by pool. SV2 `SubmitSharesSuccess` is a batch ack
    /// covering every share submitted up through `last_sequence_number`,
    /// not a per-job acknowledgement. `job_id` is always 0 — SV2 identifies
    /// the batch by sequence range, not job id — and is kept as a sentinel
    /// for backwards compatibility with V1-shaped consumers.
    ShareAccepted {
        job_id: u32,
        /// Last share sequence number the pool acknowledges in this batch.
        last_sequence_number: u32,
        /// Number of submits the pool accepted in this batch.
        new_submits_accepted_count: u32,
        /// Total share-difficulty sum the pool credited in this batch.
        /// Per SV2 spec this is the sum of difficulties for the accepted shares,
        /// expressed as a 64-bit unsigned integer.
        new_shares_sum: u64,
    },
    /// Share rejected by pool
    ShareRejected { job_id: u32, reason: String },
    /// Connection established (SetupConnection accepted)
    Connected,
    /// Connection lost
    Disconnected(String),
    /// Pool requested reconnect
    Reconnect { host: String, port: u16 },
    /// Pool accepted a custom mining job introduced by this proxy.
    CustomJobAccepted {
        channel_id: u32,
        request_id: u32,
        job_id: u32,
    },
    /// Pool rejected a custom mining job introduced by this proxy.
    CustomJobRejected {
        channel_id: u32,
        request_id: u32,
        reason: String,
    },
    /// Pool assigned one or more channels to a group channel.
    GroupChannelAssigned {
        group_channel_id: u32,
        channel_ids: Vec<u32>,
    },
    /// Pool changed the extranonce prefix for a mining channel.
    ExtranoncePrefixChanged { channel_id: u32, prefix: Vec<u8> },
}

/// Mining channel state machine
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelState {
    /// Not connected
    Disconnected,
    /// TCP connected, Noise handshake in progress
    Handshaking,
    /// Noise authenticated, SetupConnection sent and awaiting response
    SettingUp,
    /// SetupConnection accepted, OpenStandardMiningChannel sent
    OpeningChannel,
    /// Channel open, actively mining
    Mining { channel_id: u32 },
    /// Unrecoverable error — caller must reset and reconnect
    Error(String),
}

// ---------------------------------------------------------------------------
// Sv2MiningChannel
// ---------------------------------------------------------------------------

/// SV2 Mining Channel client — full protocol state machine.
///
/// # Lifecycle
/// 1. Call `make_setup_connection()` right after Noise handshake completes.
///    Send the returned bytes over TCP.
/// 2. When `feed_data()` emits `Sv2Event::Connected`, call `make_open_channel()`
///    and send those bytes.
/// 3. When jobs arrive (via `NewJob` events), pass them to the ASIC dispatcher.
/// 4. When the ASIC finds a valid nonce, call `make_submit_share()` and send it.
/// 5. On `Disconnected`, call `reset()` and reconnect.
///
/// Hashrate (H/s) at/above which a Standard SV2 mining channel exhausts its nonce search space
/// fast enough to stall. mining-core-bible.md §2: "Below 1 TH/s standard is fine; above it use
/// Extended." 1 TH/s = 1e12 H/s.
pub(crate) const STANDARD_CHANNEL_NONCE_EXHAUSTION_HS: f64 = 1.0e12;

/// `true` if a Standard SV2 channel at this nominal hashrate (H/s) risks nonce-space exhaustion
/// (≥ 1 TH/s). Pure + host-testable; drives only an advisory log, never the channel-type choice.
pub(crate) fn standard_channel_nonce_exhaustion_risk(nominal_hashrate_hs: f64) -> bool {
    nominal_hashrate_hs >= STANDARD_CHANNEL_NONCE_EXHAUSTION_HS
}

pub struct Sv2MiningChannel {
    /// Current protocol state
    pub state: ChannelState,
    /// Noise encryption session (post-handshake transport mode)
    noise: NoiseSession,
    /// Streaming frame decoder
    decoder: FrameDecoder,
    /// Monotonically increasing request ID for correlating responses
    request_counter: u32,
    /// Share sequence number — incremented on every submission
    sequence_number: u32,
    /// Worker identity string sent in OpenStandardMiningChannel
    worker_identity: String,
    /// Nominal hashrate in H/s (derived from GH/s at construction)
    nominal_hashrate: f32,
    /// Jobs received with `future_job=true`, waiting for SetNewPrevHash
    pending_jobs: Vec<(u32, NewMiningJob)>,
    /// Extended-channel future jobs waiting for SetNewPrevHash
    pending_extended_jobs: Vec<(u32, NewExtendedMiningJob)>,
    /// The most recently active prev_hash state
    current_prev_hash: Option<SetNewPrevHash>,
    /// Accumulated events from the current `feed_data()` call
    events: Vec<Sv2Event>,
    /// Ring buffer of recent protocol messages (for protocol inspector)
    message_history: VecDeque<super::types::Sv2MessageRecord>,
    /// Actual connected pool host (for SetupConnection endpoint_host field).
    endpoint_host: String,
    /// Actual connected pool port (for SetupConnection endpoint_port field).
    endpoint_port: u16,
    /// Whether this connection negotiates miner-selected work.
    work_selection_enabled: bool,
    /// Extranonce prefix assigned by an extended-channel upstream.
    channel_extranonce_prefix: Vec<u8>,
    /// Extranonce byte count assigned by an extended-channel upstream.
    channel_extranonce_size: u16,
    /// Current group channel for this mining channel, when assigned.
    group_channel_id: Option<u32>,
}

impl Sv2MiningChannel {
    /// Create a new channel. `hashrate_ghs` is the device's nominal hashrate
    /// in GH/s (e.g. 500.0 for a BM1366-based BitAxe Ultra).
    pub fn new(worker: &str, hashrate_ghs: f32) -> Self {
        Self {
            state: ChannelState::Disconnected,
            noise: NoiseSession::new(),
            decoder: FrameDecoder::new(),
            request_counter: 0,
            sequence_number: 0,
            worker_identity: worker.to_string(),
            nominal_hashrate: hashrate_ghs * 1e9,
            pending_jobs: Vec::new(),
            pending_extended_jobs: Vec::new(),
            current_prev_hash: None,
            events: Vec::new(),
            message_history: VecDeque::new(),
            endpoint_host: String::new(),
            endpoint_port: 0,
            work_selection_enabled: false,
            channel_extranonce_prefix: Vec::new(),
            channel_extranonce_size: 0,
            group_channel_id: None,
        }
    }

    /// Reset the channel to its initial disconnected state.
    ///
    /// Call this before attempting reconnection after `Disconnected` or
    /// `Error` events. Clears all pending jobs, resets Noise session, and
    /// resets sequence counters.
    pub fn reset(&mut self) {
        self.state = ChannelState::Disconnected;
        self.noise = NoiseSession::new();
        self.decoder.reset();
        self.request_counter = 0;
        self.sequence_number = 0;
        self.pending_jobs.clear();
        self.pending_extended_jobs.clear();
        self.current_prev_hash = None;
        self.events.clear();
        self.message_history.clear();
        self.channel_extranonce_prefix.clear();
        self.channel_extranonce_size = 0;
        self.group_channel_id = None;
    }

    /// Set the actual connected pool endpoint (used in SetupConnection).
    pub fn set_endpoint(&mut self, host: &str, port: u16) {
        self.endpoint_host = host.to_string();
        self.endpoint_port = port;
    }

    /// Enable Mining Protocol work selection for Job Declaration custom jobs.
    pub fn enable_work_selection(&mut self) {
        self.work_selection_enabled = true;
    }

    pub fn work_selection_enabled(&self) -> bool {
        self.work_selection_enabled
    }

    fn targets_current_channel(&self, channel_id: u32) -> bool {
        self.channel_id() == Some(channel_id)
    }

    fn targets_current_channel_or_group(&self, channel_id: u32) -> bool {
        self.targets_current_channel(channel_id) || self.group_channel_id == Some(channel_id)
    }

    /// Get a reference to the noise session (for checking state)
    pub fn noise_session(&self) -> &NoiseSession {
        &self.noise
    }

    /// Get a mutable reference to the noise session (for handshake setup)
    ///
    /// The caller is responsible for performing the Noise_NX handshake
    /// (secp256k1 ECDH, EllSwift encoding) and calling
    /// `noise_session_mut().set_transport_keys(...)` once complete.
    pub fn noise_session_mut(&mut self) -> &mut NoiseSession {
        &mut self.noise
    }

    /// Mark the channel as handshaking (TCP connected, Noise in progress).
    pub fn begin_handshake(&mut self) {
        self.state = ChannelState::Handshaking;
        self.noise.mark_waiting();
    }

    /// Build the SetupConnection frame to send after Noise handshake completes.
    ///
    /// Transitions state to `SettingUp`. The caller must send the returned
    /// bytes to the pool. If Noise is in transport mode the bytes are NOT
    /// pre-encrypted here — the TCP send path should call `noise.encrypt()`
    /// before writing to the socket.
    pub fn make_setup_connection(&mut self) -> Vec<u8> {
        // BUG FIX (2026-04-11): Was hardcoded to "v2.stratum.braiins.com:3336".
        // Now uses the actual connected host/port stored at channel creation.
        let flags = if self.work_selection_enabled {
            REQUIRES_WORK_SELECTION | REQUIRES_VERSION_ROLLING
        } else {
            REQUIRES_STANDARD_JOBS | REQUIRES_VERSION_ROLLING
        };
        let msg = SetupConnection {
            protocol: 0, // 0 = Mining Protocol (SV2 spec §5.1)
            min_version: 2,
            max_version: 2,
            flags,
            endpoint_host: self.endpoint_host.clone(),
            endpoint_port: self.endpoint_port,
            vendor: "D-Central Technologies".into(),
            hardware_version: "DCENT_OS".into(),
            firmware: "dcentrald".into(),
            device_id: String::new(),
        };
        let payload = msg.to_bytes();
        self.record_message(
            "sent",
            mining::SETUP_CONNECTION,
            "SetupConnection",
            payload.len(),
        );
        let frame = Sv2Frame::new(EXTENSION_TYPE_MINING, mining::SETUP_CONNECTION, payload);
        self.state = ChannelState::SettingUp;
        frame.to_bytes()
    }

    /// Build the OpenStandardMiningChannel frame.
    ///
    /// Call this after receiving a `Connected` event (SetupConnectionSuccess).
    /// Transitions state to `OpeningChannel`.
    pub fn make_open_channel(&mut self) -> Vec<u8> {
        self.request_counter += 1;
        // Compute max_target from nominal hashrate so the pool assigns a
        // reasonable share difficulty. Target ~1 share every 5 seconds:
        //   difficulty = hashrate_h/s * 5 / 2^32
        //   max_target = pdiff_1_target / difficulty
        // With [0xFF; 32] we accepted 17 BILLION difficulty from Braiins Pool,
        // which an S9 at 10 TH/s could never meet (1 share per 20 days).
        let max_target = {
            // Target difficulty: hashrate * 5s / 2^32
            let target_diff = (self.nominal_hashrate as f64 * 5.0) / (u32::MAX as f64);
            // Clamp to reasonable range (min diff 256, max diff 1M)
            let diff = target_diff.clamp(256.0, 1_000_000.0);
            crate::work::difficulty_to_target(diff)
        };
        let (msg_type, msg_name, payload) = if self.work_selection_enabled {
            let msg = OpenExtendedMiningChannel {
                request_id: self.request_counter,
                user_identity: self.worker_identity.clone(),
                nominal_hash_rate: self.nominal_hashrate,
                max_target,
                min_extranonce_size: 0,
            };
            (
                mining::OPEN_EXTENDED_MINING_CHANNEL,
                "OpenExtendedMiningChannel",
                msg.to_bytes(),
            )
        } else {
            // PARITY (RE 2026-06-02, mining-core-bible.md §2): a Standard SV2 channel exhausts
            // its 2^32 nonce + 2^16 version search space in seconds at any DCENT-class hashrate
            // (an S9 at ~13.5 TH/s burns it in ~2.5 s), then silently stalls. Above ~1 TH/s the
            // bible says use an Extended channel (operator enables `work_selection`). Emit an
            // advisory — we do NOT hard-flip the channel type (that would change the negotiated
            // protocol behind the operator's back); the operator opts into work-selection.
            if standard_channel_nonce_exhaustion_risk(self.nominal_hashrate as f64) {
                tracing::warn!(
                    nominal_hashrate_ghs = (self.nominal_hashrate as f64) / 1.0e9,
                    "SV2 Standard mining channel selected at >= 1 TH/s — the 2^32 nonce space \
                     exhausts in seconds and the channel will stall. Enable work-selection \
                     (Extended channel) for any DCENT-class miner. (advisory; channel type unchanged)"
                );
            }
            let msg = OpenStandardMiningChannel {
                request_id: self.request_counter,
                user_identity: self.worker_identity.clone(),
                nominal_hash_rate: self.nominal_hashrate,
                max_target,
            };
            (
                mining::OPEN_STANDARD_MINING_CHANNEL,
                "OpenStandardMiningChannel",
                msg.to_bytes(),
            )
        };
        self.record_message("sent", msg_type, msg_name, payload.len());
        let frame = Sv2Frame::new(EXTENSION_TYPE_MINING, msg_type, payload);
        self.state = ChannelState::OpeningChannel;
        frame.to_bytes()
    }

    /// Build a SubmitSharesStandard frame for a found nonce.
    ///
    /// # Arguments
    /// * `channel_id` — from `channel_id()` (set when channel opened)
    /// * `job_id`     — from the `NewJob` event that produced this nonce
    /// * `nonce`      — the 32-bit nonce found by the ASIC
    /// * `ntime`      — the ntime used when hashing (may be rolled from job ntime)
    /// * `version`    — the version field used (may be rolled via BIP320)
    pub fn make_submit_share(
        &mut self,
        channel_id: u32,
        job_id: u32,
        nonce: u32,
        ntime: u32,
        version: u32,
    ) -> Vec<u8> {
        self.sequence_number += 1;
        let msg = SubmitSharesStandard {
            channel_id,
            sequence_number: self.sequence_number,
            job_id,
            nonce,
            ntime,
            version,
        };
        let payload = msg.to_bytes();
        self.record_message(
            "sent",
            mining::SUBMIT_SHARES_STANDARD,
            "SubmitSharesStandard",
            payload.len(),
        );
        let frame = Sv2Frame::new(
            EXTENSION_TYPE_MINING,
            mining::SUBMIT_SHARES_STANDARD,
            payload,
        );
        frame.to_bytes()
    }

    /// Build a SubmitSharesExtended frame for a custom job on an extended channel.
    pub fn make_submit_share_extended(
        &mut self,
        channel_id: u32,
        job_id: u32,
        nonce: u32,
        ntime: u32,
        version: u32,
        extranonce: &[u8],
    ) -> Vec<u8> {
        self.sequence_number += 1;
        let msg = SubmitSharesExtended {
            channel_id,
            sequence_number: self.sequence_number,
            job_id,
            nonce,
            ntime,
            version,
            extranonce: extranonce.to_vec(),
        };
        let payload = msg.to_bytes();
        self.record_message(
            "sent",
            mining::SUBMIT_SHARES_EXTENDED,
            "SubmitSharesExtended",
            payload.len(),
        );
        let frame = Sv2Frame::new(
            EXTENSION_TYPE_MINING,
            mining::SUBMIT_SHARES_EXTENDED,
            payload,
        );
        frame.to_bytes()
    }

    /// Build a SetCustomMiningJob frame for the currently open extended channel.
    #[cfg(feature = "jd")]
    pub fn make_set_custom_mining_job(
        &mut self,
        candidate: &CustomJobCandidate,
    ) -> Result<(u32, Vec<u8>), String> {
        let channel_id = self
            .channel_id()
            .ok_or_else(|| "SV2: cannot set custom job before channel open".to_string())?;
        if !self.work_selection_enabled {
            return Err("SV2: custom jobs require work-selection mode".to_string());
        }
        self.request_counter = self.request_counter.saturating_add(1);
        let request_id = self.request_counter;
        let msg = candidate.to_set_custom_mining_job(channel_id, request_id);
        let payload = msg.to_bytes()?;
        self.record_message(
            "sent",
            mining::SET_CUSTOM_MINING_JOB,
            "SetCustomMiningJob",
            payload.len(),
        );
        let frame = Sv2Frame::new(
            EXTENSION_TYPE_MINING,
            mining::SET_CUSTOM_MINING_JOB,
            payload,
        );
        Ok((request_id, frame.to_bytes()))
    }

    /// Feed **plaintext** SV2 frame data into the state machine.
    ///
    /// The caller is responsible for Noise decryption before calling this.
    /// Returns all events produced by complete frames in this batch.
    pub fn feed_data(&mut self, data: &[u8]) -> Vec<Sv2Event> {
        self.events.clear();

        self.decoder.feed(data);

        // Drain all complete frames
        loop {
            match self.decoder.next_frame() {
                Ok(Some(frame)) => self.handle_frame(frame),
                Ok(None) => break,
                Err(e) => {
                    let msg = format!("SV2: frame decode error: {}", e);
                    error!("{}", msg);
                    self.state = ChannelState::Error(msg.clone());
                    self.events.push(Sv2Event::Disconnected(msg));
                    break;
                }
            }
        }

        std::mem::take(&mut self.events)
    }

    /// Dispatch a single decoded frame to the appropriate handler.
    fn handle_frame(&mut self, frame: Sv2Frame) {
        match frame.header.msg_type {
            // ------------------------------------------------------------------
            // SetupConnection responses
            // ------------------------------------------------------------------
            mining::SETUP_CONNECTION_SUCCESS => {
                self.record_message(
                    "recv",
                    mining::SETUP_CONNECTION_SUCCESS,
                    "SetupConnectionSuccess",
                    frame.payload.len(),
                );
                info!("SV2: SetupConnection accepted (pool ready)");
                // Emit Connected so the caller knows to send OpenStandardMiningChannel
                self.events.push(Sv2Event::Connected);
            }

            mining::SETUP_CONNECTION_ERROR => {
                self.record_message(
                    "recv",
                    mining::SETUP_CONNECTION_ERROR,
                    "SetupConnectionError",
                    frame.payload.len(),
                );
                // Payload: error_code STR0_255 + error_msg STR0_255
                let reason = if !frame.payload.is_empty() {
                    // Skip error_code, take the human-readable message
                    if let Ok((code, consumed)) = decode_str(&frame.payload, 0) {
                        if let Ok((msg, _)) = decode_str(&frame.payload, consumed) {
                            format!("{}: {}", code, msg)
                        } else {
                            code
                        }
                    } else {
                        String::from_utf8_lossy(&frame.payload).to_string()
                    }
                } else {
                    "SetupConnection rejected (no reason given)".to_string()
                };
                error!("SV2: SetupConnection rejected: {}", reason);
                self.state = ChannelState::Error(reason.clone());
                self.events.push(Sv2Event::Disconnected(reason));
            }

            // ------------------------------------------------------------------
            // OpenStandardMiningChannel responses
            // ------------------------------------------------------------------
            mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS => {
                self.record_message(
                    "recv",
                    mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS,
                    "OpenStandardMiningChannelSuccess",
                    frame.payload.len(),
                );
                // Payload: request_id(4) + channel_id(4) + target(32) + extranonce_prefix(var)
                if frame.payload.len() >= 40 {
                    let _request_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let channel_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    // BUG FIX (2026-04-11): Parse initial share target from channel open response.
                    // Was ignored — current_share_target stayed [0xFF;32] until a later SetTarget.
                    let mut initial_target = [0u8; 32];
                    initial_target.copy_from_slice(&frame.payload[8..40]);
                    if frame.payload.len() > 40 {
                        let prefix_len = frame.payload[40] as usize;
                        let prefix_start = 41;
                        let prefix_end = prefix_start + prefix_len;
                        if prefix_len > 32 {
                            warn!(
                                "SV2: OpenStandardMiningChannelSuccess extranonce prefix too long ({})",
                                prefix_len
                            );
                            return;
                        }
                        if frame.payload.len() < prefix_end {
                            warn!(
                                "SV2: OpenStandardMiningChannelSuccess extranonce prefix truncated ({}/{})",
                                frame.payload.len(),
                                prefix_end
                            );
                            return;
                        }
                        self.channel_extranonce_prefix =
                            frame.payload[prefix_start..prefix_end].to_vec();
                        self.channel_extranonce_size = 0;
                        self.group_channel_id = if frame.payload.len() >= prefix_end + 4 {
                            Some(u32::from_le_bytes([
                                frame.payload[prefix_end],
                                frame.payload[prefix_end + 1],
                                frame.payload[prefix_end + 2],
                                frame.payload[prefix_end + 3],
                            ]))
                        } else {
                            None
                        };
                    } else {
                        self.channel_extranonce_prefix.clear();
                        self.channel_extranonce_size = 0;
                        self.group_channel_id = None;
                    }
                    // Emit DifficultyChanged so client picks up the initial target.
                    let approx_diff = approximate_difficulty_from_target(&initial_target);
                    info!(
                        "SV2: mining channel {} opened, initial target approx_diff≈{}",
                        channel_id, approx_diff
                    );
                    self.state = ChannelState::Mining { channel_id };
                    self.events.push(Sv2Event::DifficultyChanged(approx_diff));
                } else if frame.payload.len() >= 8 {
                    // Fallback: payload too short for target, still parse channel_id
                    let channel_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    info!(
                        "SV2: mining channel {} opened (no target in payload)",
                        channel_id
                    );
                    self.state = ChannelState::Mining { channel_id };
                    self.channel_extranonce_size = 0;
                    self.channel_extranonce_prefix.clear();
                    self.group_channel_id = None;
                } else {
                    warn!(
                        "SV2: OpenStandardMiningChannelSuccess payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS => {
                self.record_message(
                    "recv",
                    mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
                    "OpenExtendedMiningChannelSuccess",
                    frame.payload.len(),
                );
                // Payload:
                // request_id(4) + channel_id(4) + target(32) +
                // extranonce_size(2) + extranonce_prefix(B0_32) + group_channel_id(4)
                if frame.payload.len() >= 43 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    let mut initial_target = [0u8; 32];
                    initial_target.copy_from_slice(&frame.payload[8..40]);
                    let extranonce_size =
                        u16::from_le_bytes([frame.payload[40], frame.payload[41]]);
                    let prefix_len = frame.payload[42] as usize;
                    let prefix_start = 43;
                    let prefix_end = prefix_start + prefix_len;
                    if prefix_len > 32 {
                        warn!(
                            "SV2: OpenExtendedMiningChannelSuccess extranonce prefix too long ({})",
                            prefix_len
                        );
                        return;
                    }
                    if frame.payload.len() < prefix_end {
                        warn!(
                            "SV2: OpenExtendedMiningChannelSuccess extranonce prefix truncated ({}/{})",
                            frame.payload.len(),
                            prefix_end
                        );
                        return;
                    }
                    self.channel_extranonce_size = extranonce_size;
                    self.channel_extranonce_prefix =
                        frame.payload[prefix_start..prefix_end].to_vec();
                    self.group_channel_id = if frame.payload.len() >= prefix_end + 4 {
                        Some(u32::from_le_bytes([
                            frame.payload[prefix_end],
                            frame.payload[prefix_end + 1],
                            frame.payload[prefix_end + 2],
                            frame.payload[prefix_end + 3],
                        ]))
                    } else {
                        None
                    };
                    let approx_diff = approximate_difficulty_from_target(&initial_target);
                    info!(
                        "SV2: extended mining channel {} opened, extranonce_size={}, prefix_len={}, initial target approx_diff≈{}",
                        channel_id, extranonce_size, prefix_len, approx_diff
                    );
                    self.state = ChannelState::Mining { channel_id };
                    self.events.push(Sv2Event::DifficultyChanged(approx_diff));
                } else if frame.payload.len() >= 8 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    info!(
                        "SV2: extended mining channel {} opened (short success payload)",
                        channel_id
                    );
                    self.state = ChannelState::Mining { channel_id };
                    self.channel_extranonce_size = 0;
                    self.channel_extranonce_prefix.clear();
                    self.group_channel_id = None;
                } else {
                    warn!(
                        "SV2: OpenExtendedMiningChannelSuccess payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            // ------------------------------------------------------------------
            // OpenMiningChannelError — pool refused the channel open. Without
            // this handler the client would stay stuck in `OpeningChannel`
            // forever and never reconnect. Per SV2 spec, payload is:
            //   request_id u32  + error_code STR0_255
            // (some implementations append a free-form error_msg STR0_255).
            // ------------------------------------------------------------------
            mining::OPEN_MINING_CHANNEL_ERROR => {
                self.record_message(
                    "recv",
                    mining::OPEN_MINING_CHANNEL_ERROR,
                    "OpenMiningChannelError",
                    frame.payload.len(),
                );
                let reason = if frame.payload.len() >= 4 {
                    let request_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let code = decode_str(&frame.payload, 4)
                        .map(|(s, _)| s)
                        .unwrap_or_else(|_| "<malformed>".to_string());
                    format!(
                        "OpenMiningChannelError request_id={} code={}",
                        request_id, code
                    )
                } else {
                    format!(
                        "OpenMiningChannelError payload too short ({} bytes)",
                        frame.payload.len()
                    )
                };
                error!("SV2: pool refused mining channel open: {}", reason);
                self.state = ChannelState::Error(reason.clone());
                self.events.push(Sv2Event::Disconnected(reason));
            }

            // ------------------------------------------------------------------
            // CloseChannel — pool tells us a specific channel is now closed.
            // Per SV2 spec, payload is:
            //   channel_id u32 + reason_code STR0_255
            // If the closure targets our currently open channel (or our group
            // channel), emit `Disconnected` so client.run_session() reconnects;
            // closures for other channels are recorded but do not tear down
            // the session.
            // ------------------------------------------------------------------
            mining::CLOSE_CHANNEL => {
                self.record_message(
                    "recv",
                    mining::CLOSE_CHANNEL,
                    "CloseChannel",
                    frame.payload.len(),
                );
                if frame.payload.len() < 4 {
                    warn!(
                        "SV2: CloseChannel payload too short ({} bytes)",
                        frame.payload.len()
                    );
                    return;
                }
                let channel_id = u32::from_le_bytes([
                    frame.payload[0],
                    frame.payload[1],
                    frame.payload[2],
                    frame.payload[3],
                ]);
                let reason_code = decode_str(&frame.payload, 4)
                    .map(|(s, _)| s)
                    .unwrap_or_else(|_| "<malformed>".to_string());
                if self.targets_current_channel_or_group(channel_id) {
                    let reason = format!(
                        "CloseChannel channel_id={} reason={}",
                        channel_id, reason_code
                    );
                    warn!("SV2: pool closed our mining channel: {}", reason);
                    self.state = ChannelState::Error(reason.clone());
                    self.events.push(Sv2Event::Disconnected(reason));
                } else {
                    info!(
                        channel_id,
                        reason = %reason_code,
                        current_channel_id = ?self.channel_id(),
                        "SV2: CloseChannel for unrelated channel — ignoring"
                    );
                }
            }

            mining::SET_GROUP_CHANNEL => {
                self.record_message(
                    "recv",
                    mining::SET_GROUP_CHANNEL,
                    "SetGroupChannel",
                    frame.payload.len(),
                );
                match parse_set_group_channel_payload(&frame.payload) {
                    Ok((group_channel_id, channel_ids)) => {
                        let expected_len = 6 + channel_ids.len() * 4;
                        if frame.payload.len() > expected_len {
                            warn!(
                                "SV2: SetGroupChannel has {} trailing bytes",
                                frame.payload.len() - expected_len
                            );
                        }
                        if let Some(current_channel_id) = self.channel_id() {
                            if channel_ids.contains(&current_channel_id) {
                                self.group_channel_id = Some(group_channel_id);
                            }
                        }
                        info!(
                            group_channel_id,
                            channel_count = channel_ids.len(),
                            "SV2: group channel assignment received"
                        );
                        self.events.push(Sv2Event::GroupChannelAssigned {
                            group_channel_id,
                            channel_ids,
                        });
                    }
                    Err(error) => warn!(%error, "SV2: failed to parse SetGroupChannel"),
                }
            }

            // ------------------------------------------------------------------
            // Job delivery
            // ------------------------------------------------------------------
            mining::NEW_MINING_JOB => {
                self.record_message(
                    "recv",
                    mining::NEW_MINING_JOB,
                    "NewMiningJob",
                    frame.payload.len(),
                );
                match NewMiningJob::from_bytes(&frame.payload) {
                    Ok(job) => {
                        if !self.targets_current_channel(job.channel_id) {
                            warn!(
                                "SV2: ignoring NewMiningJob for channel {} (current={:?})",
                                job.channel_id,
                                self.channel_id()
                            );
                            return;
                        }
                        debug!(
                            "SV2: NewMiningJob job_id={} future={}",
                            job.job_id, job.future_job
                        );

                        if !job.future_job {
                            // Immediately usable — emit if we have a prev_hash
                            if let Some(ref ph) = self.current_prev_hash {
                                self.events.push(Sv2Event::NewJob {
                                    job_id: job.job_id,
                                    version: job.version,
                                    prev_hash: ph.prev_hash,
                                    merkle_root: job.merkle_root,
                                    nbits: ph.nbits,
                                    ntime: ph.min_ntime,
                                    clean_jobs: true,
                                });
                            }
                        }
                        // Always store — SetNewPrevHash may arrive later
                        self.pending_jobs.push((job.job_id, job));
                        // Bound memory: keep only the 16 most recent jobs
                        if self.pending_jobs.len() > 16 {
                            self.pending_jobs.remove(0);
                        }
                    }
                    Err(e) => warn!("SV2: failed to parse NewMiningJob: {}", e),
                }
            }

            mining::NEW_EXTENDED_MINING_JOB => {
                self.record_message(
                    "recv",
                    mining::NEW_EXTENDED_MINING_JOB,
                    "NewExtendedMiningJob",
                    frame.payload.len(),
                );
                match NewExtendedMiningJob::from_bytes(&frame.payload) {
                    Ok(job) => {
                        if !self.targets_current_channel(job.channel_id) {
                            warn!(
                                "SV2: ignoring NewExtendedMiningJob for channel {} (current={:?})",
                                job.channel_id,
                                self.channel_id()
                            );
                            return;
                        }
                        debug!(
                            "SV2: NewExtendedMiningJob job_id={} future={} version_rolling_allowed={} merkle_path_len={} prefix={}B suffix={}B",
                            job.job_id,
                            job.future_job,
                            job.version_rolling_allowed,
                            job.merkle_path.len(),
                            job.coinbase_tx_prefix.len(),
                            job.coinbase_tx_suffix.len(),
                        );

                        if !job.future_job {
                            // Immediately usable — emit if we already have a prev_hash
                            if let Some(ref ph) = self.current_prev_hash {
                                self.events.push(Sv2Event::NewExtendedJob {
                                    job_id: job.job_id,
                                    version: job.version,
                                    version_rolling_allowed: job.version_rolling_allowed,
                                    prev_hash: ph.prev_hash,
                                    nbits: ph.nbits,
                                    ntime: ph.min_ntime,
                                    merkle_path: job.merkle_path.clone(),
                                    coinbase_tx_prefix: job.coinbase_tx_prefix.clone(),
                                    coinbase_tx_suffix: job.coinbase_tx_suffix.clone(),
                                    clean_jobs: true,
                                });
                            }
                        }
                        // Always store — SetNewPrevHash may arrive later
                        self.pending_extended_jobs.push((job.job_id, job));
                        // Bound memory: keep only the 16 most recent extended jobs
                        if self.pending_extended_jobs.len() > 16 {
                            self.pending_extended_jobs.remove(0);
                        }
                    }
                    Err(e) => warn!("SV2: failed to parse NewExtendedMiningJob: {}", e),
                }
            }

            mining::SET_NEW_PREV_HASH => {
                self.record_message(
                    "recv",
                    mining::SET_NEW_PREV_HASH,
                    "SetNewPrevHash",
                    frame.payload.len(),
                );
                match SetNewPrevHash::from_bytes(&frame.payload) {
                    Ok(ph) => {
                        if !self.targets_current_channel_or_group(ph.channel_id) {
                            warn!(
                                "SV2: ignoring SetNewPrevHash for channel/group {} (current={:?}, group={:?})",
                                ph.channel_id,
                                self.channel_id(),
                                self.group_channel_id
                            );
                            return;
                        }
                        info!(
                            "SV2: SetNewPrevHash for job_id={} nbits=0x{:08x}",
                            ph.job_id, ph.nbits
                        );

                        // Find the matching job — or use the most recent if job_id=0
                        let matched_job = if ph.job_id == 0 {
                            // job_id=0 means "use the latest job"
                            self.pending_jobs.last().map(|(_, job)| job.clone())
                        } else {
                            self.pending_jobs
                                .iter()
                                .find(|(id, _)| *id == ph.job_id)
                                .map(|(_, job)| job.clone())
                        };

                        // Match against either a standard job or an extended job
                        // — extended channels never receive NewMiningJob (0x15);
                        // standard channels never receive NewExtendedMiningJob (0x1f).
                        let matched_extended = if ph.job_id == 0 {
                            self.pending_extended_jobs.last().map(|(_, j)| j.clone())
                        } else {
                            self.pending_extended_jobs
                                .iter()
                                .find(|(id, _)| *id == ph.job_id)
                                .map(|(_, j)| j.clone())
                        };

                        if let Some(job) = matched_job {
                            self.events.push(Sv2Event::NewJob {
                                job_id: job.job_id,
                                version: job.version,
                                prev_hash: ph.prev_hash,
                                merkle_root: job.merkle_root,
                                nbits: ph.nbits,
                                ntime: ph.min_ntime,
                                clean_jobs: true,
                            });
                        } else if let Some(job) = matched_extended {
                            self.events.push(Sv2Event::NewExtendedJob {
                                job_id: job.job_id,
                                version: job.version,
                                version_rolling_allowed: job.version_rolling_allowed,
                                prev_hash: ph.prev_hash,
                                nbits: ph.nbits,
                                ntime: ph.min_ntime,
                                merkle_path: job.merkle_path,
                                coinbase_tx_prefix: job.coinbase_tx_prefix,
                                coinbase_tx_suffix: job.coinbase_tx_suffix,
                                clean_jobs: true,
                            });
                        } else {
                            warn!(
                                "SV2: SetNewPrevHash job_id={} — no matching job yet (will match on next NewMiningJob/NewExtendedMiningJob)",
                                ph.job_id
                            );
                        }
                        // Drop stale jobs
                        if ph.job_id > 0 {
                            self.pending_jobs.retain(|(id, _)| *id >= ph.job_id);
                            self.pending_extended_jobs
                                .retain(|(id, _)| *id >= ph.job_id);
                        }
                        self.current_prev_hash = Some(ph);
                    }
                    Err(e) => warn!("SV2: failed to parse SetNewPrevHash: {}", e),
                }
            }

            // ------------------------------------------------------------------
            // Share submission responses
            // ------------------------------------------------------------------
            mining::SUBMIT_SHARES_SUCCESS => {
                self.record_message(
                    "recv",
                    mining::SUBMIT_SHARES_SUCCESS,
                    "SubmitSharesSuccess",
                    frame.payload.len(),
                );
                // SV2 spec payload (20 bytes total):
                //   channel_id                 u32  4 B
                //   last_sequence_number       u32  4 B
                //   new_submits_accepted_count u32  4 B
                //   new_shares_sum             u64  8 B
                if frame.payload.len() >= 20 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let last_sequence_number = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    let new_submits_accepted_count = u32::from_le_bytes([
                        frame.payload[8],
                        frame.payload[9],
                        frame.payload[10],
                        frame.payload[11],
                    ]);
                    let new_shares_sum = u64::from_le_bytes([
                        frame.payload[12],
                        frame.payload[13],
                        frame.payload[14],
                        frame.payload[15],
                        frame.payload[16],
                        frame.payload[17],
                        frame.payload[18],
                        frame.payload[19],
                    ]);
                    info!(
                        channel_id,
                        last_sequence_number,
                        new_submits_accepted_count,
                        new_shares_sum,
                        "SV2: SubmitSharesSuccess batch"
                    );
                    // SV2 acks by sequence range, not job_id — caller correlates
                    // by tracking submitted sequence numbers.
                    self.events.push(Sv2Event::ShareAccepted {
                        job_id: 0,
                        last_sequence_number,
                        new_submits_accepted_count,
                        new_shares_sum,
                    });
                } else if frame.payload.len() >= 8 {
                    // Backwards-compatible fallback: some pool implementations
                    // shipped only the first two fields. Report what we have
                    // and zero the rest so downstream rate accounting at least
                    // sees acceptance.
                    let channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let last_sequence_number = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    warn!(
                        channel_id,
                        last_sequence_number,
                        payload_bytes = frame.payload.len(),
                        "SV2: SubmitSharesSuccess truncated below spec — reporting last_sequence_number only"
                    );
                    self.events.push(Sv2Event::ShareAccepted {
                        job_id: 0,
                        last_sequence_number,
                        new_submits_accepted_count: 0,
                        new_shares_sum: 0,
                    });
                } else {
                    warn!(
                        "SV2: SubmitSharesSuccess payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            mining::SUBMIT_SHARES_ERROR => {
                self.record_message(
                    "recv",
                    mining::SUBMIT_SHARES_ERROR,
                    "SubmitSharesError",
                    frame.payload.len(),
                );
                // Payload: channel_id u32 + sequence_number u32 + error STR0_255
                let reason = if frame.payload.len() > 8 {
                    if let Ok((msg, _)) = decode_str(&frame.payload, 8) {
                        msg
                    } else {
                        String::from_utf8_lossy(&frame.payload[8..]).to_string()
                    }
                } else {
                    "unknown error".to_string()
                };
                warn!("SV2: share rejected: {}", reason);
                self.events.push(Sv2Event::ShareRejected {
                    job_id: 0, // SV2 identifies by sequence number, not job_id
                    reason,
                });
            }

            mining::SET_CUSTOM_MINING_JOB_SUCCESS => {
                self.record_message(
                    "recv",
                    mining::SET_CUSTOM_MINING_JOB_SUCCESS,
                    "SetCustomMiningJobSuccess",
                    frame.payload.len(),
                );
                if frame.payload.len() >= 12 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let request_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    let job_id = u32::from_le_bytes([
                        frame.payload[8],
                        frame.payload[9],
                        frame.payload[10],
                        frame.payload[11],
                    ]);
                    info!(
                        channel_id,
                        request_id, job_id, "SV2: custom mining job accepted"
                    );
                    self.events.push(Sv2Event::CustomJobAccepted {
                        channel_id,
                        request_id,
                        job_id,
                    });
                } else {
                    warn!(
                        "SV2: SetCustomMiningJobSuccess payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            mining::SET_CUSTOM_MINING_JOB_ERROR => {
                self.record_message(
                    "recv",
                    mining::SET_CUSTOM_MINING_JOB_ERROR,
                    "SetCustomMiningJobError",
                    frame.payload.len(),
                );
                if frame.payload.len() >= 9 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let request_id = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    let reason = decode_str(&frame.payload, 8)
                        .map(|(value, _)| value)
                        .unwrap_or_else(|_| {
                            String::from_utf8_lossy(&frame.payload[8..]).to_string()
                        });
                    warn!(
                        channel_id,
                        request_id,
                        reason = %reason,
                        "SV2: custom mining job rejected"
                    );
                    self.events.push(Sv2Event::CustomJobRejected {
                        channel_id,
                        request_id,
                        reason,
                    });
                } else {
                    warn!(
                        "SV2: SetCustomMiningJobError payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            // ------------------------------------------------------------------
            // Extranonce prefix
            // ------------------------------------------------------------------
            mining::SET_EXTRANONCE_PREFIX => {
                self.record_message(
                    "recv",
                    mining::SET_EXTRANONCE_PREFIX,
                    "SetExtranoncePrefix",
                    frame.payload.len(),
                );
                // Payload: channel_id u32 + extranonce_prefix (B0_32)
                if frame.payload.len() < 5 {
                    warn!(
                        "SV2: SetExtranoncePrefix payload too short ({})",
                        frame.payload.len()
                    );
                    return;
                }

                let channel_id = u32::from_le_bytes([
                    frame.payload[0],
                    frame.payload[1],
                    frame.payload[2],
                    frame.payload[3],
                ]);
                let prefix_len = frame.payload[4] as usize;
                if prefix_len > 32 {
                    warn!("SV2: SetExtranoncePrefix prefix too long ({})", prefix_len);
                    return;
                }
                let prefix_end = 5 + prefix_len;
                if frame.payload.len() < prefix_end {
                    warn!(
                        "SV2: SetExtranoncePrefix prefix truncated ({}/{})",
                        frame.payload.len(),
                        prefix_end
                    );
                    return;
                }

                let prefix = frame.payload[5..prefix_end].to_vec();
                if frame.payload.len() > prefix_end {
                    warn!(
                        "SV2: SetExtranoncePrefix has {} trailing bytes",
                        frame.payload.len() - prefix_end
                    );
                }
                if self.channel_id() == Some(channel_id) {
                    self.channel_extranonce_prefix = prefix.clone();
                }
                info!(channel_id, prefix_len, "SV2: extranonce prefix changed");
                self.events
                    .push(Sv2Event::ExtranoncePrefixChanged { channel_id, prefix });
            }

            // ------------------------------------------------------------------
            // Target / difficulty
            // ------------------------------------------------------------------
            mining::SET_TARGET => {
                self.record_message("recv", mining::SET_TARGET, "SetTarget", frame.payload.len());
                // Payload: channel_id u32 + max_target U256 (32 bytes LE)
                if frame.payload.len() >= 36 {
                    let channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    if !self.targets_current_channel_or_group(channel_id) {
                        warn!(
                            "SV2: ignoring SetTarget for channel/group {} (current={:?}, group={:?})",
                            channel_id,
                            self.channel_id(),
                            self.group_channel_id
                        );
                        return;
                    }
                    let mut target = [0u8; 32];
                    target.copy_from_slice(&frame.payload[4..36]);
                    let approx_diff = approximate_difficulty_from_target(&target);
                    info!("SV2: SetTarget approx_diff≈{}", approx_diff);
                    self.events.push(Sv2Event::DifficultyChanged(approx_diff));
                }
            }

            // ------------------------------------------------------------------
            // Pool-initiated reconnect
            // ------------------------------------------------------------------
            common::RECONNECT => {
                self.record_message("recv", common::RECONNECT, "Reconnect", frame.payload.len());
                // Payload: new_host STR0_255 + new_port u16 (optional)
                let (host, port) = if !frame.payload.is_empty() {
                    if let Ok((h, consumed)) = decode_str(&frame.payload, 0) {
                        let p = if frame.payload.len() >= consumed + 2 {
                            u16::from_le_bytes([
                                frame.payload[consumed],
                                frame.payload[consumed + 1],
                            ])
                        } else {
                            0
                        };
                        (h, p)
                    } else {
                        (String::new(), 0)
                    }
                } else {
                    (String::new(), 0)
                };
                info!("SV2: pool requested reconnect to {}:{}", host, port);
                self.events.push(Sv2Event::Reconnect { host, port });
            }

            // ------------------------------------------------------------------
            // ChannelEndpointChanged — pool tells us its downstream/upstream
            // for this channel now lives at a different endpoint. SV2 spec
            // payload is a single channel_id u32. The miner does not need to
            // act on this (the existing TCP connection stays valid), but we
            // record it in message-history and surface a structured log line
            // so operators can correlate topology changes with downstream
            // hashrate/share latency.
            // ------------------------------------------------------------------
            common::CHANNEL_ENDPOINT_CHANGED => {
                self.record_message(
                    "recv",
                    common::CHANNEL_ENDPOINT_CHANGED,
                    "ChannelEndpointChanged",
                    frame.payload.len(),
                );
                if frame.payload.len() < 4 {
                    warn!(
                        "SV2: ChannelEndpointChanged payload too short ({} bytes)",
                        frame.payload.len()
                    );
                    return;
                }
                let channel_id = u32::from_le_bytes([
                    frame.payload[0],
                    frame.payload[1],
                    frame.payload[2],
                    frame.payload[3],
                ]);
                if self.targets_current_channel_or_group(channel_id) {
                    info!(
                        channel_id,
                        current_channel_id = ?self.channel_id(),
                        group_channel_id = ?self.group_channel_id,
                        "SV2: pool reported endpoint change for our channel"
                    );
                } else {
                    debug!(
                        channel_id,
                        current_channel_id = ?self.channel_id(),
                        "SV2: ChannelEndpointChanged for unrelated channel"
                    );
                }
            }

            // ------------------------------------------------------------------
            // UpdateChannelError — pool refused a miner-initiated UpdateChannel.
            // DCENT_OS does not currently send UpdateChannel, so receiving this
            // is unexpected. Record it, log a warning if it concerns our
            // channel, and otherwise note-and-ignore. SV2 spec payload:
            //   channel_id u32 + error_code STR0_255
            // ------------------------------------------------------------------
            mining::UPDATE_CHANNEL_ERROR => {
                self.record_message(
                    "recv",
                    mining::UPDATE_CHANNEL_ERROR,
                    "UpdateChannelError",
                    frame.payload.len(),
                );
                if frame.payload.len() < 4 {
                    warn!(
                        "SV2: UpdateChannelError payload too short ({} bytes)",
                        frame.payload.len()
                    );
                    return;
                }
                let channel_id = u32::from_le_bytes([
                    frame.payload[0],
                    frame.payload[1],
                    frame.payload[2],
                    frame.payload[3],
                ]);
                let error_code = decode_str(&frame.payload, 4)
                    .map(|(s, _)| s)
                    .unwrap_or_else(|_| "<malformed>".to_string());
                if self.targets_current_channel_or_group(channel_id) {
                    warn!(
                        channel_id,
                        error_code = %error_code,
                        "SV2: pool returned UpdateChannelError for our channel — DCENT_OS does not send UpdateChannel, this is unexpected"
                    );
                } else {
                    debug!(
                        channel_id,
                        error_code = %error_code,
                        current_channel_id = ?self.channel_id(),
                        "SV2: UpdateChannelError for unrelated channel"
                    );
                }
            }

            // ------------------------------------------------------------------
            // Unknown / unhandled
            // ------------------------------------------------------------------
            other => {
                debug!(
                    "SV2: unhandled msg_type=0x{:02x} extension=0x{:04x} len={}",
                    other,
                    frame.header.extension_type,
                    frame.payload.len()
                );
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Message history
    // ---------------------------------------------------------------------------

    /// Record a protocol message in the ring buffer for protocol inspection.
    fn record_message(
        &mut self,
        direction: &'static str,
        msg_type: u8,
        msg_name: &str,
        payload_size: usize,
    ) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if self.message_history.len() >= MAX_MESSAGE_HISTORY {
            self.message_history.pop_front();
        }
        self.message_history
            .push_back(super::types::Sv2MessageRecord {
                direction,
                msg_type,
                msg_name: msg_name.to_string(),
                timestamp_ms: ts,
                payload_size,
            });
    }

    // ---------------------------------------------------------------------------
    // Accessors
    // ---------------------------------------------------------------------------

    /// Returns the channel ID if the channel is in Mining state, else None.
    pub fn channel_id(&self) -> Option<u32> {
        match self.state {
            ChannelState::Mining { channel_id } => Some(channel_id),
            _ => None,
        }
    }

    /// Returns true if the channel has an open mining channel and is receiving jobs.
    pub fn is_mining(&self) -> bool {
        matches!(self.state, ChannelState::Mining { .. })
    }

    /// Current share sequence number (last submitted).
    pub fn sequence_number(&self) -> u32 {
        self.sequence_number
    }

    pub fn channel_extranonce_prefix(&self) -> &[u8] {
        &self.channel_extranonce_prefix
    }

    pub fn channel_extranonce_size(&self) -> u16 {
        self.channel_extranonce_size
    }

    pub fn group_channel_id(&self) -> Option<u32> {
        self.group_channel_id
    }

    pub fn fixed_custom_job_extranonce(&self) -> Option<Vec<u8>> {
        if !self.work_selection_enabled {
            return None;
        }
        let len = usize::from(self.channel_extranonce_size);
        if len > 32 {
            return None;
        }
        Some(vec![0u8; len])
    }

    ///  strat-04: deterministic-but-distinct extranonce derivation.
    ///
    /// Returns an extranonce of `channel_extranonce_size` bytes derived from a
    /// `(channel_id, job_seq)` pair using a SipHash-style mixing function
    /// (here: `wrapping_mul` cascade — no crypto needed, just collision
    /// avoidance over the 16-bit work_id space). This is **opt-in**;
    /// `fixed_custom_job_extranonce` remains the default to preserve the
    /// shipped behavior. Once strat-04 has live extended-pool proof, the
    /// SV2 client should switch to this path.
    ///
    /// **Why deterministic and not random?** The miner submits shares with
    /// `(job_id, extranonce)`. If extranonce is random, we must persist the
    /// per-job mapping. With a deterministic derivation from the SAME inputs
    /// the pool already has (channel_id, job_id), we can re-derive at submit
    /// time without bookkeeping — and by including `job_seq` we still get
    /// no collisions across work cycles within a channel.
    pub fn derive_custom_job_extranonce(&self, channel_id: u32, job_seq: u64) -> Option<Vec<u8>> {
        if !self.work_selection_enabled {
            return None;
        }
        let len = usize::from(self.channel_extranonce_size);
        if len == 0 || len > 32 {
            return None;
        }
        // Cheap deterministic mixer; output bytes are spread across len.
        let seed = (channel_id as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(job_seq.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        let mut out = Vec::with_capacity(len);
        let mut state = seed;
        while out.len() < len {
            state ^= state >> 30;
            state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            state ^= state >> 27;
            state = state.wrapping_mul(0x94D0_49BB_1331_11EB);
            state ^= state >> 31;
            for b in state.to_le_bytes() {
                if out.len() < len {
                    out.push(b);
                }
            }
        }
        Some(out)
    }

    /// Number of pending (future) jobs buffered.
    pub fn pending_job_count(&self) -> usize {
        self.pending_jobs.len()
    }

    /// Returns a reference to the message history ring buffer.
    pub fn message_history(&self) -> &VecDeque<super::types::Sv2MessageRecord> {
        &self.message_history
    }

    /// Count of messages sent (from the history buffer).
    pub fn messages_sent(&self) -> u64 {
        self.message_history
            .iter()
            .filter(|r| r.direction == "sent")
            .count() as u64
    }

    /// Count of messages received (from the history buffer).
    pub fn messages_received(&self) -> u64 {
        self.message_history
            .iter()
            .filter(|r| r.direction == "recv")
            .count() as u64
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::framing::{Sv2Frame, FRAME_HEADER_SIZE};
    use super::*;
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Message construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn standard_channel_nonce_exhaustion_threshold() {
        // RE 2026-06-02 (mining-core-bible.md §2): ≥ 1 TH/s → Standard channel exhausts.
        assert_eq!(STANDARD_CHANNEL_NONCE_EXHAUSTION_HS, 1.0e12);
        assert!(!standard_channel_nonce_exhaustion_risk(0.0));
        assert!(!standard_channel_nonce_exhaustion_risk(999.0e9)); // 999 GH/s — under 1 TH/s
        assert!(standard_channel_nonce_exhaustion_risk(1.0e12)); // exactly 1 TH/s
        assert!(standard_channel_nonce_exhaustion_risk(13.5e12)); // S9 class
        assert!(standard_channel_nonce_exhaustion_risk(100.0e12)); // S21 class
    }

    #[test]
    fn test_setup_connection_message() {
        let mut channel = Sv2MiningChannel::new("test.worker", 500.0);
        let msg = channel.make_setup_connection();

        // Must be larger than the header alone
        assert!(msg.len() > FRAME_HEADER_SIZE);

        // State transitions to SettingUp
        assert_eq!(channel.state, ChannelState::SettingUp);

        // Parse the frame back out
        let (frame, consumed) = Sv2Frame::from_bytes(&msg).unwrap();
        assert_eq!(consumed, msg.len());
        assert_eq!(frame.header.msg_type, mining::SETUP_CONNECTION);
        assert_eq!(frame.header.extension_type, EXTENSION_TYPE_MINING);

        // Payload must include at least protocol(1) + min(2) + max(2) + flags(4) = 9 bytes
        // plus string-encoded fields
        assert!(frame.payload.len() >= 9);
    }

    #[test]
    fn test_open_channel_message() {
        let mut channel = Sv2MiningChannel::new("mypool.worker1", 1200.0);
        let msg = channel.make_open_channel();

        assert_eq!(channel.state, ChannelState::OpeningChannel);

        let (frame, _) = Sv2Frame::from_bytes(&msg).unwrap();
        assert_eq!(frame.header.msg_type, mining::OPEN_STANDARD_MINING_CHANNEL);
        assert_eq!(frame.header.extension_type, EXTENSION_TYPE_MINING);

        // request_id(4) + str(user_identity) + hashrate(4) + max_target(32)
        let worker = "mypool.worker1";
        let expected_min = 4 + 1 + worker.len() + 4 + 32;
        assert_eq!(frame.payload.len(), expected_min);

        // request_id starts at 1
        let req_id = u32::from_le_bytes([
            frame.payload[0],
            frame.payload[1],
            frame.payload[2],
            frame.payload[3],
        ]);
        assert_eq!(req_id, 1);
    }

    #[test]
    fn test_submit_share_message() {
        let mut channel = Sv2MiningChannel::new("test.worker", 500.0);
        let msg = channel.make_submit_share(1, 42, 0xDEAD_BEEF, 1_234_567_890, 0x2000_0000);

        let (frame, _) = Sv2Frame::from_bytes(&msg).unwrap();
        assert_eq!(frame.header.msg_type, mining::SUBMIT_SHARES_STANDARD);

        // 6 x u32 = 24 bytes
        assert_eq!(frame.payload.len(), 24);

        // Verify field values round-trip
        let channel_id = u32::from_le_bytes([
            frame.payload[0],
            frame.payload[1],
            frame.payload[2],
            frame.payload[3],
        ]);
        let seq_num = u32::from_le_bytes([
            frame.payload[4],
            frame.payload[5],
            frame.payload[6],
            frame.payload[7],
        ]);
        let job_id = u32::from_le_bytes([
            frame.payload[8],
            frame.payload[9],
            frame.payload[10],
            frame.payload[11],
        ]);
        let nonce = u32::from_le_bytes([
            frame.payload[12],
            frame.payload[13],
            frame.payload[14],
            frame.payload[15],
        ]);
        assert_eq!(channel_id, 1);
        assert_eq!(seq_num, 1); // first submission
        assert_eq!(job_id, 42);
        assert_eq!(nonce, 0xDEAD_BEEF);
    }

    #[test]
    fn test_submit_share_sequence_increments() {
        let mut channel = Sv2MiningChannel::new("test", 100.0);
        channel.make_submit_share(1, 1, 0, 0, 0);
        channel.make_submit_share(1, 2, 0, 0, 0);
        channel.make_submit_share(1, 3, 0, 0, 0);
        assert_eq!(channel.sequence_number(), 3);
    }

    #[test]
    fn test_submit_share_extended_message() {
        let mut channel = Sv2MiningChannel::new("test", 100.0);
        let msg = channel.make_submit_share_extended(1, 2, 3, 4, 5, &[0xaa, 0xbb]);
        let (frame, _) = Sv2Frame::from_bytes(&msg).unwrap();
        assert_eq!(frame.header.msg_type, mining::SUBMIT_SHARES_EXTENDED);
        assert_eq!(frame.payload.len(), 27);
        assert_eq!(frame.payload[24], 2);
        assert_eq!(&frame.payload[25..27], &[0xaa, 0xbb]);
    }

    // -----------------------------------------------------------------------
    // State machine tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_channel_reset() {
        let mut channel = Sv2MiningChannel::new("test", 100.0);
        channel.state = ChannelState::Mining { channel_id: 42 };
        channel.sequence_number = 100;
        channel.request_counter = 5;

        channel.reset();

        assert_eq!(channel.state, ChannelState::Disconnected);
        assert_eq!(channel.sequence_number, 0);
        assert_eq!(channel.request_counter, 0);
        assert_eq!(channel.pending_job_count(), 0);
        assert!(!channel.is_mining());
        assert!(channel.channel_id().is_none());
    }

    #[test]
    fn test_channel_id_only_in_mining_state() {
        let mut channel = Sv2MiningChannel::new("test", 100.0);

        assert!(channel.channel_id().is_none());

        channel.state = ChannelState::SettingUp;
        assert!(channel.channel_id().is_none());

        channel.state = ChannelState::Mining { channel_id: 7 };
        assert_eq!(channel.channel_id(), Some(7));
    }

    #[test]
    fn test_is_mining_flag() {
        let mut channel = Sv2MiningChannel::new("test", 100.0);
        assert!(!channel.is_mining());

        channel.state = ChannelState::OpeningChannel;
        assert!(!channel.is_mining());

        channel.state = ChannelState::Mining { channel_id: 1 };
        assert!(channel.is_mining());
    }

    // -----------------------------------------------------------------------
    // feed_data / frame handling tests
    // -----------------------------------------------------------------------

    /// Build a raw (unencrypted) SV2 frame for injection into feed_data.
    fn make_raw_frame(msg_type: u8, payload: Vec<u8>) -> Vec<u8> {
        Sv2Frame::new(EXTENSION_TYPE_MINING, msg_type, payload).to_bytes()
    }

    fn make_set_group_channel_payload(group_channel_id: u32, channel_ids: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&group_channel_id.to_le_bytes());
        payload.extend_from_slice(&(channel_ids.len() as u16).to_le_bytes());
        for channel_id in channel_ids {
            payload.extend_from_slice(&channel_id.to_le_bytes());
        }
        payload
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn feed_data_never_panics_on_arbitrary_raw_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..4096)
        ) {
            let mut channel = Sv2MiningChannel::new("prop.worker", 500.0);
            let _ = channel.feed_data(&data);
        }

        #[test]
        fn handle_frame_never_panics_on_bounded_payloads(
            msg_type in any::<u8>(),
            payload in proptest::collection::vec(any::<u8>(), 0..512)
        ) {
            let mut channel = Sv2MiningChannel::new("prop.worker", 500.0);
            let frame = make_raw_frame(msg_type, payload);
            let _ = channel.feed_data(&frame);
        }
    }

    #[test]
    fn mock_v2_pool_transcript_covers_extended_channel_jobs_and_share_results() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.set_endpoint("v2.pool.example.com", 3336);
        channel.enable_work_selection();

        let setup = channel.make_setup_connection();
        let (setup_frame, _) = Sv2Frame::from_bytes(&setup).unwrap();
        assert_eq!(setup_frame.header.msg_type, mining::SETUP_CONNECTION);
        assert_eq!(channel.state, ChannelState::SettingUp);

        let events = channel.feed_data(&make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]));
        assert!(matches!(events.as_slice(), [Sv2Event::Connected]));

        let open = channel.make_open_channel();
        let (open_frame, _) = Sv2Frame::from_bytes(&open).unwrap();
        assert_eq!(
            open_frame.header.msg_type,
            mining::OPEN_EXTENDED_MINING_CHANNEL
        );
        assert_eq!(channel.state, ChannelState::OpeningChannel);

        let mut open_success = Vec::new();
        open_success.extend_from_slice(&1u32.to_le_bytes());
        open_success.extend_from_slice(&77u32.to_le_bytes());
        open_success.extend_from_slice(&[0xff; 32]);
        open_success.extend_from_slice(&2u16.to_le_bytes());
        open_success.push(3);
        open_success.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        open_success.extend_from_slice(&9u32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(
            mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
            open_success,
        ));
        assert!(matches!(
            events.as_slice(),
            [Sv2Event::DifficultyChanged(_)]
        ));
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
        assert_eq!(channel.channel_extranonce_size(), 2);
        assert_eq!(channel.channel_extranonce_prefix(), &[0xaa, 0xbb, 0xcc]);
        assert_eq!(channel.group_channel_id(), Some(9));

        let mut target = Vec::new();
        target.extend_from_slice(&77u32.to_le_bytes());
        target.extend_from_slice(&[0x7f; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_TARGET, target));
        assert!(matches!(
            events.as_slice(),
            [Sv2Event::DifficultyChanged(_)]
        ));

        let mut job = Vec::new();
        job.extend_from_slice(&77u32.to_le_bytes());
        job.extend_from_slice(&100u32.to_le_bytes());
        job.push(0x01);
        job.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job.extend_from_slice(&[0x42; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job));
        assert!(events.is_empty(), "future job waits for SetNewPrevHash");
        assert_eq!(channel.pending_job_count(), 1);

        let mut prev_hash = Vec::new();
        prev_hash.extend_from_slice(&77u32.to_le_bytes());
        prev_hash.extend_from_slice(&100u32.to_le_bytes());
        prev_hash.extend_from_slice(&[0x24; 32]);
        prev_hash.extend_from_slice(&1_700_001_000u32.to_le_bytes());
        prev_hash.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, prev_hash));
        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob {
            job_id,
            version,
            prev_hash,
            merkle_root,
            nbits,
            ntime,
            clean_jobs,
        } = &events[0]
        {
            assert_eq!(*job_id, 100);
            assert_eq!(*version, 0x2000_0000);
            assert_eq!(prev_hash, &[0x24; 32]);
            assert_eq!(merkle_root, &[0x42; 32]);
            assert_eq!(*nbits, 0x1903a30c);
            assert_eq!(*ntime, 1_700_001_000);
            assert!(*clean_jobs);
        } else {
            panic!("expected NewJob from mock SV2 transcript");
        }

        let submit = channel.make_submit_share_extended(
            77,
            100,
            0xdead_beef,
            1_700_001_010,
            0x2000_0000,
            &[0, 0],
        );
        let (submit_frame, _) = Sv2Frame::from_bytes(&submit).unwrap();
        assert_eq!(submit_frame.header.msg_type, mining::SUBMIT_SHARES_EXTENDED);

        // Truncated SubmitSharesSuccess (only channel_id+last_seq) — exercises
        // the backwards-compatible 8-byte fallback path in the parser.
        let mut accepted = Vec::new();
        accepted.extend_from_slice(&77u32.to_le_bytes());
        accepted.extend_from_slice(&1u32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(mining::SUBMIT_SHARES_SUCCESS, accepted));
        assert_eq!(events.len(), 1);
        match &events[0] {
            Sv2Event::ShareAccepted {
                job_id,
                last_sequence_number,
                new_submits_accepted_count,
                new_shares_sum,
            } => {
                assert_eq!(*job_id, 0);
                assert_eq!(*last_sequence_number, 1);
                // Truncated payload — both count fields are zeroed.
                assert_eq!(*new_submits_accepted_count, 0);
                assert_eq!(*new_shares_sum, 0);
            }
            other => panic!("expected ShareAccepted, got {:?}", other),
        }
    }

    #[test]
    fn mock_v2_pool_transcript_covers_rejects_reconnect_and_malformed_jobs() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let events = channel.feed_data(&make_raw_frame(mining::NEW_MINING_JOB, vec![0u8; 12]));
        assert!(events.is_empty(), "short NewMiningJob must not emit work");
        assert_eq!(channel.pending_job_count(), 0);

        let events = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, vec![0u8; 20]));
        assert!(
            events.is_empty(),
            "short SetNewPrevHash must not activate stale work"
        );
        assert!(channel.current_prev_hash.is_none());

        let mut rejected = Vec::new();
        rejected.extend_from_slice(&77u32.to_le_bytes());
        rejected.extend_from_slice(&2u32.to_le_bytes());
        encode_str(&mut rejected, "duplicate-share");
        let events = channel.feed_data(&make_raw_frame(mining::SUBMIT_SHARES_ERROR, rejected));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Sv2Event::ShareRejected { reason, .. } if reason == "duplicate-share"
        ));

        let mut reconnect = Vec::new();
        encode_str(&mut reconnect, "backup.v2.pool.example.com");
        reconnect.extend_from_slice(&34255u16.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(common::RECONNECT, reconnect));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Sv2Event::Reconnect { host, port }
                if host == "backup.v2.pool.example.com" && *port == 34255
        ));

        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|record| record.msg_name.as_str())
            .collect();
        assert!(names.contains(&"NewMiningJob"));
        assert!(names.contains(&"SetNewPrevHash"));
        assert!(names.contains(&"SubmitSharesError"));
        assert!(names.contains(&"Reconnect"));
    }

    #[test]
    fn test_feed_data_setup_connection_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        // SetupConnectionSuccess has no required payload in the basic form
        let data = make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::Connected));
    }

    #[test]
    fn test_feed_data_setup_connection_error() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        // Build a minimal error payload: code + message (both STR0_255)
        let mut payload = Vec::new();
        encode_str(&mut payload, "unsupported-feature");
        encode_str(&mut payload, "Server does not support version rolling");
        let data = make_raw_frame(mining::SETUP_CONNECTION_ERROR, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::Disconnected(_)));
        assert!(matches!(channel.state, ChannelState::Error(_)));
    }

    #[test]
    fn test_feed_data_open_channel_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&99u32.to_le_bytes()); // channel_id
                                                         // Remaining fields (target etc.) not strictly needed for our parser
        let data = make_raw_frame(mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS, payload);
        let events = channel.feed_data(&data);

        // No event emitted by open success (caller detects via is_mining())
        assert_eq!(events.len(), 0);
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 99 });
        assert!(channel.is_mining());
        assert_eq!(channel.channel_id(), Some(99));
    }

    #[test]
    fn test_feed_data_open_extended_channel_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.enable_work_selection();
        channel.state = ChannelState::OpeningChannel;

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&77u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&[0xff; 32]); // target
        payload.extend_from_slice(&2u16.to_le_bytes()); // extranonce_size
        payload.push(3); // prefix len
        payload.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        payload.extend_from_slice(&9u32.to_le_bytes()); // group_channel_id
        let data = make_raw_frame(mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
        assert_eq!(channel.channel_extranonce_size(), 2);
        assert_eq!(channel.channel_extranonce_prefix(), &[0xaa, 0xbb, 0xcc]);
        assert_eq!(channel.group_channel_id(), Some(9));
        assert_eq!(channel.fixed_custom_job_extranonce(), Some(vec![0, 0]));
    }

    #[test]
    fn derive_custom_job_extranonce_yields_distinct_bytes_per_job() {
        //  strat-04: deterministic per-(channel_id, job_seq) mixer
        // must produce DISTINCT extranonces across job sequence numbers
        // for the same channel. Pin against accidental zero-fill regression.
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.work_selection_enabled = true;
        channel.channel_extranonce_size = 4; // 4 bytes

        let extranonces: Vec<Vec<u8>> = (0u64..256)
            .map(|seq| {
                channel
                    .derive_custom_job_extranonce(0x1234_5678, seq)
                    .unwrap()
            })
            .collect();

        // All 256 must be unique within this batch.
        let mut sorted = extranonces.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 256, "extranonce collisions in 256 jobs");

        // None of them is zero-filled (would defeat the purpose).
        let zero = vec![0u8; 4];
        assert!(extranonces.iter().all(|e| e != &zero));

        // Length matches channel_extranonce_size.
        assert!(extranonces.iter().all(|e| e.len() == 4));
    }

    #[test]
    fn derive_custom_job_extranonce_is_deterministic() {
        // Same (channel_id, job_seq) → same extranonce. Critical so the
        // miner can re-derive at submit time without bookkeeping.
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.work_selection_enabled = true;
        channel.channel_extranonce_size = 8;

        let a = channel.derive_custom_job_extranonce(42, 7);
        let b = channel.derive_custom_job_extranonce(42, 7);
        assert_eq!(a, b);

        // Different inputs → different output.
        let c = channel.derive_custom_job_extranonce(43, 7);
        assert_ne!(a, c);
        let d = channel.derive_custom_job_extranonce(42, 8);
        assert_ne!(a, d);
    }

    #[test]
    fn derive_custom_job_extranonce_respects_work_selection_gate() {
        // Without work_selection_enabled the helper must return None,
        // matching the existing fixed_custom_job_extranonce contract.
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.work_selection_enabled = false;
        channel.channel_extranonce_size = 4;
        assert_eq!(channel.derive_custom_job_extranonce(1, 1), None);
    }

    #[test]
    fn derive_custom_job_extranonce_rejects_oversized_size() {
        // SV2 extranonce_size is u16 but our miner cap is 32 bytes.
        // Anything bigger returns None to avoid 1KB+ allocations from a
        // misbehaving pool that advertises an absurd extranonce_size.
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.work_selection_enabled = true;
        channel.channel_extranonce_size = 33;
        assert_eq!(channel.derive_custom_job_extranonce(1, 1), None);

        // Zero-size also fails (no work to do).
        channel.channel_extranonce_size = 0;
        assert_eq!(channel.derive_custom_job_extranonce(1, 1), None);
    }

    #[test]
    fn test_feed_data_open_standard_channel_success_stores_group_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&99u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&[0xff; 32]); // target
        payload.push(2); // prefix len
        payload.extend_from_slice(&[0x11, 0x22]);
        payload.extend_from_slice(&44u32.to_le_bytes()); // group_channel_id
        let data = make_raw_frame(mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 99 });
        assert_eq!(channel.channel_extranonce_size(), 0);
        assert_eq!(channel.channel_extranonce_prefix(), &[0x11, 0x22]);
        assert_eq!(channel.group_channel_id(), Some(44));
    }

    #[test]
    fn test_feed_data_set_group_channel_assigns_current_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(9);

        let payload = make_set_group_channel_payload(42, &[77, 88]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_GROUP_CHANNEL, payload));

        assert_eq!(events.len(), 1);
        assert_eq!(channel.group_channel_id(), Some(42));
        assert!(matches!(
            &events[0],
            Sv2Event::GroupChannelAssigned {
                group_channel_id: 42,
                channel_ids
            } if channel_ids.as_slice() == &[77, 88]
        ));
    }

    #[test]
    fn test_feed_data_set_group_channel_ignores_truncated_payload() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(9);

        let mut payload = Vec::new();
        payload.extend_from_slice(&42u32.to_le_bytes());
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.extend_from_slice(&77u32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(mining::SET_GROUP_CHANNEL, payload));

        assert!(events.is_empty());
        assert_eq!(channel.group_channel_id(), Some(9));
    }

    #[test]
    fn test_feed_data_set_extranonce_prefix_updates_current_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.channel_extranonce_prefix = vec![0xaa];

        let mut payload = Vec::new();
        payload.extend_from_slice(&77u32.to_le_bytes());
        payload.push(2);
        payload.extend_from_slice(&[0x11, 0x22]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_EXTRANONCE_PREFIX, payload));

        assert_eq!(events.len(), 1);
        assert_eq!(channel.channel_extranonce_prefix(), &[0x11, 0x22]);
        assert!(matches!(
            &events[0],
            Sv2Event::ExtranoncePrefixChanged { channel_id: 77, prefix }
                if prefix.as_slice() == &[0x11, 0x22]
        ));
    }

    #[test]
    fn test_feed_data_set_extranonce_prefix_ignores_other_channel_state() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.channel_extranonce_prefix = vec![0xaa];

        let mut payload = Vec::new();
        payload.extend_from_slice(&88u32.to_le_bytes());
        payload.push(1);
        payload.push(0x22);
        let events = channel.feed_data(&make_raw_frame(mining::SET_EXTRANONCE_PREFIX, payload));

        assert_eq!(events.len(), 1);
        assert_eq!(channel.channel_extranonce_prefix(), &[0xaa]);
        assert!(matches!(
            &events[0],
            Sv2Event::ExtranoncePrefixChanged { channel_id: 88, prefix }
                if prefix.as_slice() == &[0x22]
        ));
    }

    #[test]
    fn test_feed_data_set_extranonce_prefix_rejects_malformed_payload() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.channel_extranonce_prefix = vec![0xaa];

        let mut too_long = Vec::new();
        too_long.extend_from_slice(&77u32.to_le_bytes());
        too_long.push(33);
        too_long.extend_from_slice(&[0x11; 33]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_EXTRANONCE_PREFIX, too_long));
        assert!(events.is_empty());
        assert_eq!(channel.channel_extranonce_prefix(), &[0xaa]);

        let mut truncated = Vec::new();
        truncated.extend_from_slice(&77u32.to_le_bytes());
        truncated.push(2);
        truncated.push(0x11);
        let events = channel.feed_data(&make_raw_frame(mining::SET_EXTRANONCE_PREFIX, truncated));
        assert!(events.is_empty());
        assert_eq!(channel.channel_extranonce_prefix(), &[0xaa]);
    }

    #[test]
    fn test_feed_data_set_target_accepts_assigned_group_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(42);

        let mut payload = Vec::new();
        payload.extend_from_slice(&42u32.to_le_bytes());
        payload.extend_from_slice(&[0x7f; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_TARGET, payload));

        assert!(matches!(
            events.as_slice(),
            [Sv2Event::DifficultyChanged(_)]
        ));
    }

    #[test]
    fn test_feed_data_set_target_ignores_unrelated_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(42);

        let mut payload = Vec::new();
        payload.extend_from_slice(&88u32.to_le_bytes());
        payload.extend_from_slice(&[0x7f; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_TARGET, payload));

        assert!(events.is_empty());
    }

    #[test]
    fn test_feed_data_set_new_prev_hash_accepts_assigned_group_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(42);

        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&77u32.to_le_bytes());
        job_payload.extend_from_slice(&5u32.to_le_bytes());
        job_payload.push(0x01);
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_payload.extend_from_slice(&[0xcc; 32]);
        assert!(channel
            .feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job_payload))
            .is_empty());

        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&42u32.to_le_bytes());
        ph_payload.extend_from_slice(&5u32.to_le_bytes());
        ph_payload.extend_from_slice(&[0xdd; 32]);
        ph_payload.extend_from_slice(&1_700_001_000u32.to_le_bytes());
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload));

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Sv2Event::NewJob { job_id: 5, .. }));
    }

    #[test]
    fn test_feed_data_new_mining_job_ignores_unrelated_channel() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&88u32.to_le_bytes());
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.push(0x01);
        payload.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        payload.extend_from_slice(&[0xcc; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::NEW_MINING_JOB, payload));

        assert!(events.is_empty());
        assert_eq!(channel.pending_job_count(), 0);
    }

    #[test]
    fn test_feed_data_new_mining_job_with_prevhash() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Inject a prev_hash so non-future jobs are emitted immediately
        channel.current_prev_hash = Some(SetNewPrevHash {
            channel_id: 1,
            job_id: 10,
            prev_hash: [0xAA; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1a03a30c,
        });

        // Build a non-future NewMiningJob
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&42u32.to_le_bytes()); // job_id
        payload.push(0x00); // future_job = false
        payload.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        payload.extend_from_slice(&[0xBB; 32]); // merkle_root

        let data = make_raw_frame(mining::NEW_MINING_JOB, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob {
            job_id,
            nbits,
            prev_hash,
            clean_jobs,
            ..
        } = &events[0]
        {
            assert_eq!(*job_id, 42);
            assert_eq!(*nbits, 0x1a03a30c);
            assert_eq!(prev_hash, &[0xAAu8; 32]);
            assert!(*clean_jobs);
        } else {
            panic!("Expected NewJob event");
        }
        assert_eq!(channel.pending_job_count(), 1);
    }

    #[test]
    fn test_feed_data_future_job_then_prevhash() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Future job arrives first — no event yet
        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_payload.extend_from_slice(&5u32.to_le_bytes()); // job_id
        job_payload.push(0x01); // future_job = true
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_payload.extend_from_slice(&[0xCC; 32]); // merkle_root

        let job_data = make_raw_frame(mining::NEW_MINING_JOB, job_payload);
        let events = channel.feed_data(&job_data);
        assert_eq!(events.len(), 0, "Future job must not emit event");
        assert_eq!(channel.pending_job_count(), 1);

        // Now SetNewPrevHash arrives referencing that job — should emit event
        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_payload.extend_from_slice(&5u32.to_le_bytes()); // job_id (matches)
        ph_payload.extend_from_slice(&[0xDD; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_001_000u32.to_le_bytes()); // min_ntime
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes()); // nbits

        let ph_data = make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload);
        let events = channel.feed_data(&ph_data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob {
            job_id, prev_hash, ..
        } = &events[0]
        {
            assert_eq!(*job_id, 5);
            assert_eq!(prev_hash, &[0xDDu8; 32]);
        } else {
            panic!("Expected NewJob event after SetNewPrevHash");
        }
    }

    #[test]
    fn test_feed_data_submit_shares_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&3u32.to_le_bytes()); // last_seq
        let data = make_raw_frame(mining::SUBMIT_SHARES_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::ShareAccepted { .. }));
    }

    #[test]
    fn submit_shares_success_full_spec_payload_carries_count_and_sum() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&12345u32.to_le_bytes()); // last_seq
        payload.extend_from_slice(&3u32.to_le_bytes()); // new_submits_accepted_count
        payload.extend_from_slice(&98_765_432_100u64.to_le_bytes()); // new_shares_sum
        let events = channel.feed_data(&make_raw_frame(mining::SUBMIT_SHARES_SUCCESS, payload));

        assert_eq!(events.len(), 1);
        match &events[0] {
            Sv2Event::ShareAccepted {
                job_id,
                last_sequence_number,
                new_submits_accepted_count,
                new_shares_sum,
            } => {
                assert_eq!(*job_id, 0);
                assert_eq!(*last_sequence_number, 12345);
                assert_eq!(*new_submits_accepted_count, 3);
                assert_eq!(*new_shares_sum, 98_765_432_100);
            }
            other => panic!("expected ShareAccepted with full payload, got {:?}", other),
        }
    }

    #[test]
    fn submit_shares_success_zero_count_payload_emits_zeroed_event() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Spec-compliant 20-byte payload but reporting zero accepted shares.
        // Real pools shouldn't send this in a SUCCESS message but we must not
        // panic, must not silently drop the event, and must not invent counts.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&7u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes()); // count = 0
        payload.extend_from_slice(&0u64.to_le_bytes()); // sum = 0
        let events = channel.feed_data(&make_raw_frame(mining::SUBMIT_SHARES_SUCCESS, payload));

        assert_eq!(events.len(), 1);
        match &events[0] {
            Sv2Event::ShareAccepted {
                last_sequence_number,
                new_submits_accepted_count,
                new_shares_sum,
                ..
            } => {
                assert_eq!(*last_sequence_number, 7);
                assert_eq!(*new_submits_accepted_count, 0);
                assert_eq!(*new_shares_sum, 0);
            }
            other => panic!("expected zero-count ShareAccepted, got {:?}", other),
        }
    }

    #[test]
    fn submit_shares_success_too_short_payload_does_not_emit() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Only 4 bytes — below even the 8-byte legacy fallback threshold.
        let events =
            channel.feed_data(&make_raw_frame(mining::SUBMIT_SHARES_SUCCESS, vec![0u8; 4]));
        assert!(events.is_empty());
    }

    #[test]
    fn test_feed_data_submit_shares_error() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&2u32.to_le_bytes()); // seq
        encode_str(&mut payload, "difficulty-too-low");
        let data = make_raw_frame(mining::SUBMIT_SHARES_ERROR, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::ShareRejected { reason, .. } = &events[0] {
            assert!(reason.contains("difficulty-too-low"));
        } else {
            panic!("Expected ShareRejected");
        }
    }

    #[test]
    fn test_feed_data_custom_job_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&44u32.to_le_bytes());
        let data = make_raw_frame(mining::SET_CUSTOM_MINING_JOB_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            Sv2Event::CustomJobAccepted {
                channel_id: 1,
                request_id: 8,
                job_id: 44
            }
        ));
    }

    #[test]
    fn test_feed_data_streaming_partial_frame() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        let data = make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]);

        // Feed first half — no events yet
        let mid = data.len() / 2;
        let events = channel.feed_data(&data[..mid]);
        assert_eq!(events.len(), 0);

        // Feed second half — should now emit Connected
        let events = channel.feed_data(&data[mid..]);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::Connected));
    }

    #[test]
    fn test_feed_data_multiple_frames_in_one_call() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        // Two success frames concatenated
        let frame1 = make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]);
        let frame2 = make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]);
        let mut combined = frame1;
        combined.extend(frame2);

        let events = channel.feed_data(&combined);
        assert_eq!(events.len(), 2, "Both frames should produce events");
    }

    // -----------------------------------------------------------------------
    // Serialization round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_mining_job_roundtrip() {
        let original = NewMiningJob {
            channel_id: 7,
            job_id: 99,
            future_job: true,
            version: 0x2000_0000,
            merkle_root: [0x42; 32],
        };

        // Serialize manually (same layout as from_bytes expects)
        let mut buf = Vec::new();
        buf.extend_from_slice(&original.channel_id.to_le_bytes());
        buf.extend_from_slice(&original.job_id.to_le_bytes());
        buf.push(if original.future_job { 1 } else { 0 });
        buf.extend_from_slice(&original.version.to_le_bytes());
        buf.extend_from_slice(&original.merkle_root);

        let parsed = NewMiningJob::from_bytes(&buf).unwrap();
        assert_eq!(parsed.channel_id, original.channel_id);
        assert_eq!(parsed.job_id, original.job_id);
        assert_eq!(parsed.future_job, original.future_job);
        assert_eq!(parsed.version, original.version);
        assert_eq!(parsed.merkle_root, original.merkle_root);
    }

    #[test]
    fn test_set_new_prev_hash_roundtrip() {
        let original = SetNewPrevHash {
            channel_id: 3,
            job_id: 55,
            prev_hash: [0xFF; 32],
            min_ntime: 1_700_123_456,
            nbits: 0x1a012345,
        };

        let mut buf = Vec::new();
        buf.extend_from_slice(&original.channel_id.to_le_bytes());
        buf.extend_from_slice(&original.job_id.to_le_bytes());
        buf.extend_from_slice(&original.prev_hash);
        buf.extend_from_slice(&original.min_ntime.to_le_bytes());
        buf.extend_from_slice(&original.nbits.to_le_bytes());

        let parsed = SetNewPrevHash::from_bytes(&buf).unwrap();
        assert_eq!(parsed.channel_id, original.channel_id);
        assert_eq!(parsed.job_id, original.job_id);
        assert_eq!(parsed.prev_hash, original.prev_hash);
        assert_eq!(parsed.min_ntime, original.min_ntime);
        assert_eq!(parsed.nbits, original.nbits);
    }

    #[test]
    fn test_new_mining_job_too_short() {
        let result = NewMiningJob::from_bytes(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_new_prev_hash_too_short() {
        let result = SetNewPrevHash::from_bytes(&[0u8; 20]);
        assert!(result.is_err());
    }

    #[test]
    fn test_submit_shares_standard_size() {
        let msg = SubmitSharesStandard {
            channel_id: 1,
            sequence_number: 42,
            job_id: 7,
            nonce: 0xDEAD_BEEF,
            ntime: 1_700_000_000,
            version: 0x2000_0000,
        };
        assert_eq!(msg.to_bytes().len(), 24);
    }

    #[test]
    fn test_encode_decode_str() {
        let mut buf = Vec::new();
        encode_str(&mut buf, "hello");
        let (decoded, consumed) = decode_str(&buf, 0).unwrap();
        assert_eq!(decoded, "hello");
        assert_eq!(consumed, 6); // 1 len byte + 5 chars

        // Empty string
        let mut buf2 = Vec::new();
        encode_str(&mut buf2, "");
        let (decoded2, consumed2) = decode_str(&buf2, 0).unwrap();
        assert_eq!(decoded2, "");
        assert_eq!(consumed2, 1);
    }

    #[test]
    fn test_encode_str_truncates_at_255() {
        let long = "a".repeat(300);
        let mut buf = Vec::new();
        encode_str(&mut buf, &long);
        assert_eq!(buf[0], 255); // length clamped
        assert_eq!(buf.len(), 256); // 1 + 255
    }

    // -----------------------------------------------------------------------
    // NewExtendedMiningJob (msg_type 0x1f) — strat-04 extended mining
    // -----------------------------------------------------------------------

    /// Build a NewExtendedMiningJob payload with the SRI/older-spec wire layout
    /// matching `NewExtendedMiningJob::from_bytes`.
    fn make_extended_job_payload(
        channel_id: u32,
        job_id: u32,
        future_job: bool,
        version: u32,
        version_rolling_allowed: bool,
        merkle_path: &[[u8; 32]],
        coinbase_tx_prefix: &[u8],
        coinbase_tx_suffix: &[u8],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&channel_id.to_le_bytes());
        buf.extend_from_slice(&job_id.to_le_bytes());
        buf.push(if future_job { 1 } else { 0 });
        buf.extend_from_slice(&version.to_le_bytes());
        buf.push(if version_rolling_allowed { 1 } else { 0 });
        buf.push(merkle_path.len() as u8);
        for hash in merkle_path {
            buf.extend_from_slice(hash);
        }
        buf.extend_from_slice(&(coinbase_tx_prefix.len() as u16).to_le_bytes());
        buf.extend_from_slice(coinbase_tx_prefix);
        buf.extend_from_slice(&(coinbase_tx_suffix.len() as u16).to_le_bytes());
        buf.extend_from_slice(coinbase_tx_suffix);
        buf
    }

    #[test]
    fn extended_mining_job_parses_full_payload() {
        let merkle = [[0xaa; 32], [0xbb; 32]];
        let prefix = b"\x01\x00\x00\x00\xff\xff\xff\xff".to_vec();
        let suffix = b"\xff\xff\xff\xff\x00\x00\x00\x00".to_vec();
        let payload =
            make_extended_job_payload(42, 7, true, 0x2000_0000, true, &merkle, &prefix, &suffix);
        let job = NewExtendedMiningJob::from_bytes(&payload).unwrap();
        assert_eq!(job.channel_id, 42);
        assert_eq!(job.job_id, 7);
        assert!(job.future_job);
        assert_eq!(job.version, 0x2000_0000);
        assert!(job.version_rolling_allowed);
        assert_eq!(job.merkle_path.len(), 2);
        assert_eq!(job.merkle_path[0], [0xaa; 32]);
        assert_eq!(job.merkle_path[1], [0xbb; 32]);
        assert_eq!(job.coinbase_tx_prefix, prefix);
        assert_eq!(job.coinbase_tx_suffix, suffix);
    }

    #[test]
    fn extended_mining_job_rejects_short_header() {
        // Less than 19 bytes: missing length fields.
        let result = NewExtendedMiningJob::from_bytes(&[0u8; 15]);
        assert!(result.is_err());
    }

    #[test]
    fn extended_mining_job_rejects_truncated_merkle_path() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        buf.extend_from_slice(&2u32.to_le_bytes()); // job_id
        buf.push(0); // future_job=false
        buf.extend_from_slice(&0u32.to_le_bytes()); // version
        buf.push(0); // version_rolling_allowed=false
        buf.push(3); // merkle_path_count = 3
        buf.extend_from_slice(&[0u8; 32]); // only 1 hash present
        buf.extend_from_slice(&0u16.to_le_bytes()); // prefix len
        buf.extend_from_slice(&0u16.to_le_bytes()); // suffix len
        let result = NewExtendedMiningJob::from_bytes(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn extended_mining_job_rejects_truncated_coinbase_prefix() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.push(0);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.push(0);
        buf.push(0); // no merkle hashes
        buf.extend_from_slice(&8u16.to_le_bytes()); // prefix len = 8
        buf.extend_from_slice(&[0u8; 4]); // only 4 bytes present
        let result = NewExtendedMiningJob::from_bytes(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn extended_mining_job_rejects_truncated_coinbase_suffix() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.push(0);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.push(0);
        buf.push(0); // no merkle hashes
        buf.extend_from_slice(&0u16.to_le_bytes()); // prefix len = 0
        buf.extend_from_slice(&5u16.to_le_bytes()); // suffix len = 5
        buf.extend_from_slice(&[0u8; 2]); // only 2 bytes present
        let result = NewExtendedMiningJob::from_bytes(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn extended_mining_job_future_job_waits_for_set_new_prev_hash() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.enable_work_selection();
        channel.state = ChannelState::Mining { channel_id: 77 };

        let merkle = [[0x11; 32]];
        let payload = make_extended_job_payload(
            77,
            55,
            true,
            0x2000_4000,
            true,
            &merkle,
            b"\xaa\xbb",
            b"\xcc\xdd",
        );
        let events = channel.feed_data(&make_raw_frame(mining::NEW_EXTENDED_MINING_JOB, payload));
        assert!(
            events.is_empty(),
            "future extended job waits for SetNewPrevHash"
        );

        let mut prev = Vec::new();
        prev.extend_from_slice(&77u32.to_le_bytes());
        prev.extend_from_slice(&55u32.to_le_bytes());
        prev.extend_from_slice(&[0x42; 32]);
        prev.extend_from_slice(&1_700_002_000u32.to_le_bytes());
        prev.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, prev));
        assert_eq!(events.len(), 1);
        match &events[0] {
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
                assert_eq!(*job_id, 55);
                assert_eq!(*version, 0x2000_4000);
                assert!(*version_rolling_allowed);
                assert_eq!(prev_hash, &[0x42; 32]);
                assert_eq!(*nbits, 0x1903a30c);
                assert_eq!(*ntime, 1_700_002_000);
                assert_eq!(merkle_path.len(), 1);
                assert_eq!(merkle_path[0], [0x11; 32]);
                assert_eq!(coinbase_tx_prefix.as_slice(), b"\xaa\xbb");
                assert_eq!(coinbase_tx_suffix.as_slice(), b"\xcc\xdd");
                assert!(*clean_jobs);
            }
            other => panic!("expected NewExtendedJob event, got {:?}", other),
        }
    }

    #[test]
    fn extended_mining_job_immediate_uses_stored_prev_hash() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.enable_work_selection();
        channel.state = ChannelState::Mining { channel_id: 77 };

        // Pre-populate prev_hash so the immediate job can fire right away.
        channel.current_prev_hash = Some(SetNewPrevHash {
            channel_id: 77,
            job_id: 0,
            prev_hash: [0x99; 32],
            min_ntime: 1_700_005_000,
            nbits: 0x1903abcd,
        });

        let payload =
            make_extended_job_payload(77, 81, false, 0x2000_8000, false, &[], b"\x01", b"\x02");
        let events = channel.feed_data(&make_raw_frame(mining::NEW_EXTENDED_MINING_JOB, payload));
        assert_eq!(events.len(), 1);
        match &events[0] {
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
                assert_eq!(*job_id, 81);
                assert_eq!(*version, 0x2000_8000);
                assert!(!*version_rolling_allowed);
                assert_eq!(prev_hash, &[0x99; 32]);
                assert_eq!(*nbits, 0x1903abcd);
                assert_eq!(*ntime, 1_700_005_000);
                assert!(merkle_path.is_empty());
                assert_eq!(coinbase_tx_prefix.as_slice(), b"\x01");
                assert_eq!(coinbase_tx_suffix.as_slice(), b"\x02");
                assert!(*clean_jobs);
            }
            other => panic!("expected NewExtendedJob event, got {:?}", other),
        }
    }

    #[test]
    fn extended_mining_job_for_unrelated_channel_is_ignored() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.enable_work_selection();
        channel.state = ChannelState::Mining { channel_id: 77 };

        let payload = make_extended_job_payload(
            999, // unrelated channel id
            12,
            false,
            0,
            false,
            &[],
            &[],
            &[],
        );
        let events = channel.feed_data(&make_raw_frame(mining::NEW_EXTENDED_MINING_JOB, payload));
        assert!(events.is_empty());
        // Pending list must not have absorbed it either.
        assert_eq!(channel.pending_extended_jobs.len(), 0);
    }

    #[test]
    fn mock_v2_pool_transcript_covers_proper_extended_channel_message_flow() {
        // End-to-end transcript proving NEW_EXTENDED_MINING_JOB (0x1f) — the
        // correct extended-channel job message — drives the channel state
        // machine through Connected → channel-open → SetTarget → future job
        // → SetNewPrevHash → emitted Sv2Event::NewExtendedJob whose fields
        // pipe straight into `extended_job_to_job_template` to derive a
        // deterministic merkle root and JobTemplate. Existing
        // `mock_v2_pool_transcript_covers_extended_channel_jobs_and_share_results`
        // exercises NEW_MINING_JOB (0x15) on an extended channel which is
        // technically a spec deviation; this test closes that gap.
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.set_endpoint("v2.pool.example.com", 3336);
        channel.enable_work_selection();

        // Step 1: Setup
        let _setup = channel.make_setup_connection();
        let events = channel.feed_data(&make_raw_frame(mining::SETUP_CONNECTION_SUCCESS, vec![]));
        assert!(matches!(events.as_slice(), [Sv2Event::Connected]));

        // Step 2: Open extended mining channel
        let _open = channel.make_open_channel();
        let mut open_success = Vec::new();
        open_success.extend_from_slice(&1u32.to_le_bytes()); // request_id
        open_success.extend_from_slice(&77u32.to_le_bytes()); // channel_id
        open_success.extend_from_slice(&[0xff; 32]); // target
        open_success.extend_from_slice(&4u16.to_le_bytes()); // extranonce_size = 4
        open_success.push(2); // prefix len = 2
        open_success.extend_from_slice(&[0xab, 0xcd]); // extranonce_prefix
        open_success.extend_from_slice(&9u32.to_le_bytes()); // group_channel_id
        let events = channel.feed_data(&make_raw_frame(
            mining::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
            open_success,
        ));
        assert!(matches!(
            events.as_slice(),
            [Sv2Event::DifficultyChanged(_)]
        ));
        assert_eq!(channel.channel_extranonce_size(), 4);
        assert_eq!(channel.channel_extranonce_prefix(), &[0xab, 0xcd]);
        assert_eq!(channel.channel_id(), Some(77));

        // Step 3: SetTarget tightens difficulty
        let mut set_target = Vec::new();
        set_target.extend_from_slice(&77u32.to_le_bytes());
        set_target.extend_from_slice(&[0x7f; 32]);
        let events = channel.feed_data(&make_raw_frame(mining::SET_TARGET, set_target));
        assert!(matches!(
            events.as_slice(),
            [Sv2Event::DifficultyChanged(_)]
        ));

        // Step 4: NEW_EXTENDED_MINING_JOB (future_job=true) — must wait for prevhash
        let merkle_path = [[0x55u8; 32], [0x66u8; 32]];
        let coinbase_prefix = b"\x02\x00\x00\x00\x01\x00\x00\x00\x00\x00".to_vec();
        let coinbase_suffix = b"\xff\xff\xff\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        let extended_job_payload = make_extended_job_payload(
            77,
            500,
            true, // future_job
            0x2000_4000,
            true, // version_rolling_allowed
            &merkle_path,
            &coinbase_prefix,
            &coinbase_suffix,
        );
        let events = channel.feed_data(&make_raw_frame(
            mining::NEW_EXTENDED_MINING_JOB,
            extended_job_payload,
        ));
        assert!(
            events.is_empty(),
            "future extended job must wait for SetNewPrevHash"
        );
        assert_eq!(channel.pending_extended_jobs.len(), 1);

        // Step 5: SetNewPrevHash matches the buffered extended job
        let mut prev = Vec::new();
        prev.extend_from_slice(&77u32.to_le_bytes());
        prev.extend_from_slice(&500u32.to_le_bytes());
        prev.extend_from_slice(&[0x33; 32]); // prev_hash
        prev.extend_from_slice(&1_700_009_000u32.to_le_bytes()); // min_ntime
        prev.extend_from_slice(&0x1903_a30cu32.to_le_bytes()); // nbits
        let events = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, prev));
        assert_eq!(events.len(), 1);

        // Step 6: feed event into adapter to derive a JobTemplate, then verify
        // the merkle root against an independent hand-rolled computation.
        let prev_hash;
        let nbits;
        let ntime;
        let merkle_path_bytes;
        let prefix_bytes;
        let suffix_bytes;
        match &events[0] {
            Sv2Event::NewExtendedJob {
                job_id,
                version,
                version_rolling_allowed,
                prev_hash: ph,
                nbits: nb,
                ntime: nt,
                merkle_path: mp,
                coinbase_tx_prefix: pfx,
                coinbase_tx_suffix: sfx,
                clean_jobs,
            } => {
                assert_eq!(*job_id, 500);
                assert_eq!(*version, 0x2000_4000);
                assert!(*version_rolling_allowed);
                assert_eq!(ph, &[0x33; 32]);
                assert_eq!(*nb, 0x1903_a30c);
                assert_eq!(*nt, 1_700_009_000);
                assert_eq!(mp.len(), 2);
                assert_eq!(mp[0], [0x55u8; 32]);
                assert_eq!(mp[1], [0x66u8; 32]);
                assert_eq!(pfx, &coinbase_prefix);
                assert_eq!(sfx, &coinbase_suffix);
                assert!(*clean_jobs);
                prev_hash = *ph;
                nbits = *nb;
                ntime = *nt;
                merkle_path_bytes = mp.clone();
                prefix_bytes = pfx.clone();
                suffix_bytes = sfx.clone();
            }
            other => panic!("expected NewExtendedJob event, got {:?}", other),
        }

        // Step 7: Run the event payload through the production adapter and
        // independently recompute the merkle root using the same SHA-256d
        // walk to prove the two produce identical output.
        let extranonce = vec![0u8; usize::from(channel.channel_extranonce_size())];
        let extranonce_prefix = channel.channel_extranonce_prefix().to_vec();
        let template = crate::v2::adapter::extended_job_to_job_template(
            crate::v2::adapter::ExtendedJobAssembly {
                job_id: 500,
                version: 0x2000_4000,
                version_rolling_allowed: true,
                prev_hash,
                nbits,
                ntime,
                coinbase_tx_prefix: &prefix_bytes,
                coinbase_tx_suffix: &suffix_bytes,
                merkle_path: &merkle_path_bytes,
                extranonce_prefix: &extranonce_prefix,
                extranonce: &extranonce,
                version_mask: 0x1fff_e000,
                share_target: [0x7f; 32],
            },
        );

        // Manual independent merkle computation
        let mut coinbase = Vec::new();
        coinbase.extend_from_slice(&prefix_bytes);
        coinbase.extend_from_slice(&extranonce_prefix);
        coinbase.extend_from_slice(&extranonce);
        coinbase.extend_from_slice(&suffix_bytes);
        let mut hash = crate::work::double_sha256(&coinbase);
        for branch in &merkle_path_bytes {
            let mut combined = [0u8; 64];
            combined[..32].copy_from_slice(&hash);
            combined[32..].copy_from_slice(branch);
            hash = crate::work::double_sha256(&combined);
        }
        assert_eq!(template.merkle_root, hash);
        assert_eq!(template.job_id, "500");
        assert_eq!(template.prev_block_hash, [0x33; 32]);
        assert_eq!(template.nbits, 0x1903_a30c);
        assert_eq!(template.ntime, 1_700_009_000);
        assert_eq!(template.extranonce1, vec![0xab, 0xcd]);
        assert_eq!(template.version_mask, 0x1fff_e000);
        assert!(template.coinbase1.is_empty());
        assert!(template.coinbase2.is_empty());
        assert!(template.merkle_branches.is_empty());

        // Step 8: Confirm the message-history ring buffer recorded the proper
        // wire-level message names we exercised, not the standard-channel ones.
        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"OpenExtendedMiningChannel"));
        assert!(names.contains(&"OpenExtendedMiningChannelSuccess"));
        assert!(names.contains(&"NewExtendedMiningJob"));
        assert!(names.contains(&"SetNewPrevHash"));
        assert!(
            !names.contains(&"NewMiningJob"),
            "extended-channel transcript must not exercise NewMiningJob (0x15)"
        );
    }

    // -----------------------------------------------------------------------
    // OpenMiningChannelError + CloseChannel handlers
    // -----------------------------------------------------------------------

    #[test]
    fn open_mining_channel_error_unblocks_client_with_disconnect() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        let mut payload = Vec::new();
        payload.extend_from_slice(&7u32.to_le_bytes()); // request_id
        encode_str(&mut payload, "low-hashrate"); // error_code
        let events = channel.feed_data(&make_raw_frame(mining::OPEN_MINING_CHANNEL_ERROR, payload));

        assert_eq!(events.len(), 1);
        match &events[0] {
            Sv2Event::Disconnected(reason) => {
                assert!(reason.contains("low-hashrate"), "got: {}", reason);
                assert!(reason.contains("request_id=7"), "got: {}", reason);
            }
            other => panic!("expected Disconnected, got {:?}", other),
        }
        assert!(matches!(channel.state, ChannelState::Error(_)));

        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"OpenMiningChannelError"));
    }

    #[test]
    fn open_mining_channel_error_short_payload_yields_descriptive_disconnect() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        let events = channel.feed_data(&make_raw_frame(
            mining::OPEN_MINING_CHANNEL_ERROR,
            vec![0u8; 2],
        ));
        assert_eq!(events.len(), 1);
        if let Sv2Event::Disconnected(reason) = &events[0] {
            assert!(reason.to_lowercase().contains("too short"));
        } else {
            panic!("expected Disconnected for short payload");
        }
        assert!(matches!(channel.state, ChannelState::Error(_)));
    }

    #[test]
    fn close_channel_for_current_channel_emits_disconnect() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&77u32.to_le_bytes());
        encode_str(&mut payload, "operator-disabled");
        let events = channel.feed_data(&make_raw_frame(mining::CLOSE_CHANNEL, payload));

        assert_eq!(events.len(), 1);
        match &events[0] {
            Sv2Event::Disconnected(reason) => {
                assert!(reason.contains("operator-disabled"), "got: {}", reason);
                assert!(reason.contains("channel_id=77"), "got: {}", reason);
            }
            other => panic!("expected Disconnected, got {:?}", other),
        }
        assert!(matches!(channel.state, ChannelState::Error(_)));
    }

    #[test]
    fn close_channel_for_group_channel_emits_disconnect() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(9);

        let mut payload = Vec::new();
        payload.extend_from_slice(&9u32.to_le_bytes()); // group channel id
        encode_str(&mut payload, "group-shutdown");
        let events = channel.feed_data(&make_raw_frame(mining::CLOSE_CHANNEL, payload));

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::Disconnected(_)));
    }

    #[test]
    fn close_channel_for_unrelated_channel_does_not_disrupt_session() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&999u32.to_le_bytes()); // unrelated
        encode_str(&mut payload, "other-channel");
        let events = channel.feed_data(&make_raw_frame(mining::CLOSE_CHANNEL, payload));

        assert!(events.is_empty(), "got: {:?}", events);
        // State must NOT have transitioned to Error.
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
        // History still records that we received the message.
        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"CloseChannel"));
    }

    // -----------------------------------------------------------------------
    // ChannelEndpointChanged + UpdateChannelError handlers
    // -----------------------------------------------------------------------

    #[test]
    fn channel_endpoint_changed_for_current_channel_is_logged_and_recorded() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&77u32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(common::CHANNEL_ENDPOINT_CHANGED, payload));

        // ChannelEndpointChanged is informational — must NOT emit an event
        // and must NOT change state.
        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });

        // Message history must record the wire-level message name.
        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"ChannelEndpointChanged"));
    }

    #[test]
    fn channel_endpoint_changed_for_group_channel_is_logged() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };
        channel.group_channel_id = Some(9);

        let mut payload = Vec::new();
        payload.extend_from_slice(&9u32.to_le_bytes()); // group channel id
        let events = channel.feed_data(&make_raw_frame(common::CHANNEL_ENDPOINT_CHANGED, payload));

        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
    }

    #[test]
    fn channel_endpoint_changed_for_unrelated_channel_does_not_disrupt() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&999u32.to_le_bytes());
        let events = channel.feed_data(&make_raw_frame(common::CHANNEL_ENDPOINT_CHANGED, payload));

        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
    }

    #[test]
    fn channel_endpoint_changed_short_payload_does_not_panic() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let events = channel.feed_data(&make_raw_frame(
            common::CHANNEL_ENDPOINT_CHANGED,
            vec![0u8; 2],
        ));
        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
        // History still records the recv attempt for diagnostics.
        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"ChannelEndpointChanged"));
    }

    #[test]
    fn update_channel_error_for_current_channel_records_history_without_disrupting() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&77u32.to_le_bytes());
        encode_str(&mut payload, "invalid-nominal-hashrate");
        let events = channel.feed_data(&make_raw_frame(mining::UPDATE_CHANNEL_ERROR, payload));

        // DCENT_OS doesn't send UpdateChannel, so receiving this is unexpected
        // but should not tear down the session — the pool's state is intact.
        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });

        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"UpdateChannelError"));
    }

    #[test]
    fn update_channel_error_for_unrelated_channel_does_not_disrupt() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let mut payload = Vec::new();
        payload.extend_from_slice(&999u32.to_le_bytes());
        encode_str(&mut payload, "wrong-channel");
        let events = channel.feed_data(&make_raw_frame(mining::UPDATE_CHANNEL_ERROR, payload));

        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
    }

    #[test]
    fn update_channel_error_short_payload_does_not_panic() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let events = channel.feed_data(&make_raw_frame(mining::UPDATE_CHANNEL_ERROR, vec![0u8; 2]));
        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
    }

    #[test]
    fn close_channel_short_payload_does_not_panic_or_disrupt() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 77 };

        let events = channel.feed_data(&make_raw_frame(mining::CLOSE_CHANNEL, vec![0u8; 2]));
        assert!(events.is_empty());
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 77 });
    }

    #[test]
    fn extended_mining_job_short_payload_does_not_panic_or_store() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.enable_work_selection();
        channel.state = ChannelState::Mining { channel_id: 77 };

        // 12 bytes — less than the 19-byte minimum.
        let events = channel.feed_data(&make_raw_frame(
            mining::NEW_EXTENDED_MINING_JOB,
            vec![0u8; 12],
        ));
        assert!(events.is_empty());
        assert_eq!(channel.pending_extended_jobs.len(), 0);
        // Message history should still record the recv attempt for debug purposes.
        let names: Vec<&str> = channel
            .message_history()
            .iter()
            .map(|r| r.msg_name.as_str())
            .collect();
        assert!(names.contains(&"NewExtendedMiningJob"));
    }
}
