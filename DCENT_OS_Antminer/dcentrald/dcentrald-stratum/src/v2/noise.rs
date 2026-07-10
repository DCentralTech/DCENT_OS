//! Noise Protocol Framework — Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256
//!
//! Implements the full Noise_NX handshake as an initiator (client), plus
//! ChaChaPoly1305 transport encryption/decryption for post-handshake messages.
//!
//! # Cipher suite
//! `Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256`
//!
//! # Handshake pattern (initiator perspective)
//! ```text
//! -> e                    (client sends ephemeral public key, 64 bytes EllSwift)
//! <- e, ee, s, es         (server sends ephemeral + static keys, derives shared secrets)
//! ```
//!
//! # Key encoding
//! Public keys on the wire use **ElligatorSwift** encoding (64 bytes, BIP324).
//! The `secp256k1` crate (libsecp256k1 C bindings) provides native EllSwift support.
//!
//! # ECDH operations
//! - `ee` DH: BIP324 tagged EllSwift ECDH (`secp256k1_ellswift_xdh`)
//! - `es` DH: Standard secp256k1 ECDH (server static key is not EllSwift-encoded)
//!
//! # Certificate validation
//! After handshake, the server sends SIGNATURE_NOISE_MESSAGE (encrypted).
//! Currently TOFU (trust-on-first-use) — certificate is parsed and logged
//! but the BIP340 Schnorr signature is not verified.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use sha2::{Digest, Sha256};

// secp256k1 C bindings for EllSwift and ECDH
use secp256k1::ellswift::{ElligatorSwift, ElligatorSwiftParty};
use secp256k1::{Secp256k1, SecretKey};

/// Protocol name — must match SRI (Stratum Reference Implementation) exactly.
/// Capital 'S' in Secp256k1, WITH '+EllSwift' suffix.
/// SHA256 of this = [46,180,120,129,32,142,158,238,31,102,159,103,198,110,231,14,
///                   169,234,136,9,13,80,63,232,48,220,75,200,62,41,191,16]
/// This is longer than 32 bytes, so h is initialised as SHA256(protocol_name).
const PROTOCOL_NAME: &[u8] = b"Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";

/// ElligatorSwift-encoded secp256k1 public key size (64 bytes).
pub const ELLSWIFT_SIZE: usize = 64;

/// Server static key size — EllSwift encoded (64 bytes), same as ephemeral.
pub const STATIC_KEY_SIZE: usize = 64;

/// Maximum encrypted message size (16KB payload + 16 byte AEAD tag)
pub const MAX_ENCRYPTED_SIZE: usize = 16384 + 16;

/// SIGNATURE_NOISE_MESSAGE size: version(2) + valid_from(4) + not_valid_after(4) + signature(64)
const SIGNATURE_NOISE_MESSAGE_SIZE: usize = 74;

/// Minimum server response size:
/// server_e(64) + encrypted_s(64+16=80) + encrypted_cert(74+16=90) = 234 bytes
pub const MIN_SERVER_RESPONSE: usize =
    ELLSWIFT_SIZE + (STATIC_KEY_SIZE + 16) + (SIGNATURE_NOISE_MESSAGE_SIZE + 16);

/// Noise handshake state
#[derive(Debug, Clone, PartialEq)]
pub enum NoiseState {
    /// Initial state — ready to start handshake
    Init,
    /// Sent first handshake message (-> e), waiting for server response
    WaitingForResponse,
    /// Handshake complete — transport encryption active
    Transport,
    /// Handshake failed — session unusable
    Failed(String),
}

/// SIGNATURE_NOISE_MESSAGE — server certificate (SV2 extension)
#[derive(Debug)]
pub struct SignatureNoiseMessage {
    pub version: u16,
    pub valid_from: u32,
    pub not_valid_after: u32,
    pub signature: [u8; 64], // BIP340 Schnorr signature
}

impl SignatureNoiseMessage {
    /// Verify the BIP340 Schnorr signature against a pinned pool authority key.
    ///
    /// # Spec
    ///
    /// Per the Stratum V2 spec §4.5.2 `SignatureNoiseMessage` and the SRI
    /// `noise_sv2` crate, the signed artifact is:
    ///
    /// ```text
    ///   SHA256( version(U16 LE) || valid_from(U32 LE)
    ///           || not_valid_after(U32 LE) || server_static_key )
    /// ```
    ///
    /// `server_static_key` is the **exact bytes the server committed to on
    /// the wire** — in the current `…Secp256k1+EllSwift…` cipher suite that
    /// is the **64-byte ElligatorSwift encoding** of the server static key
    /// (the older `…25519…` suite used a 32-byte raw key — that form is not
    /// what SRI/Braiins speak today). The verifier MUST sign over the wire
    /// bytes; it cannot substitute the decoded point because EllSwift is a
    /// one-way-on-the-wire commitment.
    ///
    /// The 32-byte digest is the BIP340 message `m` (the `secp256k1` crate's
    /// `verify_schnorr` consumes a pre-reduced 32-byte `Message`, matching
    /// SRI's `Message::from_hashed_data`-equivalent convention).
    ///
    /// Verification key is the **pool authority** x-only key (pinned from
    /// the SV2 URL, see [`crate::v2::auth`]), NOT the server static key.
    ///
    /// # Arguments
    /// * `authority_key` — pool authority public key (x-only, 32 bytes),
    ///   parsed from the SV2 URL.
    /// * `server_static_wire` — the server static key **as it appeared on
    ///   the wire** (64-byte EllSwift encoding for this cipher suite).
    pub fn verify_schnorr(
        &self,
        authority_key: &[u8; 32],
        server_static_wire: &[u8],
    ) -> Result<(), String> {
        use secp256k1::schnorr::Signature;
        use secp256k1::{Message, XOnlyPublicKey};

        if server_static_wire.len() != STATIC_KEY_SIZE {
            return Err(format!(
                "server static key wrong length: {} (expected {} EllSwift bytes)",
                server_static_wire.len(),
                STATIC_KEY_SIZE
            ));
        }

        // Build the signed buffer: header fields (LE) || full wire static key.
        let mut msg_data = Vec::with_capacity(2 + 4 + 4 + STATIC_KEY_SIZE);
        msg_data.extend_from_slice(&self.version.to_le_bytes());
        msg_data.extend_from_slice(&self.valid_from.to_le_bytes());
        msg_data.extend_from_slice(&self.not_valid_after.to_le_bytes());
        msg_data.extend_from_slice(server_static_wire);

        let msg_hash = Sha256::digest(&msg_data);
        let message = Message::from_digest_slice(&msg_hash)
            .map_err(|e| format!("invalid message hash: {}", e))?;

        let pubkey = XOnlyPublicKey::from_slice(authority_key)
            .map_err(|e| format!("invalid authority key: {}", e))?;
        let sig = Signature::from_slice(&self.signature)
            .map_err(|e| format!("invalid signature: {}", e))?;

        let secp = Secp256k1::verification_only();
        secp.verify_schnorr(&sig, &message, &pubkey)
            .map_err(|e| format!("signature verification failed: {}", e))
    }

    /// Verify certificate validity window against `now` (UNIX epoch seconds).
    ///
    /// SV2 spec §4.5.2: the certificate is invalid before `valid_from` and
    /// after `not_valid_after`. A pinned-authority client MUST reject an
    /// expired or not-yet-valid certificate.
    pub fn verify_validity(&self, now_unix_s: u64) -> Result<(), String> {
        let from = self.valid_from as u64;
        let until = self.not_valid_after as u64;
        if now_unix_s < from {
            return Err(format!(
                "certificate not yet valid (valid_from={}, now={})",
                from, now_unix_s
            ));
        }
        if now_unix_s > until {
            return Err(format!(
                "certificate expired (not_valid_after={}, now={})",
                until, now_unix_s
            ));
        }
        Ok(())
    }

    /// Parse from a 74-byte decrypted payload.
    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < SIGNATURE_NOISE_MESSAGE_SIZE {
            return Err(format!(
                "SIGNATURE_NOISE_MESSAGE too short: {} bytes, need {}",
                data.len(),
                SIGNATURE_NOISE_MESSAGE_SIZE
            ));
        }
        let version = u16::from_le_bytes([data[0], data[1]]);
        let valid_from = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
        let not_valid_after = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[10..74]);
        Ok(Self {
            version,
            valid_from,
            not_valid_after,
            signature,
        })
    }
}

/// Noise transport session.
///
/// # Lifecycle
/// 1. Create with `NoiseSession::new()`
/// 2. Call `initiator_handshake_start(rng_seed)` → send returned bytes to server
/// 3. Receive server response → call `initiator_handshake_finish(server_msg)`
/// 4. Use `encrypt()` / `decrypt()` for all subsequent messages
///
/// For testing without a real server, call `set_test_keys()` to skip the handshake.
pub struct NoiseSession {
    /// Current handshake/transport state
    pub state: NoiseState,

