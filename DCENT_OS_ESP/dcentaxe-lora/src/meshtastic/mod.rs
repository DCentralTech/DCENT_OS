// SPDX-License-Identifier: GPL-3.0-or-later
//! Meshtastic interop (fork plan §4.4 option B) — **first-class, no longer a
//! stub.** This turns a DCENT_axe from an island speaking only the native `$DCM`
//! grammar into a genuine **Router** on any existing Meshtastic mesh: it programs
//! the same PHY ([`phy`]), packs the same 16-byte [`header`], encrypts with the
//! same shared-PSK AES-CTR ([`channel`]), speaks the same protobuf [`wire`]
//! envelope, and rebroadcasts with the same managed-flood discipline ([`router`]).
//!
//! ## The "make mesh viable in the first place" multiplier
//! A DCENT_axe is mains-powered and always-on — exactly the never-sleeping
//! infrastructure a battery-starved LoRa mesh lacks. By joining the *existing*
//! Meshtastic ecosystem instead of building a private island, every miner owner
//! donates a free, permanent Router that strengthens the whole off-grid network.
//!
//! ## Scope + honest boundaries (Phase 2)
//! Implemented + host-tested: channel crypto (NIST-KAT'd), the `Data`/`User`/
//! `Position` protobufs, the packet header, the modem PHY, and the managed router.
//! **Deliberately deferred / gated** (documented, not silently missing):
//!   * The channel-frequency *slot hash* is not re-derived — the operator supplies
//!     the frequency (see [`phy`]); we ship the well-known defaults.
//!   * PKC / admin (public-key) messages are out of scope — owner *control* over
//!     the mesh stays on the HMAC-authenticated `$DCM` path ([`crate::auth`]).
//!   * The 1-byte channel hash can collide; [`decode_packet`] takes the first
//!     matching-hash channel that yields a decodable `Data` (upstream has the same
//!     limitation). On-air multi-node validation is the true proof and remains a
//!     hardware-gated follow-up.

pub mod channel;
pub mod header;
pub mod phy;
pub mod router;
pub mod wire;

pub use channel::{Channel, ChannelKey, DEFAULT_CHANNEL_NAME, DEFAULT_KEY};
pub use header::{PacketHeader, BROADCAST_ADDR, HEADER_LEN};
pub use phy::{MeshtasticPhyConfig, ModemPreset, SYNC_WORD_MESHTASTIC};
pub use router::{MeshtasticRouter, RelayDecision, RelayPacket};
pub use wire::{hw_model, portnum, Data, Position, User, WireError};

use crate::mesh::Identify;
use crate::LoraError;

/// LoRa PHY hard cap on one packet (header + encrypted payload).
pub const MAX_PACKET_LEN: usize = 255;

/// A packet decoded for LOCAL processing: the header (always), plus the decoded
/// `Data` and channel name when we hold a matching channel key. `encrypted_payload`
/// is retained so an unreadable packet can still be relayed verbatim (a Router
/// forwards channels it cannot read).
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPacket {
    pub header: PacketHeader,
    /// Name of the channel that decrypted this packet, if any.
    pub channel_name: Option<String>,
    /// The decoded `Data`, if a held channel key decrypted it.
    pub data: Option<Data>,
    /// The still-encrypted payload bytes (for blind relay / topology).
    pub encrypted_payload: Vec<u8>,
}

/// Format a NodeNum as Meshtastic's canonical id string (`"!deadbeef"`).
pub fn node_id_str(node: u32) -> String {
    format!("!{node:08x}")
}

/// Build a Meshtastic [`User`] (NodeInfo) for this node. `short_name` is
/// truncated to Meshtastic's 4-char limit; `hw_model` is honestly `PRIVATE_HW`
/// (a DCENT_axe is not a registered Meshtastic hardware model).
pub fn build_user(node: u32, long_name: &str, short_name: &str) -> User {
    User {
        id: node_id_str(node),
        long_name: long_name.to_string(),
        short_name: short_name.chars().take(4).collect(),
        hw_model: hw_model::PRIVATE_HW,
        is_licensed: false,
    }
}

/// Bridge our native `$DCM` [`Identify`] beacon into a Meshtastic NodeInfo
/// [`User`] so a DCENT_axe appears by name in every stock Meshtastic client's
/// node list. The device model becomes the long name.
pub fn user_from_identify(node: u32, identify: &Identify, short_name: &str) -> User {
    build_user(node, &identify.device_model, short_name)
}

/// Wrap a [`User`] into a `Data` on [`portnum::NODEINFO_APP`].
pub fn nodeinfo_data(user: &User) -> Data {
    Data::new(portnum::NODEINFO_APP, user.encode())
}

/// A human-readable text message `Data` on [`portnum::TEXT_MESSAGE_APP`] — the
/// universal portnum every Meshtastic client renders, so a miner-status line is
/// visible to ANY node on the channel.
pub fn text_data(msg: &str) -> Data {
    Data::new(portnum::TEXT_MESSAGE_APP, msg.as_bytes().to_vec())
}

