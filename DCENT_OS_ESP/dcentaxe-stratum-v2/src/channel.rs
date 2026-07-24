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

use crate::framing::{FrameDecoder, Sv2Frame};
use crate::noise::NoiseSession;
use crate::types::*;

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
        target: [u8; 32],
        clean_jobs: bool,
    },
    /// Pool difficulty changed
    DifficultyChanged(f64),
    /// Share accepted by pool
    ShareAccepted {
        sequence_number: u32,
        accepted_count: u32,
    },
    /// Share rejected by pool
    ShareRejected {
        sequence_number: u32,
        reason: String,
    },
    /// Connection established (SetupConnection accepted)
    Connected,
    /// Connection lost
    Disconnected(String),
    /// Pool requested reconnect
    Reconnect { host: String, port: u16 },
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
    /// Endpoint host advertised in SetupConnection.
    endpoint_host: String,
    /// Endpoint port advertised in SetupConnection.
    endpoint_port: u16,
    /// Nominal hashrate in H/s (derived from GH/s at construction)
    nominal_hashrate: f32,
    /// Jobs received with `min_ntime=None`, waiting for SetNewPrevHash.
    pending_jobs: Vec<(u32, NewMiningJob)>,
    /// The most recently active prev_hash state
    current_prev_hash: Option<SetNewPrevHash>,
    /// MED-1: set when a SetNewPrevHash tip change was BUFFERED with no matching
    /// job yet (out-of-order delivery, or the legacy job_id==0 case). The tip
    /// genuinely moved but we emitted NOTHING — so the dispatcher (which flushes
    /// in-flight ASIC work only on `clean_jobs=true`) has NOT been told to drop
    /// the old-tip work. We carry that pending "tip moved" signal here and emit
    /// `clean_jobs=true` on the FIRST NewMiningJob that pairs with this buffered
    /// prev_hash, then clear it so subsequent incremental jobs under the same
    /// (now-announced) tip revert to SV2-4's `clean_jobs=false`.
    prev_hash_unannounced: bool,
    /// Current channel target in big-endian comparison order.
    current_target: [u8; 32],
    /// Accumulated events from the current `feed_data()` call
    events: Vec<Sv2Event>,
    /// Optional pinned SV2 pool authority public key (x-only, 32 bytes).
    /// `Some` ⇒ the Noise session verifies the server certificate fail-closed;
    /// `None` ⇒ TOFU (default). Stored here so it survives `reset()`, which
    /// rebuilds the `NoiseSession` (which itself always starts as `None`).
    pool_authority_key: Option<[u8; 32]>,
}

impl Sv2MiningChannel {
    /// Create a new channel. `hashrate_ghs` is the device's nominal hashrate
    /// in GH/s (e.g. 500.0 for a BM1366-based BitAxe Ultra).
    pub fn new(worker: &str, hashrate_ghs: f32) -> Self {
        Self::new_with_endpoint(worker, hashrate_ghs, "v2.stratum.braiins.com", 3336)
    }

    pub fn new_with_endpoint(
        worker: &str,
        hashrate_ghs: f32,
        endpoint_host: &str,
        endpoint_port: u16,
    ) -> Self {
        // Default: no pinned authority key ⇒ TOFU (unchanged behavior).
        Self::new_with_endpoint_and_authority(
            worker,
            hashrate_ghs,
            endpoint_host,
            endpoint_port,
            None,
        )
    }