    /// Key for encrypting outgoing messages (k1 from final HKDF split)
    sending_key: [u8; 32],
    /// Key for decrypting incoming messages (k2 from final HKDF split)
    receiving_key: [u8; 32],
    /// Nonce counter for sending — incremented per message, never reused
    sending_nonce: u64,
    /// Nonce counter for receiving — incremented per message, never reused
    receiving_nonce: u64,
    /// Running handshake hash h (also serves as channel binding after handshake)
    handshake_hash: [u8; 32],

    // Handshake-phase state (only valid during Init / WaitingForResponse):
    /// Noise chaining key ck — evolves through each DH step
    chaining_key: [u8; 32],
    /// Ephemeral secret key stored as raw bytes; zeroed after handshake
    ephemeral_secret: Option<[u8; 32]>,
    /// Our EllSwift encoding — needed for BIP324 tagged ECDH
    our_ellswift: Option<[u8; 64]>,
    /// Optional pool authority public key (x-only, 32 bytes) for BIP340 Schnorr
    /// verification of the server certificate. None = TOFU mode.
    pub pool_authority_key: Option<[u8; 32]>,

    // ── Metadata tracking ────────────────────────────────────────────────────
    /// Total bytes encrypted (plaintext input to encrypt())
    bytes_encrypted_total: u64,
    /// Total bytes decrypted (plaintext output from decrypt())
    bytes_decrypted_total: u64,
    /// Server's static public key (first 32 bytes, captured during handshake)
    server_static_key: Option<[u8; 32]>,
    /// Certificate valid_from timestamp (UNIX epoch, from SIGNATURE_NOISE_MESSAGE)
    certificate_valid_from: u64,
    /// Certificate not_valid_after timestamp (UNIX epoch, from SIGNATURE_NOISE_MESSAGE)
    certificate_not_after: u64,
    /// Instant when initiator_handshake_start() was called
    handshake_start_time: Option<std::time::Instant>,
    /// Handshake round-trip latency in milliseconds
    handshake_latency_ms: Option<u64>,

    /// **INSECURE** plaintext passthrough mode. When `true`, `encrypt`
    /// and `decrypt` are identity functions (no ChaChaPoly, no tag) so
    /// SV2 frames go on the wire in cleartext. Only ever set by the
    /// explicit `DCENT_SV2_INSECURE_NO_NOISE` operator opt-out, exclusively
    /// for lab testing against a mock pool. Never reachable by default.
    plaintext_passthrough: bool,
}

impl NoiseSession {
    /// Create a new session in `Init` state.
    pub fn new() -> Self {
        Self {
            state: NoiseState::Init,
            sending_key: [0u8; 32],
            receiving_key: [0u8; 32],
            sending_nonce: 0,
            receiving_nonce: 0,
            handshake_hash: [0u8; 32],
            chaining_key: [0u8; 32],
            ephemeral_secret: None,
            our_ellswift: None,
            pool_authority_key: None,
            bytes_encrypted_total: 0,
            bytes_decrypted_total: 0,
            server_static_key: None,
            certificate_valid_from: 0,
            certificate_not_after: 0,
            handshake_start_time: None,
            handshake_latency_ms: None,
            plaintext_passthrough: false,
        }
    }

    /// **INSECURE — explicit operator opt-out only.** Put the session
    /// directly into transport state with cleartext passthrough: no Noise
    /// handshake is performed and `encrypt`/`decrypt` are identity. SV2
    /// frames travel in the clear. Only the `DCENT_SV2_INSECURE_NO_NOISE`
    /// gate calls this, and only for lab testing against a mock pool the
    /// operator controls. Unreachable on any default path.
    pub fn enable_insecure_plaintext_passthrough(&mut self) {
        self.plaintext_passthrough = true;
        self.state = NoiseState::Transport;
        tracing::error!(
            "Noise: *** PLAINTEXT PASSTHROUGH ENABLED — SV2 TRANSPORT IS UNENCRYPTED *** \
             (DCENT_SV2_INSECURE_NO_NOISE) — lab/mock use only"
        );
    }

    /// Whether this session is in the insecure cleartext-passthrough mode.
    pub fn is_plaintext_passthrough(&self) -> bool {
        self.plaintext_passthrough
    }

    // =========================================================================
    // Handshake — Step 1: initiator sends -> e
    // =========================================================================

    /// Begin the Noise_NX handshake as the initiator (client).
    ///
    /// Initialises the handshake state from the protocol name, generates an
    /// ephemeral secp256k1 keypair from `rng_seed`, encodes the public key
    /// as ElligatorSwift (64 bytes), mixes it into the handshake hash, and
    /// returns the first handshake message to send (`-> e`).
    ///
    /// # Arguments
    /// * `rng_seed` — 64 random bytes from `esp_random()`.
    ///   Bytes [0..32] are used as the ephemeral secret key.
    ///   Bytes [32..64] are used as the EllSwift encoding randomizer.
    ///
    /// # Errors
    /// Returns `Err` if called in any state other than `Init`, or if `rng_seed`
    /// produces an invalid secp256k1 secret key (astronomically unlikely).
    pub fn initiator_handshake_start(&mut self, rng_seed: [u8; 64]) -> Result<Vec<u8>, String> {
        if self.state != NoiseState::Init {
            return Err(format!(
                "Noise: handshake_start called in wrong state: {:?}",
                self.state
            ));
        }

        // Record handshake start time for latency measurement
        self.handshake_start_time = Some(std::time::Instant::now());

        // ── Initialise h and ck from protocol name ───────────────────────────
        // Noise spec §5.2: if len(protocol_name) <= HASHLEN, h = pad to 32;
        // else h = HASH(protocol_name). Our name is >32 bytes → hash it.
        let h = {
            let mut hasher = Sha256::new();
            hasher.update(PROTOCOL_NAME);
            let out = hasher.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&out);
            arr
        };
        self.handshake_hash = h;
        self.chaining_key = h; // ck = h initially (per Noise spec)

        // ── MixHash(prologue) — Noise spec §5.3 ──────────────────────────────
        // Even with empty prologue, MixHash must be called.
        // h = SHA256(h || prologue) where prologue = "" for SV2.
        self.mix_hash(b"");

        // ── Generate ephemeral keypair + EllSwift encoding ────────────────────
        let secret_bytes = &rng_seed[..32];
        let mut aux_rand = [0u8; 32];
        aux_rand.copy_from_slice(&rng_seed[32..64]);

        let sk = SecretKey::from_slice(secret_bytes)
            .map_err(|e| format!("Noise: invalid secret key from RNG seed: {}", e))?;

        // Encode our public key as ElligatorSwift (64 bytes)
        // Secp256k1::new() allocates ~1MB context — needed for ecmult_gen tables.
        // This is allocated once during handshake and dropped after.
        let secp = Secp256k1::new();
        let our_es = ElligatorSwift::from_seckey(&secp, sk, Some(aux_rand));
        let e_pub_arr = our_es.to_array();

        // Store ephemeral secret and EllSwift for use in step 2
        let mut e_sec_bytes = [0u8; 32];
        e_sec_bytes.copy_from_slice(secret_bytes);
        self.ephemeral_secret = Some(e_sec_bytes);
        self.our_ellswift = Some(e_pub_arr);

        // mix_hash(e_pub): h = SHA256(h || e_pub)
        self.mix_hash(&e_pub_arr);

        // EncryptAndHash(empty payload) — Noise spec §5.2 message processing.
        // The -> e message has an empty payload. With k=None (no key yet),
        // encrypt_and_hash just does mix_hash(""). SRI: self.encrypt_and_hash(&mut vec![])
        self.mix_hash(b"");

