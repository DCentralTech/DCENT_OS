//! Stratum V2 TCP Client
//!
//! Connects to an SV2 pool, performs the Noise handshake, opens a mining
//! channel, and processes mining work. Emits events compatible with the
//! V1 dispatcher.
//!
//! # Noise Transport Framing
//! After the Noise_NX handshake, all messages are encrypted. Each Noise
//! transport frame is:
//!   - `length: u16 (LE)` — length of the encrypted payload (plaintext + 16 MAC)
//!   - `ciphertext: [u8; length]` — ChaChaPoly1305 encrypted data
//!
//! Inside the decrypted payload is the standard SV2 frame (6-byte header + payload).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::channel::{Sv2Event, Sv2MiningChannel};
use crate::noise::MIN_SERVER_RESPONSE;

/// Initial / growth granularity for the Noise handshake read buffer (SV2-8).
///
/// Must be >= [`MIN_SERVER_RESPONSE`] (the 234-byte handshake) and is sized with
/// headroom so the common case — the pool coalescing the handshake plus the first
/// encrypted transport frame onto a single read — fits without a resize. The read
/// loop grows the buffer by this amount whenever it would otherwise be full, so an
/// unusually large coalesced read is never truncated and never mis-read as EOF.
const HANDSHAKE_READ_BUF_LEN: usize = 1024;

/// Split a fully-read Noise handshake byte buffer into its post-handshake surplus.
///
/// `initiator_handshake_finish` consumes exactly [`MIN_SERVER_RESPONSE`] bytes; any
/// bytes after that are the start of the encrypted transport stream (the first
/// frame the pool coalesced onto the same TCP read). Those bytes must be queued for
/// decryption with `receiving_nonce = 0`, not dropped. Returns the surplus slice
/// (empty when the read was an exact-length handshake).
///
/// Pure + total: callable from host tests without a live socket (SV2-8 regression).
fn handshake_surplus(handshake_buf: &[u8]) -> &[u8] {
    if handshake_buf.len() > MIN_SERVER_RESPONSE {
        &handshake_buf[MIN_SERVER_RESPONSE..]
    } else {
        &[]
    }
}

/// SV2 client configuration
#[derive(Debug, Clone)]
pub struct Sv2Config {
    /// Pool hostname or IP
    pub host: String,
    /// Pool port (typically 34255 for SV2)
    pub port: u16,
    /// Worker identity (e.g., "bc1q...address.worker")
    pub worker: String,
    /// Expected hashrate in GH/s
    pub hashrate_ghs: f32,
    /// Connection timeout in seconds
    pub timeout_secs: u64,
    /// Optional pinned SV2 pool authority public key.
    ///
    /// Accepts either a bare base58check token or a full SV2 URL whose path
    /// carries it (see [`crate::noise::parse_pool_authority_pubkey`]). When set
    /// and parseable, the Noise handshake verifies the server certificate
    /// fail-closed (MITM defense). When `None` — or when the value fails to
    /// parse — the client falls back to trust-on-first-use (TOFU), the default
    /// behavior, so a malformed key never bricks the connection.
    pub pool_authority_pubkey: Option<String>,
}

impl Default for Sv2Config {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 34255,
            worker: String::new(),
            hashrate_ghs: 500.0,
            timeout_secs: 30,
            pool_authority_pubkey: None,
        }
    }
}

/// SV2 TCP Client state
pub enum ClientState {
    Disconnected,
    Connecting,
    Connected,
    Mining,
    Error(String),
}

/// The SV2 TCP client
pub struct Sv2Client {
    config: Sv2Config,
    channel: Sv2MiningChannel,
    stream: Option<TcpStream>,
    state: ClientState,
    /// Raw TCP read buffer
    read_buf: Vec<u8>,
    /// Accumulator for incoming Noise transport frames
    noise_recv_buf: Vec<u8>,
    /// Decrypted header waiting for its payload (fixes nonce desync)
    pending_header: Option<[u8; 6]>,
    /// RNG seed for Noise handshake (64 bytes from esp_random)
    rng_seed: [u8; 64],
}

impl Sv2Client {
    pub fn new(config: Sv2Config) -> Self {
        // Parse the optional pinned authority key ONCE at construction. On a
        // parse error we log a warning and fall back to TOFU (None) rather than
        // bricking the client thread — fail-open-to-TOFU, never fail-open-to-
        // unsignaled. A valid key activates fail-closed certificate verification
        // in the Noise handshake.
        let pool_authority_key = match config.pool_authority_pubkey.as_deref() {
            Some(s) if !s.trim().is_empty() => match crate::noise::parse_pool_authority_pubkey(s) {
                Ok(key) => {
                    log::info!(
                            "SV2: pinned pool authority key configured — certificate verification fail-closed"
                        );
                    Some(key)
                }
                Err(e) => {
                    log::warn!(
                            "SV2: invalid sv2_authority_pubkey ({}) — falling back to TOFU (insecure against MITM)",
                            e
                        );
                    None
                }
            },
            _ => None,
        };

        let channel = Sv2MiningChannel::new_with_endpoint_and_authority(
            &config.worker,
            config.hashrate_ghs,
            &config.host,
            config.port,
            pool_authority_key,
        );
        Self {
            config,
            channel,
            stream: None,
            state: ClientState::Disconnected,
            read_buf: vec![0u8; 4096],
            noise_recv_buf: Vec::with_capacity(4096),
            pending_header: None,
            rng_seed: [0u8; 64],
        }
    }

    /// Set the RNG seed for the Noise handshake.
    /// Must be called before connect(). Fill with esp_random() output.
    /// Bytes [0..32]: ephemeral secret key seed.
    /// Bytes [32..64]: EllSwift encoding randomizer.
    pub fn set_rng_seed(&mut self, seed: [u8; 64]) {
        self.rng_seed = seed;
    }

    /// Connect to the SV2 pool and perform handshake
    pub fn connect(&mut self) -> Result<(), String> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        log::info!("SV2: connecting to {}", addr);
        self.state = ClientState::Connecting;

