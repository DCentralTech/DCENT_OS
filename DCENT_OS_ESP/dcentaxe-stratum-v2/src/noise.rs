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
//! When `pool_authority_key` is `Some`, the BIP340 Schnorr certificate signature
//! is verified **fail-closed**: a certificate parse/length error or a signature
//! mismatch sets the session to `Failed` and aborts the handshake (no transport,
//! no mining) BEFORE transport keys are derived — unless an explicit default-OFF
//! lab override (`DCENTAXE_SV2_INSECURE_SKIP_CERT_VERIFY`) is enabled.
//! When `pool_authority_key` is `None`, the session uses TOFU (trust-on-first-use):
//! the certificate is parsed and logged but the signature is not verified.

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

/// Transport-nonce ceiling. The ChaChaPoly nonce counter must never wrap (nonce
/// reuse with the same key is catastrophic). We fail closed one short of the wrap
/// point: once a counter reaches this value, the session is marked `Failed` and a
/// reconnect (which re-runs the handshake → fresh keys) is required. A miner
/// reconnects long before 2^64 frames, so this never trips in practice; it is
/// latent defense-in-depth.
const MAX_TRANSPORT_NONCE: u64 = u64::MAX - 1;

/// SIGNATURE_NOISE_MESSAGE size: version(2) + valid_from(4) + not_valid_after(4) + signature(64)
const SIGNATURE_NOISE_MESSAGE_SIZE: usize = 74;

/// Bitcoin Base58 alphabet (the same alphabet SRI / the `bs58` crate use for
/// SV2 authority keys). Ported verbatim from DCENT_OS
/// `dcentrald-stratum/src/v2/auth.rs`.
const B58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode a Base58Check string into its payload (checksum stripped + verified).
///
/// Base58Check = base58( data || first4(sha256(sha256(data))) ). This is the
/// exact encoding `bs58::encode(..).with_check()` (used by SRI for SV2
/// authority keys) produces. Self-contained over the already-present `sha2`;
/// no new crate dependency. Ported from DCENT_OS `v2/auth.rs::base58check_decode`.
fn base58check_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Err("empty string".into());
    }
    // Base58 → big-endian byte vector.
    let mut bytes: Vec<u8> = vec![0];
    for ch in s.bytes() {
        let val = B58_ALPHABET
            .iter()
            .position(|&c| c == ch)
            .ok_or_else(|| format!("invalid base58 character: {:?}", ch as char))?
            as u32;
        let mut carry = val;
        for b in bytes.iter_mut() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    // Leading '1' chars are leading zero bytes.
    for ch in s.bytes() {
        if ch == b'1' {
            bytes.push(0);
        } else {
            break;
        }
    }
    bytes.reverse();

    if bytes.len() < 4 {
        return Err("decoded payload shorter than checksum".into());
    }
    let (payload, checksum) = bytes.split_at(bytes.len() - 4);
    let h1 = Sha256::digest(payload);
    let h2 = Sha256::digest(h1);
    if checksum != &h2[..4] {
        return Err("base58check checksum mismatch".into());
    }
    Ok(payload.to_vec())
}