    /// Like [`Self::new_with_endpoint`] but pins an optional SV2 pool authority
    /// public key (x-only, 32 bytes). When `Some`, the Noise handshake verifies
    /// the server's certificate fail-closed (BIP340 Schnorr) instead of TOFU.
    ///
    /// The key is stored on the channel so it is re-applied after every
    /// [`Self::reset`] (which rebuilds the `NoiseSession`); a constructor-only
    /// pin would silently revert to TOFU on the first reconnect.
    pub fn new_with_endpoint_and_authority(
        worker: &str,
        hashrate_ghs: f32,
        endpoint_host: &str,
        endpoint_port: u16,
        pool_authority_key: Option<[u8; 32]>,
    ) -> Self {
        let mut noise = NoiseSession::new();
        noise.set_pool_authority_key(pool_authority_key);
        Self {
            state: ChannelState::Disconnected,
            noise,
            decoder: FrameDecoder::new(),
            request_counter: 0,
            sequence_number: 0,
            worker_identity: worker.to_string(),
            endpoint_host: endpoint_host.to_string(),
            endpoint_port,
            nominal_hashrate: hashrate_ghs * 1e9,
            pending_jobs: Vec::new(),
            current_prev_hash: None,
            prev_hash_unannounced: false,
            // ES-5: the initial/pre-open target is the TIGHTEST (all-zeros), not the
            // loosest. Before the pool assigns a real target (OpenChannelSuccess /
            // SetTarget), no local hash can meet an all-zero target, so the channel
            // is FAIL-CLOSED (submits nothing) instead of mining a pre-open job at
            // ~diff 2^-32 and flooding the pool. `target_be_to_difficulty([0;32])`
            // is already handled as +inf (see its unit test).
            current_target: [0x00; 32],
            events: Vec::new(),
            pool_authority_key,
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
        // CRITICAL: NoiseSession::new() resets the authority key to None. Re-pin
        // the channel's stored key so a reconnect keeps fail-closed certificate
        // verification instead of silently reverting to TOFU.
        self.noise.set_pool_authority_key(self.pool_authority_key);
        self.decoder.reset();
        self.request_counter = 0;
        self.sequence_number = 0;
        self.pending_jobs.clear();
        self.current_prev_hash = None;
        self.prev_hash_unannounced = false;
        // ES-5: reset to the tightest (fail-closed) target — a reconnected channel
        // must not mine/submit until the pool re-assigns a real target.
        self.current_target = [0x00; 32];
        self.events.clear();
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
        let msg = SetupConnection {
            protocol: 0, // 0 = Mining Protocol (SV2 spec §5.1)
            min_version: 2,
            max_version: 2,
            flags: REQUIRES_STANDARD_JOBS | REQUIRES_VERSION_ROLLING,
            endpoint_host: self.endpoint_host.clone(),
            endpoint_port: self.endpoint_port,
            vendor: "D-Central Technologies".into(),
            hardware_version: "BitAxe".into(),
            firmware: "DCENT_axe".into(),
            device_id: String::new(),
        };
        let payload = msg.to_bytes();
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
        let msg = OpenStandardMiningChannel {
            request_id: self.request_counter,
            user_identity: self.worker_identity.clone(),
            nominal_hash_rate: self.nominal_hashrate,
            max_target: [0xFF; 32], // Accept any difficulty pool assigns
        };
        let payload = msg.to_bytes();
        let frame = Sv2Frame::new(
            EXTENSION_TYPE_MINING,
            mining::OPEN_STANDARD_MINING_CHANNEL,
            payload,
        );
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
        let frame = Sv2Frame::new(
            EXTENSION_TYPE_CHANNEL_MSG,
            mining::SUBMIT_SHARES_STANDARD,
            payload,
        );
        frame.to_bytes()
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
                    log::error!("{}", msg);
                    self.state = ChannelState::Error(msg.clone());
                    self.events.push(Sv2Event::Disconnected(msg));
                    break;
                }
            }
        }

        std::mem::take(&mut self.events)
    }

    /// Validate the SV2 frame scope (channel_msg bit + extension id) of an inbound
    /// frame. Returns `Some(warning)` for an unexpected combination, `None` if the
    /// scope is acceptable.
    ///
    /// SV2 header semantics: the high bit (0x8000) of `extension_type` is the
    /// `channel_msg` flag; the low 15 bits are the extension id. We register no SV2
    /// extensions, so any inbound frame must carry extension id 0 (mining base
    /// protocol). The `channel_msg` bit itself is informational and is NOT a reason
    /// to reject — over-strict rejection could break interop with conformant pools
    /// that set bits we do not model. This is detection/observability only.
    fn validate_frame_scope(ext: u16, msg_type: u8) -> Option<String> {
        let ext_id = ext & 0x7FFF; // strip the channel_msg flag (high bit)
        if ext_id != 0 {
            return Some(format!(
                "SV2: inbound frame with unsupported extension id 0x{:04x} \
                 (msg_type=0x{:02x}); we register no extensions — dispatching by msg_type",
                ext_id, msg_type
            ));
        }
        None
    }

    /// Heuristic: does a decoded SetupConnectionError reason indicate a
    /// flags/feature (version-rolling) mismatch? Pure + case-insensitive so it is
    /// host-testable and does not depend on a specific pool's exact wording.
    fn is_flags_mismatch_error(reason: &str) -> bool {
        let r = reason.to_ascii_lowercase();
        r.contains("version-rolling")
            || r.contains("version rolling")
            || r.contains("unsupported-feature")
            || r.contains("flags")
    }

    /// Dispatch a single decoded frame to the appropriate handler.
    fn handle_frame(&mut self, frame: Sv2Frame) {
        // SV2-11: detect (but do not drop) frames carrying an unexpected extension
        // scope. We dispatch purely on msg_type below — this only adds a warning so
        // a mis-scoped/wrong-extension frame is observable rather than silent.
        if let Some(warning) =
            Self::validate_frame_scope(frame.header.extension_type, frame.header.msg_type)
        {
            log::warn!("{}", warning);
        }

        match frame.header.msg_type {
            // ------------------------------------------------------------------
            // SetupConnection responses
            // ------------------------------------------------------------------
            mining::SETUP_CONNECTION_SUCCESS => {
                log::info!("SV2: SetupConnection accepted (pool ready)");
                // Emit Connected so the caller knows to send OpenStandardMiningChannel
                self.events.push(Sv2Event::Connected);
            }

            mining::SETUP_CONNECTION_ERROR => {
                // Payload (SV2 spec): flags U32 + error_code STR0_255. The
                // earlier decode read TWO STR0_255 from offset 0, so against a
                // conformant pool the first flags byte was taken as a length
                // (flags=0 => "": ""), the real error_code was never extracted,
                // and the SV2-9 flags-mismatch decoration below went dead. Skip
                // the 4 flags bytes and take the human-readable error_code.
                let reason = if frame.payload.len() >= 4 {
                    match decode_str(&frame.payload, 4) {
                        Ok((code, _)) if !code.is_empty() => code,
                        _ => "SetupConnection rejected (no reason given)".to_string(),
                    }
                } else {
                    "SetupConnection rejected (no reason given)".to_string()
                };
                // If the pool rejected because of a flags/feature mismatch (e.g. it
                // does not support version rolling on a standard channel), surface a
                // distinct, actionable reason naming the flags we advertised, so the
                // operator/log sees a concrete incompatibility instead of a generic
                // "rejected". A graceful reconnect-with-reduced-flags retry lives in
                // the TCP-driving layer (out of this module's scope).
                let reason = if Self::is_flags_mismatch_error(&reason) {
                    format!(
                        "{} — pool rejected our advertised flags \
                         (REQUIRES_STANDARD_JOBS|REQUIRES_VERSION_ROLLING); \
                         this pool does not support version rolling on a standard channel",
                        reason
                    )
                } else {
                    reason
                };
                log::error!("SV2: SetupConnection rejected: {}", reason);
                self.state = ChannelState::Error(reason.clone());
                self.events.push(Sv2Event::Disconnected(reason));
            }

            // ------------------------------------------------------------------
            // OpenStandardMiningChannel responses
            // ------------------------------------------------------------------
            mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS => {
                // Payload: request_id u32 + channel_id u32 + target [u8;32] + ...
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
                    match sv2_target_le_to_be(&frame.payload[8..40]) {
                        Ok(target) => {
                            self.current_target = target;
                            let diff = target_be_to_difficulty(&self.current_target);
                            self.events.push(Sv2Event::DifficultyChanged(diff));
                        }
                        Err(e) => log::warn!("SV2: invalid open-channel target: {}", e),
                    }
                    log::info!("SV2: mining channel {} opened", channel_id);
                    self.state = ChannelState::Mining { channel_id };
                } else {
                    // Fail-closed: a truncated Success is unusable (no channel_id
                    // / target). Warn-only left the state in OpeningChannel with
                    // no event, wedging the miner exactly like a missing 0x12
                    // handler did. Surface it so the caller can reconnect/retry.
                    let reason = format!(
                        "OpenStandardMiningChannelSuccess payload too short ({} < 40)",
                        frame.payload.len()
                    );
                    log::error!("SV2: {}", reason);
                    self.state = ChannelState::Error(reason.clone());
                    self.events.push(Sv2Event::Disconnected(reason));
                }
            }

            mining::OPEN_MINING_CHANNEL_ERROR => {
                // Payload: request_id U32 + error_code STR0_255. Without this arm
                // a pool rejection of OpenStandardMiningChannel (unknown user
                // identity, hashrate=0, unsupported flags, …) fell through to the
                // catch-all debug log: NO event was emitted, the state stayed in
                // `OpeningChannel` forever, and the TCP-driving `poll` never
                // retried — the miner idled indefinitely with the failure
                // unsurfaced. Surface it like SetupConnection.Error so the caller
                // can reconnect/retry.
                let reason = if frame.payload.len() >= 4 {
                    match decode_str(&frame.payload, 4) {
                        Ok((code, _)) if !code.is_empty() => {
                            format!("pool rejected OpenStandardMiningChannel: {}", code)
                        }
                        _ => {
                            "pool rejected OpenStandardMiningChannel (no reason given)".to_string()
                        }
                    }
                } else {
                    "pool rejected OpenStandardMiningChannel (malformed error)".to_string()
                };
                log::error!("SV2: {}", reason);
                self.state = ChannelState::Error(reason.clone());
                self.events.push(Sv2Event::Disconnected(reason));
            }

            // ------------------------------------------------------------------
            // Job delivery
            // ------------------------------------------------------------------
            mining::NEW_MINING_JOB => {
                match NewMiningJob::from_bytes(&frame.payload) {
                    Ok(job) => {
                        log::debug!(
                            "SV2: NewMiningJob job_id={} future={}",
                            job.job_id,
                            job.is_future()
                        );

                        if let Some(min_ntime) = job.min_ntime {
                            // Immediately usable — emit if we have a prev_hash
                            if let Some(ref ph) = self.current_prev_hash {
                                // MED-1: if a tip change was BUFFERED with no matching job
                                // (SetNewPrevHash NO-MATCH else-branch set
                                // `prev_hash_unannounced`), this is the FIRST job pairing
                                // with that new tip. The tip genuinely moved but nothing was
                                // emitted then, so the dispatcher never flushed the OLD-tip
                                // work. Emit clean_jobs=true for THIS ONE pairing to restore
                                // the tip flush, then clear the flag so subsequent
                                // incremental jobs under this (now-announced) tip revert to
                                // SV2-4's clean_jobs=false.
                                //
                                // Otherwise (the common case): an immediately-usable
                                // NewMiningJob pairs with the ALREADY-ACTIVE prev_hash, so
                                // the tip did NOT move — this only ADDS work under the
                                // existing tip. Signal clean_jobs=false so the dispatcher
                                // does not needlessly flush in-flight ASIC work (SV2's
                                // incremental-job design). A genuine tip change otherwise
                                // comes via SetNewPrevHash, which emits clean_jobs=true.
                                let clean_jobs = self.prev_hash_unannounced;
                                self.prev_hash_unannounced = false;
                                self.events.push(Sv2Event::NewJob {
                                    job_id: job.job_id,
                                    version: job.version,
                                    prev_hash: ph.prev_hash,
                                    merkle_root: job.merkle_root,
                                    nbits: ph.nbits,
                                    ntime: min_ntime,
                                    target: self.current_target,
                                    clean_jobs,
                                });
                            }
                        } else if let Some(ref ph) = self.current_prev_hash {
                            // MED-1 (out-of-order recovery): a FUTURE job (min_ntime=None)
                            // that arrives AFTER the SetNewPrevHash referencing it. The
                            // SET_NEW_PREV_HASH no-match branch buffered this tip with
                            // `prev_hash_unannounced=true` and promised "will match on next
                            // NewMiningJob" — but the immediately-usable path above only fires
                            // for min_ntime=Some, and a SetNewPrevHash-referenced job is by
                            // definition a future job (min_ntime=None). Without this arm the
                            // referenced job silently never activates: the dispatcher keeps
                            // grinding the stale tip (every share stale-rejected) until some
                            // later message trips recovery. Pair it NOW, exactly as the matched
                            // SetNewPrevHash path would have — emit clean_jobs=true to flush the
                            // stale tip, then clear the flag so a re-sent copy does not re-emit.
                            if self.prev_hash_unannounced && ph.job_id == job.job_id {
                                self.prev_hash_unannounced = false;
                                self.events.push(Sv2Event::NewJob {
                                    job_id: job.job_id,
                                    version: job.version,
                                    prev_hash: ph.prev_hash,
                                    merkle_root: job.merkle_root,
                                    nbits: ph.nbits,
                                    ntime: ph.min_ntime,
                                    target: self.current_target,
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
                    Err(e) => log::warn!("SV2: failed to parse NewMiningJob: {}", e),
                }
            }

            mining::SET_NEW_PREV_HASH => {
                match SetNewPrevHash::from_bytes(&frame.payload) {
                    Ok(ph) => {
                        log::info!(
                            "SV2: SetNewPrevHash for job_id={} nbits=0x{:08x}",
                            ph.job_id,
                            ph.nbits
                        );

                        // Match the referenced job STRICTLY by job_id for ALL job_ids.
                        // Per the SV2 standard-channel spec, SetNewPrevHash references a
                        // specific (future) job_id; there is no `job_id==0 == latest`
                        // wildcard. The old "use the latest job" fallback could pair an
                        // arbitrary job's merkle_root with this new prev_hash → a header
                        // the pool never activated → rejected shares. On no match, the
                        // else-branch buffers `current_prev_hash` so a subsequent
                        // NewMiningJob under this prev_hash gets paired via the
                        // NEW_MINING_JOB `min_ntime` path.
                        let matched_job = self
                            .pending_jobs
                            .iter()
                            .find(|(id, _)| *id == ph.job_id)
                            .map(|(_, job)| job.clone());
                        let matched = matched_job.is_some();

                        if let Some(job) = matched_job {
                            // clean_jobs semantics: SetNewPrevHash is a genuine tip change
                            // (new prev_hash) — the dispatcher must flush prior work. The
                            // tip is announced HERE (clean_jobs=true), so any prior buffered
                            // "tip moved but unannounced" signal is now satisfied — clear it
                            // so the next incremental NewMiningJob is correctly non-clean.
                            self.prev_hash_unannounced = false;
                            self.events.push(Sv2Event::NewJob {
                                job_id: job.job_id,
                                version: job.version,
                                prev_hash: ph.prev_hash,
                                merkle_root: job.merkle_root,
                                nbits: ph.nbits,
                                ntime: ph.min_ntime,
                                target: self.current_target,
                                clean_jobs: true,
                            });
                        } else {
                            log::warn!(
                                "SV2: SetNewPrevHash job_id={} — no matching job yet (will match on next NewMiningJob)",
                                ph.job_id
                            );
                            // MED-1: a genuine tip change arrived (new prev_hash) but no job
                            // pairs with it yet, so we emit NOTHING here. The dispatcher
                            // flushes in-flight ASIC work ONLY on clean_jobs=true and has no
                            // independent prev_hash-change detection — so without this flag
                            // the old-tip work would keep grinding until something emits
                            // clean_jobs=true (which an SV2-4 incremental NewMiningJob never
                            // does). Mark the tip as UNANNOUNCED so the first NewMiningJob
                            // that pairs with this buffered prev_hash emits clean_jobs=true.
                            self.prev_hash_unannounced = true;
                        }
                        // Drop stale jobs. Bounded by the 16-job ring in NEW_MINING_JOB.
                        //
                        // LOW-1: on the NO-MATCH branch the awaited (future) job for THIS
                        // tip has not arrived yet, so an aggressive `retain(id >= job_id)`
                        // could prune jobs that a still-pending tip may reference if a later
                        // SetNewPrevHash with a HIGHER job_id arrives first — discarding the
                        // very job we are waiting to pair. Only prune the matched-tip case,
                        // where the referenced job is present and lower-id jobs are provably
                        // stale. The 16-job ring keeps the no-match path bounded regardless.
                        if ph.job_id > 0 && matched {
                            self.pending_jobs.retain(|(id, _)| *id >= ph.job_id);
                        }
                        self.current_prev_hash = Some(ph);
                    }
                    Err(e) => log::warn!("SV2: failed to parse SetNewPrevHash: {}", e),
                }
            }

            // ------------------------------------------------------------------
            // Share submission responses
            // ------------------------------------------------------------------
            mining::SUBMIT_SHARES_SUCCESS => {
                // Payload: channel_id u32 + last_sequence_number u32
                //        + new_submits_accepted_count u32 + new_shares_sum u64
                if frame.payload.len() >= 8 {
                    let _channel_id = u32::from_le_bytes([
                        frame.payload[0],
                        frame.payload[1],
                        frame.payload[2],
                        frame.payload[3],
                    ]);
                    let last_seq = u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ]);
                    let accepted_count = if frame.payload.len() >= 12 {
                        u32::from_le_bytes([
                            frame.payload[8],
                            frame.payload[9],
                            frame.payload[10],
                            frame.payload[11],
                        ])
                    } else {
                        1
                    };
                    log::info!(
                        "SV2: shares accepted up to seq={} count={}",
                        last_seq,
                        accepted_count
                    );
                    self.events.push(Sv2Event::ShareAccepted {
                        sequence_number: last_seq,
                        accepted_count,
                    });
                } else {
                    log::warn!(
                        "SV2: SubmitSharesSuccess payload too short ({})",
                        frame.payload.len()
                    );
                }
            }

            mining::SUBMIT_SHARES_ERROR => {
                // Payload: channel_id u32 + sequence_number u32 + error STR0_255
                let sequence_number = if frame.payload.len() >= 8 {
                    u32::from_le_bytes([
                        frame.payload[4],
                        frame.payload[5],
                        frame.payload[6],
                        frame.payload[7],
                    ])
                } else {
                    0
                };
                let reason = if frame.payload.len() > 8 {
                    if let Ok((msg, _)) = decode_str(&frame.payload, 8) {
                        msg
                    } else {
                        String::from_utf8_lossy(&frame.payload[8..]).to_string()
                    }
                } else {
                    "unknown error".to_string()
                };
                log::warn!("SV2: share rejected: {}", reason);
                self.events.push(Sv2Event::ShareRejected {
                    sequence_number,
                    reason,
                });
            }

            // ------------------------------------------------------------------
            // Extranonce prefix
            // ------------------------------------------------------------------
            mining::SET_EXTRANONCE_PREFIX => {
                // Payload: channel_id u32 + extranonce_prefix (B0_32)
                if frame.payload.len() >= 5 {
                    let prefix_len = frame.payload[4] as usize;
                    log::info!(
                        "SV2: SetExtranoncePrefix len={} prefix={:02x?}",
                        prefix_len,
                        &frame.payload[5..frame.payload.len().min(5 + prefix_len)]
                    );
                }
            }

            // ------------------------------------------------------------------
            // Target / difficulty
            // ------------------------------------------------------------------
            mining::SET_TARGET => {
                // Payload: channel_id u32 + max_target U256 (32 bytes LE)
                if frame.payload.len() >= 36 {
                    match sv2_target_le_to_be(&frame.payload[4..36]) {
                        Ok(target) => {
                            self.current_target = target;
                            let diff = target_be_to_difficulty(&self.current_target);
                            log::info!("SV2: SetTarget difficulty={:.4}", diff);
                            self.events.push(Sv2Event::DifficultyChanged(diff));
                        }
                        Err(e) => log::warn!("SV2: invalid SetTarget payload: {}", e),
                    }
                }
            }

            // ------------------------------------------------------------------
            // Pool-initiated reconnect
            // ------------------------------------------------------------------
            mining::RECONNECT => {
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
                log::info!("SV2: pool requested reconnect to {}:{}", host, port);
                self.events.push(Sv2Event::Reconnect { host, port });
            }

            // ------------------------------------------------------------------
            // Unknown / unhandled
            // ------------------------------------------------------------------
            other => {
                log::debug!(
                    "SV2: unhandled msg_type=0x{:02x} extension=0x{:04x} len={}",
                    other,
                    frame.header.extension_type,
                    frame.payload.len()
                );
            }
        }
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

    /// Number of pending (future) jobs buffered.
    pub fn pending_job_count(&self) -> usize {
        self.pending_jobs.len()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{Sv2Frame, FRAME_HEADER_SIZE};

    // -----------------------------------------------------------------------
    // Message construction tests
    // -----------------------------------------------------------------------

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
        assert_eq!(frame.header.extension_type, EXTENSION_TYPE_CHANNEL_MSG);

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

    // ── SV2-activate: authority-key plumbing through the channel ──────────────

    /// A pinned authority key reaches the Noise session, the default constructor
    /// leaves it None (TOFU), and — critically — `reset()` re-applies the pin so
    /// fail-closed certificate verification survives reconnects.
    #[test]
    fn test_authority_key_pinned_and_survives_reset() {
        // Pinned at construction → reaches NoiseSession.
        let key = [0x55u8; 32];
        let mut pinned = Sv2MiningChannel::new_with_endpoint_and_authority(
            "worker",
            500.0,
            "pool.example.com",
            3336,
            Some(key),
        );
        assert_eq!(pinned.noise_session().pool_authority_key, Some(key));

        // Reconnect path: reset() rebuilds NoiseSession (which itself starts as
        // None) but the channel re-pins the stored key — the SV2-activate fix.
        pinned.reset();
        assert_eq!(
            pinned.noise_session().pool_authority_key,
            Some(key),
            "reset() must preserve the pinned authority key (no silent TOFU revert)"
        );
    }

    /// The default constructor (and the legacy `new_with_endpoint`) pin nothing
    /// → TOFU, both before and after reset (default behavior unchanged).
    #[test]
    fn test_authority_key_default_is_tofu() {
        let mut legacy = Sv2MiningChannel::new_with_endpoint("worker", 500.0, "host", 3336);
        assert!(legacy.noise_session().pool_authority_key.is_none());
        legacy.reset();
        assert!(
            legacy.noise_session().pool_authority_key.is_none(),
            "unpinned channel stays TOFU across reset"
        );

        let plain = Sv2MiningChannel::new("worker", 500.0);
        assert!(plain.noise_session().pool_authority_key.is_none());
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

    /// Build a raw SV2 frame with an explicit extension_type (for scope tests).
    fn make_raw_frame_ext(ext: u16, msg_type: u8, payload: Vec<u8>) -> Vec<u8> {
        Sv2Frame::new(ext, msg_type, payload).to_bytes()
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

        // SV2 spec payload: flags U32 + error_code STR0_255.
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // flags
        encode_str(&mut payload, "unsupported-feature"); // error_code
        let data = make_raw_frame(mining::SETUP_CONNECTION_ERROR, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        // SV2-9: a flags/version-rolling mismatch yields a distinct, actionable
        // reason naming our advertised requirement, not a generic "rejected".
        if let Sv2Event::Disconnected(reason) = &events[0] {
            let r = reason.to_ascii_lowercase();
            assert!(
                r.contains("version rolling") || r.contains("requires_version_rolling"),
                "reason must name version rolling, got: {}",
                reason
            );
        } else {
            panic!("Expected Disconnected event");
        }
        assert!(matches!(channel.state, ChannelState::Error(_)));
    }

    /// SV2-9: a non-flags error code keeps the generic path — the reason still
    /// carries the code + message and is NOT decorated with the version-rolling note.
    #[test]
    fn test_feed_data_setup_connection_error_generic_preserved() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        // SV2 spec payload: flags U32 + error_code STR0_255.
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // flags
        encode_str(&mut payload, "protocol-version-mismatch"); // error_code
        let data = make_raw_frame(mining::SETUP_CONNECTION_ERROR, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::Disconnected(reason) = &events[0] {
            assert!(reason.contains("protocol-version-mismatch"));
            // Generic path is not decorated with the version-rolling advisory.
            assert!(
                !reason.to_ascii_lowercase().contains("advertised flags"),
                "generic error must not be decorated, got: {}",
                reason
            );
        } else {
            panic!("Expected Disconnected event");
        }
    }

    #[test]
    fn test_feed_data_open_channel_success() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id
        payload.extend_from_slice(&99u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&[0xFF; 32]); // target
        let data = make_raw_frame(mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Sv2Event::DifficultyChanged(_)));
        assert_eq!(channel.state, ChannelState::Mining { channel_id: 99 });
        assert!(channel.is_mining());
        assert_eq!(channel.channel_id(), Some(99));
    }

    #[test]
    fn test_feed_data_open_channel_error_surfaces_and_exits_opening() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        // Payload: request_id U32 + error_code STR0_255.
        let mut payload = Vec::new();
        payload.extend_from_slice(&7u32.to_le_bytes()); // request_id
        encode_str(&mut payload, "unknown-user-identity");
        let data = make_raw_frame(mining::OPEN_MINING_CHANNEL_ERROR, payload);
        let events = channel.feed_data(&data);

        // Before the fix there was NO handler arm: the frame fell to the
        // catch-all debug log, no event fired, and the channel stayed in
        // OpeningChannel forever (miner idles with the failure unsurfaced).
        assert_eq!(
            events.len(),
            1,
            "channel-open rejection must surface an event"
        );
        match &events[0] {
            Sv2Event::Disconnected(reason) => assert!(
                reason.contains("unknown-user-identity"),
                "reason must carry the pool's error code, got: {reason}"
            ),
            other => panic!("expected Disconnected, got {other:?}"),
        }
        assert!(
            matches!(channel.state, ChannelState::Error(_)),
            "state must leave OpeningChannel on rejection"
        );
    }

    #[test]
    fn test_feed_data_open_channel_success_truncated_does_not_wedge() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::OpeningChannel;

        // A Success frame too short to carry channel_id(4)+request_id(4)+
        // target(32) = 40 bytes. Warn-only used to leave the channel stuck in
        // OpeningChannel with no event (silent wedge); it must now fail closed.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // request_id only (8 < 40)
        payload.extend_from_slice(&99u32.to_le_bytes());
        let data = make_raw_frame(mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1, "a truncated Success must surface an event");
        assert!(matches!(events[0], Sv2Event::Disconnected(_)));
        assert!(
            matches!(channel.state, ChannelState::Error(_)),
            "truncated Success must not leave the channel wedged in OpeningChannel"
        );
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
        payload.push(0x01); // min_ntime is set: active job
        payload.extend_from_slice(&1_700_000_001u32.to_le_bytes());
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
            ntime,
            ..
        } = &events[0]
        {
            assert_eq!(*job_id, 42);
            assert_eq!(*nbits, 0x1a03a30c);
            assert_eq!(prev_hash, &[0xAAu8; 32]);
            assert_eq!(*ntime, 1_700_000_001);
            // SV2-4: an immediately-usable NewMiningJob under the existing prev_hash
            // ADDS work; the tip did not move → clean_jobs must be FALSE.
            assert!(!*clean_jobs);
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
        job_payload.push(0x00); // min_ntime unset: future job
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
            min_ntime: None,
            version: 0x2000_0000,
            merkle_root: [0x42; 32],
        };

        // Serialize manually (same layout as from_bytes expects)
        let mut buf = Vec::new();
        buf.extend_from_slice(&original.channel_id.to_le_bytes());
        buf.extend_from_slice(&original.job_id.to_le_bytes());
        buf.push(0);
        buf.extend_from_slice(&original.version.to_le_bytes());
        buf.extend_from_slice(&original.merkle_root);

        let parsed = NewMiningJob::from_bytes(&buf).unwrap();
        assert_eq!(parsed.channel_id, original.channel_id);
        assert_eq!(parsed.job_id, original.job_id);
        assert_eq!(parsed.min_ntime, original.min_ntime);
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
    // SV2-4: clean_jobs semantics (tip-change => true, incremental job => false)
    // -----------------------------------------------------------------------

    /// An immediately-usable NewMiningJob under the EXISTING prev_hash adds work
    /// without moving the tip → emitted clean_jobs must be FALSE.
    #[test]
    fn test_new_mining_job_same_prevhash_is_not_clean() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Seed an active prev_hash so a non-future job emits immediately.
        channel.current_prev_hash = Some(SetNewPrevHash {
            channel_id: 1,
            job_id: 10,
            prev_hash: [0xAA; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1a03a30c,
        });

        // Non-future NewMiningJob (min_ntime set).
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        payload.extend_from_slice(&77u32.to_le_bytes()); // job_id
        payload.push(0x01); // min_ntime present
        payload.extend_from_slice(&1_700_000_005u32.to_le_bytes());
        payload.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        payload.extend_from_slice(&[0xBB; 32]); // merkle_root

        let data = make_raw_frame(mining::NEW_MINING_JOB, payload);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob { clean_jobs, .. } = &events[0] {
            assert!(
                !*clean_jobs,
                "same-prev-hash incremental job must NOT signal clean_jobs"
            );
        } else {
            panic!("Expected NewJob event");
        }
    }

    /// A genuine tip change (SetNewPrevHash) emits clean_jobs == true.
    #[test]
    fn test_set_new_prev_hash_emits_clean() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Future job referenced explicitly by the prev_hash.
        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_payload.extend_from_slice(&5u32.to_le_bytes()); // job_id
        job_payload.push(0x00); // future job
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_payload.extend_from_slice(&[0xCC; 32]); // merkle_root
        let job_data = make_raw_frame(mining::NEW_MINING_JOB, job_payload);
        assert_eq!(channel.feed_data(&job_data).len(), 0);

        // SetNewPrevHash referencing that job → genuine tip change.
        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_payload.extend_from_slice(&5u32.to_le_bytes()); // job_id (matches)
        ph_payload.extend_from_slice(&[0xDD; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_001_000u32.to_le_bytes()); // min_ntime
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes()); // nbits
        let ph_data = make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload);
        let events = channel.feed_data(&ph_data);

        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob { clean_jobs, .. } = &events[0] {
            assert!(*clean_jobs, "tip change must signal clean_jobs=true");
        } else {
            panic!("Expected NewJob event after SetNewPrevHash");
        }
    }

    // -----------------------------------------------------------------------
    // MED-1: buffered (no-match) tip change → first paired NewMiningJob restores
    // the clean_jobs=true tip flush; subsequent incremental jobs stay false.
    // This is the SV2-4 × SV2-12 interaction hole the DCENT_Protocol review found.
    // -----------------------------------------------------------------------

    /// A SetNewPrevHash referencing a job_id NOT yet in pending_jobs (out-of-order
    /// delivery / legacy job_id==0) buffers the new tip and emits nothing. The
    /// FIRST NewMiningJob under that tip must emit clean_jobs=TRUE so the
    /// dispatcher flushes the OLD-tip ASIC work (tip flush restored). A subsequent
    /// incremental NewMiningJob under the SAME now-announced tip must emit
    /// clean_jobs=FALSE (SV2-4 preserved). And the spec-conformant
    /// SetNewPrevHash-matches-existing-job path still emits clean_jobs=true.
    #[test]
    fn test_buffered_tip_change_first_job_is_clean_then_incremental_is_not() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Establish an initial active tip so the channel is mining old-tip work.
        // (Future job 1 + matching SetNewPrevHash → spec-conformant clean tip.)
        let mut job1 = Vec::new();
        job1.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job1.extend_from_slice(&1u32.to_le_bytes()); // job_id
        job1.push(0x00); // future
        job1.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job1.extend_from_slice(&[0x11; 32]); // merkle_root
        assert_eq!(
            channel
                .feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job1))
                .len(),
            0
        );

        let mut ph1 = Vec::new();
        ph1.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph1.extend_from_slice(&1u32.to_le_bytes()); // job_id (matches job 1)
        ph1.extend_from_slice(&[0xA1; 32]); // prev_hash (tip A)
        ph1.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        ph1.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let ev = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, ph1));
        // (c) spec-conformant matched path still emits clean_jobs=true.
        assert_eq!(ev.len(), 1, "matched SetNewPrevHash must emit a job");
        if let Sv2Event::NewJob { clean_jobs, .. } = &ev[0] {
            assert!(
                *clean_jobs,
                "matched SetNewPrevHash (spec path) must signal clean_jobs=true"
            );
        } else {
            panic!("Expected NewJob from matched SetNewPrevHash");
        }

        // Now a GENUINE TIP CHANGE arrives out-of-order: SetNewPrevHash references
        // job_id=9 which has NOT arrived yet → buffered, emits nothing.
        let mut ph2 = Vec::new();
        ph2.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph2.extend_from_slice(&9u32.to_le_bytes()); // job_id (no match yet)
        ph2.extend_from_slice(&[0xB2; 32]); // prev_hash (tip B — moved!)
        ph2.extend_from_slice(&1_700_000_100u32.to_le_bytes());
        ph2.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let ev = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, ph2));
        assert_eq!(
            ev.len(),
            0,
            "out-of-order tip change buffers, emits nothing"
        );
        assert!(
            channel.current_prev_hash.is_some(),
            "new tip must be buffered"
        );

        // (a) The FIRST NewMiningJob under the new (tip B) buffered prev_hash must
        // emit clean_jobs=TRUE — restoring the tip flush the dispatcher needs to
        // drop the stale old-tip work.
        let mut job_b1 = Vec::new();
        job_b1.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_b1.extend_from_slice(&10u32.to_le_bytes()); // job_id
        job_b1.push(0x01); // min_ntime present (immediately usable)
        job_b1.extend_from_slice(&1_700_000_101u32.to_le_bytes());
        job_b1.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_b1.extend_from_slice(&[0xCC; 32]); // merkle_root
        let ev = channel.feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job_b1));
        assert_eq!(ev.len(), 1);
        if let Sv2Event::NewJob {
            clean_jobs,
            prev_hash,
            job_id,
            ..
        } = &ev[0]
        {
            assert_eq!(*job_id, 10);
            assert_eq!(prev_hash, &[0xB2u8; 32], "must pair with the new tip B");
            assert!(
                *clean_jobs,
                "MED-1: first job pairing a buffered tip change MUST signal clean_jobs=true (tip flush restored)"
            );
        } else {
            panic!("Expected NewJob pairing the buffered tip change");
        }

        // (b) A SUBSEQUENT incremental NewMiningJob under the SAME (now-announced)
        // tip B must revert to clean_jobs=FALSE — SV2-4 incremental behavior holds.
        let mut job_b2 = Vec::new();
        job_b2.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_b2.extend_from_slice(&11u32.to_le_bytes()); // job_id
        job_b2.push(0x01); // min_ntime present
        job_b2.extend_from_slice(&1_700_000_102u32.to_le_bytes());
        job_b2.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_b2.extend_from_slice(&[0xDD; 32]); // merkle_root
        let ev = channel.feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job_b2));
        assert_eq!(ev.len(), 1);
        if let Sv2Event::NewJob { clean_jobs, .. } = &ev[0] {
            assert!(
                !*clean_jobs,
                "SV2-4: incremental job under an already-announced tip must signal clean_jobs=false"
            );
        } else {
            panic!("Expected incremental NewJob");
        }
    }

    /// LOW-1: an out-of-order (no-match) SetNewPrevHash buffering a HIGHER job_id
    /// must NOT prune a lower-id future job that an EARLIER, still-pending tip is
    /// awaiting. Before the fix, the no-match `retain(id >= job_id)` discarded the
    /// awaited lower job; after the fix the no-match branch skips the aggressive
    /// retain (only the matched path prunes), so the earlier tip can still pair.
    #[test]
    fn test_no_match_prevhash_does_not_prune_awaited_lower_job() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // Future job 5 arrives (the job an earlier tip will reference).
        let mut job5 = Vec::new();
        job5.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job5.extend_from_slice(&5u32.to_le_bytes()); // job_id
        job5.push(0x00); // future
        job5.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job5.extend_from_slice(&[0x55; 32]); // merkle_root
        assert_eq!(
            channel
                .feed_data(&make_raw_frame(mining::NEW_MINING_JOB, job5))
                .len(),
            0
        );

        // A no-match SetNewPrevHash with a HIGHER job_id (8) arrives first. With the
        // old aggressive retain this would prune job 5; the fix keeps it.
        let mut ph_hi = Vec::new();
        ph_hi.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_hi.extend_from_slice(&8u32.to_le_bytes()); // job_id (no match, > 5)
        ph_hi.extend_from_slice(&[0x88; 32]); // prev_hash
        ph_hi.extend_from_slice(&1_700_000_200u32.to_le_bytes());
        ph_hi.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        assert_eq!(
            channel
                .feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, ph_hi))
                .len(),
            0,
            "no-match tip buffers, emits nothing"
        );
        assert_eq!(
            channel.pending_job_count(),
            1,
            "LOW-1: awaited lower-id future job must survive a no-match higher-id tip buffer"
        );

        // Prove the survived job is still job 5 and can still be paired by its own
        // (earlier) tip referencing job_id=5.
        let mut ph5 = Vec::new();
        ph5.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph5.extend_from_slice(&5u32.to_le_bytes()); // job_id (matches the survivor)
        ph5.extend_from_slice(&[0x05; 32]); // prev_hash
        ph5.extend_from_slice(&1_700_000_201u32.to_le_bytes());
        ph5.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let ev = channel.feed_data(&make_raw_frame(mining::SET_NEW_PREV_HASH, ph5));
        assert_eq!(ev.len(), 1, "the survived job must still pair with its tip");
        if let Sv2Event::NewJob { job_id, .. } = &ev[0] {
            assert_eq!(*job_id, 5);
        } else {
            panic!("Expected NewJob from the survived job pairing");
        }
    }

    // -----------------------------------------------------------------------
    // SV2-11: receive-side frame-scope validation (detect, don't drop)
    // -----------------------------------------------------------------------

    #[test]
    fn test_frame_scope_ok() {
        // Connection-scoped message, mining extension → OK.
        assert!(Sv2MiningChannel::validate_frame_scope(
            EXTENSION_TYPE_MINING,
            mining::SETUP_CONNECTION_SUCCESS
        )
        .is_none());
        // channel_msg bit set + extension id 0 → still OK (valid SV2 scope).
        assert!(Sv2MiningChannel::validate_frame_scope(
            EXTENSION_TYPE_CHANNEL_MSG,
            mining::SUBMIT_SHARES_STANDARD
        )
        .is_none());
    }

    #[test]
    fn test_frame_scope_bad_extension() {
        // Extension id 7 (we register no extensions) → warning.
        assert!(Sv2MiningChannel::validate_frame_scope(0x0007, mining::NEW_MINING_JOB).is_some());
        // Same low bits but with the channel_msg flag set → still a warning
        // (the flag does not whitelist the extension id).
        assert!(Sv2MiningChannel::validate_frame_scope(0x8007, mining::NEW_MINING_JOB).is_some());
    }

    /// A frame carrying an unexpected extension id still DISPATCHES by msg_type
    /// (log-but-don't-drop): the event is produced, not silently swallowed.
    #[test]
    fn test_feed_data_unexpected_extension_still_dispatches() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::SettingUp;

        let data = make_raw_frame_ext(0x0007, mining::SETUP_CONNECTION_SUCCESS, vec![]);
        let events = channel.feed_data(&data);

        assert_eq!(events.len(), 1, "frame must still dispatch by msg_type");
        assert!(matches!(events[0], Sv2Event::Connected));
    }

    // -----------------------------------------------------------------------
    // SV2-12: SetNewPrevHash matches strictly by job_id (no job_id==0 wildcard)
    // -----------------------------------------------------------------------

    /// A SetNewPrevHash with job_id==0 must NOT pair an arbitrary latest job; it
    /// buffers the prev_hash and waits for the referenced future job. A subsequent
    /// NewMiningJob under that prev_hash then emits the paired NewJob.
    #[test]
    fn test_set_new_prev_hash_job_id_zero_buffers_not_pairs() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // A future job exists (job_id=5) — previously job_id==0 would wrongly pair it.
        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_payload.extend_from_slice(&5u32.to_le_bytes()); // job_id
        job_payload.push(0x00); // future
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job_payload.extend_from_slice(&[0xCC; 32]);
        let job_data = make_raw_frame(mining::NEW_MINING_JOB, job_payload);
        assert_eq!(channel.feed_data(&job_data).len(), 0);

        // SetNewPrevHash with job_id==0 → must emit nothing and buffer the prev_hash.
        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_payload.extend_from_slice(&0u32.to_le_bytes()); // job_id == 0
        ph_payload.extend_from_slice(&[0xEE; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_002_000u32.to_le_bytes());
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let ph_data = make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload);
        let events = channel.feed_data(&ph_data);
        assert_eq!(events.len(), 0, "job_id==0 must NOT pair an arbitrary job");
        assert!(
            channel.current_prev_hash.is_some(),
            "prev_hash must be buffered for the referenced future job"
        );

        // Now a NewMiningJob under that buffered prev_hash emits the paired NewJob.
        let mut job2 = Vec::new();
        job2.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job2.extend_from_slice(&6u32.to_le_bytes()); // job_id
        job2.push(0x01); // min_ntime present (immediately usable)
        job2.extend_from_slice(&1_700_002_001u32.to_le_bytes());
        job2.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        job2.extend_from_slice(&[0xFF; 32]); // merkle_root
        let job2_data = make_raw_frame(mining::NEW_MINING_JOB, job2);
        let events = channel.feed_data(&job2_data);
        assert_eq!(events.len(), 1);
        if let Sv2Event::NewJob {
            job_id, prev_hash, ..
        } = &events[0]
        {
            assert_eq!(*job_id, 6);
            assert_eq!(
                prev_hash, &[0xEEu8; 32],
                "paired with the buffered prev_hash"
            );
        } else {
            panic!("Expected NewJob after buffered prev_hash pairs with a new job");
        }
    }

    /// An unknown job_id (no such pending job) emits nothing and buffers prev_hash.
    #[test]
    fn test_set_new_prev_hash_unknown_job_id_no_event() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_payload.extend_from_slice(&999u32.to_le_bytes()); // job_id with no match
        ph_payload.extend_from_slice(&[0xAB; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_003_000u32.to_le_bytes());
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes());
        let ph_data = make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload);
        let events = channel.feed_data(&ph_data);

        assert_eq!(events.len(), 0, "no matching job → no event");
        assert!(
            channel.current_prev_hash.is_some(),
            "prev_hash must be buffered"
        );
    }

    /// Out-of-order: a SetNewPrevHash arrives BEFORE the future job it references,
    /// then that FUTURE job (min_ntime=None) arrives. It must activate the buffered
    /// tip now — before the fix, the future-job path skipped the emit (gated on
    /// min_ntime=Some) so the referenced job silently never activated and the
    /// dispatcher kept grinding the stale tip.
    #[test]
    fn test_future_job_after_set_new_prev_hash_activates() {
        let mut channel = Sv2MiningChannel::new("worker", 500.0);
        channel.state = ChannelState::Mining { channel_id: 1 };

        // SetNewPrevHash references job_id=7, which has NOT arrived yet → buffer the tip.
        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        ph_payload.extend_from_slice(&7u32.to_le_bytes()); // job_id (future)
        ph_payload.extend_from_slice(&[0xB7; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_000_000u32.to_le_bytes()); // min_ntime
        ph_payload.extend_from_slice(&0x1903a30cu32.to_le_bytes()); // nbits
        let ph_data = make_raw_frame(mining::SET_NEW_PREV_HASH, ph_payload);
        assert_eq!(
            channel.feed_data(&ph_data).len(),
            0,
            "no job yet → buffer, no event"
        );

        // The referenced FUTURE job (min_ntime=None, job_id=7) now arrives out of order.
        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        job_payload.extend_from_slice(&7u32.to_le_bytes()); // job_id
        job_payload.push(0x00); // future — min_ntime absent
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        job_payload.extend_from_slice(&[0x77; 32]); // merkle_root
        let job_data = make_raw_frame(mining::NEW_MINING_JOB, job_payload);
        let events = channel.feed_data(&job_data);

        assert_eq!(
            events.len(),
            1,
            "the referenced future job must activate the buffered tip"
        );
        if let Sv2Event::NewJob {
            job_id,
            prev_hash,
            nbits,
            ntime,
            clean_jobs,
            ..
        } = &events[0]
        {
            assert_eq!(*job_id, 7);
            assert_eq!(prev_hash, &[0xB7u8; 32], "paired with the buffered tip");
            assert_eq!(*nbits, 0x1903a30c, "nbits from the buffered tip");
            assert_eq!(*ntime, 1_700_000_000, "ntime from the buffered tip");
            assert!(*clean_jobs, "a genuine tip change must flush stale work");
        } else {
            panic!("expected NewJob activating the out-of-order future job");
        }

        // The flag is cleared → a re-sent copy of the same future job does not re-emit.
        let mut dup = Vec::new();
        dup.extend_from_slice(&1u32.to_le_bytes());
        dup.extend_from_slice(&7u32.to_le_bytes());
        dup.push(0x00);
        dup.extend_from_slice(&0x2000_0000u32.to_le_bytes());
        dup.extend_from_slice(&[0x77; 32]);
        let dup_data = make_raw_frame(mining::NEW_MINING_JOB, dup);
        assert_eq!(
            channel.feed_data(&dup_data).len(),
            0,
            "unannounced flag cleared → no duplicate activation"
        );
    }
}