        // DNS resolve + TCP connect
        use std::net::ToSocketAddrs;
        log::info!("SV2: resolving {}", addr);

        let socket_addr = addr
            .to_socket_addrs()
            .map_err(|e| format!("SV2: DNS resolve failed for {}: {}", addr, e))?
            .next()
            .ok_or_else(|| format!("SV2: no addresses found for {}", addr))?;

        log::info!("SV2: resolved to {}", socket_addr);

        let stream =
            TcpStream::connect_timeout(&socket_addr, Duration::from_secs(self.config.timeout_secs))
                .map_err(|e| format!("SV2: TCP connect to {} failed: {}", socket_addr, e))?;

        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| format!("SV2: set timeout failed: {}", e))?;
        stream.set_nodelay(true).ok();

        self.stream = Some(stream);
        log::info!("SV2: TCP connected to {}", addr);

        // ── Noise_NX handshake ──────────────────────────────────────────
        // Step 1: Generate ephemeral keypair and send -> e
        let noise = self.channel.noise_session_mut();
        let handshake_msg = noise
            .initiator_handshake_start(self.rng_seed)
            .map_err(|e| format!("SV2: Noise handshake start failed: {}", e))?;

        log::info!("SV2: Noise -> e sent ({} bytes)", handshake_msg.len());
        if let Some(ref mut s) = self.stream {
            s.write_all(&handshake_msg)
                .map_err(|e| format!("SV2: failed to send handshake -> e: {}", e))?;
        }

        // Step 2: Read server response <- e, ee, s, es
        if let Some(ref s) = self.stream {
            s.set_read_timeout(Some(Duration::from_secs(10))).ok();
        }
        // SV2-8: the buffer must be at least MIN_SERVER_RESPONSE (the 234-byte
        // handshake) AND able to absorb whatever the pool coalesces AFTER it (the
        // first encrypted transport frame — SetupConnection.Success, etc.). A
        // fixed 512-byte buffer dropped that surplus and, on a >512-byte coalesced
        // read, left a zero-length read slice that `read()` reports as `Ok(0)` —
        // mis-detected as EOF. Start at a comfortable size and GROW so a single
        // large coalesced read is never truncated and never mis-read as EOF.
        let mut hs_buf = vec![0u8; HANDSHAKE_READ_BUF_LEN];
        let mut hs_len = 0usize;
        let deadline = Instant::now() + Duration::from_secs(10);
        while hs_len < MIN_SERVER_RESPONSE {
            if Instant::now() >= deadline {
                return Err(format!(
                    "SV2: timed out reading Noise response ({} of {} bytes)",
                    hs_len, MIN_SERVER_RESPONSE
                ));
            }
            // Never hand `read()` a zero-length slice — that returns `Ok(0)` and
            // would be mis-detected as a closed connection. Grow first.
            if hs_len == hs_buf.len() {
                hs_buf.resize(hs_buf.len() + HANDSHAKE_READ_BUF_LEN, 0u8);
            }
            let read_result = if let Some(ref mut s) = self.stream {
                s.read(&mut hs_buf[hs_len..])
            } else {
                return Err("SV2: stream lost during handshake".into());
            };
            match read_result {
                Ok(0) => {
                    return Err("SV2: server closed connection during handshake".into());
                }
                Ok(n) => hs_len += n,
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("SV2: failed to read handshake response: {}", e)),
            }
        }

        log::info!("SV2: Noise <- response received ({} bytes)", hs_len);

        // Process server handshake response. `initiator_handshake_finish` parses
        // EXACTLY the first MIN_SERVER_RESPONSE (234) bytes; anything past that is
        // already-encrypted transport data the pool coalesced onto the same read.
        let noise = self.channel.noise_session_mut();
        noise
            .initiator_handshake_finish(&hs_buf[..hs_len])
            .map_err(|e| {
                log::error!(
                    "SV2: Server response hex: {:02x?}",
                    &hs_buf[..hs_len.min(32)]
                );
                format!("SV2: Noise handshake failed: {}", e)
            })?;

        // SV2-8: carry any post-handshake surplus into the Noise receive buffer so
        // the first poll() decrypts it with receiving_nonce=0 (the correct nonce
        // for the very first transport frame) instead of dropping it. Without this,
        // a coalesced first frame is lost and the NEXT real frame decrypts with the
        // wrong nonce → permanent receive desync.
        let surplus = handshake_surplus(&hs_buf[..hs_len]);
        if !surplus.is_empty() {
            log::info!(
                "SV2: carrying {} post-handshake byte(s) into Noise receive buffer",
                surplus.len()
            );
            self.noise_recv_buf.extend_from_slice(surplus);
        }

        log::info!("SV2: Noise_NX handshake COMPLETE — encrypted transport active!");

        // Restore normal read timeout
        if let Some(ref s) = self.stream {
            s.set_read_timeout(Some(Duration::from_millis(100))).ok();
        }

        // Send SetupConnection (encrypted via Noise transport frame)
        let setup_msg = self.channel.make_setup_connection();
        self.send_noise_frame(&setup_msg)?;
        log::info!(
            "SV2: SetupConnection sent ({} bytes plaintext)",
            setup_msg.len()
        );

        self.state = ClientState::Connected;
        Ok(())
    }

    /// Send an SV2 frame through the Noise encrypted transport.
    ///
    /// SV2 Noise transport encrypts the **header and payload separately**:
    ///   1. EncryptWithAd([], 6-byte SV2 header) → 22 bytes (6 + 16 MAC)
    ///   2. EncryptWithAd([], payload)            → payload_len + 16 MAC
    ///
    /// Each encrypted block uses a separate nonce (auto-incremented).
    /// No length prefix — the pool knows the header block is always 22 bytes,
    /// and derives payload size from the decrypted header's msg_length field.
    fn send_noise_frame(&mut self, sv2_frame: &[u8]) -> Result<(), String> {
        if sv2_frame.len() < 6 {
            return Err("SV2: frame too short for header".into());
        }

        if let Some(ref mut stream) = self.stream {
            let header = &sv2_frame[..6];
            let payload = &sv2_frame[6..];

            // Encrypt SV2 header (6 bytes → 22 bytes with MAC)
            let encrypted_header = self
                .channel
                .noise_session_mut()
                .encrypt(header)
                .map_err(|e| format!("SV2: Noise encrypt header failed: {}", e))?;

            // Encrypt SV2 payload (variable → payload_len + 16 with MAC)
            let encrypted_payload = self
                .channel
                .noise_session_mut()
                .encrypt(payload)
                .map_err(|e| format!("SV2: Noise encrypt payload failed: {}", e))?;

            // Write both encrypted blocks
            stream
                .write_all(&encrypted_header)
                .map_err(|e| format!("SV2: failed to write encrypted header: {}", e))?;
            stream
                .write_all(&encrypted_payload)
                .map_err(|e| format!("SV2: failed to write encrypted payload: {}", e))?;

            log::info!(
                "SV2: sent encrypted frame: hdr={}B + payload={}B ({} total on wire, {} plaintext)",
                encrypted_header.len(),
                encrypted_payload.len(),
                encrypted_header.len() + encrypted_payload.len(),
                sv2_frame.len()
            );
            Ok(())
        } else {
            Err("SV2: not connected".into())
        }
    }

    /// Submit a share
    pub fn submit_share(
        &mut self,
        job_id: u32,
        nonce: u32,
        ntime: u32,
        version: u32,
    ) -> Result<u32, String> {
        let channel_id = self
            .channel
            .channel_id()
            .ok_or_else(|| "SV2: no active channel".to_string())?;
        let msg = self
            .channel
            .make_submit_share(channel_id, job_id, nonce, ntime, version);
        let sequence_number = self.channel.sequence_number();
        self.send_noise_frame(&msg)?;
        Ok(sequence_number)
    }

    /// Poll for incoming data and process it.
    ///
    /// Reads raw TCP data, accumulates into `noise_recv_buf`, parses
    /// Noise transport frames (2-byte length + ciphertext), decrypts each
    /// frame, and feeds the plaintext SV2 frames to the channel state machine.
    pub fn poll(&mut self) -> Vec<Sv2Event> {
        let mut events = Vec::new();

        if let Some(ref mut stream) = self.stream {
            // Try to read data from TCP
            match stream.read(&mut self.read_buf) {
                Ok(0) => {
                    events.push(Sv2Event::Disconnected("Connection closed by pool".into()));
                    self.state = ClientState::Disconnected;
                    return events;
                }
                Ok(n) => {
                    self.noise_recv_buf.extend_from_slice(&self.read_buf[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available — normal for non-blocking read
                }
                Err(e) => {
                    events.push(Sv2Event::Disconnected(format!("Read error: {}", e)));
                    self.state = ClientState::Disconnected;
                    return events;
                }
            }
        }

        // Process complete Noise encrypted SV2 frames from the buffer.
        // SV2 Noise transport: [encrypted_header (22 bytes)] [encrypted_payload (N+16 bytes)]
        // Decrypt header first (22 → 6 bytes), read msg_length, then decrypt payload.
        // The pending_header buffer prevents nonce desync when payload arrives in a later poll.
        loop {
            const ENCRYPTED_HEADER_SIZE: usize = 6 + 16; // 22 bytes

            // Step 1: Get the decrypted header (either from pending or decrypt now)
            let header_plain: [u8; 6] = if let Some(h) = self.pending_header {
                // We already decrypted the header in a previous poll — reuse it
                h
            } else {
                // Need at least 22 bytes for the encrypted header block
                if self.noise_recv_buf.len() < ENCRYPTED_HEADER_SIZE {
                    break;
                }

                let header_cipher = self.noise_recv_buf[..ENCRYPTED_HEADER_SIZE].to_vec();
                let h = match self.channel.noise_session_mut().decrypt(&header_cipher) {
                    Ok(h) => h,
                    Err(e) => {
                        log::error!("SV2: Noise decrypt header failed: {}", e);
                        events.push(Sv2Event::Disconnected(format!(
                            "Noise header decrypt: {}",
                            e
                        )));
                        self.state = ClientState::Disconnected;
                        break;
                    }
                };

                if h.len() != 6 {
                    log::error!("SV2: decrypted header wrong size: {}", h.len());
                    events.push(Sv2Event::Disconnected("bad header size".into()));
                    self.state = ClientState::Disconnected;
                    break;
                }

                // Drain the encrypted header from the buffer NOW (nonce already incremented)
                self.noise_recv_buf.drain(..ENCRYPTED_HEADER_SIZE);

                let mut arr = [0u8; 6];
                arr.copy_from_slice(&h);
                arr
            };

            // Step 2: Parse payload length from the decrypted SV2 header
            let payload_len =
                u32::from_le_bytes([header_plain[3], header_plain[4], header_plain[5], 0]) as usize;

            // ES-3: bound the claimed payload length BEFORE we wait for / allocate
            // that many bytes. The authoritative frame decoder (framing.rs) rejects
            // `payload_len > MAX_PAYLOAD_SIZE`, but this encrypted poll path decrypts
            // inline and bypassed that guard — so a peer PAST the Noise handshake (a
            // malicious pool, or a TOFU-accepted MITM) could claim up to ~16 MB and
            // drive `noise_recv_buf` growth / an oversized allocation into an OOM
            // abort (panic=abort → crash-loop) on the ESP32. Reject it as a protocol
            // violation and tear the session down, mirroring the decrypt-failure path.
            if payload_len > crate::framing::MAX_PAYLOAD_SIZE as usize {
                log::error!(
                    "SV2: peer claimed payload_len {}B > MAX_PAYLOAD_SIZE {}B — protocol violation, disconnecting",
                    payload_len,
                    crate::framing::MAX_PAYLOAD_SIZE
                );
                events.push(Sv2Event::Disconnected(format!(
                    "SV2 payload too large: {}B",
                    payload_len
                )));
                self.state = ClientState::Disconnected;
                break;
            }
            let encrypted_payload_size = payload_len + 16; // payload + MAC

            // Step 3: Check if we have the full payload
            if self.noise_recv_buf.len() < encrypted_payload_size {
                // Save the header for next poll — nonce is safe because we already
                // drained the encrypted header bytes and incremented the nonce
                self.pending_header = Some(header_plain);
                break;
            }

            // Clear pending header — we're processing this frame now
            self.pending_header = None;

            // Step 4: Decrypt the payload
            let payload_cipher = self.noise_recv_buf[..encrypted_payload_size].to_vec();
            self.noise_recv_buf.drain(..encrypted_payload_size);

            let payload_plain = match self.channel.noise_session_mut().decrypt(&payload_cipher) {
                Ok(p) => p,
                Err(e) => {
                    log::error!("SV2: Noise decrypt payload failed: {}", e);
                    events.push(Sv2Event::Disconnected(format!(
                        "Noise payload decrypt: {}",
                        e
                    )));
                    self.state = ClientState::Disconnected;
                    break;
                }
            };

            // Reconstruct the full SV2 frame (header + payload) and feed to channel
            let mut sv2_frame = Vec::with_capacity(6 + payload_plain.len());
            sv2_frame.extend_from_slice(&header_plain);
            sv2_frame.extend_from_slice(&payload_plain);

            log::info!(
                "SV2: decrypted frame: msg_type=0x{:02x} payload={}B",
                header_plain[2],
                payload_plain.len()
            );

            let channel_events = self.channel.feed_data(&sv2_frame);

            for event in &channel_events {
                match event {
                    Sv2Event::Connected => {
                        log::info!("SV2: SetupConnection accepted, opening channel");
                        let open_msg = self.channel.make_open_channel();
                        if let Err(e) = self.send_noise_frame(&open_msg) {
                            log::error!("SV2: failed to send OpenChannel: {}", e);
                        }
                    }
                    Sv2Event::NewJob { .. } => {
                        self.state = ClientState::Mining;
                    }
                    Sv2Event::Disconnected(reason) => {
                        log::error!("SV2: disconnected: {}", reason);
                        self.state = ClientState::Disconnected;
                    }
                    _ => {}
                }
            }
            events.extend(channel_events);
        }

        events
    }

    /// Disconnect from the pool
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.channel.reset();
        self.noise_recv_buf.clear();
        self.pending_header = None;
        self.state = ClientState::Disconnected;
        log::info!("SV2: disconnected");
    }

    /// Check if mining is active
    pub fn is_mining(&self) -> bool {
        matches!(self.state, ClientState::Mining)
    }

    /// Check if connected (may not be mining yet)
    pub fn is_connected(&self) -> bool {
        matches!(self.state, ClientState::Connected | ClientState::Mining)
    }
}

