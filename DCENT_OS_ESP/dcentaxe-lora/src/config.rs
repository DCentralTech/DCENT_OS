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
    /// Meshtastic channel PSK as hex (Phase-2 interop). Variable length per
    /// Meshtastic: empty ⇒ the default public channel (well-known key); 1 byte ⇒
    /// a default-key selector; 16 bytes ⇒ AES-128; 32 bytes ⇒ AES-256. Consumed
    /// by [`meshtastic_channel`](Self::meshtastic_channel).
    pub channel_psk_hex: String,
    /// Telemetry beacon cadence (seconds); see [`effective_telemetry_interval_s`](Self::effective_telemetry_interval_s).
    pub telemetry_interval_s: u16,
    /// Phase-4: master enable for offline solo tip-relay mining (default OFF).
    /// Also requires a non-empty [`solo_payout_address`] and
    /// `mining_source == "solo_mesh_empty"`.
    pub solo_relay_enabled: bool,
    /// Work source for mesh solo: `"off"` (default) | `"solo_mesh_empty"`.
    /// Pool mining is the normal stratum path; this only gates mesh tip work.
    pub mining_source: String,
    /// Payout address for solo coinbase (bc1/bcrt1/…). Empty ⇒ solo path
    /// stays disabled even if solo_relay_enabled (fail-closed).
    pub solo_payout_address: String,
    /// Solo chain policy: `"regtest"` (default) | `"testnet"` | `"mainnet"`.
    pub solo_chain: String,

    // ── Phase-2 Meshtastic interop ────────────────────────────────────────────
    /// Run the radio as a **Meshtastic Router** instead of a native `$DCM` node.
    /// A single SX1262 is on ONE sync word/modulation at a time, so this selects
    /// the radio's operating mode. OFF by default (a plain `$DCM` mesh node).
    /// Only honoured when the binary is built with the `meshtastic` feature.
    pub meshtastic_mode: bool,
    /// Meshtastic channel name; empty ⇒ the modem-preset name (e.g. `"LongFast"`),
    /// matching Meshtastic's unnamed-channel behaviour. Feeds the channel hash.
    pub meshtastic_channel_name: String,
    /// Meshtastic modem preset token (`"LongFast"` default | `"ShortFast"` | …);
    /// see [`ModemPreset`](crate::meshtastic::ModemPreset).
    pub meshtastic_preset: String,
    /// Explicit channel centre frequency (Hz); `0` ⇒ use the region LongFast
    /// default. Read it from your Meshtastic app (Radio Config → LoRa) for a
    /// non-LongFast channel — the exact slot is not re-derived here (see `phy`).
    pub meshtastic_freq_hz: u32,
    /// Our Meshtastic short name (≤ 4 chars on the wire); empty ⇒ derived from the
    /// board model by the binary.
    pub meshtastic_short_name: String,
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
            mining_source: "off".into(),
            solo_payout_address: String::new(),
            solo_chain: "regtest".into(),
            meshtastic_mode: false,
            meshtastic_channel_name: String::new(),
            meshtastic_preset: "LongFast".into(),
            meshtastic_freq_hz: 0,
            meshtastic_short_name: String::new(),
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

    /// `true` when operator opted into solo mesh empty-block work.
    /// Fail-closed: requires solo_relay_enabled + mining_source token + payout.
    pub fn solo_mesh_empty_active(&self) -> bool {
        self.solo_relay_enabled
            && self.mining_source.eq_ignore_ascii_case("solo_mesh_empty")
            && !self.solo_payout_address.trim().is_empty()
    }

    /// `true` once an owner key is provisioned (air control is possible).
    pub fn is_provisioned(&self) -> bool {
        self.owner_key().is_some()
    }

    /// The Meshtastic channel PSK as raw bytes: empty hex ⇒ `[1]` (the default
    /// public-channel key selector); otherwise the parsed hex bytes (any length —
    /// [`meshtastic_channel`](Self::meshtastic_channel) validates 1/16/32). Malformed
    /// hex also falls back to the default public key rather than an empty PSK, so a
    /// typo can never silently drop a private channel to *plaintext*.
    pub fn meshtastic_psk_bytes(&self) -> Vec<u8> {
        let h = self.channel_psk_hex.trim();
        if h.is_empty() {
            return vec![1];
        }
        parse_hex_bytes(h).unwrap_or_else(|| vec![1])
    }

    /// The resolved [`ModemPreset`](crate::meshtastic::ModemPreset) — defaults to
    /// `LongFast` if the token is unrecognized.
    #[cfg(feature = "meshtastic-interop")]
    pub fn meshtastic_modem_preset(&self) -> crate::meshtastic::ModemPreset {
        crate::meshtastic::ModemPreset::from_name(&self.meshtastic_preset).unwrap_or_default()
    }

    /// The resolved Meshtastic [`Channel`](crate::meshtastic::Channel), or `None`
    /// when the PSK length is invalid (⇒ the caller should not enter Meshtastic
    /// mode). An empty channel name uses the preset name for the channel hash.
    #[cfg(feature = "meshtastic-interop")]
    pub fn meshtastic_channel(&self) -> Option<crate::meshtastic::Channel> {
        crate::meshtastic::Channel::new(&self.meshtastic_channel_name, &self.meshtastic_psk_bytes())
    }

    /// The resolved [`MeshtasticPhyConfig`](crate::meshtastic::MeshtasticPhyConfig)
    /// to program the radio. Uses the configured frequency, or `default_freq_hz`
    /// (the caller-supplied region LongFast default) when `meshtastic_freq_hz == 0`.
    #[cfg(feature = "meshtastic-interop")]
    pub fn meshtastic_phy(&self, default_freq_hz: u32) -> crate::meshtastic::MeshtasticPhyConfig {
        let freq = if self.meshtastic_freq_hz != 0 {
            self.meshtastic_freq_hz
        } else {
            default_freq_hz
        };
        crate::meshtastic::MeshtasticPhyConfig::new(self.meshtastic_modem_preset(), freq)
    }
}

