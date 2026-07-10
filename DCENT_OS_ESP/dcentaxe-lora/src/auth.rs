// SPDX-License-Identifier: GPL-3.0-or-later
//! Owner-authentication for over-the-air `$DCM` control frames.
//!
//! A control frame received over the air ([`MeshCommand`](crate::mesh::MeshCommand))
//! must clear TWO gates before it may reach any hardware write:
//!
//!   1. **Authenticity** — a valid HMAC-SHA256 tag computed over a canonical
//!      message that binds the source node, the frame sequence number, and the
//!      full verb/param/value ([`verify_command_mac`] /
//!      [`MeshCommand::authorize`](crate::mesh::MeshCommand::authorize)). No key
//!      configured, or a missing/short/long/non-hex tag ⇒ refused (fail-closed).
//!   2. **Freshness** — an anti-replay check ([`ReplayGuard`]) so a
//!      captured-and-rebroadcast frame (tag and all) is refused the second time.
//!
//! [`MeshAuthenticator`] bundles both so the mesh task calls a single method and
//! cannot forget the replay step. The MAC verify runs *before* the replay window
//! is advanced, so a forged frame can never burn a real source's sequence space.
//!
//! Crypto uses the audited RustCrypto `hmac` + `sha2` crates (already in the
//! workspace lock → zero new lock entries). `Mac::verify_slice` is a
//! `subtle`-backed constant-time comparison, so there is deliberately NO
//! `tag == expected` byte compare anywhere in this crate — the CI ban-gate that
//! forbids the old placeholder string-equality is satisfied by construction.

use crate::mesh::{MeshCommand, NodeId};
use crate::LoraError;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Length of a raw HMAC-SHA256 owner tag.
pub const MAC_TAG_LEN: usize = 32;
/// Length of the lowercase-hex wire form carried in `MeshCommand.auth`.
pub const MAC_HEX_LEN: usize = MAC_TAG_LEN * 2;

/// Owner-key length. A 256-bit key matches the tag width and the HMAC block use.
pub const OWNER_KEY_LEN: usize = 32;

/// Default forward replay window over the wrapping `u8` sequence space: a command
/// whose seq is within this many steps ahead of the last one accepted from that
/// source is admitted; anything at or behind the high-water mark is a replay.
pub const DEFAULT_REPLAY_WINDOW: u8 = 32;
/// Default number of distinct source nodes the [`ReplayGuard`] tracks. Bounded so
/// air traffic cannot grow the guard without limit (matches the peer-table cap).
pub const DEFAULT_REPLAY_TRACKED: usize = 32;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The canonical message an owner tag is computed over. Binding `src` + `seq`
/// (the nonce) + the mutation (`verb`/`param`/`value`) means no field can be
/// altered without invalidating the tag, and a tag minted for node A / seq N can
/// never be replayed as node B or seq M.
pub fn command_mac_message(src: NodeId, seq: u8, verb: &str, param: &str, value: &str) -> Vec<u8> {
    format!("dcm-cmd:{}:{seq:02x}:{verb}:{param}:{value}", src.to_hex()).into_bytes()
}

/// Compute the raw 32-byte owner tag for a command (used by senders and tests).
pub fn command_mac(
    key: &[u8],
    src: NodeId,
    seq: u8,
    verb: &str,
    param: &str,
    value: &str,
) -> [u8; MAC_TAG_LEN] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&command_mac_message(src, seq, verb, param, value));
    mac.finalize().into_bytes().into()
}

