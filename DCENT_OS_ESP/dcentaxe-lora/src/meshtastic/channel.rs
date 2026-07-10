// SPDX-License-Identifier: GPL-3.0-or-later
//! Meshtastic channel crypto: the shared-PSK AES-CTR that protects a channel's
//! payload, plus the channel-hash byte that tags which channel a packet is on.
//!
//! **Threat model (be honest).** Meshtastic channel encryption is *shared-key*:
//! every node on a channel holds the same PSK, and the common case is the
//! well-known 1-byte default key. It keeps casual listeners out and lets a Router
//! carry traffic it cannot read — it is NOT a strong per-node confidentiality
//! guarantee, and DCENT_axe does not present it as one. (Owner *control* over the
//! mesh rides the separate HMAC-authenticated `$DCM` path in [`crate::auth`], not
//! this channel crypto.) We use the audited, constant-time RustCrypto `aes`+`ctr`
//! so the primitive itself is sound; the weakness is the shared-key model, which
//! is Meshtastic's design, not ours.
//!
//! Wire-exact details that MUST match upstream or interop silently fails:
//!   * **Cipher:** AES-128-CTR (16-byte key) or AES-256-CTR (32-byte key), with
//!     the full 16-byte nonce used as a **big-endian** counter block — RustCrypto
//!     [`ctr::Ctr128BE`] is byte-identical to the mbedtls `aes_crypt_ctr` the
//!     device firmware uses.
//!   * **Nonce:** `packetId` (LE, low 32 bits) ‖ 0×4 ‖ `fromNode` (LE) ‖ 0×4.
//!   * **Key expansion:** a 1-byte PSK `n` selects the 16-byte default key with
//!     its last byte offset by `n-1`; `0` = no crypto; 16/32 bytes = use directly.
//!   * **Channel hash:** `xorHash(name) ^ xorHash(expandedKeyBytes)` — the byte
//!     placed in the packet header so a receiver can pick the channel fast.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{KeyIvInit, StreamCipher};
use aes::{Aes128, Aes256};

/// Full 16-byte big-endian counter-block CTR — the mode Meshtastic/mbedtls use.
type Aes128Ctr = ctr::Ctr128BE<Aes128>;
type Aes256Ctr = ctr::Ctr128BE<Aes256>;

/// Meshtastic's built-in default 16-byte AES key (the base for 1-byte PSKs).
/// This is a PUBLIC, well-known constant — anyone can read the default channel.
pub const DEFAULT_KEY: [u8; 16] = [
    0xd4, 0xf1, 0xbb, 0x3a, 0x20, 0x29, 0x07, 0x59, 0xf0, 0xbc, 0xff, 0xab, 0xcf, 0x4e, 0x69, 0x01,
];

/// The preset name of Meshtastic's default primary channel — the string hashed
/// into the channel byte when a channel carries no explicit name.
pub const DEFAULT_CHANNEL_NAME: &str = "LongFast";

/// XOR of every byte — Meshtastic's `xorHash`, used for the channel hash.
pub fn xor_hash(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc ^ b)
}

/// The expanded channel key: no crypto, AES-128, or AES-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelKey {
    /// Plaintext channel (`psk` empty or the single byte `0`).
    None,
    Aes128([u8; 16]),
    Aes256([u8; 32]),
}

impl ChannelKey {
    /// Expand a raw PSK into a channel key, per Meshtastic's rules:
    ///   * `[]` or `[0]` ⇒ [`None`](Self::None) (plaintext).
    ///   * `[n]` (n≥1) ⇒ default key with its last byte set to `0x01 + (n-1)`
    ///     (so `[1]` is the exact default key).
    ///   * 16 bytes ⇒ AES-128; 32 bytes ⇒ AES-256, used verbatim.
    ///   * any other length ⇒ `None` (invalid PSK, rejected).
    pub fn from_psk(psk: &[u8]) -> Option<ChannelKey> {
        match psk.len() {
            0 => Some(ChannelKey::None),
            1 => {
                if psk[0] == 0 {
                    Some(ChannelKey::None)
                } else {
                    let mut k = DEFAULT_KEY;
                    k[15] = 0x01u8.wrapping_add(psk[0] - 1);
                    Some(ChannelKey::Aes128(k))
                }
            }
            16 => Some(ChannelKey::Aes128(psk.try_into().unwrap())),
            32 => Some(ChannelKey::Aes256(psk.try_into().unwrap())),
            _ => None,
        }
    }

    /// The raw key bytes fed into the channel hash (empty for a plaintext channel).
    pub fn key_bytes(&self) -> &[u8] {
        match self {
            ChannelKey::None => &[],
            ChannelKey::Aes128(k) => k,
            ChannelKey::Aes256(k) => k,
        }
    }