#[cfg(test)]
impl Sv2Client {
    /// Expose the resolved pinned authority key (as it reached the channel's
    /// Noise session) for plumbing tests.
    fn resolved_authority_key_for_test(&self) -> Option<[u8; 32]> {
        self.channel.noise_session().pool_authority_key
    }

    /// Expose the Noise receive buffer for SV2-8 surplus-carry tests — these are
    /// the bytes the first post-handshake `poll()` will decrypt with nonce 0.
    fn noise_recv_buf_for_test(&self) -> &[u8] {
        &self.noise_recv_buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::parse_pool_authority_pubkey;
    use sha2::{Digest, Sha256};

    const B58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    fn base58check_encode(payload: &[u8]) -> String {
        let h1 = Sha256::digest(payload);
        let h2 = Sha256::digest(h1);
        let mut data = payload.to_vec();
        data.extend_from_slice(&h2[..4]);
        let zeros = data.iter().take_while(|&&b| b == 0).count();
        let mut num = data;
        let mut out: Vec<u8> = Vec::new();
        let mut start = 0;
        while start < num.len() {
            let mut remainder = 0u32;
            let mut all_zero = true;
            for b in num.iter_mut().skip(start) {
                let acc = (remainder << 8) | (*b as u32);
                *b = (acc / 58) as u8;
                remainder = acc % 58;
                if *b != 0 && all_zero {
                    all_zero = false;
                }
            }
            out.push(B58_ALPHABET[remainder as usize]);
            if all_zero {
                while start < num.len() && num[start] == 0 {
                    start += 1;
                }
            }
        }
        for _ in 0..zeros {
            out.push(b'1');
        }
        out.reverse();
        String::from_utf8(out).unwrap()
    }

    fn token_for(key: [u8; 32]) -> String {
        let mut payload = vec![0x01u8, 0x00];
        payload.extend_from_slice(&key);
        base58check_encode(&payload)
    }

    fn base_config() -> Sv2Config {
        Sv2Config {
            host: "pool.example.com".into(),
            port: 34255,
            worker: "worker".into(),
            hashrate_ghs: 500.0,
            timeout_secs: 15,
            pool_authority_pubkey: None,
        }
    }

    /// Default config (no pinned key) → TOFU: the channel's Noise session has no
    /// authority key. Preserves today's default behavior end-to-end.
    #[test]
    fn test_default_config_is_tofu() {
        let client = Sv2Client::new(base_config());
        assert!(client.resolved_authority_key_for_test().is_none());
    }

    /// A valid pinned key in config threads all the way to the Noise session.
    #[test]
    fn test_valid_authority_key_pins_through_to_noise() {
        let key = [0x7Au8; 32];
        let mut cfg = base_config();
        cfg.pool_authority_pubkey = Some(token_for(key));
        // Sanity: the standalone parser agrees on the key.
        assert_eq!(parse_pool_authority_pubkey(&token_for(key)).unwrap(), key);

        let client = Sv2Client::new(cfg);
        assert_eq!(client.resolved_authority_key_for_test(), Some(key));
    }

    /// A malformed pinned key does NOT brick the client — it falls back to TOFU
    /// (fail-open-to-TOFU, never fail-open-to-unsignaled).
    #[test]
    fn test_malformed_authority_key_falls_back_to_tofu() {
        let mut cfg = base_config();
        cfg.pool_authority_pubkey = Some("garbage-not-valid-0OIl".into());
        let client = Sv2Client::new(cfg);
        assert!(
            client.resolved_authority_key_for_test().is_none(),
            "malformed key must degrade to TOFU, not pin garbage"
        );
    }

    /// An empty / whitespace-only string is treated as unset (TOFU).
    #[test]
    fn test_empty_authority_key_is_tofu() {
        let mut cfg = base_config();
        cfg.pool_authority_pubkey = Some("   ".into());
        let client = Sv2Client::new(cfg);
        assert!(client.resolved_authority_key_for_test().is_none());
    }

    // ── SV2-8: post-handshake surplus carry ──────────────────────────────────
    //
    // After the Noise handshake, the pool typically coalesces the first encrypted
    // transport frame onto the SAME TCP read as the 234-byte handshake. The
    // handshake parser consumes EXACTLY MIN_SERVER_RESPONSE bytes, so the surplus
    // must be queued for decryption with receiving_nonce=0 — not dropped, which
    // would desync every subsequent frame.

    /// An exact-length (234-byte) handshake has no surplus.
    #[test]
    fn test_handshake_surplus_empty_for_exact_length() {
        let buf = vec![0u8; MIN_SERVER_RESPONSE];
        assert!(
            handshake_surplus(&buf).is_empty(),
            "exact-length handshake must carry no surplus"
        );
    }

    /// A coalesced read (handshake + trailing transport bytes) yields exactly the
    /// trailing bytes, byte-for-byte, in order.
    #[test]
    fn test_handshake_surplus_returns_trailing_bytes() {
        let trailing: Vec<u8> = (0u8..37).collect();
        let mut buf = vec![0xABu8; MIN_SERVER_RESPONSE];
        buf.extend_from_slice(&trailing);

        let surplus = handshake_surplus(&buf);
        assert_eq!(
            surplus.len(),
            trailing.len(),
            "surplus length must equal the coalesced trailing length"
        );
        assert_eq!(
            surplus,
            &trailing[..],
            "surplus must be the trailing bytes verbatim (not dropped/reordered)"
        );
    }

    /// End-to-end of the connect() carry step: trailing post-handshake bytes land
    /// in the Noise receive buffer (where the first poll() decrypts them with
    /// nonce 0), while an exact-length handshake leaves the buffer empty.
    #[test]
    fn test_coalesced_surplus_is_queued_not_dropped() {
        // Normal exact-length handshake: nothing queued.
        let mut client = Sv2Client::new(base_config());
        let exact = vec![0u8; MIN_SERVER_RESPONSE];
        let surplus_exact = handshake_surplus(&exact);
        if !surplus_exact.is_empty() {
            client.noise_recv_buf.extend_from_slice(surplus_exact);
        }
        assert!(
            client.noise_recv_buf_for_test().is_empty(),
            "exact-length handshake must not queue any receive bytes"
        );

        // Coalesced handshake + first transport frame: the frame bytes are queued
        // verbatim for the first poll() to decrypt with nonce 0.
        let first_frame: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05];
        let mut coalesced = vec![0x5Au8; MIN_SERVER_RESPONSE];
        coalesced.extend_from_slice(&first_frame);

        let mut client2 = Sv2Client::new(base_config());
        let surplus = handshake_surplus(&coalesced);
        if !surplus.is_empty() {
            client2.noise_recv_buf.extend_from_slice(surplus);
        }
        assert_eq!(
            client2.noise_recv_buf_for_test(),
            &first_frame[..],
            "coalesced first transport frame must be preserved in the receive \
             buffer (queued for nonce-0 decryption), never dropped"
        );
    }