/// Carry an opaque native `$DCM` payload on the DCENT-private portnum
/// ([`portnum::DCENT_DCM_APP`]) for structured DCENT↔DCENT telemetry over a
/// Meshtastic mesh (Phase 3 hook). Stock nodes relay it but ignore its contents.
pub fn dcm_private_data(payload: &[u8]) -> Data {
    Data::new(portnum::DCENT_DCM_APP, payload.to_vec())
}

/// Assemble a full on-air Meshtastic packet: stamp the channel hash into the
/// header, protobuf-encode + channel-encrypt the `Data`, and prepend the header.
/// Errors if the result exceeds the LoRa [`MAX_PACKET_LEN`].
pub fn encode_packet(
    ch: &Channel,
    mut header: PacketHeader,
    data: &Data,
) -> Result<Vec<u8>, LoraError> {
    header.channel = ch.hash();
    let mut payload = data.encode();
    ch.encrypt(header.from, header.id, &mut payload);
    let total = HEADER_LEN + payload.len();
    if total > MAX_PACKET_LEN {
        return Err(LoraError::Protocol(format!(
            "meshtastic packet {total} > {MAX_PACKET_LEN} bytes"
        )));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Convenience: a broadcast packet from `from`/`id` on channel `ch` with
/// `hop_limit` hops carrying `data`.
pub fn build_broadcast(
    ch: &Channel,
    from: u32,
    id: u32,
    hop_limit: u8,
    data: &Data,
) -> Result<Vec<u8>, LoraError> {
    let header = PacketHeader::broadcast(from, id, ch.hash(), hop_limit);
    encode_packet(ch, header, data)
}

/// Decode a received packet for local processing. The header is always returned
/// (a Router needs it even for channels it cannot read); `data`/`channel_name`
/// are filled when a held channel key with the matching hash decodes the payload.
/// Returns `None` only for a runt frame shorter than the 16-byte header.
pub fn decode_packet(channels: &[Channel], bytes: &[u8]) -> Option<DecodedPacket> {
    let header = PacketHeader::decode(bytes)?;
    let encrypted_payload = bytes[HEADER_LEN..].to_vec();
    for ch in channels {
        if ch.hash() == header.channel {
            let mut buf = encrypted_payload.clone();
            ch.decrypt(header.from, header.id, &mut buf);
            if let Ok(data) = Data::decode(&buf) {
                return Some(DecodedPacket {
                    header,
                    channel_name: Some(ch.name.clone()),
                    data: Some(data),
                    encrypted_payload,
                });
            }
        }
    }
    Some(DecodedPacket {
        header,
        channel_name: None,
        data: None,
        encrypted_payload,
    })
}

/// A Meshtastic peer learned off the air (from a decoded NodeInfo), for the mesh
/// view. Distinct from the native-`$DCM` [`crate::mesh::Peer`] because a
/// Meshtastic node carries a long/short name + hardware model rather than the
/// DCENT device/asic strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshtasticNode {
    pub node: u32,
    pub long_name: String,
    pub short_name: String,
    pub hw_model: u32,
    pub hops_away: u8,
    pub rssi_dbm: i16,
    pub snr_db: i8,
}

impl MeshtasticNode {
    /// Build a peer record from a decoded NodeInfo packet + its link quality.
    pub fn from_nodeinfo(header: &PacketHeader, user: &User, rssi_dbm: i16, snr_db: i8) -> Self {
        Self {
            node: header.from,
            long_name: user.long_name.clone(),
            short_name: user.short_name.clone(),
            hw_model: user.hw_model,
            hops_away: header.hops_away(),
            rssi_dbm,
            snr_db,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flood::RelayRole;

    // ---- full packet round-trip on the default channel ----

    #[test]
    fn nodeinfo_packet_round_trips_on_default_channel() {
        let ch = Channel::default_primary();
        let user = build_user(0x0000_00a1, "DCENT_axe Hex BM1397", "DCAX");
        let bytes = build_broadcast(&ch, 0x0000_00a1, 0x1234, 3, &nodeinfo_data(&user)).unwrap();

        // Header stamped with the LongFast channel hash (0x08).
        let header = PacketHeader::decode(&bytes).unwrap();
        assert_eq!(header.channel, 0x08);
        assert_eq!(header.from, 0x0000_00a1);
        assert!(header.is_broadcast());

        // Decode with the held channel → recover the NodeInfo.
        let dec = decode_packet(&[ch.clone()], &bytes).unwrap();
        assert_eq!(dec.channel_name.as_deref(), Some("LongFast"));
        let data = dec.data.expect("decoded Data");
        assert_eq!(data.portnum, portnum::NODEINFO_APP);
        let back = User::decode(&data.payload).unwrap();
        assert_eq!(back, user);
    }

    #[test]
    fn text_message_packet_round_trips() {
        let ch = Channel::default_primary();
        let bytes =
            build_broadcast(&ch, 0xbeef, 7, 3, &text_data("gm from a space heater")).unwrap();
        let dec = decode_packet(&[ch], &bytes).unwrap();
        let data = dec.data.unwrap();
        assert_eq!(data.portnum, portnum::TEXT_MESSAGE_APP);
        assert_eq!(data.payload, b"gm from a space heater");
    }

    #[test]
    fn wrong_channel_leaves_data_unreadable_but_header_relayable() {
        let sender = Channel::default_primary();
        let bytes = build_broadcast(&sender, 0xa1, 1, 3, &text_data("secret")).unwrap();
        // A node holding only a *different* channel cannot read it, but still gets
        // the header for relay/topology (the Router-relays-blind property).
        let other = Channel::new("Private", &[0x99u8; 32]).unwrap();
        assert_ne!(other.hash(), sender.hash());
        let dec = decode_packet(&[other], &bytes).unwrap();
        assert!(dec.data.is_none());
        assert!(dec.channel_name.is_none());
        assert_eq!(dec.header.from, 0xa1);
        assert!(!dec.encrypted_payload.is_empty());
    }

    #[test]
    fn decode_rejects_runt_frame() {
        assert!(decode_packet(&[Channel::default_primary()], &[0u8; 10]).is_none());
    }

    #[test]
    fn oversize_packet_is_rejected() {
        let ch = Channel::default_primary();
        // A payload that pushes header+payload past 255 bytes must error.
        let big = vec![0xABu8; 260];
        let data = Data::new(portnum::PRIVATE_APP, big);
        assert!(matches!(
            build_broadcast(&ch, 1, 1, 3, &data),
            Err(LoraError::Protocol(_))
        ));
    }

    // ---- DCENT ↔ Meshtastic bridge ----

    #[test]
    fn node_id_string_is_meshtastic_format() {
        assert_eq!(node_id_str(0x0000_00a1), "!000000a1");
        assert_eq!(node_id_str(0xdead_beef), "!deadbeef");
    }

    #[test]
    fn identify_bridges_to_nodeinfo() {
        let id = Identify {
            device_model: "DCENT_axe Gamma".into(),
            asic_model: "BM1370".into(),
        };
        let user = user_from_identify(0x0102_0304, &id, "GAMM");
        assert_eq!(user.id, "!01020304");
        assert_eq!(user.long_name, "DCENT_axe Gamma");
        assert_eq!(user.short_name, "GAMM");
        assert_eq!(user.hw_model, hw_model::PRIVATE_HW);
    }

    #[test]
    fn short_name_truncated_to_four_chars() {
        let u = build_user(1, "Long Name Here", "TOOLONG");
        assert_eq!(u.short_name, "TOOL");
    }

    #[test]
    fn meshtastic_node_from_nodeinfo_captures_hops_and_link() {
        let ch = Channel::default_primary();
        let user = build_user(0x77, "Solar Node", "SOLR");
        let bytes = build_broadcast(&ch, 0x77, 5, 3, &nodeinfo_data(&user)).unwrap();
        // Simulate the packet having travelled one hop before we heard it.
        let mut header = PacketHeader::decode(&bytes).unwrap();
        header.hop_limit = 2; // hop_start 3, hop_limit 2 → 1 hop away
        let node = MeshtasticNode::from_nodeinfo(&header, &user, -95, -3);
        assert_eq!(node.node, 0x77);
        assert_eq!(node.long_name, "Solar Node");
        assert_eq!(node.hops_away, 1);
        assert_eq!((node.rssi_dbm, node.snr_db), (-95, -3));
    }

    // ---- end-to-end: encode → radio bytes → router relay ----

    #[test]
    fn packet_flows_through_router_and_relays_hop_decremented() {
        let ch = Channel::default_primary();
        let own: u32 = 0x0000_00a1;
        let src: u32 = 0x0000_00b2;

        // A neighbour originates a broadcast text.
        let bytes = build_broadcast(&ch, src, 42, 3, &text_data("relay me")).unwrap();

        // Our node receives it: split header/payload and route it.
        let header = PacketHeader::decode(&bytes).unwrap();
        let payload = &bytes[HEADER_LEN..];
        let mut router = MeshtasticRouter::new(own, RelayRole::Router);
        assert!(matches!(
            router.on_receive(&header, payload, -8.0, 1_000),
            RelayDecision::Scheduled { .. }
        ));

        // At its slot the relay is emitted, hop-decremented + relay-stamped.
        let out = router.due(1_000_000);
        assert_eq!(out.len(), 1);
        let relayed = out[0].to_bytes();
        let rhdr = PacketHeader::decode(&relayed).unwrap();
        assert_eq!(rhdr.hop_limit, 2);
        assert_eq!(rhdr.relay_node, (own & 0xFF) as u8);
        assert_eq!(rhdr.from, src, "originator preserved across relay");

        // And the relayed payload still decrypts to the original text — a relay
        // forwards the ciphertext untouched, so downstream nodes read it fine.
        let dec = decode_packet(&[ch], &relayed).unwrap();
        assert_eq!(dec.data.unwrap().payload, b"relay me");
    }
}