        self.state = NoiseState::WaitingForResponse;
        tracing::info!(
            "Noise: handshake started (EllSwift), e_pub[0..4]={:02x?}",
            &e_pub_arr[..4]
        );
        Ok(e_pub_arr.to_vec())
    }

    // =========================================================================
    // Handshake — Step 2: initiator processes <- e, ee, s, es
    // =========================================================================

    /// Complete the Noise_NX handshake by processing the server's response.
    ///
    /// Parses the server message (EllSwift encoding), performs the two DH
    /// operations (ee via EllSwift ECDH, es via standard ECDH), evolves the
    /// chaining key, decrypts the server's SIGNATURE_NOISE_MESSAGE certificate,
    /// and derives the final transport keys.
    ///
    /// After success the session transitions to `Transport` state and
    /// `encrypt()` / `decrypt()` become active.
    ///
    /// # Expected server message layout
    /// ```text
    /// [0..64]     server_ephemeral_pub  (EllSwift, 64 bytes, plaintext)
    /// [64..144]   encrypted_static_pub  (64 bytes EllSwift + 16 bytes MAC = 80 bytes)
    /// [144..234]  encrypted SIGNATURE_NOISE_MESSAGE (74 bytes + 16 bytes MAC = 90 bytes)
    /// ```
    /// Total: 234 bytes.
    ///
    /// # Errors
    /// - Not in `WaitingForResponse` state
    /// - Message too short (< 234 bytes)
    /// - DH produced invalid result
    /// - AEAD authentication failed
    pub fn initiator_handshake_finish(&mut self, server_msg: &[u8]) -> Result<(), String> {
        if self.state != NoiseState::WaitingForResponse {
            return Err(format!(
                "Noise: handshake_finish called in wrong state: {:?}",
                self.state
            ));
        }

        if server_msg.len() < MIN_SERVER_RESPONSE {
            return Err(format!(
                "Noise: server message too short ({} bytes, need >= {})",
                server_msg.len(),
                MIN_SERVER_RESPONSE
            ));
        }

        // Recover ephemeral secret and our EllSwift encoding
        let e_sec_bytes = self
            .ephemeral_secret
            .take()
            .ok_or("Noise: missing ephemeral secret")?;
        let our_ellswift_bytes = self
            .our_ellswift
            .take()
            .ok_or("Noise: missing our EllSwift encoding")?;

        let e_secret = SecretKey::from_slice(&e_sec_bytes)
            .map_err(|e| format!("Noise: stored ephemeral secret is invalid: {}", e))?;
        let our_es = ElligatorSwift::from_array(our_ellswift_bytes);

        // ── Parse and mix server ephemeral public key (EllSwift, 64 bytes) ───
        let server_e_bytes = &server_msg[..ELLSWIFT_SIZE];
        let mut server_e_arr = [0u8; 64];
        server_e_arr.copy_from_slice(server_e_bytes);
        let server_e_es = ElligatorSwift::from_array(server_e_arr);

        // mix_hash(server_e)
        self.mix_hash(server_e_bytes);

        // ── DH(ee) — EllSwift ECDH (BIP324 tagged hash) ──────────────────────
        // SRI uses ElligatorSwift::shared_secret() with default BIP324 hash.
        let ee_shared = ElligatorSwift::shared_secret(
            our_es,      // party A (us, initiator)
            server_e_es, // party B (server, responder)
            e_secret,    // our secret key
            ElligatorSwiftParty::A,
            None, // BIP324 default hash function
        );
        let ee_bytes: [u8; 32] = ee_shared.to_secret_bytes();

        // MixKey(ee): (ck, k_ee) = HKDF(ck, ee)
        let (ck_after_ee, k_ee) = Self::hkdf_sha256(&self.chaining_key, &ee_bytes);
        self.chaining_key = ck_after_ee;

        // ── DecryptAndHash(encrypted_static) → server static public key ──────
        let encrypted_static_start = ELLSWIFT_SIZE;
        let encrypted_static_end = encrypted_static_start + STATIC_KEY_SIZE + 16;
        let encrypted_static = &server_msg[encrypted_static_start..encrypted_static_end];

        // Decrypt with k_ee, nonce=0, AAD = current h
        let server_s_bytes =
            Self::aead_decrypt(&k_ee, 0, &self.handshake_hash, encrypted_static)
                .map_err(|e| format!("Noise: server static key decryption failed: {}", e))?;

        if server_s_bytes.len() != STATIC_KEY_SIZE {
            return Err(format!(
                "Noise: decrypted static key has wrong length: {} (expected {})",
                server_s_bytes.len(),
                STATIC_KEY_SIZE
            ));
        }

        // Capture server static key (first 32 bytes) for metadata tracking
        {
            let mut key = [0u8; 32];
            key.copy_from_slice(&server_s_bytes[..32]);
            self.server_static_key = Some(key);
        }

        // mix_hash(encrypted_static): hash the CIPHERTEXT (Noise spec §5.2)
        self.mix_hash(encrypted_static);

        // ── DH(es) — EllSwift ECDH with server's static key ─────────────────
        let mut server_s_arr = [0u8; 64];
        server_s_arr.copy_from_slice(&server_s_bytes);
        let server_s_es = ElligatorSwift::from_array(server_s_arr);

        let es_shared = ElligatorSwift::shared_secret(
            our_es,      // party A (us, initiator)
            server_s_es, // party B (server static)
            e_secret,    // our ephemeral secret
            ElligatorSwiftParty::A,
            None, // BIP324 default hash function
        );
        let es_bytes: [u8; 32] = es_shared.to_secret_bytes();

        // MixKey(es): (ck, k_es) = HKDF(ck, es)
        let (ck_after_es, k_es) = Self::hkdf_sha256(&self.chaining_key, &es_bytes);
        self.chaining_key = ck_after_es;

        // ── DecryptAndHash(encrypted SIGNATURE_NOISE_MESSAGE) ────────────────
        let encrypted_cert_start = encrypted_static_end;
        let encrypted_cert_end = encrypted_cert_start + SIGNATURE_NOISE_MESSAGE_SIZE + 16;

        if server_msg.len() < encrypted_cert_end {
            return Err(format!(
                "Noise: server message too short for certificate ({} bytes, need >= {})",
                server_msg.len(),
                encrypted_cert_end
            ));
        }

        let encrypted_cert = &server_msg[encrypted_cert_start..encrypted_cert_end];

        let cert_bytes = Self::aead_decrypt(&k_es, 0, &self.handshake_hash, encrypted_cert)
            .map_err(|e| format!("Noise: certificate decryption failed: {}", e))?;

        // mix_hash(encrypted_cert)
        self.mix_hash(encrypted_cert);

        // Parse and log the certificate (TOFU — no signature verification yet)
        match SignatureNoiseMessage::from_bytes(&cert_bytes) {
            Ok(cert) => {
                tracing::info!(
                    "Noise: server certificate v{}, valid_from={}, not_valid_after={}, sig[0..4]={:02x?}",
                    cert.version,
                    cert.valid_from,
                    cert.not_valid_after,
                    &cert.signature[..4]
                );
                // Capture certificate timestamps for metadata tracking
                self.certificate_valid_from = cert.valid_from as u64;
                self.certificate_not_after = cert.not_valid_after as u64;

                // Verify BIP340 Schnorr signature when a pool authority key
                // is pinned. The signed buffer is
                //   version(U16 LE) || valid_from(U32 LE)
                //     || not_valid_after(U32 LE) || server_static_wire
                // where server_static_wire is the full 64-byte EllSwift
                // encoding the server committed to on the wire (SV2 spec
                // §4.5.2 / SRI noise_sv2). Verification key = pinned pool
                // authority key (from the SV2 URL), NOT the server static.
                if let Some(ref authority_key) = self.pool_authority_key {
                    // Pinned authority — signature verification is MANDATORY
                    // and so is the validity window (SV2 spec: an expired or
                    // not-yet-valid cert MUST be rejected). No TOFU fallback.
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if let Err(e) = cert.verify_validity(now) {
                        tracing::error!(
                            "SV2 certificate REJECTED (validity window): {} — aborting handshake (authority key pinned)",
                            e
                        );
                        self.state = NoiseState::Failed("certificate validity check failed".into());
                        return Err(format!(
                            "Noise: SV2 certificate validity check failed — aborting (authority key pinned): {}",
                            e
                        ));
                    }
                    match cert.verify_schnorr(authority_key, &server_s_bytes) {
                        Ok(()) => {
                            tracing::info!(
                                "Noise: server certificate VERIFIED via BIP340 Schnorr against pinned authority key"
                            )
                        }
                        Err(e) => {
                            tracing::error!(
                                "SV2 certificate signature INVALID: {} — aborting handshake (TOFU disabled when authority key is set)",
                                e
                            );
                            self.state = NoiseState::Failed("signature verification failed".into());
                            return Err(format!(
                                "Noise: SV2 certificate signature INVALID — aborting (TOFU disabled when authority key is set): {}",
                                e
                            ));
                        }
                    }
                } else {
                    // No authority key pinned — TOFU mode. This is a MITM
                    // exposure: log it at WARN every session so the operator
                    // is told their pool URL is missing the pinned key. The
                    // SV2 spec considers authority-key pinning mandatory; we
                    // do not hard-fail here only to preserve compatibility
                    // with pools that publish no key, but the warning must
                    // never be silenced.
                    tracing::warn!(
                        "Noise: SV2 server certificate NOT authenticated (no authority key pinned in pool URL) — operating in TOFU mode; an active network attacker could MITM this session. Add the pool's authority key to the SV2 URL to enable verification."
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Noise: failed to parse server certificate: {} (continuing with TOFU)",
                    e
                );
            }
        }

        // ── Split: derive final transport keys ───────────────────────────────
        // Noise spec §5.2 Split(): (k1, k2) = HKDF(ck, empty)
        // Initiator sends with k1, receives with k2.
        let (sending_key, receiving_key) = Self::hkdf_sha256(&self.chaining_key, &[]);

        // Zero handshake-phase state
        self.chaining_key = [0u8; 32];

        // Capture h before set_transport_keys overwrites it
        let final_h = self.handshake_hash;

        // Compute handshake latency
        if let Some(start) = self.handshake_start_time.take() {
            let elapsed = start.elapsed();
            self.handshake_latency_ms = Some(elapsed.as_millis() as u64);
        }

        // Activate transport
        self.set_transport_keys(sending_key, receiving_key, final_h);

        tracing::info!("Noise: Noise_NX handshake COMPLETE — transport active");
        Ok(())
    }

    // =========================================================================
    // Transport: encrypt / decrypt
    // =========================================================================

    /// Set transport keys directly (after handshake completion or for testing).
    pub fn set_transport_keys(&mut self, sending: [u8; 32], receiving: [u8; 32], h: [u8; 32]) {
        self.sending_key = sending;
        self.receiving_key = receiving;
        self.handshake_hash = h;
        self.sending_nonce = 0;
        self.receiving_nonce = 0;
        self.state = NoiseState::Transport;
        tracing::info!("Noise: transport keys set, ChaChaPoly1305 encryption active");
    }

    /// Mark the session as waiting for the server's handshake response.
    pub fn mark_waiting(&mut self) {
        self.state = NoiseState::WaitingForResponse;
    }

    /// Mark the session as failed with a reason.
    pub fn fail(&mut self, reason: impl Into<String>) {
        let msg = reason.into();
        tracing::error!("Noise: session failed: {}", msg);
        self.state = NoiseState::Failed(msg);
    }

    /// Encrypt a plaintext message for sending.
    ///
    /// Returns ciphertext with 16-byte Poly1305 AEAD tag appended.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        if self.state != NoiseState::Transport {
            return Err(format!(
                "Noise: encrypt called in wrong state: {:?}",
                self.state
            ));
        }

        // INSECURE opt-out: cleartext identity passthrough. No tag, no
        // nonce advance — the wire bytes are exactly the plaintext.
        if self.plaintext_passthrough {
            self.bytes_encrypted_total += plaintext.len() as u64;
            return Ok(plaintext.to_vec());
        }

        let cipher = ChaCha20Poly1305::new_from_slice(&self.sending_key)
            .map_err(|e| format!("Noise: cipher init failed: {}", e))?;

        let nonce = Self::make_nonce(self.sending_nonce);
        let ciphertext = cipher.encrypt(&nonce, plaintext).map_err(|e| {
            format!(
                "Noise: encrypt failed (nonce={}): {}",
                self.sending_nonce, e
            )
        })?;

        self.sending_nonce += 1;
        self.bytes_encrypted_total += plaintext.len() as u64;
        Ok(ciphertext)
    }

    /// Decrypt a received ciphertext message.
    ///
    /// Input must include the trailing 16-byte Poly1305 AEAD tag.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        if self.state != NoiseState::Transport {
            return Err(format!(
                "Noise: decrypt called in wrong state: {:?}",
                self.state
            ));
        }

        // INSECURE opt-out: cleartext identity passthrough.
        if self.plaintext_passthrough {
            self.bytes_decrypted_total += ciphertext.len() as u64;
            return Ok(ciphertext.to_vec());
        }

        if ciphertext.len() < 16 {
            return Err(format!(
                "Noise: ciphertext too short ({} bytes, need at least 16 for AEAD tag)",
                ciphertext.len()
            ));
        }

        let cipher = ChaCha20Poly1305::new_from_slice(&self.receiving_key)
            .map_err(|e| format!("Noise: cipher init failed: {}", e))?;

        let nonce = Self::make_nonce(self.receiving_nonce);
        let plaintext = cipher.decrypt(&nonce, ciphertext).map_err(|e| {
            format!(
                "Noise: decrypt failed (nonce={}, likely auth tag mismatch): {}",
                self.receiving_nonce, e
            )
        })?;

        self.receiving_nonce += 1;
        self.bytes_decrypted_total += plaintext.len() as u64;
        Ok(plaintext)
    }

    // =========================================================================
    // Development bypass
    // =========================================================================

    /// **DEVELOPMENT ONLY** — bypass the Noise handshake using deterministic test keys.
    pub fn set_test_keys(&mut self) {
        let mut hasher = Sha256::new();
        hasher.update(b"DCENT_axe_test_key_DO_NOT_USE_IN_PRODUCTION");
        let out = hasher.finalize();
        let mut test_key = [0u8; 32];
        test_key.copy_from_slice(&out);

        self.set_transport_keys(test_key, test_key, [0u8; 32]);
        tracing::warn!("Noise: *** USING TEST KEYS — NOT SECURE — DEVELOPMENT ONLY ***");
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    /// Returns true if the session is in post-handshake transport mode.
    pub fn is_transport(&self) -> bool {
        self.state == NoiseState::Transport
    }

    /// Current outgoing nonce counter (for diagnostics/testing).
    pub fn sending_nonce(&self) -> u64 {
        self.sending_nonce
    }

    /// Current incoming nonce counter (for diagnostics/testing).
    pub fn receiving_nonce(&self) -> u64 {
        self.receiving_nonce
    }

    /// The accumulated handshake hash (for channel binding verification).
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.handshake_hash
    }

    /// Total bytes of plaintext that have been encrypted.
    pub fn bytes_encrypted(&self) -> u64 {
        self.bytes_encrypted_total
    }

    /// Total bytes of plaintext that have been decrypted.
    pub fn bytes_decrypted(&self) -> u64 {
        self.bytes_decrypted_total
    }

    /// Hex string of the first 8 bytes of the server's static public key, if available.
    pub fn server_static_key_hex(&self) -> Option<String> {
        self.server_static_key.map(|key| {
            key[..8]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        })
    }

    /// Certificate validity timestamps (valid_from, not_after) if a certificate was received.
    /// Returns `None` if no certificate has been parsed (both timestamps are zero).
    pub fn certificate_info(&self) -> Option<(u64, u64)> {
        if self.certificate_valid_from == 0 && self.certificate_not_after == 0 {
            None
        } else {
            Some((self.certificate_valid_from, self.certificate_not_after))
        }
    }

    /// Handshake round-trip latency in milliseconds, if the handshake has completed.
    pub fn handshake_latency_ms(&self) -> Option<u64> {
        self.handshake_latency_ms
    }

    /// Current outgoing nonce counter (alias for diagnostics).
    pub fn nonce_tx(&self) -> u64 {
        self.sending_nonce
    }

    /// Current incoming nonce counter (alias for diagnostics).
    pub fn nonce_rx(&self) -> u64 {
        self.receiving_nonce
    }

    // =========================================================================
    // Private cryptographic helpers
    // =========================================================================

    /// Create a 12-byte ChaChaPoly nonce from a u64 counter.
    ///
    /// Noise spec: nonce is little-endian u64 in bytes [4..12], bytes [0..4] = zero.
    fn make_nonce(counter: u64) -> Nonce {
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[4..12].copy_from_slice(&counter.to_le_bytes());
        *Nonce::from_slice(&nonce_bytes)
    }

    /// HKDF-SHA256 (Noise framework variant) — derives two 32-byte keys.
    pub fn hkdf_sha256(chaining_key: &[u8; 32], ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
        let prk = Self::hmac_sha256(chaining_key, ikm);
        let t1 = Self::hmac_sha256(&prk, &[0x01u8]);
        let mut input2 = [0u8; 33];
        input2[..32].copy_from_slice(&t1);
        input2[32] = 0x02;
        let t2 = Self::hmac_sha256(&prk, &input2);
        (t1, t2)
    }

    /// HMAC-SHA256 (RFC 2104) with a 32-byte key.
    fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for i in 0..32 {
            ipad[i] ^= key[i];
            opad[i] ^= key[i];
        }
        let mut inner = Sha256::new();
        inner.update(ipad);
        inner.update(data);
        let inner_hash = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(opad);
        outer.update(inner_hash);
        let result = outer.finalize();

        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Mix data into the running handshake hash: h = SHA256(h || data)
    pub fn mix_hash(&mut self, data: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(self.handshake_hash);
        hasher.update(data);
        let result = hasher.finalize();
        self.handshake_hash.copy_from_slice(&result);
    }

    /// ChaChaPoly1305 AEAD decrypt with explicit key, nonce counter, and AAD.
    fn aead_decrypt(
        key: &[u8; 32],
        nonce_counter: u64,
        aad: &[u8; 32],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, String> {
        use chacha20poly1305::aead::Payload;
        let cipher =
            ChaCha20Poly1305::new_from_slice(key).map_err(|e| format!("key init: {}", e))?;
        let nonce = Self::make_nonce(nonce_counter);
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|e| format!("AEAD: {}", e))
    }
}

