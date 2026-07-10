// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime configuration for the `$DCM` mesh.
//!
//! The 2026-07 maturity audit found that region, relay role, cadence, and keys
//! were all **hardcoded consts** in `lora_task` — an operator could not enable
//! meshing, mark a node as a backbone router, or provision an owner key without a
//! recompile. [`MeshConfig`] is the missing knob surface: a serde struct with
//! `#[serde(default)]` on every field so a legacy NVS blob round-trips, mirroring
//! the binary's `MqttConfig`. The `dcentaxe` binary embeds it, persists it to
//! NVS, and hands it to `lora_task` at boot; the pure accessors below turn the
//! stored tokens/hex into the typed values the mesh stack consumes.
//!
//! **Fail-closed defaults:** meshing is OFF, and with no `owner_key_hex` the
//! [`CommandGate`](crate::gate::CommandGate) refuses every over-the-air control.

use crate::auth::OWNER_KEY_LEN;
use crate::flood::RelayRole;
use serde::{Deserialize, Serialize};

/// Default telemetry beacon cadence (seconds).
pub const DEFAULT_TELEMETRY_INTERVAL_S: u16 = 300;
/// Floor on the telemetry cadence so a misconfig can't spam the band (airtime is
/// still duty-cycle-bounded on top of this).
pub const MIN_TELEMETRY_INTERVAL_S: u16 = 10;
/// Length of a 256-bit key in lowercase-hex characters.
pub const KEY_HEX_LEN: usize = 64;

/// Operator-facing mesh configuration. Every field is `#[serde(default)]` so a
/// blob written by an older firmware (without these keys) still deserializes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshConfig {
    /// Master enable. OFF by default — a board only meshes when the operator opts
    /// in (and the `lora` firmware feature is built into the image).
    pub enabled: bool,
    /// Region token selecting band + duty envelope: `"na915"` | `"eu868"` | ….
    pub region: String,
    /// Relay role token (see [`RelayRole`]): `"router"` (default) | `"repeater"`
    /// | `"router_late"` | `"client"`.
    pub role: String,
    /// This node uplinks the fleet (pulls internet, rebroadcasts tips/price/blocks).
    pub is_gateway: bool,
    /// 32-byte owner HMAC key as lowercase hex; empty ⇒ **unprovisioned** ⇒ the
    /// command gate refuses ALL over-the-air control (fail-closed).
    pub owner_key_hex: String,
    /// 32-byte AES channel PSK as hex for Phase-2 Meshtastic interop; empty ⇒
    /// none. Reserved — not consumed until the interop pass.
    pub channel_psk_hex: String,
    /// Telemetry beacon cadence (seconds); see [`effective_telemetry_interval_s`](Self::effective_telemetry_interval_s).
    pub telemetry_interval_s: u16,
    /// Phase-4: allow the offline solo tip-relay mining path (see the fork plan).
    pub solo_relay_enabled: bool,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            region: "na915".into(),
            role: RelayRole::Router.as_str().into(),
            is_gateway: false,
            owner_key_hex: String::new(),
            channel_psk_hex: String::new(),
            telemetry_interval_s: DEFAULT_TELEMETRY_INTERVAL_S,
            solo_relay_enabled: false,
        }
    }
}

impl MeshConfig {
    /// Parsed relay role — defaults to [`RelayRole::Router`] if the token is
    /// unrecognized (a mains-powered axe should relay unless told otherwise).
    pub fn role(&self) -> RelayRole {
        RelayRole::from_token(&self.role).unwrap_or_default()
    }

    /// Parsed 32-byte owner key, or `None` when unset/malformed (⇒ the command
    /// gate stays fail-closed and accepts no air control).
    pub fn owner_key(&self) -> Option<[u8; OWNER_KEY_LEN]> {
        parse_key32(&self.owner_key_hex)
    }

    /// Parsed 32-byte channel PSK, or `None` when unset/malformed.
    pub fn channel_psk(&self) -> Option<[u8; 32]> {
        parse_key32(&self.channel_psk_hex)
    }

    /// Telemetry cadence clamped to at least [`MIN_TELEMETRY_INTERVAL_S`].
    pub fn effective_telemetry_interval_s(&self) -> u16 {
        self.telemetry_interval_s.max(MIN_TELEMETRY_INTERVAL_S)
    }

    /// `true` once an owner key is provisioned (air control is possible).
    pub fn is_provisioned(&self) -> bool {
        self.owner_key().is_some()
    }
}

/// Parse exactly 32 bytes of lowercase/uppercase hex, or `None`.
fn parse_key32(hex: &str) -> Option<[u8; 32]> {
    let h = hex.trim();
    if h.len() != KEY_HEX_LEN {
        return None;
    }
    let b = h.as_bytes();
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (hexval(b[2 * i])? << 4) | hexval(b[2 * i + 1])?;
    }
    Some(out)
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_and_router_and_unprovisioned() {
        let c = MeshConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.role(), RelayRole::Router);
        assert!(!c.is_provisioned());
        assert_eq!(c.owner_key(), None);
        assert_eq!(
            c.effective_telemetry_interval_s(),
            DEFAULT_TELEMETRY_INTERVAL_S
        );
    }

    #[test]
    fn serde_round_trips() {
        let c = MeshConfig {
            enabled: true,
            role: "client".into(),
            is_gateway: true,
            owner_key_hex: "ab".repeat(32),
            telemetry_interval_s: 600,
            ..MeshConfig::default()
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: MeshConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
        assert_eq!(back.role(), RelayRole::Client);
        assert!(back.is_provisioned());
    }

    #[test]
    fn legacy_blob_missing_fields_deserializes_to_defaults() {
        // An older NVS blob that predates these fields → all defaults, no error.
        let back: MeshConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(back, MeshConfig::default());
        // A partial blob keeps what it has and defaults the rest.
        let partial: MeshConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.role(), RelayRole::Router);
    }

    #[test]
    fn owner_key_parses_valid_hex_only() {
        let mut c = MeshConfig::default();
        assert_eq!(c.owner_key(), None, "empty ⇒ None");

        c.owner_key_hex = "00".repeat(32);
        assert_eq!(c.owner_key(), Some([0u8; 32]));

        c.owner_key_hex = "FF".repeat(32); // uppercase accepted
        assert_eq!(c.owner_key(), Some([0xFFu8; 32]));

        c.owner_key_hex = "zz".repeat(32); // non-hex
        assert_eq!(c.owner_key(), None);

        c.owner_key_hex = "ab".repeat(31); // wrong length
        assert_eq!(c.owner_key(), None);
    }

    #[test]
    fn role_defaults_on_garbage_token() {
        let c = MeshConfig {
            role: "not_a_role".into(),
            ..MeshConfig::default()
        };
        assert_eq!(c.role(), RelayRole::Router);
    }

    #[test]
    fn telemetry_interval_has_a_floor() {
        let c = MeshConfig {
            telemetry_interval_s: 1,
            ..MeshConfig::default()
        };
        assert_eq!(c.effective_telemetry_interval_s(), MIN_TELEMETRY_INTERVAL_S);
    }
}