/// Parse an even-length hex string into bytes, or `None` on odd length / non-hex.
fn parse_hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let h = hex.trim();
    if h.len() % 2 != 0 {
        return None;
    }
    let b = h.as_bytes();
    let mut out = Vec::with_capacity(h.len() / 2);
    for i in 0..h.len() / 2 {
        out.push((hexval(b[2 * i])? << 4) | hexval(b[2 * i + 1])?);
    }
    Some(out)
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
        assert!(!c.solo_mesh_empty_active(), "solo mesh fail-closed by default");
        assert_eq!(c.mining_source, "off");
        assert_eq!(c.solo_chain, "regtest");
        assert_eq!(c.owner_key(), None);
        assert_eq!(
            c.effective_telemetry_interval_s(),
            DEFAULT_TELEMETRY_INTERVAL_S
        );
    }

    #[test]
    fn solo_mesh_empty_requires_all_three_gates() {
        let mut c = MeshConfig::default();
        c.solo_relay_enabled = true;
        assert!(!c.solo_mesh_empty_active());
        c.mining_source = "solo_mesh_empty".into();
        assert!(!c.solo_mesh_empty_active());
        c.solo_payout_address = "bcrt1qtest".into();
        assert!(c.solo_mesh_empty_active());
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

    #[test]
    fn meshtastic_defaults_off_and_longfast() {
        let c = MeshConfig::default();
        assert!(!c.meshtastic_mode);
        assert_eq!(c.meshtastic_preset, "LongFast");
        assert_eq!(c.meshtastic_freq_hz, 0);
    }

    #[test]
    fn meshtastic_psk_bytes_defaults_to_public_key_selector() {
        let mut c = MeshConfig::default();
        // Empty ⇒ the [1] default public-channel selector.
        assert_eq!(c.meshtastic_psk_bytes(), vec![1]);
        // Explicit 1-byte selector.
        c.channel_psk_hex = "01".into();
        assert_eq!(c.meshtastic_psk_bytes(), vec![1]);
        // A 32-byte private key parses to its 32 bytes.
        c.channel_psk_hex = "ab".repeat(32);
        assert_eq!(c.meshtastic_psk_bytes(), vec![0xab; 32]);
        // Malformed hex falls back to the public selector (never plaintext).
        c.channel_psk_hex = "zz".into();
        assert_eq!(c.meshtastic_psk_bytes(), vec![1]);
        c.channel_psk_hex = "abc".into(); // odd length
        assert_eq!(c.meshtastic_psk_bytes(), vec![1]);
    }

    #[test]
    fn meshtastic_fields_round_trip_and_legacy_blob_defaults_them() {
        let c = MeshConfig {
            meshtastic_mode: true,
            meshtastic_channel_name: "MyMesh".into(),
            meshtastic_preset: "ShortFast".into(),
            meshtastic_freq_hz: 906_875_000,
            meshtastic_short_name: "DCAX".into(),
            ..MeshConfig::default()
        };
        let back: MeshConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c, back);
        // A legacy blob without the meshtastic keys defaults them (no error).
        let legacy: MeshConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(!legacy.meshtastic_mode);
        assert_eq!(legacy.meshtastic_preset, "LongFast");
    }

    #[cfg(feature = "meshtastic-interop")]
    #[test]
    fn meshtastic_resolvers_produce_typed_values() {
        use crate::meshtastic::ModemPreset;

        // Default: unnamed LongFast channel on the default public key → hash 0x08.
        let c = MeshConfig::default();
        assert_eq!(c.meshtastic_modem_preset(), ModemPreset::LongFast);
        let ch = c.meshtastic_channel().expect("default channel resolves");
        assert_eq!(ch.name, "LongFast");
        assert_eq!(ch.hash(), 0x08);

        // freq default is used when meshtastic_freq_hz == 0.
        let phy = c.meshtastic_phy(906_875_000);
        assert_eq!(phy.freq_hz, 906_875_000);
        assert_eq!(phy.sf, 11);

        // An explicit frequency overrides the default; preset token honoured.
        let c2 = MeshConfig {
            meshtastic_preset: "ShortFast".into(),
            meshtastic_freq_hz: 869_525_000,
            ..MeshConfig::default()
        };
        assert_eq!(c2.meshtastic_modem_preset(), ModemPreset::ShortFast);
        assert_eq!(c2.meshtastic_phy(906_875_000).freq_hz, 869_525_000);

        // A custom named + 32-byte-PSK channel resolves to AES-256.
        let c3 = MeshConfig {
            meshtastic_channel_name: "Private".into(),
            channel_psk_hex: "5a".repeat(32),
            ..MeshConfig::default()
        };
        let ch3 = c3.meshtastic_channel().unwrap();
        assert_eq!(ch3.name, "Private");
        assert!(ch3.key.is_encrypted());

        // An invalid PSK length ⇒ no channel (caller must not enter mesh mode).
        let bad = MeshConfig {
            channel_psk_hex: "abcd".into(), // 2 bytes → invalid Meshtastic PSK len
            ..MeshConfig::default()
        };
        assert!(bad.meshtastic_channel().is_none());
    }
}