    /// The handshake read buffer is sized so the handshake itself always fits
    /// without a resize, and the grow-before-read invariant guarantees we never
    /// hand `read()` a zero-length slice (the >512 coalesced EOF mis-detection).
    #[test]
    fn test_handshake_read_buf_len_covers_handshake() {
        assert!(
            HANDSHAKE_READ_BUF_LEN >= MIN_SERVER_RESPONSE,
            "read buffer must hold at least the full handshake without growing"
        );
    }
}

// ===========================================================================
// SV2-e2e: encrypted end-to-end harness
// ===========================================================================
//
// Pairs an in-process responder `NoiseSession` (the "pool"/server side) with the
// real client-side `Sv2MiningChannel` + initiator `NoiseSession` — i.e. the exact
// pieces `Sv2Client` composes — and drives the FULL path through REAL ENCRYPTED
// transport frames:
//
//   Noise_NX handshake (both parties)
//     -> SetupConnection            (client -> server, encrypted)
//     <- SetupConnection.Success    (server -> client, encrypted) -> Connected
//     -> OpenStandardMiningChannel  (client -> server, encrypted)
//     <- OpenChannel.Success        (server -> client, encrypted) -> Mining{ch}
//     <- NewMiningJob + SetNewPrevHash (+ SetTarget) (server -> client)
//     -> SubmitSharesStandard       (client -> server, encrypted)
//     <- SubmitShares.Success | SubmitShares.Error  (BOTH outcomes covered)
//
// This closes the recon gap: the existing channel tests feed PLAINTEXT frames to
// `feed_data`, bypassing Noise. Here every wire byte between the two parties goes
// through `NoiseSession::encrypt`/`decrypt`, using the SAME two-block transport
// framing the production `Sv2Client::send_noise_frame` / `poll` use (encrypt the
// 6-byte SV2 header as one AEAD block, then the payload as a second block; decrypt
// the 22-byte header block first, parse `payload_len`, then decrypt the payload).
//
// WHY channel-level and not `Sv2Client::connect()`/`poll()` directly: those methods
// are hardwired to `std::net::TcpStream` with no injectable transport seam, and the
// harness is deliberately socket-free + timing-free (deterministic: the two state
// machines are driven by hand-passing the encrypted byte buffers). The only test
// seam added is exposing the existing `noise::build_server_response` fixture
// `pub(crate)` (test-only) so the responder crypto has a single source of truth;
// no production API, the Noise/cert fail-closed posture, the BIP320 mask, or
// `set_test_keys`' `#[cfg(test)]` gating is touched. This is honest TEST
// infrastructure — it does NOT make SV2 "live-proven".
#[cfg(test)]
mod e2e_encrypted {
    use crate::channel::{ChannelState, Sv2Event, Sv2MiningChannel};
    use crate::framing::Sv2Frame;
    use crate::noise::tests::build_server_response;
    use crate::noise::NoiseSession;
    use crate::types::{mining, EXTENSION_TYPE_CHANNEL_MSG, EXTENSION_TYPE_MINING};