/// Parse an operator-configured SV2 pool authority public key into a 32-byte
/// x-only secp256k1 key suitable for [`NoiseSession::set_pool_authority_key`].
///
/// Accepts either form:
///   * a bare base58check token — `base58check( [0x01,0x00] || pubkey32 )`, OR
///   * a full SV2 URL whose path carries that token —
///     `stratum2+tcp://host:port/<base58check( [0x01,0x00] || pubkey32 )>`.
///
/// The encoded payload is `version_le_u16(==1) || x_only_pubkey(32)` (34 bytes),
/// matching the SV2 spec / SRI and DCENT_OS
/// `dcentrald-stratum/src/v2/auth.rs::parse_authority_key_from_sv2_url`.
///
/// On any malformed input this returns `Err` so the caller can log a warning and
/// fall back to TOFU (the documented fail-open-to-TOFU posture) rather than brick
/// the connection. It intentionally does NOT validate that the bytes are a valid
/// secp256k1 point — an invalid point still parses here and fails later, still
/// fail-closed, in `SignatureNoiseMessage::verify_schnorr`.
pub fn parse_pool_authority_pubkey(s: &str) -> Result<[u8; 32], String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty authority key".into());
    }

    // If an SV2 URL was supplied, take the first path segment as the token.
    // Otherwise treat the whole string as a bare base58check token.
    let token = if trimmed.contains("://") || trimmed.contains('/') {
        let after_scheme = trimmed
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        let path = after_scheme
            .split_once('/')
            .map(|(_authority, path)| path)
            .ok_or("no authority key in SV2 URL")?;
        path.split(['/', '?', '#']).next().unwrap_or("").trim()
    } else {
        trimmed
    };

    if token.is_empty() {
        return Err("no authority key token present".into());
    }

    let payload = base58check_decode(token).map_err(|e| format!("invalid base58check: {}", e))?;

    // Expected: 2-byte LE version prefix (0x0001) + 32-byte x-only pubkey.
    if payload.len() != 34 {
        return Err(format!(
            "expected 34 bytes (2 version + 32 key), got {}",
            payload.len()
        ));
    }
    let version = u16::from_le_bytes([payload[0], payload[1]]);
    if version != 1 {
        return Err(format!(
            "unsupported authority-key version {} (expected 1)",
            version
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&payload[2..34]);
    Ok(key)
}

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
    /// Verify the BIP340 Schnorr signature against a pool authority key.
    ///
    /// The signed message (per SV2 spec) is the plain SHA256 of:
    ///   version(2 LE) || valid_from(4 LE) || not_valid_after(4 LE) || server_static_xonly(32)
    ///
    /// # Arguments
    /// * `authority_key` — Pool's authority public key (x-only, 32 bytes)
    /// * `server_static_xonly` — Server's static key, x-only-serialized (32 bytes),
    ///   recovered from the EllSwift handshake encoding via
    ///   `NoiseSession::ellswift_to_xonly`. This is NOT the raw EllSwift bytes.
    pub fn verify_schnorr(
        &self,
        authority_key: &[u8; 32],
        server_static_xonly: &[u8],
    ) -> Result<(), String> {
        use secp256k1::schnorr::Signature;
        use secp256k1::{Message, XOnlyPublicKey};

        if server_static_xonly.len() < 32 {
            return Err("server static pubkey too short".into());
        }
        let mut xonly = [0u8; 32];
        xonly.copy_from_slice(&server_static_xonly[..32]);

        // SHA256 digest of the signed message. BIP340 uses tagged hashing in
        // general, but the SV2/SRI SignatureNoiseMessage uses plain single SHA256.
        let digest =
            Self::signed_digest(self.version, self.valid_from, self.not_valid_after, &xonly);
        let message = Message::from_digest_slice(&digest)
            .map_err(|e| format!("invalid message hash: {}", e))?;

        let pubkey = XOnlyPublicKey::from_slice(authority_key)
            .map_err(|e| format!("invalid authority key: {}", e))?;
        let sig = Signature::from_slice(&self.signature)
            .map_err(|e| format!("invalid signature: {}", e))?;

        let secp = Secp256k1::verification_only();
        secp.verify_schnorr(&sig, &message, &pubkey)
            .map_err(|e| format!("signature verification failed: {}", e))
    }

    /// Compute the BIP340 signed-message digest for an SV2 SignatureNoiseMessage.
    ///
    /// This is the single source of truth for the signed bytes, shared by
    /// [`Self::verify_schnorr`] and the test harness so the two can never drift.
    ///
    /// The signed message (per SV2 / SRI) is the plain SHA256 of:
    ///   version(2 LE) || valid_from(4 LE) || not_valid_after(4 LE) || server_static_xonly(32)
    ///
    /// `server_static_xonly` is the x-only-serialized server static key recovered
    /// from its EllSwift handshake encoding (NOT the raw EllSwift bytes).
    fn signed_digest(
        version: u16,
        valid_from: u32,
        not_valid_after: u32,
        server_static_xonly: &[u8; 32],
    ) -> [u8; 32] {
        let mut msg_data = Vec::with_capacity(42);
        msg_data.extend_from_slice(&version.to_le_bytes());
        msg_data.extend_from_slice(&valid_from.to_le_bytes());
        msg_data.extend_from_slice(&not_valid_after.to_le_bytes());
        msg_data.extend_from_slice(server_static_xonly);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&Sha256::digest(&msg_data));
        digest
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
        }
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
        log::info!(
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

        // Validate the server certificate — FAIL-CLOSED when a pool authority key
        // is pinned. A certificate parse/length error or a BIP340 Schnorr signature
        // mismatch aborts the handshake (state=Failed, return Err) BEFORE transport
        // keys are derived, so connect() tears down TCP and never mines. When no
        // authority key is configured we fall through to TOFU (parse + log only).
        let authority_pinned = self.pool_authority_key.is_some();
        let cert_check: Result<(), String> = match SignatureNoiseMessage::from_bytes(&cert_bytes) {
            Ok(cert) => {
                log::info!(
                    "Noise: server certificate v{}, valid_from={}, not_valid_after={}, sig[0..4]={:02x?}",
                    cert.version,
                    cert.valid_from,
                    cert.not_valid_after,
                    &cert.signature[..4]
                );
                // The signed message is: version(2) || valid_from(4) || not_valid_after(4)
                // concatenated with the server's static public key (32 bytes, x-only)
                // recovered from its EllSwift encoding; checked against the pool's
                // authority public key.
                match self.pool_authority_key {
                    Some(ref authority_key) => Self::ellswift_to_xonly(&server_s_bytes)
                        .and_then(|xonly| cert.verify_schnorr(authority_key, &xonly)),
                    None => {
                        log::info!("Noise: no pool authority key configured — TOFU mode");
                        Ok(())
                    }
                }
            }
            Err(e) => Err(format!("certificate parse failed: {}", e)),
        };

        if let Err(e) = &cert_check {
            if Self::cert_must_fail_closed(
                authority_pinned,
                false,
                Self::cert_verify_override_enabled(),
            ) {
                let msg = format!("Noise: server certificate verification failed: {}", e);
                log::error!("{}", msg);
                self.state = NoiseState::Failed(msg.clone());
                return Err(msg);
            }
            log::warn!(
                "Noise: certificate problem: {} (continuing — {})",
                e,
                if authority_pinned {
                    "LAB OVERRIDE ENABLED, INSECURE"
                } else {
                    "TOFU mode (no authority key)"
                }
            );
        } else if authority_pinned {
            log::info!("Noise: server certificate VERIFIED via BIP340 Schnorr");
        }

        // ── Split: derive final transport keys ───────────────────────────────
        // Noise spec §5.2 Split(): (k1, k2) = HKDF(ck, empty)
        // Initiator sends with k1, receives with k2.
        let (sending_key, receiving_key) = Self::hkdf_sha256(&self.chaining_key, &[]);

        // Zero handshake-phase state
        self.chaining_key = [0u8; 32];

        // Capture h before set_transport_keys overwrites it
        let final_h = self.handshake_hash;

        // Activate transport
        self.set_transport_keys(sending_key, receiving_key, final_h);

        log::info!("Noise: Noise_NX handshake COMPLETE — transport active");
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
        log::info!("Noise: transport keys set, ChaChaPoly1305 encryption active");
    }

    /// Mark the session as waiting for the server's handshake response.
    pub fn mark_waiting(&mut self) {
        self.state = NoiseState::WaitingForResponse;
    }

    /// Pin the pool authority public key (x-only, 32 bytes) used to verify the
    /// server's `SIGNATURE_NOISE_MESSAGE` certificate fail-closed.
    ///
    /// `Some(key)` activates BIP340 Schnorr certificate verification (MITM
    /// defense); `None` keeps the default trust-on-first-use (TOFU) behavior.
    /// Callers must re-apply this after any `reset()`/`new()` that rebuilds the
    /// session, otherwise a reconnect silently reverts to TOFU.
    pub fn set_pool_authority_key(&mut self, key: Option<[u8; 32]>) {
        self.pool_authority_key = key;
    }

    /// Mark the session as failed with a reason.
    pub fn fail(&mut self, reason: impl Into<String>) {
        let msg = reason.into();
        log::error!("Noise: session failed: {}", msg);
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

        // Fail closed at the nonce ceiling BEFORE encrypting, so the current
        // (about-to-be-reused) nonce is never emitted and we never wrap to 0.
        if self.sending_nonce >= MAX_TRANSPORT_NONCE {
            let msg = "Noise: sending nonce ceiling reached — reconnect required".to_string();
            self.state = NoiseState::Failed(msg.clone());
            return Err(msg);
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

        // Overflow-safe increment: the ceiling check above guarantees this never
        // wraps, but compute it fail-closed regardless.
        match self.sending_nonce.checked_add(1) {
            Some(next) => self.sending_nonce = next,
            None => {
                let msg = "Noise: sending nonce overflow — reconnect required".to_string();
                self.state = NoiseState::Failed(msg.clone());
                return Err(msg);
            }
        }
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

        if ciphertext.len() < 16 {
            return Err(format!(
                "Noise: ciphertext too short ({} bytes, need at least 16 for AEAD tag)",
                ciphertext.len()
            ));
        }

        // Fail closed at the nonce ceiling BEFORE decrypting (symmetric with
        // encrypt): never reuse the receiving nonce / wrap to 0.
        if self.receiving_nonce >= MAX_TRANSPORT_NONCE {
            let msg = "Noise: receiving nonce ceiling reached — reconnect required".to_string();
            self.state = NoiseState::Failed(msg.clone());
            return Err(msg);
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

        match self.receiving_nonce.checked_add(1) {
            Some(next) => self.receiving_nonce = next,
            None => {
                let msg = "Noise: receiving nonce overflow — reconnect required".to_string();
                self.state = NoiseState::Failed(msg.clone());
                return Err(msg);
            }
        }
        Ok(plaintext)
    }

    // =========================================================================
    // Development bypass
    // =========================================================================

    /// **DEVELOPMENT / TEST ONLY** — bypass the Noise handshake using deterministic
    /// test keys.
    ///
    /// SV2-10: gated behind `#[cfg(test)]` so it CANNOT exist in any non-test
    /// (release) build. This closes the supply-chain hole where a release path could
    /// install an identical, all-devices-shared transport key and skip the handshake.
    /// The only callers are the in-crate `#[cfg(test)]` module.
    #[cfg(test)]
    pub fn set_test_keys(&mut self) {
        let mut hasher = Sha256::new();
        hasher.update(b"DCENT_axe_test_key_DO_NOT_USE_IN_PRODUCTION");
        let out = hasher.finalize();
        let mut test_key = [0u8; 32];
        test_key.copy_from_slice(&out);

        self.set_transport_keys(test_key, test_key, [0u8; 32]);
        log::warn!("Noise: *** USING TEST KEYS — NOT SECURE — DEVELOPMENT ONLY ***");
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

    /// Recover the x-only-serialized secp256k1 public key (32 bytes) from a
    /// 64-byte EllSwift handshake encoding.
    ///
    /// The SV2 SignatureNoiseMessage is signed over the server's x-only static
    /// key, not over the raw EllSwift bytes, so the certificate signature can only
    /// be verified after decoding the EllSwift encoding back to a public key.
    fn ellswift_to_xonly(ellswift: &[u8]) -> Result<[u8; 32], String> {
        use secp256k1::PublicKey;

        if ellswift.len() != ELLSWIFT_SIZE {
            return Err(format!(
                "EllSwift encoding wrong length: {} (expected {})",
                ellswift.len(),
                ELLSWIFT_SIZE
            ));
        }
        let mut arr = [0u8; ELLSWIFT_SIZE];
        arr.copy_from_slice(ellswift);
        let es = ElligatorSwift::from_array(arr);
        let pk = PublicKey::from_ellswift(es);
        Ok(pk.x_only_public_key().0.serialize())
    }

    /// **LAB-ONLY / INSECURE** — when set, allows the Noise handshake to continue
    /// past a failed/unparseable server certificate even when a pool authority key
    /// is pinned. Default-OFF. MUST NEVER be enabled in shipped firmware; it exists
    /// only for bench bring-up. Mirrors the `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS`
    /// option_env!/env::var convention used elsewhere in the firmware.
    fn cert_verify_override_enabled() -> bool {
        option_env!("DCENTAXE_SV2_INSECURE_SKIP_CERT_VERIFY") == Some("1")
            || std::env::var("DCENTAXE_SV2_INSECURE_SKIP_CERT_VERIFY")
                .map(|v| v == "1")
                .unwrap_or(false)
    }

    /// Pure fail-closed decision: abort the handshake ONLY when a pool authority
    /// key is pinned, the certificate did not verify, and the lab override is off.
    fn cert_must_fail_closed(
        authority_pinned: bool,
        cert_ok: bool,
        override_enabled: bool,
    ) -> bool {
        authority_pinned && !cert_ok && !override_enabled
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

// SV2-e2e: this test module is exposed `pub(crate)` (still `#[cfg(test)]`-gated, so
// it does not exist in any release build) so the encrypted end-to-end harness in
// `client.rs` can reuse the in-process responder fixture `build_server_response`
// instead of duplicating the responder-side Noise crypto. Test-only seam.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_transport_session(send_key: [u8; 32], recv_key: [u8; 32]) -> NoiseSession {
        let mut s = NoiseSession::new();
        s.set_transport_keys(send_key, recv_key, [0u8; 32]);
        s
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
    ///
    /// SV2-e2e: `pub(crate)` (test-only) so the encrypted end-to-end harness in
    /// `client.rs` can pair this in-process responder against the real client
    /// without re-implementing the responder Noise crypto. Logic is unchanged.
    pub(crate) fn build_server_response(
        client_e_ellswift: &[u8; ELLSWIFT_SIZE],
        client_h_after_e: &[u8; 32],
        client_ck_after_init: &[u8; 32],
        server_e_seed: [u8; 64],
        server_s_seed: [u8; 64],
        authority_secret: Option<[u8; 32]>,
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

        // EncryptAndHash(SIGNATURE_NOISE_MESSAGE) — certificate.
        // When `authority_secret` is Some, build a GENUINE BIP340 Schnorr signature
        // over the same signed-message digest the client verifies (via the shared
        // `signed_digest` source of truth); otherwise use a dummy 0xAA signature.
        // version/valid_from/not_valid_after are bound to locals so the payload and
        // the signed digest use byte-identical values.
        let cert_version: u16 = 0;
        let cert_valid_from: u32 = 0;
        let cert_not_valid_after: u32 = u32::MAX;
        let cert_signature: [u8; 64] = match authority_secret {
            Some(auth_secret) => {
                use secp256k1::{Keypair, Message, PublicKey};
                // Recover the server's x-only static key from its EllSwift encoding,
                // exactly as the client does in `ellswift_to_xonly`.
                let static_xonly = PublicKey::from_ellswift(rs_es)
                    .x_only_public_key()
                    .0
                    .serialize();
                let digest = SignatureNoiseMessage::signed_digest(
                    cert_version,
                    cert_valid_from,
                    cert_not_valid_after,
                    &static_xonly,
                );
                let sk = SecretKey::from_slice(&auth_secret).unwrap();
                let kp = Keypair::from_secret_key(&secp, &sk);
                secp.sign_schnorr_no_aux_rand(&Message::from_digest_slice(&digest).unwrap(), &kp)
                    .serialize()
            }
            None => [0xAA; 64],
        };

        let mut cert_payload = Vec::with_capacity(SIGNATURE_NOISE_MESSAGE_SIZE);
        cert_payload.extend_from_slice(&cert_version.to_le_bytes()); // version
        cert_payload.extend_from_slice(&cert_valid_from.to_le_bytes()); // valid_from
        cert_payload.extend_from_slice(&cert_not_valid_after.to_le_bytes()); // not_valid_after
        cert_payload.extend_from_slice(&cert_signature); // signature

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

    // ── SV2-6: nonce ceiling fail-closed ──────────────────────────────────────

    /// At the sending-nonce ceiling, encrypt() must fail closed (Err + Failed
    /// state) BEFORE encrypting, and must keep failing — never wrapping to 0.
    #[test]
    fn test_encrypt_fails_at_nonce_ceiling() {
        let mut s = make_transport_session([0x11u8; 32], [0x22u8; 32]);
        s.set_sending_nonce_for_test(MAX_TRANSPORT_NONCE);

        let r1 = s.encrypt(b"at ceiling");
        assert!(r1.is_err(), "encrypt at ceiling must fail");
        assert!(
            matches!(s.state, NoiseState::Failed(_)),
            "must enter Failed"
        );
        // Counter did NOT wrap to 0.
        assert_eq!(s.sending_nonce(), MAX_TRANSPORT_NONCE);

        // A subsequent encrypt still errs (no reuse of nonce 0).
        let r2 = s.encrypt(b"again");
        assert!(r2.is_err(), "must keep failing after ceiling");
    }

    /// Symmetric: decrypt() fails closed at the receiving-nonce ceiling.
    #[test]
    fn test_decrypt_fails_at_nonce_ceiling() {
        // Build a real ciphertext from a fresh session first.
        let key = [0x33u8; 32];
        let mut enc = make_transport_session(key, [0u8; 32]);
        let ct = enc.encrypt(b"payload").unwrap();

        let mut dec = make_transport_session([0u8; 32], key);
        dec.set_receiving_nonce_for_test(MAX_TRANSPORT_NONCE);

        let r1 = dec.decrypt(&ct);
        assert!(r1.is_err(), "decrypt at ceiling must fail");
        assert!(
            matches!(dec.state, NoiseState::Failed(_)),
            "must enter Failed"
        );
        assert_eq!(dec.receiving_nonce(), MAX_TRANSPORT_NONCE, "no wrap to 0");
    }

    /// Regression: normal nonce increment is unaffected by the ceiling guard —
    /// two encrypts from 0 still advance the counter to 2.
    #[test]
    fn test_normal_nonce_increment_unaffected() {
        let mut s = make_transport_session([0x44u8; 32], [0x55u8; 32]);
        assert_eq!(s.sending_nonce(), 0);
        let _ = s.encrypt(b"one").unwrap();
        let _ = s.encrypt(b"two").unwrap();
        assert_eq!(s.sending_nonce(), 2);
        assert_eq!(s.state, NoiseState::Transport, "still healthy");
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
            None,
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
            None,
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
            None,
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

    // ── SV2-1: certificate fail-closed state transitions ──────────────────────

    /// Pinned authority key + an invalid (dummy) cert signature MUST abort the
    /// handshake: state=Failed, Err returned, never reaches Transport. This is the
    /// core SV2-1 security property — connect() propagates the Err and never mines.
    #[test]
    fn test_cert_fail_closed_when_pinned_and_sig_invalid() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xD1u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        // Pin a valid authority x-only key (derived from a known secret).
        let secp = Secp256k1::new();
        let auth_sk = SecretKey::from_slice(&[0x07u8; 32]).unwrap();
        let auth_xonly = auth_sk.x_only_public_key(&secp).0.serialize();
        client.pool_authority_key = Some(auth_xonly);

        // Server sends a dummy (0xAA) signature → must fail-closed.
        let (server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE7u8; 64],
            [0x7Eu8; 64],
            None,
        );

        let result = client.initiator_handshake_finish(&server_msg);
        assert!(
            result.is_err(),
            "pinned authority + invalid cert must abort"
        );
        assert!(
            matches!(client.state, NoiseState::Failed(_)),
            "state must be Failed, got {:?}",
            client.state
        );
        assert!(!client.is_transport(), "must not reach Transport");
    }

    /// With no authority key pinned, the handshake falls through to TOFU and
    /// completes into Transport — preserving today's default behavior.
    #[test]
    fn test_cert_tofu_when_unpinned() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xD2u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        // pool_authority_key left None → TOFU.
        let (server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE8u8; 64],
            [0x8Eu8; 64],
            None,
        );

        assert!(client.pool_authority_key.is_none());
        client.initiator_handshake_finish(&server_msg).unwrap();
        assert_eq!(client.state, NoiseState::Transport);
    }

    // ── SV2-2: EllSwift decode + correct signed message ───────────────────────

    /// Positive path: the server signs the certificate over the SV2 signed digest
    /// (EllSwift-decoded x-only static key + LE fields + single SHA256) with the
    /// matching authority secret → verification succeeds and Transport is reached.
    #[test]
    fn test_cert_verified_when_correctly_signed() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xD3u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        let secp = Secp256k1::new();
        let auth_secret = [0x11u8; 32];
        let auth_sk = SecretKey::from_slice(&auth_secret).unwrap();
        let auth_xonly = auth_sk.x_only_public_key(&secp).0.serialize();
        client.pool_authority_key = Some(auth_xonly);

        let (server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xE9u8; 64],
            [0x9Eu8; 64],
            Some(auth_secret),
        );

        client
            .initiator_handshake_finish(&server_msg)
            .expect("correctly-signed cert must verify");
        assert_eq!(client.state, NoiseState::Transport);
        assert!(client.is_transport());
    }

    /// Negative path: the server signs with authority key A, but the client pins a
    /// different authority key B (MITM / wrong-pinned-key). Must fail-closed.
    #[test]
    fn test_cert_fail_closed_wrong_authority_key() {
        let mut client = NoiseSession::new();
        let e_pub_vec = client.initiator_handshake_start([0xD4u8; 64]).unwrap();
        let mut e_pub_arr = [0u8; ELLSWIFT_SIZE];
        e_pub_arr.copy_from_slice(&e_pub_vec);

        let client_h = *client.handshake_hash();
        let client_ck = client.chaining_key_for_test();

        let secp = Secp256k1::new();
        let auth_secret_a = [0x21u8; 32];
        let auth_secret_b = [0x22u8; 32];
        // Client pins key B; server signs with key A.
        let auth_sk_b = SecretKey::from_slice(&auth_secret_b).unwrap();
        let auth_xonly_b = auth_sk_b.x_only_public_key(&secp).0.serialize();
        client.pool_authority_key = Some(auth_xonly_b);

        let (server_msg, _, _) = build_server_response(
            &e_pub_arr,
            &client_h,
            &client_ck,
            [0xEAu8; 64],
            [0xAEu8; 64],
            Some(auth_secret_a),
        );

        let result = client.initiator_handshake_finish(&server_msg);
        assert!(result.is_err(), "wrong pinned authority key must abort");
        assert!(matches!(client.state, NoiseState::Failed(_)));
        assert!(!client.is_transport());
    }

    /// `ellswift_to_xonly` must round-trip an EllSwift encoding back to the same
    /// x-only pubkey the secret key yields, and reject wrong-length input.
    #[test]
    fn test_ellswift_to_xonly_matches_pubkey() {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[0x33u8; 32]).unwrap();
        let es = ElligatorSwift::from_seckey(&secp, sk, None);
        let decoded = NoiseSession::ellswift_to_xonly(&es.to_array()).unwrap();
        let expected = sk.x_only_public_key(&secp).0.serialize();
        assert_eq!(
            decoded, expected,
            "EllSwift decode must match x-only pubkey"
        );

        // Wrong-length input must error.
        assert!(NoiseSession::ellswift_to_xonly(&[0u8; 10]).is_err());
        assert!(NoiseSession::ellswift_to_xonly(&[0u8; 65]).is_err());
    }

    /// Exhaustive truth table for the pure fail-closed decision — covers the lab
    /// override semantics with NO global env mutation (zero parallel-test flakiness).
    #[test]
    fn test_cert_must_fail_closed_truth_table() {
        // Aborts ONLY when (pinned && !cert_ok && !override).
        assert!(
            NoiseSession::cert_must_fail_closed(true, false, false),
            "pinned + bad cert + no override => abort"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(true, false, true),
            "override suppresses abort"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(true, true, false),
            "good cert never aborts"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(true, true, true),
            "good cert + override never aborts"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(false, false, false),
            "unpinned (TOFU) never aborts"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(false, false, true),
            "unpinned + override never aborts"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(false, true, false),
            "unpinned + good cert never aborts"
        );
        assert!(
            !NoiseSession::cert_must_fail_closed(false, true, true),
            "unpinned + good cert + override never aborts"
        );
    }

    // ── SV2-activate: authority-pubkey config parsing (base58check) ───────────

    /// Re-encode a payload as Base58Check so the round-trip is testable without
    /// pulling in the `bs58` crate. Ported (test-only) from DCENT_OS
    /// `dcentrald-stratum/src/v2/auth.rs::tests::base58check_encode`.
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

    /// Encode a version-1 (LE `0x0001`) authority-key token for a 32-byte key.
    fn encode_authority_token(key: [u8; 32]) -> String {
        let mut payload = vec![0x01u8, 0x00];
        payload.extend_from_slice(&key);
        base58check_encode(&payload)
    }

    /// Bare base58check token round-trips back to the exact 32-byte key.
    #[test]
    fn test_parse_pool_authority_pubkey_roundtrip() {
        let key = [0x7Au8; 32];
        let token = encode_authority_token(key);
        let parsed = parse_pool_authority_pubkey(&token).unwrap();
        assert_eq!(parsed, key);
    }

    /// The same token carried in an SV2 URL path also parses to the key.
    #[test]
    fn test_parse_pool_authority_pubkey_from_sv2_url() {
        let key = [0x5Au8; 32];
        let token = encode_authority_token(key);
        let url = format!("stratum2+tcp://pool.example.com:34254/{}", token);
        let parsed = parse_pool_authority_pubkey(&url).unwrap();
        assert_eq!(parsed, key);
    }

    /// Malformed / empty / wrong-version / wrong-length inputs all error (so the
    /// caller can fall back to TOFU instead of mis-pinning).
    #[test]
    fn test_parse_pool_authority_pubkey_rejects_bad_input() {
        // Empty.
        assert!(parse_pool_authority_pubkey("").is_err());
        assert!(parse_pool_authority_pubkey("   ").is_err());

        // Invalid base58 character ('0', 'O', 'I', 'l' are not in the alphabet).
        assert!(parse_pool_authority_pubkey("not-valid-0OIl").is_err());

        // Checksum tamper: flip the last char of a valid token.
        let token = encode_authority_token([0x33u8; 32]);
        let mut chars: Vec<char> = token.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(parse_pool_authority_pubkey(&tampered).is_err());

        // Wrong version prefix (0x0009 instead of 0x0001).
        let mut wrong_ver = vec![0x09u8, 0x00];
        wrong_ver.extend_from_slice(&[0x11u8; 32]);
        assert!(parse_pool_authority_pubkey(&base58check_encode(&wrong_ver)).is_err());

        // Short payload (< 34 bytes: version + only 16 key bytes).
        let mut short = vec![0x01u8, 0x00];
        short.extend_from_slice(&[0x22u8; 16]);
        assert!(parse_pool_authority_pubkey(&base58check_encode(&short)).is_err());

        // SV2 URL with no path component (no key) is an error.
        assert!(parse_pool_authority_pubkey("stratum2+tcp://pool.example.com:3336").is_err());
    }

    /// `set_pool_authority_key` toggles the pinned-key state used by the
    /// fail-closed branch in `initiator_handshake_finish`.
    #[test]
    fn test_set_pool_authority_key_toggles_state() {
        let mut s = NoiseSession::new();
        assert!(s.pool_authority_key.is_none(), "default is TOFU (None)");
        s.set_pool_authority_key(Some([0x55u8; 32]));
        assert_eq!(s.pool_authority_key, Some([0x55u8; 32]));
        s.set_pool_authority_key(None);
        assert!(s.pool_authority_key.is_none());
    }

    /// `signed_digest` is deterministic and sensitive to every field — locks the
    /// signed-message field order/inclusion structurally.
    #[test]
    fn test_signed_digest_field_sensitive() {
        let xonly = [0x44u8; 32];
        let base = SignatureNoiseMessage::signed_digest(1, 100, 200, &xonly);
        // Same inputs → identical digest.
        assert_eq!(
            base,
            SignatureNoiseMessage::signed_digest(1, 100, 200, &xonly)
        );
        // Each field flip changes the digest.
        assert_ne!(
            base,
            SignatureNoiseMessage::signed_digest(2, 100, 200, &xonly)
        );
        assert_ne!(
            base,
            SignatureNoiseMessage::signed_digest(1, 101, 200, &xonly)
        );
        assert_ne!(
            base,
            SignatureNoiseMessage::signed_digest(1, 100, 201, &xonly)
        );
        let mut xonly2 = xonly;
        xonly2[0] ^= 0x01;
        assert_ne!(
            base,
            SignatureNoiseMessage::signed_digest(1, 100, 200, &xonly2)
        );
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
    /// Force the sending nonce counter (for SV2-6 ceiling tests).
    pub fn set_sending_nonce_for_test(&mut self, n: u64) {
        self.sending_nonce = n;
    }
    /// Force the receiving nonce counter (for SV2-6 ceiling tests).
    pub fn set_receiving_nonce_for_test(&mut self, n: u64) {
        self.receiving_nonce = n;
    }
}