    /// `true` for an encrypting channel.
    pub fn is_encrypted(&self) -> bool {
        !matches!(self, ChannelKey::None)
    }
}

/// Build the 16-byte AES-CTR nonce (initial counter block) for a packet:
/// `packetId` little-endian in bytes 0..4, zero in 4..8, `fromNode`
/// little-endian in 8..12, zero in 12..16 (the extra-nonce slot, unused for
/// shared-PSK channels).
pub fn nonce(from_node: u32, packet_id: u32) -> [u8; 16] {
    let mut n = [0u8; 16];
    n[0..4].copy_from_slice(&packet_id.to_le_bytes());
    n[8..12].copy_from_slice(&from_node.to_le_bytes());
    n
}

/// A resolved Meshtastic channel: its name + expanded key. Encrypt/decrypt of a
/// payload is symmetric (CTR), and [`Channel::hash`] gives the header channel byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub key: ChannelKey,
}

impl Channel {
    /// Build a channel from a name + raw PSK. An empty name uses
    /// [`DEFAULT_CHANNEL_NAME`] for hashing (Meshtastic uses the preset name when
    /// a channel is unnamed). Returns `None` for an invalid PSK length.
    pub fn new(name: &str, psk: &[u8]) -> Option<Channel> {
        let key = ChannelKey::from_psk(psk)?;
        let name = if name.is_empty() {
            DEFAULT_CHANNEL_NAME.to_string()
        } else {
            name.to_string()
        };
        Some(Channel { name, key })
    }

    /// The default Meshtastic primary channel (`LongFast`, default key).
    pub fn default_primary() -> Channel {
        Channel::new(DEFAULT_CHANNEL_NAME, &[1]).expect("default psk is valid")
    }

    /// The channel hash byte placed in the packet header:
    /// `xorHash(name) ^ xorHash(expandedKeyBytes)`.
    pub fn hash(&self) -> u8 {
        xor_hash(self.name.as_bytes()) ^ xor_hash(self.key.key_bytes())
    }

    /// Encrypt (or, on a plaintext channel, leave untouched) `data` in place for
    /// the given `(from, id)`. CTR is symmetric, so [`Self::decrypt`] is the same
    /// operation — provided for call-site clarity.
    pub fn encrypt(&self, from_node: u32, packet_id: u32, data: &mut [u8]) {
        self.apply(from_node, packet_id, data);
    }

    /// Decrypt `data` in place (identical to [`Self::encrypt`] — CTR keystream).
    pub fn decrypt(&self, from_node: u32, packet_id: u32, data: &mut [u8]) {
        self.apply(from_node, packet_id, data);
    }