    // Encrypted-header block size on the wire: 6-byte SV2 header + 16-byte AEAD tag.
    const ENC_HEADER: usize = 6 + 16;

    /// Wrap a serialized SV2 message body in a complete plaintext SV2 frame.
    fn build_frame(ext: u16, msg_type: u8, payload: Vec<u8>) -> Vec<u8> {
        Sv2Frame::new(ext, msg_type, payload).to_bytes()
    }

    /// Encrypt one plaintext SV2 frame into the on-wire two-block Noise transport
    /// form EXACTLY as `Sv2Client::send_noise_frame` does: header block, then
    /// payload block, each its own AEAD invocation (separate auto-incremented
    /// nonces). Works for either direction (client->server or server->client).
    fn transport_encrypt(tx: &mut NoiseSession, sv2_frame: &[u8]) -> Vec<u8> {
        assert!(sv2_frame.len() >= 6, "frame must carry a 6-byte SV2 header");
        let header = &sv2_frame[..6];
        let payload = &sv2_frame[6..];
        let mut wire = tx.encrypt(header).expect("transport encrypt header");
        let mut enc_payload = tx.encrypt(payload).expect("transport encrypt payload");
        wire.append(&mut enc_payload);
        wire
    }

    /// Decrypt one on-wire Noise transport frame back into a plaintext SV2 frame
    /// EXACTLY as `Sv2Client::poll` does: decrypt the 22-byte header block, parse
    /// `payload_len` from the SV2 header, decrypt the payload block, reassemble.
    /// Returns `(reconstructed_sv2_frame, bytes_consumed)`.
    fn transport_decrypt(rx: &mut NoiseSession, wire: &[u8]) -> (Vec<u8>, usize) {
        assert!(wire.len() >= ENC_HEADER, "need full encrypted header block");
        let header_plain = rx.decrypt(&wire[..ENC_HEADER]).expect("decrypt header");
        assert_eq!(header_plain.len(), 6, "SV2 header must decrypt to 6 bytes");
        let payload_len =
            u32::from_le_bytes([header_plain[3], header_plain[4], header_plain[5], 0]) as usize;
        let end = ENC_HEADER + payload_len + 16;
        assert!(wire.len() >= end, "need full encrypted payload block");
        let payload_plain = rx.decrypt(&wire[ENC_HEADER..end]).expect("decrypt payload");
        let mut frame = Vec::with_capacity(6 + payload_plain.len());
        frame.extend_from_slice(&header_plain);
        frame.extend_from_slice(&payload_plain);
        (frame, end)
    }