/// Lowercase-hex wire form of a raw tag — what a sender places in
/// `MeshCommand.auth`.
pub fn tag_to_hex(tag: &[u8; MAC_TAG_LEN]) -> String {
    let mut s = String::with_capacity(MAC_HEX_LEN);
    for b in tag {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_to_tag(s: &str) -> Option<[u8; MAC_TAG_LEN]> {
    if s.len() != MAC_HEX_LEN {
        return None;
    }
    let b = s.as_bytes();
    let mut out = [0u8; MAC_TAG_LEN];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (hexval(b[2 * i])? << 4) | hexval(b[2 * i + 1])?;
    }
    Some(out)
}

/// Verify a command's carried owner tag against `key` in constant time.
///
/// Fail-closed: an absent/empty key ⇒ [`LoraError::Unauthorized`]; a missing,
/// wrong-length, or non-hex tag ⇒ [`LoraError::Unauthorized`]. This is the
/// authenticity gate only — it is stateless and does NOT advance replay state
/// (that is [`ReplayGuard`]/[`MeshAuthenticator`]).
pub fn verify_command_mac(
    cmd: &MeshCommand,
    key: Option<&[u8]>,
    src: NodeId,
    seq: u8,
) -> Result<(), LoraError> {
    let key = key
        .filter(|k| !k.is_empty())
        .ok_or(LoraError::Unauthorized)?;
    let tag_hex = cmd.auth.as_deref().ok_or(LoraError::Unauthorized)?;
    let tag = hex_to_tag(tag_hex).ok_or(LoraError::Unauthorized)?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&command_mac_message(
        src, seq, &cmd.verb, &cmd.param, &cmd.value,
    ));
    // `verify_slice` performs a `subtle`-backed constant-time comparison.
    mac.verify_slice(&tag).map_err(|_| LoraError::Unauthorized)
}

/// Bounded, clock-free anti-replay guard. Tracks the last-accepted sequence
/// number per source node and admits a `(src, seq)` only when `seq` advances
/// that source's counter within a forward window over the wrapping `u8` space.
///
/// The first frame observed from a source is always admitted (it establishes the
/// high-water mark). When the tracked-source table is full, the oldest-tracked
/// entry is dropped; this only ever lets an attacker replay an *old* command from
/// an *evicted* source, and such a command still needs a valid MAC and is routed
/// through the same operating-point clamp as any owner write.
#[derive(Debug, Clone)]
pub struct ReplayGuard {
    entries: Vec<(NodeId, u8)>,
    tracked: usize,
    window: u8,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayGuard {
    /// A guard with the default window and tracked-source capacity.
    pub fn new() -> Self {
        Self::with_params(DEFAULT_REPLAY_TRACKED, DEFAULT_REPLAY_WINDOW)
    }

    /// A guard with explicit parameters (each clamped to ≥ 1).
    pub fn with_params(tracked: usize, window: u8) -> Self {
        Self {
            entries: Vec::new(),
            tracked: tracked.max(1),
            window: window.max(1),
        }
    }

    /// Admit `(src, seq)` if it advances that source's sequence within the
    /// forward window, recording the new high-water seq on success. Returns
    /// `false` for an exact replay or a seq outside the window (too old / wrapped
    /// backward). Idempotent on rejection — a rejected seq never mutates state.
    pub fn admit(&mut self, src: NodeId, seq: u8) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|(id, _)| *id == src) {
            let forward = seq.wrapping_sub(entry.1);
            if forward == 0 || forward > self.window {
                return false;
            }
            entry.1 = seq;
            return true;
        }
        if self.entries.len() >= self.tracked {
            self.entries.remove(0);
        }
        self.entries.push((src, seq));
        true
    }

    /// Number of distinct sources currently tracked.
    pub fn tracked_len(&self) -> usize {
        self.entries.len()
    }
}

/// Full owner-command gate: authenticity (HMAC) THEN freshness (anti-replay).
/// Bundling both behind one call means the mesh command dispatcher cannot verify
/// the MAC and forget the replay check. The replay window is advanced only after
/// the tag verifies.
#[derive(Debug, Clone)]
pub struct MeshAuthenticator {
    key: Option<[u8; OWNER_KEY_LEN]>,
    replay: ReplayGuard,
}

impl MeshAuthenticator {
    /// Build an authenticator with the configured owner key (`None` ⇒ every
    /// command is refused — the fail-closed default before provisioning).
    pub fn new(key: Option<[u8; OWNER_KEY_LEN]>) -> Self {
        Self {
            key,
            replay: ReplayGuard::new(),
        }
    }

    /// Replace the owner key (e.g. after a bridge/NVS provisioning update).
    pub fn set_key(&mut self, key: Option<[u8; OWNER_KEY_LEN]>) {
        self.key = key;
    }