    fn apply(&self, from_node: u32, packet_id: u32, data: &mut [u8]) {
        let iv = nonce(from_node, packet_id);
        match &self.key {
            ChannelKey::None => {}
            ChannelKey::Aes128(k) => {
                let mut c =
                    Aes128Ctr::new(GenericArray::from_slice(k), GenericArray::from_slice(&iv));
                c.apply_keystream(data);
            }
            ChannelKey::Aes256(k) => {
                let mut c =
                    Aes256Ctr::new(GenericArray::from_slice(k), GenericArray::from_slice(&iv));
                c.apply_keystream(data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexb(s: &str) -> Vec<u8> {
        assert!(s.len() % 2 == 0);
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    // ---- key expansion ----

    #[test]
    fn one_byte_psk_expands_to_default_key_family() {
        // [1] is exactly the default key.
        assert_eq!(
            ChannelKey::from_psk(&[1]),
            Some(ChannelKey::Aes128(DEFAULT_KEY))
        );
        // [2] offsets the last byte by +1.
        let mut k2 = DEFAULT_KEY;
        k2[15] = 0x02;
        assert_eq!(ChannelKey::from_psk(&[2]), Some(ChannelKey::Aes128(k2)));
        // [0] and [] are plaintext.
        assert_eq!(ChannelKey::from_psk(&[0]), Some(ChannelKey::None));
        assert_eq!(ChannelKey::from_psk(&[]), Some(ChannelKey::None));
    }

    #[test]
    fn full_length_psks_used_verbatim_and_bad_length_rejected() {
        let k16 = [0x11u8; 16];
        let k32 = [0x22u8; 32];
        assert_eq!(ChannelKey::from_psk(&k16), Some(ChannelKey::Aes128(k16)));
        assert_eq!(ChannelKey::from_psk(&k32), Some(ChannelKey::Aes256(k32)));
        // 15 / 20 / 33 byte PSKs are invalid.
        assert_eq!(ChannelKey::from_psk(&[0u8; 15]), None);
        assert_eq!(ChannelKey::from_psk(&[0u8; 20]), None);
        assert_eq!(ChannelKey::from_psk(&[0u8; 33]), None);
    }

    // ---- channel hash (the Meshtastic-specific KAT) ----

    #[test]
    fn default_longfast_channel_hash_is_0x08() {
        // The single most load-bearing interop constant: a DCENT_axe on the
        // default primary channel MUST stamp header.channel = 0x08 or no stock
        // Meshtastic node will accept its packets on that channel.
        assert_eq!(Channel::default_primary().hash(), 0x08);
    }

    #[test]
    fn channel_hash_components() {
        assert_eq!(xor_hash(b"LongFast"), 0x0a);
        assert_eq!(xor_hash(&DEFAULT_KEY), 0x02);
        assert_eq!(0x0a ^ 0x02, 0x08);
    }

    // ---- nonce construction ----

    #[test]
    fn nonce_layout_is_pinned() {
        let n = nonce(0x1122_3344, 0xaabb_ccdd);
        assert_eq!(
            n,
            [
                0xdd, 0xcc, 0xbb, 0xaa, // packet_id LE
                0x00, 0x00, 0x00, 0x00, // high 32 of id = 0
                0x44, 0x33, 0x22, 0x11, // from LE
                0x00, 0x00, 0x00, 0x00, // extra nonce = 0
            ]
        );
    }

    // ---- AES-CTR correctness vs NIST SP800-38A ----

    #[test]
    fn aes128_ctr_matches_nist_sp800_38a_f51() {
        // NIST SP800-38A F.5.1 CTR-AES128.Encrypt (4 blocks — exercises the
        // ff→00 counter carry between block 1 and 2).
        let key = hexb("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hexb("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let pt = hexb(
            "6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710",
        );
        let ct = hexb(
            "874d6191b620e3261bef6864990db6ce\
             9806f66b7970fdff8617187bb9fffdff\
             5ae4df3edbd5d35e5b4f09020db03eab\
             1e031dda2fbe03d1792170a0f3009cee",
        );
        let mut c = Aes128Ctr::new(
            aes::cipher::generic_array::GenericArray::from_slice(&key),
            aes::cipher::generic_array::GenericArray::from_slice(&iv),
        );
        let mut buf = pt.clone();
        c.apply_keystream(&mut buf);
        assert_eq!(buf, ct, "AES-128-CTR keystream must match NIST");
    }

    #[test]
    fn aes256_ctr_matches_nist_sp800_38a_f55() {
        // NIST SP800-38A F.5.5 CTR-AES256.Encrypt (block 1).
        let key = hexb("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = hexb("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let pt = hexb("6bc1bee22e409f96e93d7e117393172a");
        let ct = hexb("601ec313775789a5b7a7f504bbf3d228");
        let mut c = Aes256Ctr::new(
            aes::cipher::generic_array::GenericArray::from_slice(&key),
            aes::cipher::generic_array::GenericArray::from_slice(&iv),
        );
        let mut buf = pt.clone();
        c.apply_keystream(&mut buf);
        assert_eq!(buf, ct, "AES-256-CTR keystream must match NIST");
    }

    // ---- channel encrypt/decrypt round-trip via the nonce path ----

    #[test]
    fn channel_encrypt_decrypt_round_trips() {
        let ch = Channel::default_primary();
        let plaintext = b"the times 03/jan/2009".to_vec();
        let mut buf = plaintext.clone();
        ch.encrypt(0x0000_00a1, 0x0000_1234, &mut buf);
        assert_ne!(buf, plaintext, "ciphertext must differ from plaintext");
        ch.decrypt(0x0000_00a1, 0x0000_1234, &mut buf);
        assert_eq!(buf, plaintext, "decrypt must recover the plaintext");
    }

    #[test]
    fn different_nonce_yields_different_ciphertext() {
        let ch = Channel::default_primary();
        let pt = [0u8; 32];
        let mut a = pt;
        let mut b = pt;
        ch.encrypt(1, 100, &mut a);
        ch.encrypt(1, 101, &mut b); // different packet id → different keystream
        assert_ne!(a, b);
    }

    #[test]
    fn plaintext_channel_is_a_noop() {
        let ch = Channel::new("open", &[0]).unwrap();
        assert!(!ch.key.is_encrypted());
        let mut buf = b"hello".to_vec();
        ch.encrypt(1, 2, &mut buf);
        assert_eq!(buf, b"hello", "plaintext channel must not mutate bytes");
    }

    #[test]
    fn aes256_channel_round_trips() {
        let ch = Channel::new("secure", &[0x5au8; 32]).unwrap();
        assert!(matches!(ch.key, ChannelKey::Aes256(_)));
        let pt = b"custom aes256 channel".to_vec();
        let mut buf = pt.clone();
        ch.encrypt(0xdead, 0xbeef, &mut buf);
        assert_ne!(buf, pt);
        ch.decrypt(0xdead, 0xbeef, &mut buf);
        assert_eq!(buf, pt);
    }
}