    /// Run the real Noise_NX handshake between the production client-side
    /// `Sv2MiningChannel` (initiator) and an in-process responder, and return both
    /// parties in transport mode. Uses the existing `build_server_response` fixture
    /// (deterministic keys) for the responder side.
    fn encrypted_handshake() -> (Sv2MiningChannel, NoiseSession) {
        // The real client wrapper (Sv2MiningChannel) — TOFU (no pinned key), the
        // default public-beta posture for a channel without sv2_authority_pubkey.
        let mut client = Sv2MiningChannel::new("bc1qexampleworker.dcentaxe", 500.0);

        // Step 1: initiator -> e (the exact call Sv2Client::connect() makes).
        let e_pub_vec = client
            .noise_session_mut()
            .initiator_handshake_start([0xC7u8; 64])
            .expect("client handshake start");
        let mut e_pub = [0u8; 64];
        e_pub.copy_from_slice(&e_pub_vec);

        // Capture the client's post-`-> e` handshake hash + (constant) chaining key
        // so the responder fixture can run the matching h/ck evolution.
        let client_h = *client.noise_session().handshake_hash();
        let client_ck = client.noise_session().chaining_key_for_test();

        // Step 2: responder builds <- e, ee, s, es with deterministic test keys.
        let (server_msg, server_recv_key, server_send_key) = build_server_response(
            &e_pub,
            &client_h,
            &client_ck,
            [0xE5u8; 64],
            [0x5Au8; 64],
            None,
        );

        // Client finishes the handshake (TOFU: cert parsed, not pinned).
        client
            .noise_session_mut()
            .initiator_handshake_finish(&server_msg)
            .expect("client handshake finish");

        // Build the responder transport session from the agreed keys. Client sends
        // with k1 (== server_recv) and receives with k2 (== server_send); the server
        // therefore sends with server_send and receives with server_recv.
        let mut server = NoiseSession::new();
        server.set_transport_keys(server_send_key, server_recv_key, [0u8; 32]);

        (client, server)
    }

    /// Result of driving the encrypted path all the way to a submitted share that
    /// the server has decrypted and validated — ready for the success/reject fork.
    struct Mined {
        client: Sv2MiningChannel,
        server: NoiseSession,
        channel_id: u32,
        seq: u32,
    }