    /// `true` once an owner key is configured (a mesh node with no key can only
    /// beacon/relay — never accept a command).
    pub fn is_provisioned(&self) -> bool {
        self.key.is_some()
    }

    /// Authorize an air-received command: verify the MAC, then admit it through
    /// the replay window. Both must pass; a forged tag never advances the window.
    pub fn authorize_command(
        &mut self,
        cmd: &MeshCommand,
        src: NodeId,
        seq: u8,
    ) -> Result<(), LoraError> {
        verify_command_mac(cmd, self.key.as_ref().map(|k| k.as_slice()), src, seq)?;
        if self.replay.admit(src, seq) {
            Ok(())
        } else {
            Err(LoraError::Unauthorized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshCommand;

    const SRC: NodeId = NodeId(0xdead_beef);

    fn signed(
        key: &[u8],
        src: NodeId,
        seq: u8,
        verb: &str,
        param: &str,
        value: &str,
    ) -> MeshCommand {
        MeshCommand {
            verb: verb.into(),
            param: param.into(),
            value: value.into(),
            auth: Some(tag_to_hex(&command_mac(key, src, seq, verb, param, value))),
        }
    }

    // ---- RFC 4231 HMAC-SHA256 known-answer vectors (pins our HMAC usage) ----

    fn hmac_hex(key: &[u8], data: &[u8]) -> String {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(key).unwrap();
        mac.update(data);
        let tag: [u8; 32] = mac.finalize().into_bytes().into();
        tag_to_hex(&tag)
    }

    #[test]
    fn rfc4231_case1() {
        assert_eq!(
            hmac_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc4231_case2() {
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_case4() {
        let key: Vec<u8> = (1u8..=25).collect();
        assert_eq!(
            hmac_hex(&key, &[0xcd; 50]),
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
        );
    }

    // ---- hex round-trip ----

    #[test]
    fn tag_hex_round_trips() {
        let tag = command_mac(&[0x11; 32], SRC, 7, "set", "region", "eu868");
        let hex = tag_to_hex(&tag);
        assert_eq!(hex.len(), MAC_HEX_LEN);
        assert_eq!(hex_to_tag(&hex), Some(tag));
        assert_eq!(hex_to_tag("nothex"), None);
        assert_eq!(hex_to_tag(&"a".repeat(63)), None);
        assert_eq!(hex_to_tag(&"zz".repeat(32)), None);
    }

    // ---- MAC verify accept / reject matrix ----

    #[test]
    fn verify_accepts_correct_tag_only() {
        let key = [0x11u8; 32];
        let cmd = signed(&key, SRC, 9, "set", "beacon_interval_s", "300");
        assert!(cmd.authorize(Some(&key), SRC, 9).is_ok());

        // Wrong key, wrong src, wrong seq → all fail.
        assert_eq!(
            cmd.authorize(Some(&[0x22u8; 32]), SRC, 9),
            Err(LoraError::Unauthorized)
        );
        assert_eq!(
            cmd.authorize(Some(&key), NodeId(0x1234_5678), 9),
            Err(LoraError::Unauthorized)
        );
        assert_eq!(
            cmd.authorize(Some(&key), SRC, 10),
            Err(LoraError::Unauthorized)
        );
    }

    #[test]
    fn verify_rejects_tampered_fields() {
        let key = [0x33u8; 32];
        let mut cmd = signed(&key, SRC, 3, "set", "region", "na915");
        // Same tag, but the value is swapped → MAC no longer matches.
        cmd.value = "eu868".into();
        assert_eq!(
            cmd.authorize(Some(&key), SRC, 3),
            Err(LoraError::Unauthorized)
        );
    }

    #[test]
    fn verify_fail_closed_on_missing_key_or_tag() {
        let key = [0x44u8; 32];
        let cmd = signed(&key, SRC, 1, "cmd", "identify", "");
        // No configured key ⇒ refused even with a valid tag.
        assert_eq!(cmd.authorize(None, SRC, 1), Err(LoraError::Unauthorized));
        assert_eq!(
            cmd.authorize(Some(&[]), SRC, 1),
            Err(LoraError::Unauthorized)
        );

        // No tag on the frame ⇒ refused.
        let untagged = MeshCommand {
            verb: "set".into(),
            param: "region".into(),
            value: "na915".into(),
            auth: None,
        };
        assert_eq!(
            untagged.authorize(Some(&key), SRC, 1),
            Err(LoraError::Unauthorized)
        );

        // Malformed tag (wrong length / non-hex) ⇒ refused, never panics.
        let bad = MeshCommand {
            auth: Some("deadbeef".into()),
            ..untagged.clone()
        };
        assert_eq!(
            bad.authorize(Some(&key), SRC, 1),
            Err(LoraError::Unauthorized)
        );
    }

    // ---- ReplayGuard ----

    #[test]
    fn replay_guard_admits_forward_rejects_repeat_and_old() {
        let mut g = ReplayGuard::with_params(8, 16);
        assert!(g.admit(SRC, 5), "first frame establishes high-water");
        assert!(!g.admit(SRC, 5), "exact replay rejected");
        assert!(g.admit(SRC, 6), "next in sequence admitted");
        assert!(!g.admit(SRC, 6), "replay of the new high-water rejected");
        assert!(!g.admit(SRC, 4), "behind the high-water rejected");
        assert!(g.admit(SRC, 20), "within forward window admitted");
        assert!(!g.admit(SRC, 40), "beyond forward window rejected");
    }

    #[test]
    fn replay_guard_is_wrap_tolerant() {
        let mut g = ReplayGuard::with_params(8, 16);
        assert!(g.admit(SRC, 250));
        assert!(
            g.admit(SRC, 3),
            "250 -> 3 is +9 across the u8 wrap → admitted"
        );
        assert!(
            !g.admit(SRC, 250),
            "the pre-wrap value is now old → rejected"
        );
    }

    #[test]
    fn replay_guard_tracks_per_source_and_is_bounded() {
        let mut g = ReplayGuard::with_params(2, 16);
        assert!(g.admit(NodeId(0xA), 1));
        assert!(g.admit(NodeId(0xB), 1));
        assert_eq!(g.tracked_len(), 2);
        // Third distinct source evicts the oldest-tracked (0xA).
        assert!(g.admit(NodeId(0xC), 1));
        assert_eq!(g.tracked_len(), 2);
        // 0xA was evicted → its seq-1 is treated as first-seen again.
        assert!(g.admit(NodeId(0xA), 1));
    }

    // ---- MeshAuthenticator: MAC + replay combined ----

    #[test]
    fn authenticator_requires_mac_then_freshness() {
        let key = [0x55u8; 32];
        let mut auth = MeshAuthenticator::new(Some(key));
        assert!(auth.is_provisioned());

        let cmd = signed(&key, SRC, 1, "cmd", "identify", "");
        assert!(auth.authorize_command(&cmd, SRC, 1).is_ok());
        // Replaying the exact same authenticated frame is refused by freshness.
        assert_eq!(
            auth.authorize_command(&cmd, SRC, 1),
            Err(LoraError::Unauthorized)
        );
    }

    #[test]
    fn authenticator_forged_tag_never_advances_replay_window() {
        let key = [0x66u8; 32];
        let mut auth = MeshAuthenticator::new(Some(key));

        // A forged command at seq 5 is refused AND must not consume seq 5, so a
        // later genuine seq-5 command still authenticates.
        let forged = MeshCommand {
            verb: "set".into(),
            param: "region".into(),
            value: "na915".into(),
            auth: Some("00".repeat(32)),
        };
        assert_eq!(
            auth.authorize_command(&forged, SRC, 5),
            Err(LoraError::Unauthorized)
        );

        let genuine = signed(&key, SRC, 5, "set", "region", "na915");
        assert!(auth.authorize_command(&genuine, SRC, 5).is_ok());
    }

    #[test]
    fn authenticator_unprovisioned_refuses_everything() {
        let mut auth = MeshAuthenticator::new(None);
        assert!(!auth.is_provisioned());
        let cmd = signed(&[0x77u8; 32], SRC, 1, "cmd", "ping", "");
        assert_eq!(
            auth.authorize_command(&cmd, SRC, 1),
            Err(LoraError::Unauthorized)
        );
    }
}