impl Default for NoiseSession {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_transport_session(send_key: [u8; 32], recv_key: [u8; 32]) -> NoiseSession {
        let mut s = NoiseSession::new();
        s.set_transport_keys(send_key, recv_key, [0u8; 32]);
        s
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn signature_noise_message_parse_never_panics(
            data in proptest::collection::vec(any::<u8>(), 0..256)
        ) {
            let _ = SignatureNoiseMessage::from_bytes(&data);
        }

        #[test]
        fn initiator_handshake_finish_never_panics_on_arbitrary_response(
            seed in any::<[u8; 64]>(),
            response in proptest::collection::vec(any::<u8>(), 0..512)
        ) {
            let mut session = NoiseSession::new();
            let _ = session.initiator_handshake_start(seed);
            let _ = session.initiator_handshake_finish(&response);
        }
    }

    /// Build a synthetic server Noise_NX response for testing `initiator_handshake_finish`.
    ///
    /// Acts as the server using EllSwift encoding:
    ///   1. Generates server ephemeral keypair + EllSwift from `server_e_seed`
    ///   2. Generates server static keypair + EllSwift from `server_s_seed`
    ///   3. Computes EllSwift ECDH for ee and es
    ///   4. Encrypts the server static EllSwift with k_ee
    ///   5. Encrypts a dummy SIGNATURE_NOISE_MESSAGE with k_es
    ///   6. Returns `(server_msg, server_recv_key, server_send_key)`
    fn build_server_response(
        client_e_ellswift: &[u8; ELLSWIFT_SIZE],
        client_h_after_e: &[u8; 32],
        client_ck_after_init: &[u8; 32],
        server_e_seed: [u8; 64],
        server_s_seed: [u8; 64],
    ) -> (Vec<u8>, [u8; 32], [u8; 32]) {
        use chacha20poly1305::aead::Payload;

        let secp = Secp256k1::new();

        // Server ephemeral keypair + EllSwift
        let re_sk = SecretKey::from_slice(&server_e_seed[..32]).unwrap();
        let mut re_aux = [0u8; 32];
        re_aux.copy_from_slice(&server_e_seed[32..64]);
        let re_es = ElligatorSwift::from_seckey(&secp, re_sk, Some(re_aux));
        let re_pub_arr = re_es.to_array();

        // Server static keypair + EllSwift
        let rs_sk = SecretKey::from_slice(&server_s_seed[..32]).unwrap();
        let mut rs_aux = [0u8; 32];
        rs_aux.copy_from_slice(&server_s_seed[32..64]);
        let rs_es = ElligatorSwift::from_seckey(&secp, rs_sk, Some(rs_aux));
        let rs_pub_arr = rs_es.to_array();

        // Server runs the same h evolution as the client
        let mut h = *client_h_after_e;

        // mix_hash(re_pub)
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&re_pub_arr);
            h.copy_from_slice(&hasher.finalize());
        }

        // DH(ee): EllSwift ECDH (BIP324 tagged) — server side uses Party::B
        let client_es = ElligatorSwift::from_array(*client_e_ellswift);
        let ee_shared = ElligatorSwift::shared_secret(
            client_es,              // party A (client/initiator)
            re_es,                  // party B (server/responder)
            re_sk,                  // our (server's) ephemeral secret
            ElligatorSwiftParty::B, // we are party B
            None,
        );
        let ee_bytes: [u8; 32] = ee_shared.to_secret_bytes();

        // MixKey(ee): (ck, k_ee) = HKDF(ck, ee)
        let (ck_after_ee, k_ee) = NoiseSession::hkdf_sha256(client_ck_after_init, &ee_bytes);

        // EncryptAndHash(rs_pub): encrypt server static EllSwift with k_ee
        let encrypted_static = {
            let cipher = ChaCha20Poly1305::new_from_slice(&k_ee).unwrap();
            let nonce = NoiseSession::make_nonce_pub(0);
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &rs_pub_arr,
                        aad: &h,
                    },
                )
                .unwrap()
        };

        // mix_hash(encrypted_static)
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&encrypted_static);
            h.copy_from_slice(&hasher.finalize());
        }

        // DH(es): EllSwift ECDH (BIP324 tagged)
        let es_shared = ElligatorSwift::shared_secret(
            client_es, // party A (client ephemeral)
            rs_es,     // party B (server static)
            rs_sk,     // server's static secret key
            ElligatorSwiftParty::B,
            None,
        );
        let es_bytes: [u8; 32] = es_shared.to_secret_bytes();

        // MixKey(es)
        let (ck_after_es, k_es) = NoiseSession::hkdf_sha256(&ck_after_ee, &es_bytes);

        // EncryptAndHash(SIGNATURE_NOISE_MESSAGE) — dummy certificate
        let mut cert_payload = Vec::with_capacity(SIGNATURE_NOISE_MESSAGE_SIZE);
        cert_payload.extend_from_slice(&0u16.to_le_bytes()); // version
        cert_payload.extend_from_slice(&0u32.to_le_bytes()); // valid_from
        cert_payload.extend_from_slice(&u32::MAX.to_le_bytes()); // not_valid_after
        cert_payload.extend_from_slice(&[0xAA; 64]); // dummy signature

        let encrypted_cert = {
            let cipher = ChaCha20Poly1305::new_from_slice(&k_es).unwrap();
            let nonce = NoiseSession::make_nonce_pub(0);
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &cert_payload,
                        aad: &h,
                    },
                )
                .unwrap()
        };

        // mix_hash(encrypted_cert)
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&encrypted_cert);
            h.copy_from_slice(&hasher.finalize());
        }

        // Split: (k1, k2) = HKDF(ck, empty)
        let (k1, k2) = NoiseSession::hkdf_sha256(&ck_after_es, &[]);
        let server_recv = k1;
        let server_send = k2;

        // Build server message: re_pub || encrypted_static || encrypted_cert
        let mut msg =
            Vec::with_capacity(ELLSWIFT_SIZE + encrypted_static.len() + encrypted_cert.len());
        msg.extend_from_slice(&re_pub_arr);
        msg.extend_from_slice(&encrypted_static);
        msg.extend_from_slice(&encrypted_cert);

        (msg, server_recv, server_send)
    }

    /// Like [`build_server_response`] but embeds a *real* BIP340 Schnorr
    /// `SIGNATURE_NOISE_MESSAGE` certificate so the **end-to-end pinned-
    /// authority-key path** through `initiator_handshake_finish` can be
    /// exercised (not just the standalone `verify_schnorr` unit).
    ///
    /// The cert is signed over the spec-exact buffer
    ///   SHA256( version_le16 || valid_from_le32 || not_valid_after_le32
    ///           || server_static_wire(64-byte EllSwift) )
    /// with `authority_kp`. When `corrupt_sig` is true the serialized
    /// signature's first byte is flipped, simulating a MITM that re-signed
    /// with a key the client did not pin (verification MUST then fail).
    ///
    /// Returns `(server_msg, server_recv_key, server_send_key)`.
    #[allow(clippy::too_many_arguments)]
    fn build_server_response_signed(
        client_e_ellswift: &[u8; ELLSWIFT_SIZE],
        client_h_after_e: &[u8; 32],
        client_ck_after_init: &[u8; 32],
        server_e_seed: [u8; 64],
        server_s_seed: [u8; 64],
        authority_kp: &secp256k1::Keypair,
        cert_version: u16,
        cert_valid_from: u32,
        cert_not_after: u32,
        corrupt_sig: bool,
    ) -> (Vec<u8>, [u8; 32], [u8; 32]) {
        use chacha20poly1305::aead::Payload;

        let secp = Secp256k1::new();

        // Server ephemeral keypair + EllSwift
        let re_sk = SecretKey::from_slice(&server_e_seed[..32]).unwrap();
        let mut re_aux = [0u8; 32];
        re_aux.copy_from_slice(&server_e_seed[32..64]);
        let re_es = ElligatorSwift::from_seckey(&secp, re_sk, Some(re_aux));
        let re_pub_arr = re_es.to_array();

        // Server static keypair + EllSwift — the wire bytes the cert signs over.
        let rs_sk = SecretKey::from_slice(&server_s_seed[..32]).unwrap();
        let mut rs_aux = [0u8; 32];
        rs_aux.copy_from_slice(&server_s_seed[32..64]);
        let rs_es = ElligatorSwift::from_seckey(&secp, rs_sk, Some(rs_aux));
        let rs_pub_arr = rs_es.to_array();

        let mut h = *client_h_after_e;

        // mix_hash(re_pub)
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&re_pub_arr);
            h.copy_from_slice(&hasher.finalize());
        }

        // DH(ee)
        let client_es = ElligatorSwift::from_array(*client_e_ellswift);
        let ee_shared =
            ElligatorSwift::shared_secret(client_es, re_es, re_sk, ElligatorSwiftParty::B, None);
        let ee_bytes: [u8; 32] = ee_shared.to_secret_bytes();
        let (ck_after_ee, k_ee) = NoiseSession::hkdf_sha256(client_ck_after_init, &ee_bytes);

        // EncryptAndHash(rs_pub)
        let encrypted_static = {
            let cipher = ChaCha20Poly1305::new_from_slice(&k_ee).unwrap();
            let nonce = NoiseSession::make_nonce_pub(0);
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &rs_pub_arr,
                        aad: &h,
                    },
                )
                .unwrap()
        };
        // mix_hash(encrypted_static)
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&encrypted_static);
            h.copy_from_slice(&hasher.finalize());
        }

        // DH(es)
        let es_shared =
            ElligatorSwift::shared_secret(client_es, rs_es, rs_sk, ElligatorSwiftParty::B, None);
        let es_bytes: [u8; 32] = es_shared.to_secret_bytes();
        let (ck_after_es, k_es) = NoiseSession::hkdf_sha256(&ck_after_ee, &es_bytes);

        // Real BIP340 Schnorr cert over the actual server static wire bytes.
        let mut sig = sign_cert(
            authority_kp,
            cert_version,
            cert_valid_from,
            cert_not_after,
            &rs_pub_arr,
        );
        if corrupt_sig {
            sig[0] ^= 0xFF;
        }

        let mut cert_payload = Vec::with_capacity(SIGNATURE_NOISE_MESSAGE_SIZE);
        cert_payload.extend_from_slice(&cert_version.to_le_bytes());
        cert_payload.extend_from_slice(&cert_valid_from.to_le_bytes());
        cert_payload.extend_from_slice(&cert_not_after.to_le_bytes());
        cert_payload.extend_from_slice(&sig);

        let encrypted_cert = {
            let cipher = ChaCha20Poly1305::new_from_slice(&k_es).unwrap();
            let nonce = NoiseSession::make_nonce_pub(0);
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &cert_payload,
                        aad: &h,
                    },
                )
                .unwrap()
        };
        {
            let mut hasher = Sha256::new();
            hasher.update(h);
            hasher.update(&encrypted_cert);
            h.copy_from_slice(&hasher.finalize());
        }

        let (k1, k2) = NoiseSession::hkdf_sha256(&ck_after_es, &[]);

        let mut msg =
            Vec::with_capacity(ELLSWIFT_SIZE + encrypted_static.len() + encrypted_cert.len());
        msg.extend_from_slice(&re_pub_arr);
        msg.extend_from_slice(&encrypted_static);
        msg.extend_from_slice(&encrypted_cert);

        (msg, k1, k2)
    }

    /// Drive a full client handshake (`start` → `finish`) against a server
    /// response whose certificate is signed by `authority_kp`, with the
    /// client pinning `pinned_key`. `corrupt_sig` re-signs-then-corrupts to
    /// model a MITM. Returns the `finish` result so the caller can assert
    /// accept vs reject. A far-future `not_valid_after` keeps the validity
    /// window out of the way so the test isolates the signature decision.
    fn run_finish_with_pinned_key(
        authority_kp: &secp256k1::Keypair,
        pinned_key: Option<[u8; 32]>,
        corrupt_sig: bool,
    ) -> (NoiseState, Result<(), String>) {
        let mut client = NoiseSession::new();
        client.pool_authority_key = pinned_key;

        let e_pub_vec = client.initiator_handshake_start([0x42u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);
        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        let (server_msg, _srv_recv, _srv_send) = build_server_response_signed(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0x1Cu8; 64],
            [0x2Du8; 64],
            authority_kp,
            0,        // version
            0,        // valid_from
            u32::MAX, // not_valid_after (window always open)
            corrupt_sig,
        );
        let res = client.initiator_handshake_finish(&server_msg);
        (client.state.clone(), res)
    }

    // ── End-to-end pinned-authority-key handshake-finish path ─────────────────
    //
    // These exercise the WIRING (`initiator_handshake_finish`'s pinned-vs-TOFU
    // branch), not just the standalone `verify_schnorr` unit. They are the
    // load-bearing MITM-defense regression: a correctly-signed cert under a
    // pinned key MUST complete the handshake; a wrong-key (MITM-resigned) cert
    // MUST abort it; and with no key pinned the handshake MUST still succeed
    // (backward-compatible TOFU — the task's "don't break the existing
    // handshake when no cert is configured" requirement).

    #[test]
    fn finish_accepts_correctly_signed_cert_when_key_pinned() {
        let (kp, xonly) = authority_keypair(0x71);
        let (state, res) = run_finish_with_pinned_key(&kp, Some(xonly), false);
        assert!(
            res.is_ok(),
            "a cert correctly signed by the pinned authority MUST complete the handshake: {:?}",
            res
        );
        assert_eq!(state, NoiseState::Transport);
    }

    #[test]
    fn finish_rejects_wrong_key_signed_cert_when_key_pinned() {
        // Server signs with `attacker_kp`; client pins a DIFFERENT key.
        let (attacker_kp, _attacker_xonly) = authority_keypair(0x82);
        let (_legit_kp, legit_xonly) = authority_keypair(0x93);
        let (state, res) = run_finish_with_pinned_key(&attacker_kp, Some(legit_xonly), false);
        assert!(
            res.is_err(),
            "a cert NOT signed by the pinned authority MUST abort the handshake (MITM defense)"
        );
        assert!(
            matches!(state, NoiseState::Failed(_)),
            "session must enter Failed state on signature rejection, got {:?}",
            state
        );
    }

    #[test]
    fn finish_rejects_corrupted_signature_when_key_pinned() {
        // Same authority signs, but the on-wire signature is corrupted in
        // transit. Pinned-key verification MUST reject it.
        let (kp, xonly) = authority_keypair(0xA4);
        let (state, res) = run_finish_with_pinned_key(&kp, Some(xonly), true);
        assert!(
            res.is_err(),
            "a corrupted certificate signature MUST abort the handshake"
        );
        assert!(matches!(state, NoiseState::Failed(_)));
    }

    #[test]
    fn finish_succeeds_in_tofu_when_no_key_pinned() {
        // No authority key pinned → TOFU. Even a cert signed by an arbitrary
        // key (which the client cannot check) MUST NOT block the handshake:
        // the existing no-cert-configured behavior is preserved (warn-only).
        let (kp, _xonly) = authority_keypair(0xB5);
        let (state, res) =
            run_finish_with_pinned_key(&kp, None, true /* sig irrelevant in TOFU */);
        assert!(
            res.is_ok(),
            "TOFU (no pinned key) MUST NOT hard-fail the handshake: {:?}",
            res
        );
        assert_eq!(state, NoiseState::Transport);
    }

    // ── Transport tests ───────────────────────────────────────────────────────

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32];
        let mut sender = make_transport_session(key1, key2);
        let mut receiver = make_transport_session(key2, key1);
        let plaintext = b"Hello, Stratum V2!";
        let ciphertext = sender.encrypt(plaintext).unwrap();
        assert_ne!(&ciphertext[..plaintext.len()], plaintext);
        assert_eq!(ciphertext.len(), plaintext.len() + 16);
        let decrypted = receiver.decrypt(&ciphertext).unwrap();
        assert_eq!(decrypted.as_slice(), plaintext);
    }

    #[test]
    fn test_nonce_increments_on_encrypt() {
        let mut session = make_transport_session([1u8; 32], [2u8; 32]);
        assert_eq!(session.sending_nonce(), 0);
        let _ = session.encrypt(b"msg1").unwrap();
        assert_eq!(session.sending_nonce(), 1);
        let _ = session.encrypt(b"msg2").unwrap();
        assert_eq!(session.sending_nonce(), 2);
    }

    #[test]
    fn test_nonce_increments_on_decrypt() {
        let key = [0xAAu8; 32];
        let mut enc = make_transport_session(key, [0u8; 32]);
        let mut dec = make_transport_session([0u8; 32], key);
        assert_eq!(dec.receiving_nonce(), 0);
        let ct = enc.encrypt(b"test").unwrap();
        let _ = dec.decrypt(&ct).unwrap();
        assert_eq!(dec.receiving_nonce(), 1);
    }

    #[test]
    fn test_decrypt_fails_wrong_key() {
        let mut sender = make_transport_session([0x11u8; 32], [0x22u8; 32]);
        let mut bad_receiver = make_transport_session([0x33u8; 32], [0x44u8; 32]);
        let ct = sender.encrypt(b"secret").unwrap();
        let result = bad_receiver.decrypt(&ct);
        assert!(result.is_err(), "Decrypt with wrong key must fail");
    }

    #[test]
    fn test_decrypt_fails_tampered_ciphertext() {
        let key = [0x55u8; 32];
        let mut enc = make_transport_session(key, [0u8; 32]);
        let mut dec = make_transport_session([0u8; 32], key);
        let mut ct = enc.encrypt(b"tamper me").unwrap();
        ct[0] ^= 0xFF;
        let result = dec.decrypt(&ct);
        assert!(result.is_err(), "Decrypt of tampered ciphertext must fail");
    }

    #[test]
    fn test_decrypt_fails_too_short() {
        let mut session = make_transport_session([1u8; 32], [2u8; 32]);
        let result = session.decrypt(&[0u8; 10]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too short"));
    }

    #[test]
    fn test_encrypt_fails_wrong_state() {
        let mut session = NoiseSession::new();
        assert!(session.encrypt(b"data").is_err());
    }

    #[test]
    fn test_decrypt_fails_wrong_state() {
        let mut session = NoiseSession::new();
        assert!(session.decrypt(&[0u8; 32]).is_err());
    }

    #[test]
    fn test_multiple_messages_sequential() {
        let key = [0xBBu8; 32];
        let mut enc = make_transport_session(key, [0u8; 32]);
        let mut dec = make_transport_session([0u8; 32], key);
        let messages: &[&[u8]] = &[b"first", b"second", b"third message here"];
        for msg in messages {
            let ct = enc.encrypt(msg).unwrap();
            let pt = dec.decrypt(&ct).unwrap();
            assert_eq!(pt.as_slice(), *msg);
        }
        assert_eq!(enc.sending_nonce(), 3);
        assert_eq!(dec.receiving_nonce(), 3);
    }

    #[test]
    fn test_hkdf_produces_two_different_keys() {
        let ck = [0u8; 32];
        let (k1, k2) = NoiseSession::hkdf_sha256(&ck, b"test input key material");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_hkdf_is_deterministic() {
        let ck = [0xDEu8; 32];
        let (k1a, k2a) = NoiseSession::hkdf_sha256(&ck, b"det");
        let (k1b, k2b) = NoiseSession::hkdf_sha256(&ck, b"det");
        assert_eq!(k1a, k1b);
        assert_eq!(k2a, k2b);
    }

    #[test]
    fn test_hkdf_different_ck_produces_different_keys() {
        let (k1a, _) = NoiseSession::hkdf_sha256(&[0x00u8; 32], b"same ikm");
        let (k1b, _) = NoiseSession::hkdf_sha256(&[0xFFu8; 32], b"same ikm");
        assert_ne!(k1a, k1b);
    }

    #[test]
    fn test_hkdf_different_ikm_produces_different_keys() {
        let ck = [0x42u8; 32];
        let (k1a, _) = NoiseSession::hkdf_sha256(&ck, b"ikm-a");
        let (k1b, _) = NoiseSession::hkdf_sha256(&ck, b"ikm-b");
        assert_ne!(k1a, k1b);
    }

    #[test]
    fn test_mix_hash_changes_state() {
        let mut session = NoiseSession::new();
        let initial = *session.handshake_hash();
        session.mix_hash(b"some data");
        assert_ne!(session.handshake_hash(), &initial);
    }

    #[test]
    fn test_mix_hash_is_deterministic() {
        let mut s1 = NoiseSession::new();
        let mut s2 = NoiseSession::new();
        s1.mix_hash(b"same");
        s2.mix_hash(b"same");
        assert_eq!(s1.handshake_hash(), s2.handshake_hash());
    }

    #[test]
    fn test_mix_hash_accumulates() {
        let mut session = NoiseSession::new();
        session.mix_hash(b"step1");
        let after1 = *session.handshake_hash();
        session.mix_hash(b"step2");
        assert_ne!(session.handshake_hash(), &after1);
    }

    #[test]
    fn test_state_transitions() {
        let mut session = NoiseSession::new();
        assert_eq!(session.state, NoiseState::Init);
        assert!(!session.is_transport());
        session.mark_waiting();
        assert_eq!(session.state, NoiseState::WaitingForResponse);
        session.set_transport_keys([0u8; 32], [0u8; 32], [0u8; 32]);
        assert_eq!(session.state, NoiseState::Transport);
        assert!(session.is_transport());
    }

    #[test]
    fn test_fail_state() {
        let mut session = NoiseSession::new();
        session.fail("timeout");
        assert!(matches!(session.state, NoiseState::Failed(_)));
        assert!(!session.is_transport());
    }

    #[test]
    fn test_nonce_format_little_endian() {
        let nonce = NoiseSession::make_nonce(1u64);
        let bytes: &[u8] = nonce.as_slice();
        assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
        assert_eq!(&bytes[4..12], &1u64.to_le_bytes());
        let nonce256 = NoiseSession::make_nonce(256u64);
        assert_eq!(&nonce256.as_slice()[4..12], &256u64.to_le_bytes());
    }

    #[test]
    fn test_empty_plaintext() {
        let key = [0xCCu8; 32];
        let mut enc = make_transport_session(key, [0u8; 32]);
        let mut dec = make_transport_session([0u8; 32], key);
        let ct = enc.encrypt(b"").unwrap();
        assert_eq!(ct.len(), 16);
        let pt = dec.decrypt(&ct).unwrap();
        assert_eq!(pt.len(), 0);
    }

    // ── Test key bypass ───────────────────────────────────────────────────────

    #[test]
    fn test_set_test_keys_enters_transport() {
        let mut s = NoiseSession::new();
        s.set_test_keys();
        assert_eq!(s.state, NoiseState::Transport);
        assert!(s.is_transport());
    }

    #[test]
    fn test_set_test_keys_loopback() {
        let mut s1 = NoiseSession::new();
        s1.set_test_keys();
        let mut s2 = NoiseSession::new();
        s2.set_test_keys();
        let ct = s1.encrypt(b"DCENT_axe test").unwrap();
        let pt = s2.decrypt(&ct).unwrap();
        assert_eq!(pt.as_slice(), b"DCENT_axe test");
    }

    // ── Handshake step 1 ──────────────────────────────────────────────────────

    #[test]
    fn test_handshake_start_returns_64_bytes() {
        let mut s = NoiseSession::new();
        let e_pub = s.initiator_handshake_start([0xABu8; 64]).unwrap();
        assert_eq!(e_pub.len(), ELLSWIFT_SIZE);
    }

    #[test]
    fn test_handshake_start_transitions_state() {
        let mut s = NoiseSession::new();
        s.initiator_handshake_start([0x01u8; 64]).unwrap();
        assert_eq!(s.state, NoiseState::WaitingForResponse);
    }

    #[test]
    fn test_handshake_start_fails_wrong_state() {
        let mut s = NoiseSession::new();
        s.set_transport_keys([0u8; 32], [0u8; 32], [0u8; 32]);
        assert!(s.initiator_handshake_start([0u8; 64]).is_err());
    }

    #[test]
    fn test_handshake_start_different_seeds_produce_different_keys() {
        let mut s1 = NoiseSession::new();
        let mut s2 = NoiseSession::new();
        let p1 = s1.initiator_handshake_start([0x11u8; 64]).unwrap();
        let p2 = s2.initiator_handshake_start([0x22u8; 64]).unwrap();
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_handshake_start_deterministic() {
        let seed = [0x42u8; 64];
        let mut s1 = NoiseSession::new();
        let mut s2 = NoiseSession::new();
        let p1 = s1.initiator_handshake_start(seed).unwrap();
        let p2 = s2.initiator_handshake_start(seed).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn test_handshake_start_mixes_pubkey_into_hash() {
        let mut s1 = NoiseSession::new();
        let mut s2 = NoiseSession::new();
        s1.initiator_handshake_start([0x11u8; 64]).unwrap();
        s2.initiator_handshake_start([0x22u8; 64]).unwrap();
        assert_ne!(
            s1.handshake_hash(),
            s2.handshake_hash(),
            "different ephemeral keys must diverge handshake hash"
        );
    }

    // ── Handshake step 2 ──────────────────────────────────────────────────────

    #[test]
    fn test_full_handshake_key_agreement() {
        // Client starts
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xC1u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        // Capture state for server simulation
        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        // Build server response
        let (server_msg, server_recv_key, server_send_key) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE5u8; 64],
            [0x5Au8; 64],
        );

        // Client processes server response
        client.initiator_handshake_finish(&server_msg).unwrap();
        assert_eq!(client.state, NoiseState::Transport);

        // Key agreement: client send = server recv, client recv = server send
        assert_eq!(
            client.sending_key_for_test(),
            server_recv_key,
            "send/recv key mismatch"
        );
        assert_eq!(
            client.receiving_key_for_test(),
            server_send_key,
            "recv/send key mismatch"
        );
    }

    #[test]
    fn test_handshake_finish_fails_wrong_state() {
        let mut s = NoiseSession::new();
        assert!(s
            .initiator_handshake_finish(&[0u8; MIN_SERVER_RESPONSE])
            .is_err());
    }

    #[test]
    fn test_handshake_finish_fails_too_short() {
        let mut s = NoiseSession::new();
        s.initiator_handshake_start([0x01u8; 64]).unwrap();
        let result = s.initiator_handshake_finish(&[0u8; 30]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too short"));
    }

    #[test]
    fn test_transport_encrypt_decrypt_after_full_handshake() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xAAu8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        let (server_msg, server_recv_key, server_send_key) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE1u8; 64],
            [0x51u8; 64],
        );
        client.initiator_handshake_finish(&server_msg).unwrap();

        // Client encrypts → server decrypts
        let ct = client.encrypt(b"first mining message").unwrap();
        let srv_cipher = ChaCha20Poly1305::new_from_slice(&server_recv_key).unwrap();
        let srv_nonce = NoiseSession::make_nonce_pub(0);
        use chacha20poly1305::aead::Payload;
        let pt = srv_cipher
            .decrypt(&srv_nonce, Payload { msg: &ct, aad: b"" })
            .unwrap();
        assert_eq!(pt.as_slice(), b"first mining message");

        // Server encrypts → client decrypts
        let srv_ct = {
            let sc = ChaCha20Poly1305::new_from_slice(&server_send_key).unwrap();
            let sn = NoiseSession::make_nonce_pub(0);
            sc.encrypt(
                &sn,
                Payload {
                    msg: b"server response",
                    aad: b"",
                },
            )
            .unwrap()
        };
        let pt2 = client.decrypt(&srv_ct).unwrap();
        assert_eq!(pt2.as_slice(), b"server response");
    }

    #[test]
    fn test_handshake_finish_bad_aead_fails() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xBBu8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        let (mut server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE2u8; 64],
            [0x52u8; 64],
        );

        // Flip a byte in the encrypted static key portion
        server_msg[ELLSWIFT_SIZE + 5] ^= 0xFF;

        let result = client.initiator_handshake_finish(&server_msg);
        assert!(result.is_err(), "corrupted AEAD must fail");
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("decryption failed") || err_msg.contains("AEAD"),
            "error should mention decryption, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_signature_noise_message_parsing() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u16.to_le_bytes()); // version
        payload.extend_from_slice(&1000u32.to_le_bytes()); // valid_from
        payload.extend_from_slice(&2000u32.to_le_bytes()); // not_valid_after
        payload.extend_from_slice(&[0xBB; 64]); // signature

        let cert = SignatureNoiseMessage::from_bytes(&payload).unwrap();
        assert_eq!(cert.version, 1);
        assert_eq!(cert.valid_from, 1000);
        assert_eq!(cert.not_valid_after, 2000);
        assert_eq!(cert.signature[0], 0xBB);
    }

    #[test]
    fn test_signature_noise_message_too_short() {
        let result = SignatureNoiseMessage::from_bytes(&[0u8; 10]);
        assert!(result.is_err());
    }

    // ── SV2 SIGNATURE_NOISE_MESSAGE certificate verification ──────────────────
    //
    // These build a *genuine* BIP340 Schnorr signature with a real
    // secp256k1 authority keypair over the spec-exact signed buffer
    //   SHA256( version_le16 || valid_from_le32 || not_valid_after_le32
    //           || server_static_wire(64 EllSwift) )
    // and prove: valid → accepted, wrong key → rejected, tampered field →
    // rejected, validity-window enforced.

    fn sign_cert(
        kp: &secp256k1::Keypair,
        version: u16,
        valid_from: u32,
        not_valid_after: u32,
        server_static_wire: &[u8; 64],
    ) -> [u8; 64] {
        use secp256k1::Message;
        let mut buf = Vec::with_capacity(2 + 4 + 4 + 64);
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&valid_from.to_le_bytes());
        buf.extend_from_slice(&not_valid_after.to_le_bytes());
        buf.extend_from_slice(server_static_wire);
        let digest = Sha256::digest(&buf);
        let msg = Message::from_digest_slice(&digest).unwrap();
        let secp = Secp256k1::new();
        secp.sign_schnorr_no_aux_rand(&msg, kp).serialize()
    }

    fn authority_keypair(seed: u8) -> (secp256k1::Keypair, [u8; 32]) {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed.max(1); 32]).unwrap();
        let kp = secp256k1::Keypair::from_secret_key(&secp, &sk);
        let (xonly, _) = kp.x_only_public_key();
        (kp, xonly.serialize())
    }

    #[test]
    fn cert_verify_accepts_valid_signature() {
        let (kp, authority_xonly) = authority_keypair(0xA1);
        let server_static = [0x5Au8; 64]; // wire EllSwift bytes
        let nva: u32 = 4_000_000_000;
        let sig = sign_cert(&kp, 0, 1_000, nva, &server_static);
        let cert = SignatureNoiseMessage {
            version: 0,
            valid_from: 1_000,
            not_valid_after: nva,
            signature: sig,
        };
        assert!(cert
            .verify_schnorr(&authority_xonly, &server_static)
            .is_ok());
    }

    #[test]
    fn cert_verify_rejects_wrong_authority_key() {
        let (kp, _good) = authority_keypair(0xB2);
        let (_kp2, attacker_xonly) = authority_keypair(0xC3);
        let server_static = [0x33u8; 64];
        let nva: u32 = 4_000_000_000;
        let sig = sign_cert(&kp, 0, 0, nva, &server_static);
        let cert = SignatureNoiseMessage {
            version: 0,
            valid_from: 0,
            not_valid_after: nva,
            signature: sig,
        };
        assert!(
            cert.verify_schnorr(&attacker_xonly, &server_static)
                .is_err(),
            "signature from a different authority key MUST be rejected"
        );
    }

    #[test]
    fn cert_verify_rejects_tampered_static_key() {
        let (kp, authority_xonly) = authority_keypair(0xD4);
        let signed_static = [0x11u8; 64];
        let nva: u32 = 4_000_000_000;
        let sig = sign_cert(&kp, 0, 0, nva, &signed_static);
        let cert = SignatureNoiseMessage {
            version: 0,
            valid_from: 0,
            not_valid_after: nva,
            signature: sig,
        };
        let mut tampered = signed_static;
        tampered[0] ^= 0xFF; // MITM swapped the server static key
        assert!(
            cert.verify_schnorr(&authority_xonly, &tampered).is_err(),
            "a swapped server static key MUST fail verification (MITM defense)"
        );
    }

    #[test]
    fn cert_verify_rejects_wrong_static_key_length() {
        let (_kp, authority_xonly) = authority_keypair(0xE5);
        let cert = SignatureNoiseMessage {
            version: 0,
            valid_from: 0,
            not_valid_after: 1,
            signature: [0u8; 64],
        };
        // 32 bytes (old 25519 form) is NOT accepted by the EllSwift suite.
        assert!(cert.verify_schnorr(&authority_xonly, &[0u8; 32]).is_err());
    }

    #[test]
    fn cert_validity_window_enforced() {
        let cert = SignatureNoiseMessage {
            version: 0,
            valid_from: 1_000,
            not_valid_after: 2_000,
            signature: [0u8; 64],
        };
        assert!(cert.verify_validity(999).is_err(), "before valid_from");
        assert!(cert.verify_validity(1_000).is_ok(), "at valid_from");
        assert!(cert.verify_validity(1_500).is_ok(), "inside window");
        assert!(cert.verify_validity(2_000).is_ok(), "at not_valid_after");
        assert!(cert.verify_validity(2_001).is_err(), "expired");
    }

    // ── Encrypted-by-default + explicit insecure opt-out ──────────────────────

    #[test]
    fn new_session_is_not_plaintext_by_default() {
        let s = NoiseSession::new();
        assert!(
            !s.is_plaintext_passthrough(),
            "a fresh session MUST default to secure (no plaintext passthrough)"
        );
    }

    #[test]
    fn handshake_path_is_encrypted_by_default() {
        // A full handshake never sets plaintext_passthrough → encrypt()
        // produces a tag-bearing ciphertext (len = plaintext + 16).
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0x7Fu8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);
        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();
        let (server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0x1Cu8; 64],
            [0x2Du8; 64],
        );
        client.initiator_handshake_finish(&server_msg).unwrap();
        assert!(!client.is_plaintext_passthrough());
        let ct = client.encrypt(b"job").unwrap();
        assert_eq!(ct.len(), 3 + 16, "secure path must AEAD-tag every frame");
    }

    #[test]
    fn explicit_insecure_passthrough_is_identity_and_loud() {
        let mut s = NoiseSession::new();
        s.enable_insecure_plaintext_passthrough();
        assert!(s.is_plaintext_passthrough());
        assert!(s.is_transport());
        // Identity: no tag, byte-for-byte.
        let ct = s.encrypt(b"cleartext-frame").unwrap();
        assert_eq!(ct, b"cleartext-frame");
        let pt = s.decrypt(b"cleartext-frame").unwrap();
        assert_eq!(pt, b"cleartext-frame");
    }
}

// Public accessors for test helpers (only available in test builds)
#[cfg(test)]
impl NoiseSession {
    /// Expose the internal chaining key for server-side simulation in tests.
    pub fn chaining_key_for_test(&self) -> [u8; 32] {
        self.chaining_key
    }
    /// Expose the sending key for key-agreement verification in tests.
    pub fn sending_key_for_test(&self) -> [u8; 32] {
        self.sending_key
    }
    /// Expose the receiving key for key-agreement verification in tests.
    pub fn receiving_key_for_test(&self) -> [u8; 32] {
        self.receiving_key
    }
    pub fn make_nonce_pub(counter: u64) -> Nonce {
        Self::make_nonce(counter)
    }
}