    /// Drive: handshake -> SetupConnection(.Success) -> OpenChannel(.Success) ->
    /// NewMiningJob + SetNewPrevHash (+ SetTarget) -> SubmitSharesStandard, all over
    /// REAL encrypted frames, asserting each integrated transition. Returns the live
    /// client/server pair plus the channel_id and the submitted sequence number.
    fn drive_to_submitted_share() -> Mined {
        const CHANNEL_ID: u32 = 7;
        const JOB_ID: u32 = 0x0000_1234;

        let (mut client, mut server) = encrypted_handshake();
        assert!(
            client.noise_session().is_transport(),
            "client must be in Noise transport mode after handshake"
        );
        assert!(
            server.is_transport(),
            "server must be in Noise transport mode after handshake"
        );

        // ---- SetupConnection (client -> server, encrypted) -------------------
        let setup = client.make_setup_connection();
        assert_eq!(client.state, ChannelState::SettingUp);
        let wire = transport_encrypt(client.noise_session_mut(), &setup);
        let (seen, consumed) = transport_decrypt(&mut server, &wire);
        assert_eq!(consumed, wire.len(), "server must consume the whole frame");
        let (sf, _) = Sv2Frame::from_bytes(&seen).expect("decode SetupConnection");
        assert_eq!(sf.header.msg_type, mining::SETUP_CONNECTION);
        assert_eq!(sf.header.extension_type, EXTENSION_TYPE_MINING);

        // ---- SetupConnection.Success (server -> client, encrypted) -----------
        let mut succ_payload = Vec::new();
        succ_payload.extend_from_slice(&2u16.to_le_bytes()); // used_version
        succ_payload.extend_from_slice(&0u32.to_le_bytes()); // flags
        let succ = build_frame(
            EXTENSION_TYPE_MINING,
            mining::SETUP_CONNECTION_SUCCESS,
            succ_payload,
        );
        let wire = transport_encrypt(&mut server, &succ);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);
        assert!(
            events.iter().any(|e| matches!(e, Sv2Event::Connected)),
            "decrypted SetupConnection.Success must emit Connected, got {:?}",
            events
        );

        // ---- OpenStandardMiningChannel (client -> server) --------------------
        // Mirrors Sv2Client::poll(): on Connected the client opens the channel.
        let open = client.make_open_channel();
        assert_eq!(client.state, ChannelState::OpeningChannel);
        let wire = transport_encrypt(client.noise_session_mut(), &open);
        let (seen, _) = transport_decrypt(&mut server, &wire);
        let (of, _) = Sv2Frame::from_bytes(&seen).expect("decode OpenChannel");
        assert_eq!(of.header.msg_type, mining::OPEN_STANDARD_MINING_CHANNEL);
        let req_id =
            u32::from_le_bytes([of.payload[0], of.payload[1], of.payload[2], of.payload[3]]);
        assert_eq!(req_id, 1, "first OpenChannel request_id == 1");

        // ---- OpenChannel.Success (server -> client) --------------------------
        let mut osucc_payload = Vec::new();
        osucc_payload.extend_from_slice(&req_id.to_le_bytes());
        osucc_payload.extend_from_slice(&CHANNEL_ID.to_le_bytes());
        osucc_payload.extend_from_slice(&[0xFFu8; 32]); // max target
        let osucc = build_frame(
            EXTENSION_TYPE_MINING,
            mining::OPEN_STANDARD_MINING_CHANNEL_SUCCESS,
            osucc_payload,
        );
        let wire = transport_encrypt(&mut server, &osucc);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Sv2Event::DifficultyChanged(_))),
            "OpenChannel.Success must emit DifficultyChanged, got {:?}",
            events
        );
        assert_eq!(client.channel_id(), Some(CHANNEL_ID));
        assert!(client.is_mining());

        // ---- NewMiningJob (future) then SetNewPrevHash (server -> client) -----
        let mut job_payload = Vec::new();
        job_payload.extend_from_slice(&CHANNEL_ID.to_le_bytes());
        job_payload.extend_from_slice(&JOB_ID.to_le_bytes());
        job_payload.push(0x00); // min_ntime absent => future job
        job_payload.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        job_payload.extend_from_slice(&[0xBBu8; 32]); // merkle_root
        let job = build_frame(EXTENSION_TYPE_MINING, mining::NEW_MINING_JOB, job_payload);
        let wire = transport_encrypt(&mut server, &job);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);
        assert!(
            events.is_empty(),
            "future job must buffer (no event) until SetNewPrevHash, got {:?}",
            events
        );

        let mut ph_payload = Vec::new();
        ph_payload.extend_from_slice(&CHANNEL_ID.to_le_bytes());
        ph_payload.extend_from_slice(&JOB_ID.to_le_bytes());
        ph_payload.extend_from_slice(&[0xDDu8; 32]); // prev_hash
        ph_payload.extend_from_slice(&1_700_000_000u32.to_le_bytes()); // min_ntime
        ph_payload.extend_from_slice(&0x1903_a30cu32.to_le_bytes()); // nbits
        let ph = build_frame(EXTENSION_TYPE_MINING, mining::SET_NEW_PREV_HASH, ph_payload);
        let wire = transport_encrypt(&mut server, &ph);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);
        let (job_id, ntime, version) = events
            .iter()
            .find_map(|e| match e {
                Sv2Event::NewJob {
                    job_id,
                    ntime,
                    version,
                    prev_hash,
                    clean_jobs,
                    ..
                } => {
                    assert_eq!(prev_hash, &[0xDDu8; 32], "job must carry the new prev_hash");
                    assert!(
                        *clean_jobs,
                        "a genuine tip change must signal clean_jobs=true"
                    );
                    Some((*job_id, *ntime, *version))
                }
                _ => None,
            })
            .expect("SetNewPrevHash must pair the buffered job into a NewJob");
        assert_eq!(job_id, JOB_ID);

        // ---- SetTarget (server -> client) ------------------------------------
        let mut tgt_payload = Vec::new();
        tgt_payload.extend_from_slice(&CHANNEL_ID.to_le_bytes());
        tgt_payload.extend_from_slice(&[0xFFu8; 32]); // max_target (LE)
        let settarget = build_frame(EXTENSION_TYPE_MINING, mining::SET_TARGET, tgt_payload);
        let wire = transport_encrypt(&mut server, &settarget);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Sv2Event::DifficultyChanged(_))),
            "SetTarget must emit DifficultyChanged, got {:?}",
            events
        );

        // ---- SubmitSharesStandard (client -> server, encrypted) --------------
        let nonce = 0x0BAD_F00Du32;
        let share = client.make_submit_share(CHANNEL_ID, job_id, nonce, ntime, version);
        let seq = client.sequence_number();
        assert_eq!(seq, 1, "first submission sequence_number == 1");
        let wire = transport_encrypt(client.noise_session_mut(), &share);
        let (seen, _) = transport_decrypt(&mut server, &wire);
        let (shf, _) = Sv2Frame::from_bytes(&seen).expect("decode SubmitSharesStandard");
        assert_eq!(shf.header.msg_type, mining::SUBMIT_SHARES_STANDARD);
        assert_eq!(
            shf.header.extension_type, EXTENSION_TYPE_CHANNEL_MSG,
            "share submit is a channel-scoped message"
        );
        assert_eq!(
            shf.payload.len(),
            24,
            "SubmitSharesStandard payload is 24 bytes"
        );
        let got_channel = u32::from_le_bytes([
            shf.payload[0],
            shf.payload[1],
            shf.payload[2],
            shf.payload[3],
        ]);
        let got_seq = u32::from_le_bytes([
            shf.payload[4],
            shf.payload[5],
            shf.payload[6],
            shf.payload[7],
        ]);
        let got_job = u32::from_le_bytes([
            shf.payload[8],
            shf.payload[9],
            shf.payload[10],
            shf.payload[11],
        ]);
        let got_nonce = u32::from_le_bytes([
            shf.payload[12],
            shf.payload[13],
            shf.payload[14],
            shf.payload[15],
        ]);
        assert_eq!(got_channel, CHANNEL_ID, "server sees the open channel_id");
        assert_eq!(got_seq, seq, "server sees the client's sequence_number");
        assert_eq!(
            got_job, JOB_ID,
            "server sees the job_id the share was built for"
        );
        assert_eq!(got_nonce, nonce, "server sees the submitted nonce");

        Mined {
            client,
            server,
            channel_id: CHANNEL_ID,
            seq,
        }
    }

    /// The Noise_NX handshake completes for BOTH parties and a round-trip of
    /// encrypted application data works in each direction (channel binding proof
    /// that the harness pairing is real, not two unrelated sessions).
    #[test]
    fn e2e_encrypted_handshake_completes_both_parties() {
        let (mut client, mut server) = encrypted_handshake();
        assert!(client.noise_session().is_transport());
        assert!(server.is_transport());

        // client -> server
        let c2s = client.noise_session_mut().encrypt(b"hello pool").unwrap();
        assert_eq!(server.decrypt(&c2s).unwrap(), b"hello pool");
        // server -> client
        let s2c = server.encrypt(b"welcome miner").unwrap();
        assert_eq!(
            client.noise_session_mut().decrypt(&s2c).unwrap(),
            b"welcome miner"
        );
    }

    /// FULL encrypted flow ending in SubmitShares.Success: the accepted share is
    /// surfaced as `ShareAccepted` carrying the submitted sequence number.
    #[test]
    fn e2e_encrypted_full_flow_share_accepted() {
        let Mined {
            mut client,
            mut server,
            channel_id,
            seq,
        } = drive_to_submitted_share();

        // ---- SubmitShares.Success (server -> client, encrypted) --------------
        let mut ok_payload = Vec::new();
        ok_payload.extend_from_slice(&channel_id.to_le_bytes());
        ok_payload.extend_from_slice(&seq.to_le_bytes()); // last_sequence_number
        ok_payload.extend_from_slice(&1u32.to_le_bytes()); // new_submits_accepted_count
        ok_payload.extend_from_slice(&0u64.to_le_bytes()); // new_shares_sum
        let ok = build_frame(
            EXTENSION_TYPE_MINING,
            mining::SUBMIT_SHARES_SUCCESS,
            ok_payload,
        );
        let wire = transport_encrypt(&mut server, &ok);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);

        let accepted = events
            .iter()
            .find_map(|e| match e {
                Sv2Event::ShareAccepted {
                    sequence_number,
                    accepted_count,
                } => Some((*sequence_number, *accepted_count)),
                _ => None,
            })
            .expect("SubmitShares.Success must surface a ShareAccepted event");
        assert_eq!(
            accepted.0, seq,
            "accepted up to the submitted sequence number"
        );
        assert_eq!(accepted.1, 1, "one share accepted");
        // The success path must not also report a rejection.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Sv2Event::ShareRejected { .. })),
            "accepted path must not emit ShareRejected"
        );
    }

    /// FULL encrypted flow ending in SubmitShares.Error: the rejected share is
    /// SURFACED as `ShareRejected` (not silently dropped, not a panic) carrying the
    /// pool's reason and the submitted sequence number.
    #[test]
    fn e2e_encrypted_full_flow_share_rejected() {
        let Mined {
            mut client,
            mut server,
            channel_id,
            seq,
        } = drive_to_submitted_share();

        // ---- SubmitShares.Error (server -> client, encrypted) ----------------
        // Payload: channel_id u32 + sequence_number u32 + error_code STR0_255.
        let reason = "difficulty-too-low";
        let mut err_payload = Vec::new();
        err_payload.extend_from_slice(&channel_id.to_le_bytes());
        err_payload.extend_from_slice(&seq.to_le_bytes());
        let rb = reason.as_bytes();
        err_payload.push(rb.len() as u8); // STR0_255 length prefix
        err_payload.extend_from_slice(rb);
        let err = build_frame(
            EXTENSION_TYPE_MINING,
            mining::SUBMIT_SHARES_ERROR,
            err_payload,
        );
        let wire = transport_encrypt(&mut server, &err);
        let (recon, _) = transport_decrypt(client.noise_session_mut(), &wire);
        let events = client.feed_data(&recon);

        let rejected = events
            .iter()
            .find_map(|e| match e {
                Sv2Event::ShareRejected {
                    sequence_number,
                    reason,
                } => Some((*sequence_number, reason.clone())),
                _ => None,
            })
            .expect("SubmitShares.Error must surface a ShareRejected event (not be dropped)");
        assert_eq!(
            rejected.0, seq,
            "rejection references the submitted sequence number"
        );
        assert!(
            rejected.1.contains("difficulty-too-low"),
            "rejection must carry the pool's reason, got {:?}",
            rejected.1
        );
        // A reject must NOT masquerade as an acceptance.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Sv2Event::ShareAccepted { .. })),
            "rejected path must not emit ShareAccepted"
        );
    }
}
